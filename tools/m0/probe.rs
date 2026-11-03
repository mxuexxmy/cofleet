//! Isolated M0 gateway/node probe. Not included in normal manager/agent builds.
mod proxy;
use anyhow::{Result, ensure};
use axum::{
    Router,
    extract::{
        ConnectInfo, Request, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::any,
};
use clap::{Parser, ValueEnum};
use cofleet::{
    auth::{AuthMode, Authenticator},
    config::Config,
    workspace_proxy::{PendingTunnel, agent_tunnel, serve_tunnel},
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, Semaphore, mpsc, oneshot};
use uuid::Uuid;
#[derive(Parser)]
struct Args {
    #[arg(value_enum)]
    role: Role,
    #[arg(long)]
    config: PathBuf,
    #[arg(long, default_value = "127.0.0.1:9768")]
    listen: SocketAddr,
    #[arg(long)]
    node_config: Option<PathBuf>,
}
#[derive(Clone, ValueEnum)]
enum Role {
    Gateway,
    Node,
}
#[derive(Clone, Deserialize)]
struct Settings {
    domain: String,
    owner: String,
    key: String,
    device: Uuid,
    login_port: u16,
    static_port: u16,
    tls_port: u16,
}
#[derive(Clone, Copy, Serialize, Deserialize)]
enum Target {
    Login,
    Static,
    BadTls,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Command {
    Welcome { session: Uuid },
    Open { id: Uuid, target: Target },
}
struct Node {
    session: Uuid,
    sender: mpsc::Sender<Command>,
    cancel: tokio::sync::watch::Sender<bool>,
}
struct Pending {
    tunnel: PendingTunnel,
}
#[derive(Clone)]
struct App {
    settings: Settings,
    node: Arc<Mutex<Option<Node>>>,
    pending: Arc<Mutex<HashMap<Uuid, Pending>>>,
    slots: Arc<Semaphore>,
}
impl App {
    fn host(&self, request: &Request) -> String {
        request
            .headers()
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase()
    }
    fn owner(&self, request: &Request, peer: SocketAddr) -> Result<(), StatusCode> {
        let auth = Authenticator {
            mode: AuthMode::Lazycat,
            token: String::new(),
            trusted_ingress: vec!["127.0.0.1".parse().unwrap()],
        };
        if auth.identify(request.headers(), peer.ip())? != self.settings.owner {
            return Err(StatusCode::NOT_FOUND);
        }
        Ok(())
    }
    fn key(&self, request: &Request) -> bool {
        request
            .headers()
            .get("x-cofleet-m0-key")
            .is_some_and(|v| v.as_bytes() == self.settings.key.as_bytes())
    }
    async fn stream(&self, target: Target) -> Result<tokio::io::DuplexStream, StatusCode> {
        let (session, sender) = {
            let guard = self.node.lock().await;
            let node = guard.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
            (node.session, node.sender.clone())
        };
        let a = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
        let b = Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap();
        let (front, back) = tokio::io::duplex(32 * 1024);
        let (ready, wait) = oneshot::channel();
        let id = Uuid::new_v4();
        self.pending.lock().await.insert(
            id,
            Pending {
                tunnel: PendingTunnel {
                    user: self.settings.owner.clone(),
                    device: self.settings.device,
                    session,
                    stream: back,
                    ready,
                    created: Instant::now(),
                    permits: (a, b),
                },
            },
        );
        let sent = sender.try_send(Command::Open { id, target }); // caller revalidates session when claiming
        if sent.is_err() {
            self.pending.lock().await.remove(&id);
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        let result = tokio::time::timeout(Duration::from_secs(15), wait).await;
        self.pending.lock().await.remove(&id);
        result
            .map_err(|_| StatusCode::GATEWAY_TIMEOUT)?
            .map_err(|_| StatusCode::BAD_GATEWAY)?;
        Ok(front)
    }
}
async fn control(app: App, socket: WebSocket) {
    let (mut sink, mut source) = socket.split();
    let session = Uuid::new_v4();
    let (tx, mut rx) = mpsc::channel(16);
    let (cancel, _) = tokio::sync::watch::channel(false);
    if let Some(old) = app.node.lock().await.replace(Node {
        session,
        sender: tx,
        cancel: cancel.clone(),
    }) {
        let _ = old.cancel.send(true);
    }
    let _ = sink
        .send(Message::Text(
            serde_json::to_string(&Command::Welcome { session })
                .unwrap()
                .into(),
        ))
        .await;
    let mut ping = tokio::time::interval(Duration::from_secs(10));
    loop {
        let result = tokio::select! { Some(command)=rx.recv()=>sink.send(Message::Text(serde_json::to_string(&command).unwrap().into())).await, _=ping.tick()=>sink.send(Message::Ping(Vec::new().into())).await, msg=source.next()=>{match msg {Some(Ok(Message::Ping(bytes)))=>sink.send(Message::Pong(bytes)).await,Some(Ok(Message::Pong(_)))=>continue,_=>break}}};
        if result.is_err() {
            break;
        }
    }
    let mut node = app.node.lock().await;
    if node.as_ref().is_some_and(|v| v.session == session) {
        *node = None;
    }
    let _ = cancel.send(true);
    app.pending
        .lock()
        .await
        .retain(|_, v| v.tunnel.session != session);
}
async fn route(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
) -> Result<Response, StatusCode> {
    app.owner(&request, peer)?;
    let host = app.host(&request);
    let base = &app.settings.domain;
    if request
        .headers()
        .get(header::ORIGIN)
        .is_some_and(|v| v.as_bytes() != format!("https://{host}").as_bytes())
    {
        return Err(StatusCode::FORBIDDEN);
    }
    if host == *base {
        if request.uri().path() == "/api/m0/node" && app.key(&request) {
            let (mut parts, _) = request.into_parts();
            let ws = WebSocketUpgrade::from_request_parts(&mut parts, &())
                .await
                .map_err(|_| StatusCode::BAD_REQUEST)?;
            return Ok(ws
                .max_message_size(16 * 1024)
                .on_upgrade(move |socket| control(app, socket))
                .into_response());
        }
        if request.uri().path() == "/api/m0/status" {
            return Ok(axum::Json(serde_json::json!({"host":host,"node_connected":app.node.lock().await.is_some(),"experiment":"m0"})).into_response());
        }
        if let Some(value) = request.uri().path().strip_prefix("/api/workspace-tunnel/") {
            let mut parts = value.split('/');
            let device = parts
                .next()
                .and_then(|v| Uuid::parse_str(v).ok())
                .ok_or(StatusCode::NOT_FOUND)?;
            let id = parts
                .next()
                .and_then(|v| Uuid::parse_str(v).ok())
                .ok_or(StatusCode::NOT_FOUND)?;
            if parts.next().is_some() || device != app.settings.device {
                return Err(StatusCode::NOT_FOUND);
            }
            let session = request
                .headers()
                .get("x-cofleet-session")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| Uuid::parse_str(v).ok())
                .ok_or(StatusCode::FORBIDDEN)?;
            if !request
                .headers()
                .get(header::COOKIE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| {
                    v.split(';').any(|part| {
                        part.trim() == format!("cofleet_m0_tunnel={}", app.settings.key)
                    })
                })
            {
                return Err(StatusCode::FORBIDDEN);
            }
            let mut revoked = {
                let guard = app.node.lock().await;
                let node = guard
                    .as_ref()
                    .filter(|n| n.session == session)
                    .ok_or(StatusCode::FORBIDDEN)?;
                node.cancel.subscribe()
            };
            let mut pending = app.pending.lock().await;
            let valid = pending.get(&id).is_some_and(|p| {
                p.tunnel.session == session && p.tunnel.created.elapsed() < Duration::from_secs(15)
            });
            if !valid {
                return Err(StatusCode::NOT_FOUND);
            }
            let tunnel = pending.remove(&id).unwrap().tunnel;
            drop(pending);
            let (mut parts, _) = request.into_parts();
            let ws = WebSocketUpgrade::from_request_parts(&mut parts, &())
                .await
                .map_err(|_| StatusCode::BAD_REQUEST)?;
            return Ok(ws
                .max_message_size(16 * 1024)
                .on_upgrade(move |socket| async move {
                    if *revoked.borrow() {
                        return;
                    }
                    tokio::select! {_=serve_tunnel(socket,tunnel)=>{},_=revoked.changed()=>{}}
                })
                .into_response());
        }
        return Err(StatusCode::NOT_FOUND);
    }
    let target = if host == format!("login-{base}") {
        Target::Login
    } else if host == format!("static-{base}") {
        Target::Static
    } else if host == format!("badtls-{base}") {
        Target::BadTls
    } else {
        return Err(StatusCode::NOT_FOUND);
    };
    if request.uri().path().starts_with("/api/m0/")
        || request.uri().path().starts_with("/api/workspace-tunnel/")
    {
        return Err(StatusCode::NOT_FOUND);
    }
    if !cofleet::workspace_proxy::origin_allowed(request.headers()) {
        return Err(StatusCode::FORBIDDEN);
    }
    let stream = app.stream(target).await?;
    match target {
        Target::BadTls => {
            let stream = proxy::tls(stream, "localhost").await?;
            proxy::proxy(request, stream, &host, "https://localhost").await
        }
        _ => {
            let port = match target {
                // Fixture self-redirect origins are fixed container listeners, not published host ports.
                Target::Login => 8800,
                _ => 8801,
            };
            proxy::proxy(request, stream, &host, &format!("http://127.0.0.1:{port}")).await
        }
    }
}
use axum::extract::FromRequestParts;
async fn node(settings: Settings, mut config: Config) -> Result<()> {
    config.manager_url = format!("https://{}", settings.domain);
    config.device_id = settings.device;
    config.auth = AuthMode::Lazycat;
    let cookie =
        cofleet::lazycat::session_cookie(&config.manager_url, config.lzc_core_config.as_deref())
            .await?;
    use tokio_tungstenite::tungstenite::{Message as ClientMessage, client::IntoClientRequest};
    let mut request = format!("wss://{}/api/m0/node", settings.domain).into_client_request()?;
    request
        .headers_mut()
        .insert(header::COOKIE, cookie.parse()?);
    request
        .headers_mut()
        .insert("x-cofleet-m0-key", settings.key.parse()?);
    let (socket, _) = tokio_tungstenite::connect_async(request).await?;
    let (mut sink, mut source) = socket.split();
    let mut session = None;
    let mut tasks = tokio::task::JoinSet::new();
    let mut check = tokio::time::interval(Duration::from_secs(5));
    loop {
        let incoming = tokio::select! {
            _ = check.tick() => {
                ensure!(
                    cofleet::lazycat::session_cookie(&config.manager_url, config.lzc_core_config.as_deref()).await? == cookie,
                    "platform session changed"
                );
                continue;
            },
            Some(_) = tasks.join_next() => continue,
            incoming = source.next() => incoming,
        };
        match incoming {
            Some(Ok(ClientMessage::Text(text))) => match serde_json::from_str::<Command>(&text)? {
                Command::Welcome { session: id } => session = Some(id),
                Command::Open { id, target } => {
                    let session = session.ok_or_else(|| anyhow::anyhow!("no session"))?;
                    let port = match target {
                        Target::Login => settings.login_port,
                        Target::Static => settings.static_port,
                        Target::BadTls => settings.tls_port,
                    };
                    let config = config.clone();
                    let key = settings.key.clone();
                    let cookie = format!("{cookie}; cofleet_m0_tunnel={key}");
                    tasks.spawn(async move {
                        let _ = agent_tunnel(config, session, id, port, Some(cookie)).await;
                    });
                }
            },
            Some(Ok(ClientMessage::Ping(bytes))) => sink.send(ClientMessage::Pong(bytes)).await?,
            Some(Ok(ClientMessage::Pong(_))) => {}
            _ => break,
        }
    }
    tasks.abort_all();
    Ok(())
}
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let settings: Settings = serde_json::from_slice(&std::fs::read(args.config)?)?;
    ensure!(settings.key.len() >= 32, "short key");
    match args.role {
        Role::Node => {
            node(
                settings,
                Config::load(
                    &args
                        .node_config
                        .ok_or_else(|| anyhow::anyhow!("node config required"))?,
                )?,
            )
            .await
        }
        Role::Gateway => {
            if let Ok(actual) = std::env::var("LAZYCAT_APP_DOMAIN") {
                ensure!(
                    actual == settings.domain,
                    "assigned domain differs from probe config"
                );
            }
            ensure!(
                args.listen.ip().is_loopback(),
                "probe listens only on loopback"
            );
            let app = App {
                settings,
                node: Default::default(),
                pending: Default::default(),
                slots: Arc::new(Semaphore::new(16)),
            };
            let router = Router::new().fallback(any(route)).with_state(app);
            let listener = tokio::net::TcpListener::bind(args.listen).await?;
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app() -> App {
        App {
            settings: Settings {
                domain: "probe.example".into(),
                owner: "alice".into(),
                key: "a".repeat(32),
                device: Uuid::new_v4(),
                login_port: 1,
                static_port: 2,
                tls_port: 3,
            },
            node: Default::default(),
            pending: Default::default(),
            slots: Arc::new(Semaphore::new(1)),
        }
    }
    fn request(host: &str, user: &str, path: &str) -> Request {
        Request::builder()
            .uri(path)
            .header("host", host)
            .header("x-hc-user-id", user)
            .body(axum::body::Body::empty())
            .unwrap()
    }
    #[tokio::test]
    async fn users_hosts_and_origins_are_separate_boundaries() {
        let app = app();
        let peer = "127.0.0.1:1234".parse().unwrap();
        assert_eq!(
            route(
                State(app.clone()),
                ConnectInfo(peer),
                request("probe.example", "bob", "/api/m0/status")
            )
            .await
            .unwrap_err(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            route(
                State(app.clone()),
                ConnectInfo("192.0.2.1:1234".parse().unwrap()),
                request("probe.example", "alice", "/api/m0/status")
            )
            .await
            .unwrap_err(),
            StatusCode::FORBIDDEN
        );
        for host in ["login-probe.example", "unknown-probe.example"] {
            assert_eq!(
                route(
                    State(app.clone()),
                    ConnectInfo(peer),
                    request(host, "alice", "/api/m0/status")
                )
                .await
                .unwrap_err(),
                StatusCode::NOT_FOUND
            );
        }
        let mut cross = request("probe.example", "alice", "/api/m0/status");
        cross
            .headers_mut()
            .insert("origin", "https://login-probe.example".parse().unwrap());
        assert_eq!(
            route(State(app.clone()), ConnectInfo(peer), cross)
                .await
                .unwrap_err(),
            StatusCode::FORBIDDEN
        );
        let mut wrong_scheme = request("login-probe.example", "alice", "/");
        wrong_scheme
            .headers_mut()
            .insert("origin", "http://login-probe.example".parse().unwrap());
        assert_eq!(
            route(State(app), ConnectInfo(peer), wrong_scheme)
                .await
                .unwrap_err(),
            StatusCode::FORBIDDEN
        );
    }
}
