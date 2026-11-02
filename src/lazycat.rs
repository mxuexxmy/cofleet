//! Experimental adapter for the local LazyCat desktop client APIs observed in
//! lzc-mesh. Compatibility must be verified against installed client versions.
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
struct Core {
    http_port: u16,
    extra_headers: Headers,
}
#[derive(Deserialize)]
struct Headers {
    #[serde(rename = "LZC_MISC_AUTH_TOKEN")]
    token: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BoxInfo {
    box_domain: String,
    status: i32,
}
#[derive(Deserialize)]
struct Cookie {
    name: String,
    value: String,
}
#[derive(Deserialize)]
struct Extension {
    cookies: Vec<Cookie>,
}

pub fn default_core_path() -> PathBuf {
    directories::BaseDirs::new()
        .expect("user home directory")
        .home_dir()
        .join(".config/lzc-client-desktop/core.cfg")
}
fn valid_destination(manager: &reqwest::Url, domain: &str) -> bool {
    let domain = domain.trim_end_matches('.').to_ascii_lowercase();
    let host = manager.host_str().unwrap_or("").to_ascii_lowercase();
    !domain.is_empty()
        && !domain.contains(['/', ':', '@', '\\'])
        && manager.scheme() == "https"
        && manager.port_or_known_default() == Some(443)
        && host.ends_with(&format!(".{domain}"))
        && host != domain
}
async fn json<T: serde::de::DeserializeOwned>(request: reqwest::RequestBuilder) -> Result<T> {
    let mut response = request
        .send()
        .await
        .context("local LazyCat API unavailable")?;
    anyhow::ensure!(
        response.status().is_success(),
        "local LazyCat API rejected request"
    );
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("local LazyCat API read failed")?
    {
        anyhow::ensure!(
            body.len() + chunk.len() <= 1024 * 1024,
            "local LazyCat response exceeds limit"
        );
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body)
        .map_err(|_| anyhow::anyhow!("unsupported LazyCat response format"))
}
/// Returns only an in-memory Cookie header. It is never persisted to agent.toml.
pub async fn session_cookie(manager: &str, core_path: Option<&Path>) -> Result<String> {
    let path = core_path
        .map(Path::to_path_buf)
        .unwrap_or_else(default_core_path);
    let file = std::fs::File::open(path).context("LazyCat client core.cfg not found")?;
    use std::io::Read;
    let mut bytes = Vec::new();
    file.take(65537).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= 65536, "LazyCat configuration exceeds limit");
    let core: Core = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("unsupported LazyCat client configuration"))?;
    anyhow::ensure!(
        core.http_port > 0 && !core.extra_headers.token.is_empty(),
        "incomplete LazyCat client configuration"
    );
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(5))
        .build()?;
    let base = format!("http://127.0.0.1:{}", core.http_port);
    let request = |path: &str| {
        client
            .get(format!("{base}{path}"))
            .header("LZC_MISC_AUTH_TOKEN", &core.extra_headers.token)
    };
    let info: BoxInfo = json(request("/default-box")).await?;
    anyhow::ensure!(info.status == 2, "LazyCat default box is not connected");
    let manager = reqwest::Url::parse(manager)?;
    anyhow::ensure!(
        valid_destination(&manager, &info.box_domain),
        "manager must be an HTTPS application subdomain of the connected default box on port 443"
    );
    let extension: Extension =
        json(request("/box-ext-info").query(&[("url", format!("https://{}/", info.box_domain))]))
            .await?;
    let cookie = extension
        .cookies
        .into_iter()
        .find(|c| c.name == "Lzc-Auth-Token")
        .context("LazyCat session missing; sign in with the desktop client")?;
    anyhow::ensure!(
        !cookie.value.is_empty()
            && cookie.value.len() <= 8192
            && !cookie.value.contains([';', '\r', '\n']),
        "invalid LazyCat session cookie"
    );
    Ok(format!("Lzc-Auth-Token={}", cookie.value))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cookie_destination_is_scoped_to_default_box() {
        for value in [
            "https://cofleet.mybox.heiyu.space",
            "https://cofleet.mybox.heiyu.space:443/path",
        ] {
            assert!(valid_destination(
                &reqwest::Url::parse(value).unwrap(),
                "mybox.heiyu.space"
            ));
        }
        for value in [
            "https://evilmybox.heiyu.space",
            "https://cofleet.mybox.heiyu.space.evil.com",
            "http://cofleet.mybox.heiyu.space",
            "https://mybox.heiyu.space",
            "https://cofleet.mybox.heiyu.space:444",
        ] {
            assert!(!valid_destination(
                &reqwest::Url::parse(value).unwrap(),
                "mybox.heiyu.space"
            ));
        }
    }
    #[tokio::test]
    async fn discovers_cookie_using_only_the_local_client() {
        use axum::{
            Json, Router,
            http::{HeaderMap, StatusCode},
            routing::get,
        };
        async fn default_box(headers: HeaderMap) -> Result<Json<serde_json::Value>, StatusCode> {
            if headers
                .get("LZC_MISC_AUTH_TOKEN")
                .and_then(|h| h.to_str().ok())
                != Some("local-client-secret")
            {
                return Err(StatusCode::UNAUTHORIZED);
            }
            Ok(Json(
                serde_json::json!({"boxDomain":"testbox.heiyu.space","status":2}),
            ))
        }
        async fn ext(
            axum::extract::Query(query): axum::extract::Query<
                std::collections::HashMap<String, String>,
            >,
        ) -> Json<serde_json::Value> {
            assert_eq!(query.get("url").unwrap(), "https://testbox.heiyu.space/");
            Json(
                serde_json::json!({"cookies":[{"name":"Lzc-Auth-Token","value":"ephemeral-login-cookie"}]}),
            )
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/default-box", get(default_box))
                    .route("/box-ext-info", get(ext)),
            )
            .await
            .unwrap();
        });
        let path = std::env::temp_dir().join(format!("cf-lzc-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(&path,serde_json::to_vec(&serde_json::json!({"http_port":port,"extra_headers":{"LZC_MISC_AUTH_TOKEN":"local-client-secret"}})).unwrap()).unwrap();
        let cookie = session_cookie("https://cofleet.testbox.heiyu.space", Some(&path))
            .await
            .unwrap();
        assert_eq!(cookie, "Lzc-Auth-Token=ephemeral-login-cookie");
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("ephemeral-login-cookie")
        );
        assert!(
            session_cookie("https://evil.example", Some(&path))
                .await
                .is_err()
        );
        std::fs::remove_file(path).unwrap();
        server.abort();
    }
}
