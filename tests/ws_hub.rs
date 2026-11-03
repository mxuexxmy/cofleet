//! These simulate ingress identities; they do not authenticate against LazyCat.
use cofleet::protocol::{AgentMessage, Heartbeat, HubMessage, PROTOCOL_VERSION};
use futures_util::{SinkExt, StreamExt};
use std::{
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};
use uuid::Uuid;

struct Manager {
    child: Child,
    url: String,
}
impl Drop for Manager {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Manager {
    async fn start(trusted: &str) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let child = Command::new(env!("CARGO_BIN_EXE_cofleet-manager"))
            .args([
                "--listen",
                &format!("127.0.0.1:{port}"),
                "--auth",
                "lazycat",
                "--trusted-ingress-ip",
                trusted,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let manager = Self {
            child,
            url: format!("http://127.0.0.1:{port}"),
        };
        for _ in 0..100 {
            if reqwest::get(format!("{}/healthz", manager.url))
                .await
                .is_ok()
            {
                return manager;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        panic!("manager did not start");
    }
    fn ws_request(&self, user: &str) -> tokio_tungstenite::tungstenite::http::Request<()> {
        let mut request = format!("{}/api/ws", self.url.replace("http:", "ws:"))
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("x-hc-user-id", user.parse().unwrap());
        request
    }
    async fn devices(&self, user: &str) -> serde_json::Value {
        reqwest::Client::new()
            .get(format!("{}/api/v1/devices", self.url))
            .header("x-hc-user-id", user)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }
}
fn state(device_id: Uuid, name: &str) -> Heartbeat {
    Heartbeat {
        pipeline: None,
        workspace: None,
        docker: None,
        device_id,
        name: name.into(),
        os: "linux".into(),
        arch: "x86_64".into(),
        accepting_tasks: true,
        heartbeat_secs: 1,
        version: "test".into(),
        resources: None,
        resource_policy: Default::default(),
        capabilities: Default::default(),
    }
}
async fn receive<S>(socket: &mut tokio_tungstenite::WebSocketStream<S>) -> HubMessage
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(message.to_text().unwrap()).unwrap()
}
async fn send<S>(socket: &mut tokio_tungstenite::WebSocketStream<S>, message: AgentMessage)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    socket
        .send(Message::Text(
            serde_json::to_string(&message).unwrap().into(),
        ))
        .await
        .unwrap();
}

#[tokio::test]
async fn websocket_rooms_commands_and_connection_replacement() {
    let manager = Manager::start("127.0.0.1").await;
    let device = Uuid::new_v4();
    let (mut alice, _) = connect_async(manager.ws_request("alice")).await.unwrap();
    send(
        &mut alice,
        AgentMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            state: state(device, "alice-node"),
        },
    )
    .await;
    assert!(matches!(
        receive(&mut alice).await,
        HubMessage::Welcome { .. }
    ));
    let (mut bob, _) = connect_async(manager.ws_request("bob")).await.unwrap();
    send(
        &mut bob,
        AgentMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            state: state(device, "bob-node"),
        },
    )
    .await;
    receive(&mut bob).await;
    assert_eq!(manager.devices("alice").await[0]["name"], "alice-node");
    assert_eq!(manager.devices("bob").await[0]["name"], "bob-node");
    assert_eq!(manager.devices("charlie").await, serde_json::json!([]));
    let forbidden = reqwest::Client::new()
        .post(format!("{}/api/v1/devices/{device}/refresh", manager.url))
        .header("x-hc-user-id", "charlie")
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden.status(), 404);
    let refresh: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/v1/devices/{device}/refresh", manager.url))
        .header("x-hc-user-id", "alice")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let HubMessage::RequestReport { request_id } = receive(&mut alice).await else {
        panic!("expected report request");
    };
    assert_eq!(request_id.to_string(), refresh["request_id"]);
    send(
        &mut alice,
        AgentMessage::Report {
            request_id,
            state: state(device, "alice-node"),
        },
    )
    .await;
    assert!(matches!(receive(&mut alice).await, HubMessage::Ack { .. }));
    assert_eq!(
        manager.devices("alice").await[0]["last_report_id"],
        request_id.to_string()
    );
    let (mut replacement, _) = connect_async(manager.ws_request("alice")).await.unwrap();
    send(
        &mut replacement,
        AgentMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            state: state(device, "replacement"),
        },
    )
    .await;
    receive(&mut replacement).await;
    drop(alice);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(manager.devices("alice").await[0]["online"], true);
    assert_eq!(manager.devices("alice").await[0]["name"], "replacement");
    // A registered session cannot report state for a different device identity.
    send(
        &mut replacement,
        AgentMessage::Heartbeat {
            state: state(Uuid::new_v4(), "spoofed"),
        },
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), replacement.next())
        .await
        .unwrap();
    assert_eq!(manager.devices("alice").await[0]["online"], false);
    assert_eq!(manager.devices("bob").await[0]["online"], true);
    let mut browser = manager.ws_request("alice");
    browser
        .headers_mut()
        .insert("origin", "https://evil.example".parse().unwrap());
    assert!(connect_async(browser).await.is_err());
}

#[tokio::test]
async fn spoofed_ingress_header_is_rejected_from_untrusted_peer() {
    let manager = Manager::start("127.0.0.2").await;
    let response = reqwest::Client::new()
        .get(format!("{}/api/v1/devices", manager.url))
        .header("x-hc-user-id", "alice")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
    assert!(connect_async(manager.ws_request("alice")).await.is_err());
}

#[tokio::test]
async fn incompatible_protocol_and_oversized_frames_are_disconnected() {
    let manager = Manager::start("127.0.0.1").await;
    let device = Uuid::new_v4();
    let (mut socket, _) = connect_async(manager.ws_request("alice")).await.unwrap();
    send(
        &mut socket,
        AgentMessage::Hello {
            protocol_version: PROTOCOL_VERSION + 1,
            state: state(device, "invalid"),
        },
    )
    .await;
    let _ = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .unwrap();
    assert_eq!(manager.devices("alice").await, serde_json::json!([]));
    let (mut socket, _) = connect_async(manager.ws_request("alice")).await.unwrap();
    send(
        &mut socket,
        AgentMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            state: state(device, "valid"),
        },
    )
    .await;
    receive(&mut socket).await;
    socket
        .send(Message::Text(
            "x".repeat(cofleet::protocol::MAX_FRAME_BYTES + 1).into(),
        ))
        .await
        .unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .unwrap();
    assert_eq!(manager.devices("alice").await[0]["online"], false);
    // Validate before heartbeat timeout arithmetic (malicious integers must not panic).
    let (mut socket, _) = connect_async(manager.ws_request("alice")).await.unwrap();
    send(
        &mut socket,
        AgentMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            state: state(device, "valid"),
        },
    )
    .await;
    receive(&mut socket).await;
    let mut invalid = state(device, "invalid");
    invalid.heartbeat_secs = u64::MAX;
    send(&mut socket, AgentMessage::Heartbeat { state: invalid }).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .unwrap();
    assert_eq!(manager.devices("alice").await[0]["online"], false);
}
