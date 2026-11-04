use anyhow::Result;
use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, Path, Query, Request, State, WebSocketUpgrade},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{any, delete, get, patch, post},
};
use clap::Parser;
use cofleet::{
    auth::{AuthMode, Authenticator},
    hub::Hub,
    protocol::MAX_FRAME_BYTES,
};
use std::{
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::Arc,
};
use tower::ServiceExt;

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
    /// Persistent manager state. The LazyCat package uses /lzcapp/var/cofleet.
    #[arg(long, default_value_os_t = cofleet::config::data_dir().join("manager"))]
    state_dir: PathBuf,
    /// Dedicated cookie-site base for untrusted application hosts. Without it, access endpoints are disabled.
    #[arg(long)]
    access_domain: Option<String>,
}
#[derive(Clone)]
struct App {
    workspace_cookie: String,
    auth: Arc<Authenticator>,
    hub: Hub,
    connections: Arc<tokio::sync::Semaphore>,
    endpoints: cofleet::access::EndpointStore,
    access_domain: Option<String>,
    endpoint_status:
        Arc<tokio::sync::RwLock<std::collections::HashMap<(String, uuid::Uuid), EndpointStatus>>>,
}
#[derive(Clone, serde::Serialize)]
struct EndpointStatus {
    verification: &'static str,
    detail: Option<String>,
    verified_unix: Option<u64>,
}
#[derive(serde::Serialize)]
struct EndpointView {
    #[serde(flatten)]
    endpoint: cofleet::access::AccessEndpoint,
    #[serde(flatten)]
    status: EndpointStatus,
}
#[derive(Clone)]
struct Front {
    app: App,
    management: Router,
}

#[derive(serde::Deserialize)]
struct CatalogQuery {
    cursor: Option<String>,
    limit: Option<u16>,
}
#[derive(serde::Deserialize)]
struct LogsQuery {
    tail: Option<u16>,
}
#[derive(serde::Deserialize)]
struct RevisionQuery {
    revision: u64,
}

fn request_marked(headers: &HeaderMap) -> bool {
    headers
        .get("x-cofleet-request")
        .and_then(|v| v.to_str().ok())
        == Some("1")
}
fn host(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::HOST)?
        .to_str()
        .ok()?
        .split(':')
        .next()
        .map(|v| v.trim_end_matches('.').to_ascii_lowercase())
}
fn cookie_site(value: &str) -> String {
    let labels: Vec<_> = value.trim_end_matches('.').split('.').collect();
    labels[labels.len().saturating_sub(2)..].join(".")
}
fn access_public_url(id: uuid::Uuid, domain: &str) -> String {
    format!(
        "https://e-{}.{}/",
        id.simple(),
        domain.trim().trim_end_matches('.').to_ascii_lowercase()
    )
}
async fn endpoint_view(app: &App, endpoint: cofleet::access::AccessEndpoint) -> EndpointView {
    let status = app
        .endpoint_status
        .read()
        .await
        .get(&(endpoint.owner.clone(), endpoint.definition.id))
        .cloned()
        .unwrap_or(EndpointStatus {
            verification: if endpoint.enabled {
                "pending"
            } else {
                "disabled"
            },
            detail: None,
            verified_unix: None,
        });
    EndpointView { endpoint, status }
}
async fn sync_endpoint(
    app: &App,
    user: &str,
    endpoint: &cofleet::access::AccessEndpoint,
) -> EndpointStatus {
    let status = if !endpoint.enabled {
        app.hub.cancel_access_endpoint(user, endpoint.definition.id);
        EndpointStatus {
            verification: "disabled",
            detail: None,
            verified_unix: None,
        }
    } else {
        match app
            .hub
            .observe(
                user,
                endpoint.definition.device_id,
                cofleet::docker_observe::ObserveRequest::Authorize {
                    definition: endpoint.definition.clone(),
                },
            )
            .await
        {
            Ok(cofleet::docker_observe::ObserveReply::Authorized { .. }) => EndpointStatus {
                verification: "verified",
                detail: None,
                verified_unix: Some(cofleet::protocol::unix_now()),
            },
            Ok(cofleet::docker_observe::ObserveReply::Error { message }) => EndpointStatus {
                verification: "error",
                detail: Some(message),
                verified_unix: None,
            },
            Err(status) => EndpointStatus {
                verification: "pending",
                detail: Some(format!(
                    "Agent verification unavailable ({})",
                    status.as_u16()
                )),
                verified_unix: None,
            },
            _ => EndpointStatus {
                verification: "error",
                detail: Some("unexpected Agent response".into()),
                verified_unix: None,
            },
        }
    };
    app.endpoint_status
        .write()
        .await
        .insert((user.into(), endpoint.definition.id), status.clone());
    status
}

async fn docker_catalog(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(device): Path<uuid::Uuid>,
    headers: HeaderMap,
    Query(query): Query<CatalogQuery>,
) -> Result<Json<cofleet::docker_observe::ObserveReply>, StatusCode> {
    let user = app.auth.identify(&headers, peer.ip())?;
    let reply = app
        .hub
        .observe(
            &user,
            device,
            cofleet::docker_observe::ObserveRequest::Catalog {
                cursor: query.cursor,
                limit: query.limit.unwrap_or(50),
            },
        )
        .await?;
    Ok(Json(reply))
}
async fn docker_logs(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path((device, container)): Path<(uuid::Uuid, String)>,
    headers: HeaderMap,
    Query(query): Query<LogsQuery>,
) -> Result<Json<cofleet::docker_observe::ObserveReply>, StatusCode> {
    let user = app.auth.identify(&headers, peer.ip())?;
    let reply = app
        .hub
        .observe(
            &user,
            device,
            cofleet::docker_observe::ObserveRequest::Logs {
                container_id: container,
                tail: query.tail.unwrap_or(100),
            },
        )
        .await?;
    Ok(Json(reply))
}
async fn access_endpoints(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<Vec<EndpointView>>, (StatusCode, String)> {
    let user = app
        .auth
        .identify(&headers, peer.ip())
        .map_err(|s| (s, "authentication failed".into()))?;
    let endpoints = app
        .endpoints
        .list(&user)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let mut views = Vec::with_capacity(endpoints.len());
    for endpoint in endpoints {
        views.push(endpoint_view(&app, endpoint).await)
    }
    Ok(Json(views))
}
async fn create_endpoint(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<cofleet::access::CreateEndpoint>,
) -> Result<(StatusCode, Json<EndpointView>), (StatusCode, String)> {
    if !request_marked(&headers) {
        return Err((StatusCode::FORBIDDEN, "missing request marker".into()));
    }
    let user = app
        .auth
        .identify(&headers, peer.ip())
        .map_err(|s| (s, "authentication failed".into()))?;
    if !app.hub.owns_device(&user, request.device_id).await {
        return Err((StatusCode::NOT_FOUND, "device not found".into()));
    }
    let domain = app.access_domain.as_deref().ok_or((
        StatusCode::PRECONDITION_FAILED,
        "isolated access domain is not configured".into(),
    ))?;
    let management = host(&headers).ok_or((StatusCode::BAD_REQUEST, "missing Host".into()))?;
    if cookie_site(&management) == cookie_site(domain) {
        return Err((
            StatusCode::PRECONDITION_FAILED,
            "access domain must use a different cookie site from management".into(),
        ));
    }
    let request_id = request.id;
    let endpoint = app
        .endpoints
        .create(&user, request, access_public_url(request_id, domain))
        .await
        .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?;
    sync_endpoint(&app, &user, &endpoint).await;
    Ok((
        StatusCode::CREATED,
        Json(endpoint_view(&app, endpoint).await),
    ))
}
async fn patch_endpoint(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(id): Path<uuid::Uuid>,
    headers: HeaderMap,
    Json(change): Json<cofleet::access::PatchEndpoint>,
) -> Result<Json<EndpointView>, (StatusCode, String)> {
    if !request_marked(&headers) {
        return Err((StatusCode::FORBIDDEN, "missing request marker".into()));
    }
    let user = app
        .auth
        .identify(&headers, peer.ip())
        .map_err(|s| (s, "authentication failed".into()))?;
    let endpoint = app
        .endpoints
        .patch(&user, id, change)
        .await
        .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "endpoint not found".into()))?;
    sync_endpoint(&app, &user, &endpoint).await;
    if !endpoint.enabled {
        let _ = app
            .hub
            .observe(
                &user,
                endpoint.definition.device_id,
                cofleet::docker_observe::ObserveRequest::Revoke {
                    endpoint_id: id,
                    revision: endpoint.definition.revision,
                },
            )
            .await;
    }
    Ok(Json(endpoint_view(&app, endpoint).await))
}
async fn delete_endpoint(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(id): Path<uuid::Uuid>,
    Query(query): Query<RevisionQuery>,
    headers: HeaderMap,
) -> Result<StatusCode, (StatusCode, String)> {
    if !request_marked(&headers) {
        return Err((StatusCode::FORBIDDEN, "missing request marker".into()));
    }
    let user = app
        .auth
        .identify(&headers, peer.ip())
        .map_err(|s| (s, "authentication failed".into()))?;
    let old = app
        .endpoints
        .get(&user, id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "endpoint not found".into()))?;
    if !app
        .endpoints
        .delete(&user, id, query.revision)
        .await
        .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?
    {
        return Err((StatusCode::NOT_FOUND, "endpoint not found".into()));
    }
    app.hub.cancel_access_endpoint(&user, id);
    app.endpoint_status
        .write()
        .await
        .remove(&(user.clone(), id));
    let _ = app
        .hub
        .observe(
            &user,
            old.definition.device_id,
            cofleet::docker_observe::ObserveRequest::Revoke {
                endpoint_id: id,
                revision: query.revision.saturating_add(1),
            },
        )
        .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn access_request(
    app: &App,
    endpoint: cofleet::access::AccessEndpoint,
    peer: SocketAddr,
    mut request: Request,
    authority: String,
) -> Response {
    if !endpoint.enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    let user = match app.auth.identify(request.headers(), peer.ip()) {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if user != endpoint.owner {
        return StatusCode::NOT_FOUND.into_response();
    }
    if !cofleet::access_proxy::origin_allowed(request.headers(), &authority) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if app.auth.mode == AuthMode::Development {
        request.headers_mut().remove(header::AUTHORIZATION);
    }
    let authorized = app
        .hub
        .observe(
            &user,
            endpoint.definition.device_id,
            cofleet::docker_observe::ObserveRequest::Authorize {
                definition: endpoint.definition.clone(),
            },
        )
        .await;
    let resolution = match authorized {
        Ok(cofleet::docker_observe::ObserveReply::Authorized { resolution }) => resolution,
        Ok(cofleet::docker_observe::ObserveReply::Error { .. }) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        _ => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let stream = match app.hub.access_stream(&user, &endpoint.definition).await {
        Ok(stream) => stream,
        Err(status) => return status.into_response(),
    };
    let upstream = match endpoint.definition.protocol {
        cofleet::access::AccessProtocol::Http => {
            format!("http://{}:{}", resolution.host, resolution.port)
        }
        cofleet::access::AccessProtocol::Https => format!(
            "https://{}:{}",
            endpoint
                .definition
                .tls_server_name
                .as_deref()
                .unwrap_or("invalid"),
            resolution.port
        ),
    };
    match endpoint.definition.protocol {
        cofleet::access::AccessProtocol::Http => {
            cofleet::access_proxy::proxy(request, stream, &authority, &upstream)
                .await
                .unwrap_or_else(IntoResponse::into_response)
        }
        cofleet::access::AccessProtocol::Https => {
            let name = endpoint.definition.tls_server_name.as_deref().unwrap();
            match cofleet::access_proxy::tls(stream, name).await {
                Ok(tls) => cofleet::access_proxy::proxy(request, tls, &authority, &upstream)
                    .await
                    .unwrap_or_else(IntoResponse::into_response),
                Err(status) => status.into_response(),
            }
        }
    }
}
async fn dispatch(
    State(front): State<Front>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
) -> Response {
    let authority = host(request.headers()).unwrap_or_default();
    if let Some(domain) = &front.app.access_domain {
        let suffix = format!(".{}", domain.trim_end_matches('.').to_ascii_lowercase());
        if authority.ends_with(&suffix) && authority.starts_with("e-") {
            return match front.app.endpoints.find_public_host(&authority).await {
                Ok(Some(endpoint)) => {
                    access_request(&front.app, endpoint, peer, request, authority).await
                }
                Ok(None) => StatusCode::NOT_FOUND.into_response(),
                Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            };
        }
    }
    front
        .management
        .clone()
        .oneshot(request)
        .await
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
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
async fn schedule_workload(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<cofleet::scheduler::ScheduleRequest>,
) -> Result<Json<cofleet::scheduler::Placement>, (StatusCode, String)> {
    if headers
        .get("x-cofleet-request")
        .and_then(|v| v.to_str().ok())
        != Some("1")
    {
        return Err((StatusCode::FORBIDDEN, "missing request marker".into()));
    }
    let user = app
        .auth
        .identify(&headers, peer.ip())
        .map_err(|status| (status, "authentication failed".into()))?;
    app.hub
        .schedule(&user, request)
        .await
        .map(Json)
        .map_err(|error| (StatusCode::CONFLICT, error.to_string()))
}
async fn workloads(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<Vec<cofleet::scheduler::Placement>>, StatusCode> {
    let user = app.auth.identify(&headers, peer.ip())?;
    Ok(Json(app.hub.workloads(&user).await))
}
async fn remove_workload(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(workload): Path<uuid::Uuid>,
    headers: HeaderMap,
) -> Result<StatusCode, StatusCode> {
    if headers
        .get("x-cofleet-request")
        .and_then(|v| v.to_str().ok())
        != Some("1")
    {
        return Err(StatusCode::FORBIDDEN);
    }
    let user = app.auth.identify(&headers, peer.ip())?;
    if app.hub.remove_workload(&user, workload).await {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
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
        for path in ["/workspaces/", "/applications/"] {
            response.headers_mut().append(
                header::SET_COOKIE,
                format!(
                    "cofleet_workspace={}; Path={path}; HttpOnly; SameSite=Strict",
                    app.workspace_cookie
                )
                .parse()
                .unwrap(),
            );
        }
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
    let mut response = StatusCode::NO_CONTENT.into_response();
    for path in ["/workspaces/", "/applications/"] {
        response.headers_mut().append(
            header::SET_COOKIE,
            format!("cofleet_workspace=; Path={path}; HttpOnly; SameSite=Strict; Max-Age=0")
                .parse()
                .unwrap(),
        );
    }
    Ok(response)
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
    let target = if request.uri().path().starts_with("/applications/") {
        cofleet::protocol::TunnelTarget::Application
    } else {
        cofleet::protocol::TunnelTarget::Workspace
    };
    let prefix = match target {
        cofleet::protocol::TunnelTarget::Application => format!("/applications/{device}/"),
        cofleet::protocol::TunnelTarget::Workspace => format!("/workspaces/{device}/"),
    };
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
    let stream = app.hub.workspace_stream(&user, device, target).await?;
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
    if let Some(domain) = &args.access_domain {
        anyhow::ensure!(
            !domain.contains(['/', ':', '@', '\\']) && domain.contains('.'),
            "--access-domain must be a bare DNS suffix"
        );
    }
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
    let endpoints = cofleet::access::EndpointStore::open(&args.state_dir).await?;
    let app = App {
        workspace_cookie: uuid::Uuid::new_v4().to_string(),
        auth: Arc::new(Authenticator {
            mode: args.auth,
            token,
            trusted_ingress: args.trusted_ingress_ip,
        }),
        hub: Hub::default(),
        connections: Arc::new(tokio::sync::Semaphore::new(256)),
        endpoints,
        access_domain: args
            .access_domain
            .map(|v| v.trim().trim_end_matches('.').to_ascii_lowercase()),
        endpoint_status: Default::default(),
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
        .route("/applications/{device}", any(workspace_proxy))
        .route("/applications/{device}/", any(workspace_proxy))
        .route("/applications/{device}/{*path}", any(workspace_proxy))
        .route("/workspaces/{device}", any(workspace_proxy))
        .route("/workspaces/{device}/", any(workspace_proxy))
        .route("/workspaces/{device}/{*path}", any(workspace_proxy))
        .route("/api/v1/devices", get(devices))
        .route("/api/v1/workloads", get(workloads).post(schedule_workload))
        .route("/api/v1/workloads/{workload}", delete(remove_workload))
        .route("/api/v1/devices/{device}/refresh", post(refresh))
        .route("/api/v1/devices/{device}/docker", post(docker_command))
        .route(
            "/api/v1/devices/{device}/docker/catalog",
            get(docker_catalog),
        )
        .route(
            "/api/v1/devices/{device}/docker/containers/{container}/logs",
            get(docker_logs),
        )
        .route(
            "/api/v1/access-endpoints",
            get(access_endpoints).post(create_endpoint),
        )
        .route(
            "/api/v1/access-endpoints/{id}",
            patch(patch_endpoint).delete(delete_endpoint),
        )
        .layer(DefaultBodyLimit::max(4096))
        .with_state(app.clone());
    let routes = Router::new().fallback(dispatch).with_state(Front {
        app,
        management: routes,
    });
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
