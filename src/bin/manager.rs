use anyhow::Result;
use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, Path, State, WebSocketUpgrade},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use clap::Parser;
use cofleet::{
    auth::{AuthMode, Authenticator},
    hub::Hub,
    protocol::MAX_FRAME_BYTES,
};
use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

#[derive(Parser)]
#[command(about = "CoFleet WebSocket 控制中枢（内存状态）")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:9760")]
    listen: SocketAddr,
    #[arg(long, value_enum, default_value_t = AuthMode::Development)]
    auth: AuthMode,
    /// 仅在已隔离的入口网络中信任这些直接代理对端，不信任 X-Forwarded-For
    #[arg(long)]
    trusted_ingress_ip: Vec<IpAddr>,
}
#[derive(Clone)]
struct App {
    auth: Arc<Authenticator>,
    hub: Hub,
    connections: Arc<tokio::sync::Semaphore>,
}
async fn whoami(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let user = app.auth.identify(&headers, peer.ip())?;
    Ok(Json(
        serde_json::json!({ "user":user, "auth":app.auth.mode, "version":env!("CARGO_PKG_VERSION") }),
    ))
}
async fn devices(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<Vec<cofleet::hub::Device>>, StatusCode> {
    let user = app.auth.identify(&headers, peer.ip())?;
    Ok(Json(app.hub.devices(&user).await))
}
async fn refresh(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(device): Path<uuid::Uuid>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<serde_json::Value>), StatusCode> {
    // This endpoint is only for native/API callers; reject browser cross-origin form requests.
    if headers.contains_key("origin") {
        return Err(StatusCode::FORBIDDEN);
    }
    let user = app.auth.identify(&headers, peer.ip())?;
    let request_id = app.hub.request_report(&user, device).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "request_id":request_id })),
    ))
}
async fn ws(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    // The Agent-only channel does not accept browser origins (prevents cookie-auth WS CSRF).
    if headers.contains_key("origin") {
        return StatusCode::FORBIDDEN.into_response();
    }
    let user = match app.auth.identify(&headers, peer.ip()) {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let Ok(permit) = app.connections.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    upgrade
        .max_message_size(MAX_FRAME_BYTES)
        .max_frame_size(MAX_FRAME_BYTES)
        .on_upgrade(move |socket| async move {
            let _permit = permit;
            app.hub.serve(user, socket).await;
        })
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    let token = match args.auth {
        AuthMode::Development => {
            let token = std::env::var("COFLEET_TOKEN").map_err(|_| {
                anyhow::anyhow!("set COFLEET_TOKEN before starting development manager")
            })?;
            anyhow::ensure!(
                token.trim().len() >= 16 && !token.contains(['\r', '\n']),
                "COFLEET_TOKEN must contain at least 16 characters and no newlines"
            );
            token
        }
        AuthMode::Lazycat => {
            anyhow::ensure!(
                !args.trusted_ingress_ip.is_empty(),
                "lazycat mode requires explicit --trusted-ingress-ip and network isolation from direct clients"
            );
            String::new()
        }
    };
    let app = App {
        auth: Arc::new(Authenticator {
            mode: args.auth,
            token,
            trusted_ingress: args.trusted_ingress_ip,
        }),
        hub: Hub::default(),
        connections: Arc::new(tokio::sync::Semaphore::new(256)),
    };
    let mode = app.auth.mode;
    let routes = Router::new()
        .route("/", get(|| async { Html(include_str!("../manager.html")) }))
        .route(
            "/favicon.ico",
            get(|| async {
                (
                    [
                        (header::CONTENT_TYPE, "image/vnd.microsoft.icon"),
                        (header::CACHE_CONTROL, "public, max-age=86400"),
                    ],
                    include_bytes!("../../assets/favicon.ico").as_slice(),
                )
            }),
        )
        .route(
            "/manager.css",
            get(|| async {
                (
                    [
                        (header::CONTENT_TYPE, "text/css; charset=utf-8"),
                        (header::CACHE_CONTROL, "no-cache"),
                    ],
                    include_str!("../manager.css"),
                )
            }),
        )
        .route(
            "/manager.js",
            get(|| async {
                (
                    [
                        (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
                        (header::CACHE_CONTROL, "no-cache"),
                    ],
                    include_str!("../manager.js"),
                )
            }),
        )
        .route(
            "/app-icon.png",
            get(|| async {
                (
                    [
                        (header::CONTENT_TYPE, "image/png"),
                        (header::CACHE_CONTROL, "public, max-age=86400"),
                    ],
                    include_bytes!("../../assets/cofleet-icon-256.png").as_slice(),
                )
            }),
        )
        .route("/healthz", get(|| async { "ok" }))
        .route(
            "/api/v1/auth-mode",
            get(move || async move { Json(serde_json::json!({"auth":mode})) }),
        )
        .route("/api/whoami", get(whoami))
        .route("/api/ws", get(ws))
        .route("/api/v1/devices", get(devices))
        .route("/api/v1/devices/{device}/refresh", post(refresh))
        .layer(DefaultBodyLimit::max(4096))
        .with_state(app);
    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    eprintln!(
        "Control hub listening on {}; state is not persisted",
        listener.local_addr()?
    );
    axum::serve(
        listener,
        routes.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    Ok(())
}
