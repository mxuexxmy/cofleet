use anyhow::Result;
use clap::Parser;
use cofleet::{
    auth::AuthMode,
    config::{Config, data_dir},
    ipc,
    protocol::{
        AgentMessage, AgentStatus, Heartbeat, HubMessage, LocalRequest, LocalResponse,
        MAX_FRAME_BYTES, PROTOCOL_VERSION, unix_now,
    },
};
use futures_util::{SinkExt, StreamExt};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::{Mutex, Notify};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, client::IntoClientRequest, protocol::WebSocketConfig},
};

#[derive(Parser)]
#[command(about = "CoFleet 后台节点 Agent")]
struct Args {
    #[arg(long, default_value_os_t = data_dir())]
    data_dir: PathBuf,
}
struct State {
    config: Config,
    status: AgentStatus,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    let config = Config::load(&args.data_dir)?;
    let mut listener = ipc::Listener::bind(&args.data_dir, &config)?;
    let state = Arc::new(Mutex::new(State {
        status: AgentStatus {
            device_id: config.device_id,
            name: config.name.clone(),
            manager_url: config.manager_url.clone(),
            accepting_tasks: config.accepting_tasks,
            connected: false,
            last_success_unix: None,
            last_error: None,
        },
        config,
    }));
    let notify = Arc::new(Notify::new());
    let heartbeat = tokio::spawn(heartbeat_loop(state.clone(), notify.clone()));
    eprintln!("Agent started; no task execution is enabled in this prototype");
    let slots = Arc::new(tokio::sync::Semaphore::new(16));
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            result = listener.accept() => {
                let mut stream = result?;
                let Ok(permit) = slots.clone().try_acquire_owned() else { continue; };
                let state = state.clone(); let notify = notify.clone(); let dir = args.data_dir.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let result: Result<()> = async {
                        let command: LocalRequest = ipc::read_json(&mut stream).await?;
                        let mut state = state.lock().await;
                        let mut error = None;
                        let paused = match command { LocalRequest::Pause => Some(false), LocalRequest::Resume => Some(true), LocalRequest::Status => None };
                        if let Some(accepting) = paused {
                            // Reload before updating to preserve offline edits, but require a restart for connection/identity changes.
                            match Config::load(&dir) {
                                Ok(mut config) if config.device_id == state.config.device_id && config.manager_url == state.config.manager_url && config.token == state.config.token && config.auth == state.config.auth && config.lzc_core_config == state.config.lzc_core_config && config.heartbeat_secs == state.config.heartbeat_secs && config.name == state.config.name => {
                                    config.accepting_tasks = accepting;
                                    match config.save(&dir) {
                                        Ok(()) => { state.config = config; state.status.accepting_tasks = accepting; notify.notify_one(); },
                                        Err(_) => error = Some("cannot persist setting; state unchanged".into()),
                                    }
                                }
                                _ => error = Some("configuration changed or unreadable; restart Agent before changing state".into()),
                            }
                        }
                        let response = LocalResponse { status: Some(state.status.clone()), error };
                        drop(state);
                        ipc::write_json(&mut stream, &response).await
                    }.await;
                    if result.is_err() { eprintln!("Local control request failed"); }
                });
            }
            _ = &mut shutdown => break,
        }
    }
    heartbeat.abort();
    eprintln!("Agent stopped");
    Ok(())
}

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

fn heartbeat(config: &Config) -> Heartbeat {
    Heartbeat {
        device_id: config.device_id,
        name: config.name.clone(),
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        accepting_tasks: config.accepting_tasks,
        heartbeat_secs: config.heartbeat_secs,
        version: env!("CARGO_PKG_VERSION").into(),
    }
}
async fn mark_success(state: &Arc<Mutex<State>>) {
    let mut state = state.lock().await;
    state.status.connected = true;
    state.status.last_success_unix = Some(unix_now());
    state.status.last_error = None;
}
async fn heartbeat_loop(state: Arc<Mutex<State>>, notify: Arc<Notify>) {
    let mut failures = 0u32;
    loop {
        let config = state.lock().await.config.clone();
        let result = control_session(&config, state.clone(), notify.clone()).await;
        let mut state_guard = state.lock().await;
        if state_guard.status.connected {
            failures = 0;
        }
        state_guard.status.connected = false;
        state_guard.status.last_error = Some(
            if result.is_err() {
                "control connection unavailable; check manager, login and TLS"
            } else {
                "control connection closed; reconnecting"
            }
            .into(),
        );
        drop(state_guard);
        failures = failures.saturating_add(1);
        let delay = config
            .heartbeat_secs
            .saturating_mul(1 << failures.min(4))
            .min(60);
        let jitter_ms = u64::from(uuid::Uuid::new_v4().as_bytes()[0]) * 4;
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(delay * 1000 + jitter_ms)) => {},
            _ = notify.notified() => {},
        }
    }
}

async fn control_session(
    config: &Config,
    state: Arc<Mutex<State>>,
    notify: Arc<Notify>,
) -> Result<()> {
    let mut url = reqwest::Url::parse(&config.manager_url)?;
    let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
    url.set_scheme(scheme)
        .map_err(|_| anyhow::anyhow!("invalid control URL"))?;
    url.set_path(&format!("{}/api/ws", url.path().trim_end_matches('/')));
    let mut request = url.as_str().into_client_request()?;
    let mut login_cookie = None;
    match config.auth {
        AuthMode::Development => {
            request
                .headers_mut()
                .insert("authorization", format!("Bearer {}", config.token).parse()?);
        }
        AuthMode::Lazycat => {
            // Re-read local client state on every reconnect. Cookies stay in memory.
            let cookie = cofleet::lazycat::session_cookie(
                &config.manager_url,
                config.lzc_core_config.as_deref(),
            )
            .await?;
            request.headers_mut().insert("cookie", cookie.parse()?);
            login_cookie = Some(cookie);
        }
    }
    let limits = WebSocketConfig::default()
        .max_message_size(Some(MAX_FRAME_BYTES))
        .max_frame_size(Some(MAX_FRAME_BYTES));
    let (mut socket, _) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::connect_async_with_config(request, Some(limits), true),
    )
    .await??;
    let hello = AgentMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        state: heartbeat(config),
    };
    tokio::time::timeout(
        Duration::from_secs(5),
        socket.send(Message::Text(serde_json::to_string(&hello)?.into())),
    )
    .await??;
    let welcome = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await?
        .ok_or_else(|| anyhow::anyhow!("hub closed before welcome"))??;
    let Message::Text(text) = welcome else {
        anyhow::bail!("invalid welcome");
    };
    anyhow::ensure!(
        matches!(
            serde_json::from_str::<HubMessage>(&text)?,
            HubMessage::Welcome {
                protocol_version: PROTOCOL_VERSION,
                ..
            }
        ),
        "unsupported hub protocol"
    );
    mark_success(&state).await;
    let (mut sink, mut stream) = socket.split();
    let mut interval = tokio::time::interval(Duration::from_secs(config.heartbeat_secs));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    interval.tick().await;
    let mut auth_check = tokio::time::interval(Duration::from_secs(60));
    auth_check.tick().await;
    let idle_secs = (config.heartbeat_secs * 3).max(15);
    let idle = tokio::time::sleep(Duration::from_secs(idle_secs));
    tokio::pin!(idle);
    loop {
        let message = tokio::select! {
            frame = stream.next() => {
                let Some(frame) = frame else { return Ok(()); };
                match frame? {
                    Message::Text(text) => match serde_json::from_str::<HubMessage>(&text)? {
                        HubMessage::Ack { .. } => { mark_success(&state).await; idle.as_mut().reset(tokio::time::Instant::now()+Duration::from_secs(idle_secs)); continue; },
                        HubMessage::RequestReport { request_id } => AgentMessage::Report { request_id, state: heartbeat(&state.lock().await.config) },
                        _ => anyhow::bail!("unexpected hub message"),
                    },
                    Message::Ping(bytes) => { tokio::time::timeout(Duration::from_secs(5),sink.send(Message::Pong(bytes))).await??; continue; },
                    Message::Pong(_) => continue,
                    Message::Close(_) => return Ok(()),
                    _ => anyhow::bail!("unsupported hub frame"),
                }
            }
            _ = auth_check.tick(), if login_cookie.is_some() => {
                let current = cofleet::lazycat::session_cookie(&config.manager_url, config.lzc_core_config.as_deref()).await?;
                anyhow::ensure!(login_cookie.as_deref() == Some(current.as_str()), "LazyCat login changed; reconnect required");
                continue;
            }
            _ = interval.tick() => AgentMessage::Heartbeat { state: heartbeat(&state.lock().await.config) },
            _ = notify.notified() => AgentMessage::Heartbeat { state: heartbeat(&state.lock().await.config) },
            _ = &mut idle => anyhow::bail!("hub acknowledgement timeout"),
        };
        send_message(&mut sink, &message).await?;
    }
}
async fn send_message(
    sink: &mut futures_util::stream::SplitSink<Socket, Message>,
    message: &AgentMessage,
) -> Result<()> {
    tokio::time::timeout(
        Duration::from_secs(5),
        sink.send(Message::Text(serde_json::to_string(message)?.into())),
    )
    .await??;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
