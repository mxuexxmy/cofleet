//! Bounded, owner/session-scoped reverse TCP streams to one approved workspace port.
//! Control messages never carry project content; each HTTP connection has a separate data WS.
use crate::{
    config::Config,
    protocol::{HubMessage, MAX_FRAME_BYTES},
};
use anyhow::Result;
use axum::{
    body::Body,
    extract::ws::{Message, WebSocket},
    http::{HeaderMap, Request, StatusCode, header},
    response::{IntoResponse, Response},
};
use futures_util::{SinkExt, StreamExt};
use hyper_util::rt::TokioIo;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, DuplexStream},
    sync::{OwnedSemaphorePermit, Semaphore, oneshot},
};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, protocol::WebSocketConfig};
use uuid::Uuid;
type AccessCancels =
    Arc<std::sync::Mutex<HashMap<(String, Uuid), tokio::sync::watch::Sender<bool>>>>;
struct NodeChannel {
    session: Uuid,
    sender: tokio::sync::mpsc::Sender<HubMessage>,
    slots: Arc<Semaphore>,
}

pub struct PendingTunnel {
    pub user: String,
    pub device: Uuid,
    pub session: Uuid,
    pub stream: DuplexStream,
    pub ready: oneshot::Sender<()>,
    pub created: Instant,
    pub permits: (OwnedSemaphorePermit, OwnedSemaphorePermit),
    pub cancel: tokio::sync::watch::Receiver<bool>,
}
#[derive(Clone)]
pub struct Tunnels {
    pending: Arc<std::sync::Mutex<HashMap<Uuid, PendingTunnel>>>,
    slots: Arc<Semaphore>,
    access_cancels: AccessCancels,
}
impl Default for Tunnels {
    fn default() -> Self {
        Self {
            pending: Default::default(),
            slots: Arc::new(Semaphore::new(64)),
            access_cancels: Default::default(),
        }
    }
}
struct PendingGuard {
    id: Uuid,
    pending: Arc<std::sync::Mutex<HashMap<Uuid, PendingTunnel>>>,
}
impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.pending.lock().unwrap().remove(&self.id);
    }
}
impl Tunnels {
    pub async fn open(
        &self,
        user: &str,
        device: Uuid,
        session: Uuid,
        sender: tokio::sync::mpsc::Sender<HubMessage>,
        node_slots: Arc<Semaphore>,
        target: crate::protocol::TunnelTarget,
    ) -> Result<DuplexStream, StatusCode> {
        let (_cancel, receiver) = tokio::sync::watch::channel(false);
        self.open_message(
            user,
            device,
            NodeChannel {
                session,
                sender,
                slots: node_slots,
            },
            receiver,
            |id| HubMessage::WorkspaceTunnel {
                tunnel_id: id,
                target,
            },
        )
        .await
    }
    pub async fn open_access(
        &self,
        user: &str,
        device: Uuid,
        session: Uuid,
        sender: tokio::sync::mpsc::Sender<HubMessage>,
        node_slots: Arc<Semaphore>,
        endpoint: (Uuid, u64),
    ) -> Result<DuplexStream, StatusCode> {
        let (endpoint_id, revision) = endpoint;
        let receiver = {
            let mut cancels = self.access_cancels.lock().unwrap();
            cancels
                .entry((user.into(), endpoint_id))
                .or_insert_with(|| tokio::sync::watch::channel(false).0)
                .subscribe()
        };
        self.open_message(
            user,
            device,
            NodeChannel {
                session,
                sender,
                slots: node_slots,
            },
            receiver,
            |id| HubMessage::AccessTunnel {
                tunnel_id: id,
                endpoint_id,
                revision,
            },
        )
        .await
    }
    async fn open_message<F>(
        &self,
        user: &str,
        device: Uuid,
        node: NodeChannel,
        cancel: tokio::sync::watch::Receiver<bool>,
        message: F,
    ) -> Result<DuplexStream, StatusCode>
    where
        F: FnOnce(Uuid) -> HubMessage,
    {
        let permits = (
            self.slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?,
            node.slots
                .try_acquire_owned()
                .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?,
        );
        let id = Uuid::new_v4();
        let (front, back) = tokio::io::duplex(32 * 1024);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(
            id,
            PendingTunnel {
                user: user.into(),
                device,
                session: node.session,
                stream: back,
                ready: tx,
                created: Instant::now(),
                permits,
                cancel,
            },
        );
        let _guard = PendingGuard {
            id,
            pending: self.pending.clone(),
        };
        node.sender
            .try_send(message(id))
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        tokio::time::timeout(Duration::from_secs(15), rx)
            .await
            .map_err(|_| StatusCode::GATEWAY_TIMEOUT)?
            .map_err(|_| StatusCode::BAD_GATEWAY)?;
        Ok(front)
    }
    pub fn claim(
        &self,
        user: &str,
        device: Uuid,
        session: Uuid,
        id: Uuid,
    ) -> Option<PendingTunnel> {
        let mut pending = self.pending.lock().unwrap();
        let tunnel = pending.get(&id)?;
        if tunnel.user != user
            || tunnel.device != device
            || tunnel.session != session
            || tunnel.created.elapsed() > Duration::from_secs(15)
        {
            return None;
        }
        pending.remove(&id)
    }
    pub fn cancel_access(&self, user: &str, endpoint: Uuid) {
        if let Some(sender) = self
            .access_cancels
            .lock()
            .unwrap()
            .remove(&(user.into(), endpoint))
        {
            let _ = sender.send(true);
        }
    }
}

pub async fn serve_tunnel(socket: WebSocket, tunnel: PendingTunnel) {
    let PendingTunnel {
        stream,
        ready,
        permits,
        mut cancel,
        ..
    } = tunnel;
    let _permits = permits;
    let _ = ready.send(());
    let (mut read, mut write) = tokio::io::split(stream);
    let (mut sink, mut source) = socket.split();
    let outgoing = async {
        let mut bytes = vec![0; MAX_FRAME_BYTES];
        let mut ping = tokio::time::interval(Duration::from_secs(20));
        loop {
            let message = tokio::select! {
                count=read.read(&mut bytes)=>{let count=count?;if count==0 {break;}Message::Binary(bytes[..count].to_vec().into())},
                _=ping.tick()=>Message::Ping(Vec::new().into()),
            };
            tokio::time::timeout(Duration::from_secs(30), sink.send(message)).await??;
        }
        Ok::<(), anyhow::Error>(())
    };
    let incoming = async {
        while let Some(message) = source.next().await {
            match message? {
                Message::Binary(bytes) => {
                    tokio::time::timeout(Duration::from_secs(30), write.write_all(&bytes))
                        .await??;
                }
                Message::Ping(_) | Message::Pong(_) => {}
                _ => break,
            }
        }
        Ok::<(), anyhow::Error>(())
    };
    tokio::select! {_=outgoing=>{},_=incoming=>{},_=wait_for_cancel(&mut cancel)=>{}}
}

// Legacy workspace/application tunnels have no cancellation sender after open completes.
// Only an explicit true signal revokes a tunnel; dropping that sender is not revocation.
async fn wait_for_cancel(cancel: &mut tokio::sync::watch::Receiver<bool>) {
    if cancel.wait_for(|value| *value).await.is_err() {
        std::future::pending::<()>().await;
    }
}

pub async fn agent_tunnel(
    config: Config,
    session: Uuid,
    id: Uuid,
    port: u16,
    cookie: Option<String>,
) -> Result<()> {
    agent_tunnel_target(
        config,
        session,
        id,
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        port,
        cookie,
    )
    .await
}

pub async fn agent_tunnel_target(
    config: Config,
    session: Uuid,
    id: Uuid,
    host: std::net::IpAddr,
    port: u16,
    cookie: Option<String>,
) -> Result<()> {
    use tokio_tungstenite::tungstenite::Message as ClientMessage;
    let mut url = reqwest::Url::parse(&config.manager_url)?;
    url.set_scheme(if config.manager_url.starts_with("https:") {
        "wss"
    } else {
        "ws"
    })
    .map_err(|_| anyhow::anyhow!("invalid tunnel URL"))?;
    url.set_path(&format!(
        "{}/api/workspace-tunnel/{}/{}",
        url.path().trim_end_matches('/'),
        config.device_id,
        id
    ));
    let mut request = url.as_str().into_client_request()?;
    request
        .headers_mut()
        .insert("x-cofleet-session", session.to_string().parse()?);
    if let Some(cookie) = cookie {
        request
            .headers_mut()
            .insert(header::COOKIE, cookie.parse()?);
    } else {
        request.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Bearer {}", config.token).parse()?,
        );
    }
    let target = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::TcpStream::connect((host, port)),
    )
    .await??;
    let limits = WebSocketConfig::default()
        .max_frame_size(Some(MAX_FRAME_BYTES))
        .max_message_size(Some(MAX_FRAME_BYTES));
    let (socket, _) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::connect_async_with_config(request, Some(limits), true),
    )
    .await??;
    let (mut read, mut write) = target.into_split();
    let (mut sink, mut source) = socket.split();
    let outgoing = async {
        let mut bytes = vec![0; MAX_FRAME_BYTES];
        let mut ping = tokio::time::interval(Duration::from_secs(20));
        loop {
            let message = tokio::select! {
                count=read.read(&mut bytes)=>{let count=count?;if count==0 {break;}ClientMessage::Binary(bytes[..count].to_vec().into())},
                _=ping.tick()=>ClientMessage::Ping(Vec::new().into()),
            };
            tokio::time::timeout(Duration::from_secs(30), sink.send(message)).await??;
        }
        Ok::<(), anyhow::Error>(())
    };
    let incoming = async {
        while let Some(message) = source.next().await {
            match message? {
                ClientMessage::Binary(bytes) => {
                    tokio::time::timeout(Duration::from_secs(30), write.write_all(&bytes))
                        .await??;
                }
                ClientMessage::Ping(_) | ClientMessage::Pong(_) => {}
                _ => break,
            }
        }
        Ok::<(), anyhow::Error>(())
    };
    tokio::select! {result=outgoing=>result,result=incoming=>result}
}

/// Check browser origins before proxying any HTTP method or WebSocket upgrade.
pub fn origin_allowed(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Some(host) = headers.get(header::HOST).and_then(|h| h.to_str().ok()) else {
        return false;
    };
    let Ok(origin) = origin.to_str().unwrap_or("").parse::<reqwest::Url>() else {
        return false;
    };
    matches!(origin.scheme(), "http" | "https")
        && origin.username().is_empty()
        && origin.password().is_none()
        && origin.path() == "/"
        && origin.query().is_none()
        && origin.fragment().is_none()
        && origin.authority().eq_ignore_ascii_case(host)
}

pub async fn proxy(
    mut request: Request<Body>,
    stream: DuplexStream,
    prefix: &str,
) -> Result<Response, StatusCode> {
    let path = request
        .uri()
        .path()
        .strip_prefix(prefix)
        .ok_or(StatusCode::BAD_REQUEST)?;
    let path = format!(
        "/{}{}",
        path,
        request
            .uri()
            .query()
            .map(|q| format!("?{q}"))
            .unwrap_or_default()
    );
    *request.uri_mut() = path.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
    let websocket = request
        .headers()
        .get(header::UPGRADE)
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"));
    if request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|len| len > 32 * 1024 * 1024)
    {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    // Never disclose platform login cookies/identity or hub bearer tokens to project code.
    let keep = [
        "host",
        "accept",
        "accept-encoding",
        "accept-language",
        "content-type",
        "content-length",
        "range",
        "if-none-match",
        "if-modified-since",
        "user-agent",
        "origin",
        "sec-websocket-key",
        "sec-websocket-version",
        "sec-websocket-protocol",
        "sec-websocket-extensions",
        "upgrade",
        "connection",
    ];
    let names: Vec<_> = request.headers().keys().cloned().collect();
    for name in names {
        if !keep.contains(&name.as_str()) {
            request.headers_mut().remove(name);
        }
    }
    // Node Inspector permits only loopback/IP Host values (DNS rebinding protection).
    // Public Origin/owner checks have already run; normalize only this fixed container port.
    if prefix.starts_with("/workspaces/") && path.starts_with("/proxy/9229/") {
        request
            .headers_mut()
            .insert(header::HOST, "127.0.0.1:9229".parse().unwrap());
        if request.headers().contains_key(header::ORIGIN) {
            request
                .headers_mut()
                .insert(header::ORIGIN, "http://127.0.0.1:9229".parse().unwrap());
        }
    }
    if !websocket {
        request.headers_mut().remove(header::UPGRADE);
        request.headers_mut().remove(header::CONNECTION);
    }
    let downstream_upgrade = websocket.then(|| hyper::upgrade::on(&mut request));
    let (parts, body) = request.into_parts();
    let request = Request::from_parts(
        parts,
        Body::new(http_body_util::Limited::new(body, 32 * 1024 * 1024)),
    );
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    tokio::spawn(async move {
        let _ = connection.with_upgrades().await;
    });
    let mut response = tokio::time::timeout(Duration::from_secs(30), sender.send_request(request))
        .await
        .map_err(|_| StatusCode::GATEWAY_TIMEOUT)?
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    response.headers_mut().remove(header::SET_COOKIE);
    if let Some(location) = response
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .filter(|v| v.starts_with('/') && !v.starts_with("//"))
    {
        let rewritten = format!("{}{}", prefix, location.trim_start_matches('/'));
        response.headers_mut().insert(
            header::LOCATION,
            rewritten.parse().map_err(|_| StatusCode::BAD_GATEWAY)?,
        );
    }
    if response.status() == StatusCode::SWITCHING_PROTOCOLS {
        let downstream = downstream_upgrade.ok_or(StatusCode::BAD_GATEWAY)?;
        let upstream = hyper::upgrade::on(&mut response);
        tokio::spawn(async move {
            if let (Ok(down), Ok(up)) = tokio::join!(downstream, upstream) {
                let _ =
                    tokio::io::copy_bidirectional(&mut TokioIo::new(down), &mut TokioIo::new(up))
                        .await;
            }
        });
    } else {
        response.headers_mut().remove(header::CONNECTION);
        response.headers_mut().remove(header::UPGRADE);
    }
    Ok(response.map(Body::new).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn legacy_sender_drop_keeps_tunnel_alive_but_explicit_revoke_cancels() {
        let (sender, mut receiver) = tokio::sync::watch::channel(false);
        drop(sender);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), wait_for_cancel(&mut receiver))
                .await
                .is_err()
        );
        let (sender, mut receiver) = tokio::sync::watch::channel(false);
        sender.send(true).unwrap();
        drop(sender);
        tokio::time::timeout(Duration::from_secs(1), wait_for_cancel(&mut receiver))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn tunnel_claim_checks_identity_session_and_is_one_shot() {
        let tunnels = Tunnels::default();
        let device = Uuid::new_v4();
        let session = Uuid::new_v4();
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let other = tunnels.clone();
        let task = tokio::spawn(async move {
            other
                .open(
                    "alice",
                    device,
                    session,
                    tx,
                    Arc::new(Semaphore::new(1)),
                    crate::protocol::TunnelTarget::Workspace,
                )
                .await
        });
        let HubMessage::WorkspaceTunnel { tunnel_id, .. } = rx.recv().await.unwrap() else {
            panic!("expected tunnel")
        };
        assert!(tunnels.claim("bob", device, session, tunnel_id).is_none());
        assert!(
            tunnels
                .claim("alice", device, Uuid::new_v4(), tunnel_id)
                .is_none()
        );
        let tunnel = tunnels.claim("alice", device, session, tunnel_id).unwrap();
        assert!(tunnels.claim("alice", device, session, tunnel_id).is_none());
        let _ = tunnel.ready.send(());
        assert!(task.await.unwrap().is_ok());
    }
    #[test]
    fn browser_origins_must_match_the_public_host() {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, "cofleet.example".parse().unwrap());
        h.insert(header::ORIGIN, "https://cofleet.example".parse().unwrap());
        assert!(origin_allowed(&h));
        for value in [
            "null",
            "https://evil.example",
            "https://cofleet.example:8443",
            "https://cofleet.example/evil",
        ] {
            h.insert(header::ORIGIN, value.parse().unwrap());
            assert!(!origin_allowed(&h));
        }
    }
}
