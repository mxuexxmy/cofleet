//! Trusted-prefix browser authorization; compiled as part of the manager binary.
use super::*;
use cofleet::access_sessions::{self as sessions, Binding};

pub(super) fn origin(app: &App) -> Option<String> {
    app.management_domain
        .as_ref()
        .map(|d| format!("https://{d}"))
}
fn failure(status: StatusCode, message: &str) -> Response {
    (status, message.to_owned()).into_response()
}
fn page(title: &str, body: &str) -> Response {
    let mut response = Html(format!("<!doctype html><html lang=\"zh-CN\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>{title}</title><style>body{{font:15px system-ui,sans-serif;background:#f3f6f8;color:#223544;max-width:620px;margin:12vh auto;padding:24px}}button{{background:#127568;color:white;border:0;border-radius:6px;padding:12px 20px;cursor:pointer}}p{{line-height:1.7}}</style><h1>{title}</h1>{body}</html>")).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert("referrer-policy", "strict-origin".parse().unwrap());
    response.headers_mut().insert("content-security-policy", "default-src 'none'; style-src 'unsafe-inline'; form-action https:; frame-ancestors 'none'; base-uri 'none'".parse().unwrap());
    response
}
fn redirect(url: &str) -> Response {
    let mut r = StatusCode::SEE_OTHER.into_response();
    r.headers_mut()
        .insert(header::LOCATION, url.parse().unwrap());
    r.headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    r
}
fn set(response: &mut Response, name: &str, value: &str, age: u64) {
    response.headers_mut().append(
        header::SET_COOKIE,
        sessions::set_cookie(name, value, age).parse().unwrap(),
    );
}
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
pub(super) fn management_session(app: &App, headers: &HeaderMap, owner: &str) -> Option<String> {
    sessions::cookie(headers, sessions::MANAGER_COOKIE)
        .filter(|v| app.access_sessions.manager_valid(v, owner))
}
pub(super) fn mutation_allowed(app: &App, headers: &HeaderMap, owner: &str) -> bool {
    !app.trusted_prefix
        || (management_session(app, headers, owner).is_some()
            && headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) == origin(app).as_deref()
            && request_marked(headers))
}
pub(super) async fn binding(
    app: &App,
    endpoint: &cofleet::access::AccessEndpoint,
    authority: &str,
) -> Option<Binding> {
    if !endpoint.enabled
        || !app
            .endpoints
            .is_trusted(endpoint.definition.id)
            .await
            .ok()?
    {
        return None;
    }
    Some(Binding {
        owner: endpoint.owner.clone(),
        host: authority.into(),
        endpoint: endpoint.definition.id,
        revision: endpoint.definition.revision,
        agent_session: app
            .hub
            .current_session(&endpoint.owner, endpoint.definition.device_id)
            .await?,
    })
}
async fn form(request: Request) -> Option<std::collections::HashMap<String, String>> {
    if !request
        .headers()
        .get(header::CONTENT_TYPE)?
        .to_str()
        .ok()?
        .starts_with("application/x-www-form-urlencoded")
    {
        return None;
    }
    let bytes = axum::body::to_bytes(request.into_body(), 4096).await.ok()?;
    let parsed = reqwest::Url::parse(&format!(
        "https://fixture.invalid/?{}",
        std::str::from_utf8(&bytes).ok()?
    ))
    .ok()?;
    Some(
        parsed
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect(),
    )
}
pub(super) async fn authorize(app: &App, peer: SocketAddr, request: Request) -> Response {
    let user = match app.auth.identify(request.headers(), peer.ip()) {
        Ok(v) => v,
        Err(s) => return s.into_response(),
    };
    let Some(manager) = management_session(app, request.headers(), &user) else {
        return page(
            "请先打开管理中心",
            "<p>管理会话尚未建立或已过期，请打开管理中心后重新点击应用入口。</p><a href=\"/\">打开管理中心</a>",
        );
    };
    let post = request.method() == axum::http::Method::POST;
    if post
        && request
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            != origin(app).as_deref()
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let challenge = if post {
        form(request)
            .await
            .and_then(|v| v.get("challenge").cloned())
    } else {
        request
            .uri()
            .query()
            .and_then(|q| reqwest::Url::parse(&format!("https://fixture.invalid/?{q}")).ok())
            .and_then(|u| {
                u.query_pairs()
                    .find(|(k, _)| k == "challenge")
                    .map(|(_, v)| v.into_owned())
            })
    };
    let Some(challenge) = challenge else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some((id, authority)) = app.access_sessions.challenge(&challenge) else {
        return failure(StatusCode::GONE, "授权请求已过期，请重新打开入口。");
    };
    let endpoint = match app.endpoints.get(&user, id).await {
        Ok(Some(v)) => v,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let Some(binding) = binding(app, &endpoint, &authority).await else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "应用已禁用或设备离线，请返回管理中心检查。",
        );
    };
    if !post {
        return page(
            "打开受信任应用",
            &format!(
                "<p>即将打开：<strong>{}</strong></p><p>此应用使用与微服共享父域的独立入口。仅继续访问你信任的应用。</p><form method=\"post\" action=\"/access/authorize\"><input type=\"hidden\" name=\"challenge\" value=\"{}\"><button type=\"submit\">继续打开</button></form>",
                escape(&endpoint.name),
                escape(&challenge)
            ),
        );
    }
    let Some(ticket) = app.access_sessions.issue(&challenge, &manager, binding) else {
        return failure(
            StatusCode::CONFLICT,
            "授权请求已使用或过期，请重新打开入口。",
        );
    };
    // Explicit top-level form: no ticket in URL, no inline script or permissive CORS.
    page(
        "授权已就绪",
        &format!(
            "<p>继续前往 {}。授权仅供当前浏览器使用一次。</p><form method=\"post\" action=\"https://{}/.cofleet/accept\"><input type=\"hidden\" name=\"ticket\" value=\"{}\"><button type=\"submit\">打开应用</button></form>",
            escape(&endpoint.name),
            escape(&authority),
            ticket
        ),
    )
}
pub(super) async fn application(
    app: &App,
    endpoint: cofleet::access::AccessEndpoint,
    peer: SocketAddr,
    request: Request,
    authority: String,
) -> Response {
    let user = match app.auth.identify(request.headers(), peer.ip()) {
        Ok(v) => v,
        Err(s) => return s.into_response(),
    };
    if user != endpoint.owner {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(bound) = binding(app, &endpoint, &authority).await else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "入口尚未获信任、已禁用或设备离线。",
        );
    };
    if request.uri().path().starts_with("/.cofleet/") {
        if request.uri().path() != "/.cofleet/accept"
            || request.method() != axum::http::Method::POST
        {
            return StatusCode::NOT_FOUND.into_response();
        }
        if request
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            != origin(app).as_deref()
        {
            return StatusCode::FORBIDDEN.into_response();
        }
        let Some(bootstrap) = sessions::cookie(request.headers(), sessions::BOOTSTRAP_COOKIE)
        else {
            return StatusCode::FORBIDDEN.into_response();
        };
        let Some(ticket) = form(request).await.and_then(|v| v.get("ticket").cloned()) else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        let Some(access) = app.access_sessions.redeem(&ticket, &bootstrap, &bound) else {
            return failure(
                StatusCode::FORBIDDEN,
                "授权无效、已使用或过期，请重新打开入口。",
            );
        };
        let mut r = redirect(&format!("https://{authority}/"));
        set(&mut r, sessions::ACCESS_COOKIE, &access, 900);
        set(&mut r, sessions::BOOTSTRAP_COOKIE, "", 0);
        return r;
    }
    if sessions::cookie(request.headers(), sessions::ACCESS_COOKIE)
        .is_some_and(|v| app.access_sessions.valid(&v, &bound))
    {
        return access_request(app, endpoint, peer, request, authority).await;
    }
    if request.method() != axum::http::Method::GET
        || request.headers().contains_key(header::UPGRADE)
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some((raw, challenge)) = app
        .access_sessions
        .begin(endpoint.definition.id, &authority)
    else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    let mut r = redirect(&format!(
        "{}/access/authorize?challenge={challenge}",
        origin(app).unwrap()
    ));
    set(&mut r, sessions::BOOTSTRAP_COOKIE, &raw, 60);
    r
}
pub(super) async fn logout(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let user = match app.auth.identify(&headers, peer.ip()) {
        Ok(v) => v,
        Err(s) => return s.into_response(),
    };
    if !mutation_allowed(&app, &headers, &user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Some(manager) = management_session(&app, &headers, &user) {
        app.access_sessions.logout(&manager);
    }
    if let Ok(endpoints) = app.endpoints.list(&user).await {
        for endpoint in endpoints {
            app.hub
                .cancel_access_endpoint(&user, endpoint.definition.id);
        }
    }
    let mut r = StatusCode::NO_CONTENT.into_response();
    set(&mut r, sessions::MANAGER_COOKIE, "", 0);
    r
}
pub(super) async fn dispatch(front: Front, peer: SocketAddr, request: Request) -> Response {
    let authority = host(request.headers()).unwrap_or_default();
    let management = front.app.management_domain.as_deref().unwrap_or("");
    if authority != management {
        let expected_suffix = format!("-{management}");
        if authority.starts_with("e-") && authority.ends_with(&expected_suffix) {
            return match front.app.endpoints.find_public_host(&authority).await {
                Ok(Some(endpoint)) => {
                    application(&front.app, endpoint, peer, request, authority).await
                }
                Ok(None) => StatusCode::NOT_FOUND.into_response(),
                Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            };
        }
        return StatusCode::NOT_FOUND.into_response();
    }
    if request.uri().path() == "/access/authorize" {
        return authorize(&front.app, peer, request).await;
    }
    let issue = if request.method() == axum::http::Method::GET && request.uri().path() == "/" {
        front
            .app
            .auth
            .identify(request.headers(), peer.ip())
            .ok()
            .and_then(|owner| {
                if management_session(&front.app, request.headers(), &owner).is_none() {
                    front.app.access_sessions.manager(&owner)
                } else {
                    None
                }
            })
    } else {
        None
    };
    if request.headers().contains_key(header::ORIGIN)
        && !matches!(
            *request.method(),
            axum::http::Method::GET | axum::http::Method::HEAD
        )
        && request
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            != origin(&front.app).as_deref()
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut response = front
        .management
        .clone()
        .oneshot(request)
        .await
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    if let Some(token) = issue {
        set(&mut response, sessions::MANAGER_COOKIE, &token, 8 * 3600);
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    }
    response
}

pub(super) fn leased_stream(
    app: App,
    mut upstream: tokio::io::DuplexStream,
    token: String,
    binding: Binding,
) -> tokio::io::DuplexStream {
    let (front, mut back) = tokio::io::duplex(32 * 1024);
    tokio::spawn(async move {
        let revoke = async {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                interval.tick().await;
                if !app.access_sessions.valid(&token, &binding) {
                    break;
                }
                let endpoint = match app.endpoints.get(&binding.owner, binding.endpoint).await {
                    Ok(Some(v)) => v,
                    _ => break,
                };
                if !endpoint.enabled
                    || endpoint.definition.revision != binding.revision
                    || app
                        .hub
                        .current_session(&binding.owner, endpoint.definition.device_id)
                        .await
                        != Some(binding.agent_session)
                {
                    break;
                }
            }
        };
        tokio::select! {_=tokio::io::copy_bidirectional(&mut upstream,&mut back)=>{},_=revoke=>{}}
    });
    front
}
