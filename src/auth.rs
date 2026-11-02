//! Identities come from the deployment authentication boundary, never Agent JSON.
use axum::http::{HeaderMap, StatusCode};
use clap::ValueEnum;
use std::net::IpAddr;

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    #[default]
    Development,
    Lazycat,
}

#[derive(Clone)]
pub struct Authenticator {
    pub mode: AuthMode,
    pub token: String,
    pub trusted_ingress: Vec<IpAddr>,
}
impl Authenticator {
    pub fn identify(&self, headers: &HeaderMap, peer: IpAddr) -> Result<String, StatusCode> {
        match self.mode {
            AuthMode::Development => {
                let expected = format!("Bearer {}", self.token);
                let actual = headers
                    .get("authorization")
                    .ok_or(StatusCode::UNAUTHORIZED)?
                    .as_bytes();
                let matched = actual.len() == expected.len()
                    && actual
                        .iter()
                        .zip(expected.as_bytes())
                        .fold(0u8, |d, (a, b)| d | (a ^ b))
                        == 0;
                if matched {
                    Ok("development".into())
                } else {
                    Err(StatusCode::UNAUTHORIZED)
                }
            }
            AuthMode::Lazycat => {
                if !self.trusted_ingress.contains(&peer) {
                    return Err(StatusCode::FORBIDDEN);
                }
                let uid = headers
                    .get("x-hc-user-id")
                    .and_then(|v| v.to_str().ok())
                    .map(str::trim)
                    .filter(|v| !v.is_empty() && v.len() <= 128)
                    .ok_or(StatusCode::UNAUTHORIZED)?;
                Ok(uid.into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ingress_identity_requires_the_trusted_peer() {
        let ingress = "127.0.0.2".parse().unwrap();
        let auth = Authenticator {
            mode: AuthMode::Lazycat,
            token: String::new(),
            trusted_ingress: vec![ingress],
        };
        let mut headers = HeaderMap::new();
        headers.insert("x-hc-user-id", "alice".parse().unwrap());
        assert_eq!(auth.identify(&headers, ingress).unwrap(), "alice");
        assert_eq!(
            auth.identify(&headers, "127.0.0.1".parse().unwrap()),
            Err(StatusCode::FORBIDDEN)
        );
        headers.clear();
        assert_eq!(
            auth.identify(&headers, ingress),
            Err(StatusCode::UNAUTHORIZED)
        );
    }
}
