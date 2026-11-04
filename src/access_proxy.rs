//! Header and stream policy for untrusted existing Web applications.
use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use hyper_util::rt::TokioIo;
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncRead, AsyncWrite};

pub fn reserved_cookie(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    matches!(
        name.as_str(),
        "lzc-auth-token" | "hc-auth-token" | "__host-cofleet_access"
    ) || name.starts_with("cofleet_")
        || name.starts_with("__host-cofleet_")
}
fn remove_hop(headers: &mut HeaderMap) {
    let fields: Vec<String> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(',').map(|s| s.trim().to_owned()))
        .collect();
    for field in fields {
        headers.remove(field);
    }
    for field in [
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(field);
    }
}
pub fn origin_allowed(headers: &HeaderMap, authority: &str) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
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
        && origin.authority().eq_ignore_ascii_case(authority)
}
pub fn request_headers(
    headers: &mut HeaderMap,
    authority: &str,
    websocket: bool,
) -> Result<(), StatusCode> {
    let cookies = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|part| {
            let (name, _) = part.trim().split_once('=')?;
            (!reserved_cookie(name.trim())).then(|| part.trim().to_owned())
        })
        .collect::<Vec<_>>()
        .join("; ");
    let upgrade = headers.get(header::UPGRADE).cloned();
    remove_hop(headers);
    let names: Vec<_> = headers.keys().cloned().collect();
    for name in names {
        let key = name.as_str();
        if key.starts_with("x-hc-")
            || key.starts_with("x-lzc")
            || key.starts_with("x_lzc")
            || key.starts_with("x-cofleet")
            || key.starts_with("x-forwarded-")
            || matches!(
                key,
                "forwarded" | "cookie" | "lzc-misc-auth-token" | "lzc-api-auth-token"
            )
        {
            headers.remove(name);
        }
    }
    if !cookies.is_empty() {
        headers.insert(
            header::COOKIE,
            cookies.parse().map_err(|_| StatusCode::BAD_REQUEST)?,
        );
    }
    headers.insert(
        header::HOST,
        authority.parse().map_err(|_| StatusCode::BAD_REQUEST)?,
    );
    headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
    headers.insert(
        "x-forwarded-host",
        authority.parse().map_err(|_| StatusCode::BAD_REQUEST)?,
    );
    if websocket {
        headers.insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
        if let Some(value) = upgrade {
            headers.insert(header::UPGRADE, value);
        }
    }
    Ok(())
}
pub fn response_headers(
    headers: &mut HeaderMap,
    upstream: &str,
    public: &str,
    websocket: bool,
) -> Result<(), StatusCode> {
    let cookies = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|line| {
            let mut parts = line.split(';');
            let first = parts.next()?;
            let (name, _) = first.trim().split_once('=')?;
            if reserved_cookie(name.trim()) {
                return None;
            }
            let mut attributes = vec![first.trim().to_owned()];
            for part in parts {
                if !part
                    .trim()
                    .split('=')
                    .next()
                    .unwrap_or("")
                    .eq_ignore_ascii_case("domain")
                {
                    attributes.push(part.trim().to_owned());
                }
            }
            Some(attributes.join("; "))
        })
        .collect::<Vec<_>>();
    let upgrade = headers.get(header::UPGRADE).cloned();
    remove_hop(headers);
    headers.remove(header::SET_COOKIE);
    for cookie in cookies {
        headers.append(
            header::SET_COOKIE,
            cookie.parse().map_err(|_| StatusCode::BAD_GATEWAY)?,
        );
    }
    let names: Vec<_> = headers.keys().cloned().collect();
    for name in names {
        if name.as_str().starts_with("access-control-") || name.as_str() == "clear-site-data" {
            headers.remove(name);
        }
    }
    if let Some(location) = headers.get(header::LOCATION).and_then(|v| v.to_str().ok())
        && let (Ok(url), Ok(base)) = (reqwest::Url::parse(location), reqwest::Url::parse(upstream))
    {
        if url.origin() != base.origin()
            && url
                .host_str()
                .is_some_and(|h| h == "localhost" || h.parse::<std::net::IpAddr>().is_ok())
        {
            return Err(StatusCode::BAD_GATEWAY);
        }
        if url.origin() == base.origin() {
            let rewritten = format!(
                "{public}{}{}{}",
                url.path(),
                url.query().map(|q| format!("?{q}")).unwrap_or_default(),
                url.fragment().map(|v| format!("#{v}")).unwrap_or_default()
            );
            headers.insert(
                header::LOCATION,
                rewritten.parse().map_err(|_| StatusCode::BAD_GATEWAY)?,
            );
        }
    }
    if websocket {
        headers.insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
        if let Some(value) = upgrade {
            headers.insert(header::UPGRADE, value);
        }
    }
    Ok(())
}
pub async fn proxy<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    mut request: Request,
    stream: S,
    authority: &str,
    upstream: &str,
) -> Result<Response, StatusCode> {
    let websocket = request
        .headers()
        .get(header::UPGRADE)
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"));
    request_headers(request.headers_mut(), authority, websocket)?;
    if request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|n| n > 32 * 1024 * 1024)
    {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let downstream = websocket.then(|| hyper::upgrade::on(&mut request));
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
    let switching = response.status() == StatusCode::SWITCHING_PROTOCOLS;
    response_headers(
        response.headers_mut(),
        upstream,
        &format!("https://{authority}"),
        switching,
    )?;
    if switching {
        let downstream = downstream.ok_or(StatusCode::BAD_GATEWAY)?;
        let upstream = hyper::upgrade::on(&mut response);
        tokio::spawn(async move {
            if let (Ok(down), Ok(up)) = tokio::join!(downstream, upstream) {
                let _ =
                    tokio::io::copy_bidirectional(&mut TokioIo::new(down), &mut TokioIo::new(up))
                        .await;
            }
        });
    }
    Ok(response.map(Body::new).into_response())
}
pub async fn tls(
    stream: tokio::io::DuplexStream,
    name: &str,
) -> Result<tokio_rustls::client::TlsStream<tokio::io::DuplexStream>, StatusCode> {
    let roots = tokio_rustls::rustls::RootCertStore::from_iter(
        webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
    );
    let config = tokio_rustls::rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let server = tokio_rustls::rustls::pki_types::ServerName::try_from(name.to_owned())
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    tokio::time::timeout(
        Duration::from_secs(10),
        tokio_rustls::TlsConnector::from(Arc::new(config)).connect(server, stream),
    )
    .await
    .map_err(|_| StatusCode::GATEWAY_TIMEOUT)?
    .map_err(|_| StatusCode::BAD_GATEWAY)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credentials_and_cookie_domains() {
        let mut h = HeaderMap::new();
        h.append("cookie", "HC-Auth-Token=platform; app=ok".parse().unwrap());
        h.insert("x-hc-user-id", "secret".parse().unwrap());
        h.insert("authorization", "Bearer application".parse().unwrap());
        request_headers(&mut h, "app.example", false).unwrap();
        assert_eq!(h["cookie"], "app=ok");
        assert!(!h.contains_key("x-hc-user-id"));
        assert_eq!(h["authorization"], "Bearer application");
        h.clear();
        h.append(
            "set-cookie",
            "app=ok; Domain=.example; Path=/; HttpOnly".parse().unwrap(),
        );
        h.append(
            "set-cookie",
            "HC-Auth-Token=evil; Domain=.example".parse().unwrap(),
        );
        response_headers(
            &mut h,
            "http://127.0.0.1:3000",
            "https://app.example",
            false,
        )
        .unwrap();
        assert_eq!(h.get_all("set-cookie").iter().count(), 1);
        assert!(!h["set-cookie"].to_str().unwrap().contains("Domain"));
    }
}
