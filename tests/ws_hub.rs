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
    state_dir: std::path::PathBuf,
}
impl Drop for Manager {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.state_dir);
    }
}
impl Manager {
    async fn start(trusted: &str) -> Self {
        Self::start_mode(trusted, false).await
    }
    async fn start_mode(trusted: &str, prefix: bool) -> Self {
        Self::start_configuration(trusted, prefix, false).await
    }
    async fn start_configuration(trusted: &str, prefix: bool, legacy: bool) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let state_dir =
            std::env::temp_dir().join(format!("cofleet-manager-test-{}", Uuid::new_v4()));
        let child = Command::new(env!("CARGO_BIN_EXE_cofleet-manager"))
            .args([
                "--listen",
                &format!("127.0.0.1:{port}"),
                "--auth",
                "lazycat",
                "--trusted-ingress-ip",
                trusted,
                "--state-dir",
                state_dir.to_str().unwrap(),
            ])
            .args(if legacy {
                vec![
                    "--trusted-app-prefix",
                    "--management-domain",
                    "cofleet-test.box.test",
                    "--access-prefix-domain",
                    "test.box.test",
                ]
            } else if prefix {
                vec![
                    "--trusted-app-prefix",
                    "--management-domain",
                    "cofleet.box.test",
                ]
            } else {
                vec!["--access-domain", "apps.example.net"]
            })
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let manager = Self {
            child,
            url: format!("http://127.0.0.1:{port}"),
            state_dir,
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
        docker_observe: None,
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

#[tokio::test]
async fn observation_results_and_persisted_endpoint_api_are_owner_scoped() {
    let manager = Manager::start("127.0.0.1").await;
    let device = Uuid::new_v4();
    let mut observed = state(device, "observer");
    observed.docker_observe = Some(cofleet::docker_observe::ObserveCapability {
        environment_id: "local-test".into(),
        catalog_page_limit: 50,
        log_line_limit: 500,
        endpoint_tunnel: true,
    });
    let (mut agent, _) = connect_async(manager.ws_request("alice")).await.unwrap();
    send(
        &mut agent,
        AgentMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            state: observed,
        },
    )
    .await;
    receive(&mut agent).await;
    let client = reqwest::Client::new();
    let url = format!(
        "{}/api/v1/devices/{device}/docker/catalog?limit=50",
        manager.url
    );
    let request = tokio::spawn({
        let client = client.clone();
        async move {
            client
                .get(url)
                .header("x-hc-user-id", "alice")
                .send()
                .await
                .unwrap()
        }
    });
    let HubMessage::ObserveCommand {
        call_id,
        request: cofleet::docker_observe::ObserveRequest::Catalog { .. },
    } = receive(&mut agent).await
    else {
        panic!("expected catalog command")
    };
    let reply = cofleet::docker_observe::ObserveReply::Catalog {
        page: cofleet::docker_observe::CatalogPage {
            environment_id: "local-test".into(),
            snapshot_id: Uuid::new_v4(),
            sampled_at_unix: 1,
            containers: vec![],
            next_cursor: None,
        },
    };
    send(
        &mut agent,
        AgentMessage::ObserveChunk {
            call_id,
            index: 0,
            final_chunk: true,
            payload: serde_json::to_string(&reply).unwrap(),
        },
    )
    .await;
    assert_eq!(request.await.unwrap().status(), 200);
    let forbidden = client
        .get(format!(
            "{}/api/v1/devices/{device}/docker/catalog",
            manager.url
        ))
        .header("x-hc-user-id", "bob")
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden.status(), 404);
    let endpoint = Uuid::new_v4();
    let create = serde_json::json!({"id":endpoint,"device_id":device,"environment_id":"local-test","binding":{"kind":"container","container_id":"a".repeat(64)},"container_port":8080,"protocol":"http","name":"existing app"});
    let create_request = tokio::spawn({
        let client = client.clone();
        let url = format!("{}/api/v1/access-endpoints", manager.url);
        async move {
            client
                .post(url)
                .header("host", "manager.example.org")
                .header("x-hc-user-id", "alice")
                .header("x-cofleet-request", "1")
                .json(&create)
                .send()
                .await
                .unwrap()
        }
    });
    let HubMessage::ObserveCommand {
        call_id,
        request: cofleet::docker_observe::ObserveRequest::Authorize { definition },
    } = receive(&mut agent).await
    else {
        panic!("expected authorization command")
    };
    assert_eq!(definition.id, endpoint);
    let authorized = cofleet::docker_observe::ObserveReply::Authorized {
        resolution: cofleet::docker_observe::AccessResolution {
            container_id: "a".repeat(64),
            host: "127.0.0.1".into(),
            port: 18080,
            rebound: false,
        },
    };
    send(
        &mut agent,
        AgentMessage::ObserveChunk {
            call_id,
            index: 0,
            final_chunk: true,
            payload: serde_json::to_string(&authorized).unwrap(),
        },
    )
    .await;
    let response = create_request.await.unwrap();
    assert_eq!(response.status(), 201);
    let value: serde_json::Value = response.json().await.unwrap();
    assert_eq!(value["owner"], "alice");
    assert!(
        value["public_url"]
            .as_str()
            .unwrap()
            .ends_with(".apps.example.net/")
    );
    let alice: serde_json::Value = client
        .get(format!("{}/api/v1/access-endpoints", manager.url))
        .header("x-hc-user-id", "alice")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bob: serde_json::Value = client
        .get(format!("{}/api/v1/access-endpoints", manager.url))
        .header("x-hc-user-id", "bob")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(alice.as_array().unwrap().len(), 1);
    assert_eq!(bob, serde_json::json!([]));
}

#[tokio::test]
async fn trusted_prefix_requires_owner_browser_session_and_single_use_grant() {
    trusted_prefix_case(false).await;
}
#[tokio::test]
async fn legacy_management_address_keeps_agent_and_authorization_with_new_prefix_base() {
    trusted_prefix_case(true).await;
}
async fn trusted_prefix_case(legacy: bool) {
    let manager = Manager::start_configuration("127.0.0.1", true, legacy).await;
    let management = if legacy {
        "cofleet-test.box.test"
    } else {
        "cofleet.box.test"
    };
    let prefix = if legacy {
        "test.box.test"
    } else {
        "cofleet.box.test"
    };
    let origin = format!("https://{management}");
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    if legacy {
        let primary = client
            .get(format!("{}/", manager.url))
            .header("host", prefix)
            .send()
            .await
            .unwrap();
        assert_eq!(primary.status(), 303);
        assert_eq!(primary.headers()["location"], format!("{origin}/"));
        let hidden = client
            .get(format!("{}/api/v1/devices", manager.url))
            .header("host", prefix)
            .header("x-hc-user-id", "alice")
            .send()
            .await
            .unwrap();
        assert_eq!(hidden.status(), 404);
    }
    let device = Uuid::new_v4();
    let mut observed = state(device, "trusted observer");
    observed.docker_observe = Some(cofleet::docker_observe::ObserveCapability {
        environment_id: "local-test".into(),
        catalog_page_limit: 50,
        log_line_limit: 500,
        endpoint_tunnel: true,
    });
    let mut request = manager.ws_request("alice");
    request
        .headers_mut()
        .insert("host", management.parse().unwrap());
    let (mut agent, _) = connect_async(request).await.unwrap();
    send(
        &mut agent,
        AgentMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            state: observed,
        },
    )
    .await;
    receive(&mut agent).await;
    let id = Uuid::new_v4();
    let body = serde_json::json!({"id":id,"device_id":device,"environment_id":"local-test","binding":{"kind":"container","container_id":"a".repeat(64)},"container_port":8080,"protocol":"http","name":"trusted fixture","trusted_application":true});
    let create = |cookie: String| {
        client
            .post(format!("{}/api/v1/access-endpoints", manager.url))
            .header("host", management)
            .header("x-hc-user-id", "alice")
            .header("x-cofleet-request", "1")
            .header("origin", &origin)
            .header("cookie", cookie)
            .json(&body)
    };
    assert_eq!(create(String::new()).send().await.unwrap().status(), 403);
    let root = client
        .get(format!("{}/", manager.url))
        .header("host", management)
        .header("x-hc-user-id", "alice")
        .send()
        .await
        .unwrap();
    let manager_cookie = root.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let call = tokio::spawn(create(manager_cookie.clone()).send());
    let HubMessage::ObserveCommand { call_id, .. } = receive(&mut agent).await else {
        panic!("authorization expected")
    };
    let reply = cofleet::docker_observe::ObserveReply::Authorized {
        resolution: cofleet::docker_observe::AccessResolution {
            container_id: "a".repeat(64),
            host: "127.0.0.1".into(),
            port: 18081,
            rebound: false,
        },
    };
    send(
        &mut agent,
        AgentMessage::ObserveChunk {
            call_id,
            index: 0,
            final_chunk: true,
            payload: serde_json::to_string(&reply).unwrap(),
        },
    )
    .await;
    let response = call.await.unwrap().unwrap();
    assert_eq!(response.status(), 201);
    let endpoint: serde_json::Value = response.json().await.unwrap();
    let authority = format!("e-{}-{prefix}", id.simple());
    assert_eq!(endpoint["public_url"], format!("https://{authority}/"));
    let app_get = |user: &str| {
        client
            .get(format!("{}/", manager.url))
            .header("host", &authority)
            .header("x-hc-user-id", user)
    };
    assert_eq!(app_get("bob").send().await.unwrap().status(), 404);
    let start = app_get("alice").send().await.unwrap();
    assert_eq!(start.status(), 303);
    let bootstrap = start.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let location = start.headers()["location"].to_str().unwrap();
    let challenge = location.split("challenge=").nth(1).unwrap();
    let issued = client
        .post(format!("{}/access/authorize", manager.url))
        .header("host", management)
        .header("x-hc-user-id", "alice")
        .header("cookie", &manager_cookie)
        .header("origin", &origin)
        .form(&[("challenge", challenge)])
        .send()
        .await
        .unwrap();
    assert_eq!(issued.status(), 200);
    let html = issued.text().await.unwrap();
    let ticket = html
        .split("name=\"ticket\" value=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    let redeem = || {
        client
            .post(format!("{}/.cofleet/accept", manager.url))
            .header("host", &authority)
            .header("x-hc-user-id", "alice")
            .header("origin", &origin)
            .header("cookie", &bootstrap)
            .form(&[("ticket", ticket)])
    };
    let accepted = redeem().send().await.unwrap();
    assert_eq!(accepted.status(), 303);
    assert!(
        accepted
            .headers()
            .get_all("set-cookie")
            .iter()
            .any(|v| v.to_str().unwrap().starts_with("__Host-cofleet_access="))
    );
    assert_eq!(redeem().send().await.unwrap().status(), 403);
    let unknown = client
        .get(format!("{}/api/v1/devices", manager.url))
        .header("host", "unknown-cofleet.box.test")
        .header("x-hc-user-id", "alice")
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), 404);
    let app_api = client
        .get(format!("{}/api/v1/devices", manager.url))
        .header("host", &authority)
        .header("x-hc-user-id", "alice")
        .send()
        .await
        .unwrap();
    assert_eq!(app_api.status(), 303); // Authorization gate, never the management JSON.
}
