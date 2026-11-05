//! Application-level links only. No discovery, credentials, device mapping or proxying.
use anyhow::{Result, ensure};

pub fn launch_url(value: &str) -> Result<String> {
    let url = reqwest::Url::parse(value.trim())?;
    ensure!(
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path() == "/",
        "external tool must use an HTTPS application root without credentials, query or fragment"
    );
    Ok(url.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_only_credential_free_application_roots() {
        assert_eq!(
            launch_url("https://terminal.example").unwrap(),
            "https://terminal.example/"
        );
        for value in [
            "javascript:alert(1)",
            "http://example/",
            "https://user:pass@example/",
            "https://example/?token=secret",
            "https://example/#secret",
            "https://example/desktop/device",
            "//example/",
        ] {
            assert!(launch_url(value).is_err(), "{value}");
        }
    }
}
