use anyhow::Result;
use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, Path, Request, State, WebSocketUpgrade},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{any, get, post},
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
    workspace_cookie: String,
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
async fn docker_command(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(device): Path<uuid::Uuid>,
    headers: HeaderMap,
    Json(request): Json<cofleet::docker::DockerRequest>,
) -> Result<Json<cofleet::docker::DockerReply>, StatusCode> {
    // JSON + a custom header prevents cross-site form submissions; no CORS is enabled.
    if headers
        .get("x-cofleet-request")
        .and_then(|v| v.to_str().ok())
        != Some("1")
    {
        return Err(StatusCode::FORBIDDEN);
    }
    let user = app.auth.identify(&headers, peer.ip())?;
    Ok(Json(app.hub.docker(&user, device, request).await?))
}
async fn workspace_session(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    if headers
        .get("x-cofleet-request")
        .and_then(|v| v.to_str().ok())
        != Some("1")
    {
        return Err(StatusCode::FORBIDDEN);
    }
    app.auth.identify(&headers, peer.ip())?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    if app.auth.mode == AuthMode::Development {
        response.headers_mut().insert(
            header::SET_COOKIE,
            format!(
                "cofleet_workspace={}; Path=/workspaces/; HttpOnly; SameSite=Strict",
                app.workspace_cookie
            )
            .parse()
            .unwrap(),
        );
    }
    Ok(response)
}
async fn clear_workspace_session(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    if headers
        .get("x-cofleet-request")
        .and_then(|v| v.to_str().ok())
        != Some("1")
    {
        return Err(StatusCode::FORBIDDEN);
    }
    app.auth.identify(&headers, peer.ip())?;
    Ok((
        [(
            header::SET_COOKIE,
            "cofleet_workspace=; Path=/workspaces/; HttpOnly; SameSite=Strict; Max-Age=0",
        )],
        StatusCode::NO_CONTENT,
    )
        .into_response())
}
async fn workspace_proxy(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(params): Path<std::collections::HashMap<String, String>>,
    request: Request,
) -> Result<Response, StatusCode> {
    if !cofleet::workspace_proxy::origin_allowed(request.headers()) {
        return Err(StatusCode::FORBIDDEN);
    }
    let user = match app.auth.identify(request.headers(), peer.ip()) {
        Ok(user) => user,
        Err(error) => {
            let cookie = request
                .headers()
                .get(header::COOKIE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if app.auth.mode == AuthMode::Development
                && cookie
                    .split(';')
                    .any(|v| v.trim() == format!("cofleet_workspace={}", app.workspace_cookie))
            {
                "development".into()
            } else {
                return Err(error);
            }
        }
    };
    let device = params
        .get("device")
        .ok_or(StatusCode::BAD_REQUEST)?
        .parse::<uuid::Uuid>()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let prefix = format!("/workspaces/{device}/");
    // Resolve ownership even before returning the trailing slash redirect.
    if !app.hub.owns_device(&user, device).await {
        return Err(StatusCode::NOT_FOUND);
    }
    if request.uri().path() == prefix.trim_end_matches('/') {
        let location = format!(
            "{}{}",
            prefix,
            request
                .uri()
                .query()
                .map(|q| format!("?{q}"))
                .unwrap_or_default()
        );
        return Ok(axum::response::Redirect::temporary(&location).into_response());
    }
    let stream = app.hub.workspace_stream(&user, device).await?;
    cofleet::workspace_proxy::proxy(request, stream, &prefix).await
}
async fn workspace_tunnel(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path((device, id)): Path<(uuid::Uuid, uuid::Uuid)>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, StatusCode> {
    if headers.contains_key(header::ORIGIN) {
        return Err(StatusCode::FORBIDDEN);
    }
    let user = app.auth.identify(&headers, peer.ip())?;
    let session = headers
        .get("x-cofleet-session")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let tunnel = app
        .hub
        .claim_workspace_tunnel(&user, device, session, id)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(upgrade
        .max_message_size(MAX_FRAME_BYTES)
        .max_frame_size(MAX_FRAME_BYTES)
        .on_upgrade(|socket| cofleet::workspace_proxy::serve_tunnel(socket, tunnel)))
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
        workspace_cookie: uuid::Uuid::new_v4().to_string(),
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
        .route(
            "/api/v1/workspace-session",
            post(workspace_session).delete(clear_workspace_session),
        )
        .route("/api/workspace-tunnel/{device}/{id}", get(workspace_tunnel))
        .route("/workspaces/{device}", any(workspace_proxy))
        .route("/workspaces/{device}/", any(workspace_proxy))
        .route("/workspaces/{device}/{*path}", any(workspace_proxy))
        .route("/api/v1/devices", get(devices))
        .route("/api/v1/devices/{device}/refresh", post(refresh))
        .route("/api/v1/devices/{device}/docker", post(docker_command))
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
