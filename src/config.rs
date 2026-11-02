use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    pub device_id: uuid::Uuid,
    pub name: String,
    pub manager_url: String,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub auth: crate::auth::AuthMode,
    #[serde(default)]
    pub lzc_core_config: Option<PathBuf>,
    pub heartbeat_secs: u64,
    pub accepting_tasks: bool,
}

pub fn data_dir() -> PathBuf {
    directories::ProjectDirs::from("dev", "cofleet", "cofleet")
        .expect("cannot locate user data directory")
        .data_local_dir()
        .to_path_buf()
}

pub fn private_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(path)?;
        let meta = fs::symlink_metadata(path)?;
        anyhow::ensure!(
            meta.is_dir() && !meta.file_type().is_symlink(),
            "data directory must be a real directory"
        );
        let own_uid = unsafe { libc::geteuid() };
        anyhow::ensure!(
            meta.uid() == own_uid && meta.mode() & 0o077 == 0,
            "data directory must be owned by this user and mode 0700"
        );
    }
    #[cfg(windows)]
    {
        fs::create_dir_all(path)?;
        crate::windows_security::protect_directory(path)?;
    }
    Ok(())
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        let url = reqwest::Url::parse(&self.manager_url).context("invalid manager URL")?;
        let loopback = matches!(
            url.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
        );
        if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
            bail!("manager URL requires HTTPS; HTTP is allowed only on loopback for tests");
        }
        anyhow::ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "URL must not include credentials, query or fragment"
        );
        anyhow::ensure!(url.host_str().is_some(), "manager URL needs a host");
        anyhow::ensure!(
            (1..=300).contains(&self.heartbeat_secs),
            "heartbeat_secs must be 1..300"
        );
        if self.auth == crate::auth::AuthMode::Development {
            anyhow::ensure!(
                self.token.trim().len() >= 16 && !self.token.contains(['\n', '\r']),
                "token is empty or invalid"
            );
        } else {
            anyhow::ensure!(
                url.scheme() == "https",
                "LazyCat authentication requires HTTPS"
            );
            anyhow::ensure!(
                self.token.is_empty(),
                "LazyCat mode must not store a shared token"
            );
        }
        anyhow::ensure!(
            !self.name.trim().is_empty() && self.name.len() <= 128,
            "name must be 1..128 bytes"
        );
        Ok(())
    }

    pub fn load(dir: &Path) -> Result<Self> {
        private_dir(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = fs::symlink_metadata(dir.join("agent.toml"))
                .context("run cofleetctl init first")?;
            anyhow::ensure!(
                metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && metadata.uid() == unsafe { libc::geteuid() }
                    && metadata.mode() & 0o077 == 0,
                "agent.toml must be a private file owned by this user (mode 0600)"
            );
        }
        let config: Self = toml::from_str(
            &fs::read_to_string(dir.join("agent.toml")).context("run cofleetctl init first")?,
        )
        .map_err(|_| anyhow::anyhow!("invalid agent.toml; check syntax and required fields"))?;
        config.validate()?;
        Ok(config)
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        self.validate()?;
        private_dir(dir)?;
        let path = dir.join("agent.toml");
        let tmp = dir.join(format!(".config-{}", uuid::Uuid::new_v4()));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| -> Result<()> {
            use std::io::Write;
            let mut file = options.open(&tmp)?;
            file.write_all(toml::to_string_pretty(self)?.as_bytes())?;
            file.sync_all()?;
            #[cfg(windows)]
            {
                // Windows rename cannot replace an existing file; use ReplaceFileW below.
                if path.exists() {
                    replace_windows(&tmp, &path)?;
                } else {
                    fs::rename(&tmp, &path)?;
                }
            }
            #[cfg(not(windows))]
            fs::rename(&tmp, &path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(tmp);
        }
        result
    }
}

#[cfg(windows)]
fn replace_windows(source: &Path, destination: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    unsafe extern "system" {
        fn ReplaceFileW(
            replaced: *const u16,
            replacement: *const u16,
            backup: *const u16,
            flags: u32,
            exclude: *const std::ffi::c_void,
            reserved: *const std::ffi::c_void,
        ) -> i32;
    }
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    anyhow::ensure!(
        unsafe {
            ReplaceFileW(
                destination.as_ptr(),
                source.as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                std::ptr::null(),
            )
        } != 0,
        "replace configuration failed: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_remote_plaintext_and_invalid_intervals() {
        let mut c = Config {
            device_id: uuid::Uuid::new_v4(),
            name: "node".into(),
            manager_url: "http://127.0.0.1:9760".into(),
            token: "test-token-0123456789".into(),
            auth: crate::auth::AuthMode::Development,
            lzc_core_config: None,
            heartbeat_secs: 5,
            accepting_tasks: true,
        };
        assert!(c.validate().is_ok());
        c.manager_url = "http://example.com".into();
        assert!(c.validate().is_err());
        c.manager_url = "https://example.com".into();
        assert!(c.validate().is_ok());
        c.heartbeat_secs = 0;
        assert!(c.validate().is_err());
    }
    #[test]
    fn saves_and_replaces_configuration() {
        let dir = std::env::temp_dir().join(format!("cofleet-test-{}", uuid::Uuid::new_v4()));
        let mut c = Config {
            device_id: uuid::Uuid::new_v4(),
            name: "node".into(),
            manager_url: "http://127.0.0.1:9760".into(),
            token: "test-token-0123456789".into(),
            auth: crate::auth::AuthMode::Development,
            lzc_core_config: None,
            heartbeat_secs: 5,
            accepting_tasks: true,
        };
        c.save(&dir).unwrap();
        c.accepting_tasks = false;
        c.save(&dir).unwrap();
        assert!(!Config::load(&dir).unwrap().accepting_tasks);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(dir.join("agent.toml"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
