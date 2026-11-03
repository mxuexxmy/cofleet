//! One opt-in persistent development workspace per native Linux node.
use crate::docker::{DockerReply, docker};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU16, Ordering},
    },
};
use tokio::sync::Mutex;
use uuid::Uuid;
const OWNER: &str = "xyz.mxue.cofleet.device";
const SPEC: &str = "xyz.mxue.cofleet.spec";
const VERSION: &str = "workspace-v1";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum WorkspaceRequest {
    Status,
    SetRunning { request_id: Uuid, running: bool },
    Logs,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    pub desired_running: bool,
    pub generation: u64,
    pub container_id: Option<String>,
    pub state: String,
    pub healthy: bool,
    #[serde(default)]
    pub project_service_healthy: bool,
    pub volume: String,
    pub project_path: String,
    pub sampled_at_unix: u64,
    pub error: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Receipt {
    id: Uuid,
    running: bool,
}
#[derive(Clone, Serialize, Deserialize)]
struct Desired {
    device: Uuid,
    image: String,
    running: bool,
    generation: u64,
    receipts: Vec<Receipt>,
}
struct Inner {
    dir: PathBuf,
    desired: Desired,
}
#[derive(Clone)]
pub struct WorkspaceController {
    inner: Arc<Mutex<Inner>>,
    snapshot: Arc<std::sync::RwLock<WorkspaceSnapshot>>,
    port: Arc<AtomicU16>,
}
impl WorkspaceController {
    pub fn open(dir: &Path, device: Uuid, image: String) -> Result<Self> {
        ensure!(
            cfg!(target_os = "linux"),
            "workspace execution currently requires native Linux"
        );
        ensure!(
            valid_image_id(&image),
            "workspace image must be a locally approved immutable sha256 image ID"
        );
        let desired: Desired = match std::fs::read(dir.join("workspace-state.json")) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).context("invalid workspace state; refusing reset")?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Desired {
                device,
                image: image.clone(),
                running: false,
                generation: 0,
                receipts: Vec::new(),
            },
            Err(e) => return Err(e.into()),
        };
        ensure!(
            desired.device == device && desired.image == image,
            "workspace identity/image differs; refusing automatic migration"
        );
        let snapshot = WorkspaceSnapshot {
            volume: volume_name(device),
            project_path: "/home/coder/project".into(),
            state: "unknown".into(),
            ..Default::default()
        };
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                dir: dir.into(),
                desired,
            })),
            snapshot: Arc::new(std::sync::RwLock::new(snapshot)),
            port: Default::default(),
        })
    }
    pub fn snapshot(&self) -> WorkspaceSnapshot {
        self.snapshot.read().unwrap().clone()
    }
    pub fn port(&self) -> Option<u16> {
        let port = self.port.load(Ordering::Acquire);
        (port != 0).then_some(port)
    }
    pub async fn execute(&self, request: WorkspaceRequest, accepting: bool) -> DockerReply {
        let mut inner = self.inner.lock().await;
        let mut reply = DockerReply::default();
        let result: Result<()> = async {
            match request {
                WorkspaceRequest::SetRunning {
                    request_id,
                    running,
                } => {
                    let mut next = inner.desired.clone();
                    if let Some(previous) = next.receipts.iter().find(|r| r.id == request_id) {
                        ensure!(
                            previous.running == running,
                            "request ID reused for a different workspace intent"
                        );
                        reply.duplicate = true;
                    } else {
                        ensure!(
                            !running || accepting,
                            "node paused; new workspace start rejected"
                        );
                        ensure!(next.receipts.len() < 4096, "workspace request journal full");
                        next.receipts.push(Receipt {
                            id: request_id,
                            running,
                        });
                        next.running = running;
                        next.generation += 1;
                        persist(&inner.dir, &next)?;
                        inner.desired = next;
                    }
                    inner.reconcile().await?;
                }
                WorkspaceRequest::Logs => {
                    let info = inner
                        .owned()
                        .await?
                        .context("workspace container does not exist")?;
                    let output =
                        docker(&["logs", "--tail", "40", "--timestamps", &info.id]).await?;
                    reply.logs = Some(output[..output.floor_char_boundary(4096)].into());
                }
                WorkspaceRequest::Status => {}
            }
            Ok(())
        }
        .await;
        reply.error = result.err().map(|e| e.to_string());
        let snapshot = self.update(&inner, reply.error.clone()).await;
        reply.workspace = Some(snapshot);
        reply
    }
    pub async fn reconcile(&self) {
        let mut inner = self.inner.lock().await;
        let error = inner.reconcile().await.err().map(|e| e.to_string());
        self.update(&inner, error).await;
    }
    async fn update(&self, inner: &Inner, error: Option<String>) -> WorkspaceSnapshot {
        let mut snapshot = WorkspaceSnapshot {
            desired_running: inner.desired.running,
            generation: inner.desired.generation,
            volume: volume_name(inner.desired.device),
            project_path: "/home/coder/project".into(),
            sampled_at_unix: crate::protocol::unix_now(),
            state: "absent".into(),
            error,
            ..Default::default()
        };
        let mut port = 0;
        match inner.owned().await {
            Ok(Some(info)) => {
                snapshot.container_id = Some(info.id);
                snapshot.state = info.state;
                snapshot.healthy = info.healthy;
                if snapshot.healthy && inner.desired.running {
                    port = info.port;
                }
            }
            Ok(None) => {}
            Err(e) => {
                snapshot.state = "unavailable".into();
                snapshot.error = Some(e.to_string());
            }
        }
        if port != 0
            && let Ok(client) = reqwest::Client::builder()
                .no_proxy()
                .timeout(std::time::Duration::from_secs(2))
                .redirect(reqwest::redirect::Policy::none())
                .build()
        {
            snapshot.project_service_healthy = client
                .get(format!("http://127.0.0.1:{port}/proxy/3000/health"))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success());
        }
        self.port.store(port, Ordering::Release);
        *self.snapshot.write().unwrap() = snapshot.clone();
        snapshot
    }
}
struct Owned {
    id: String,
    state: String,
    healthy: bool,
    port: u16,
}
fn container_name(device: Uuid) -> String {
    format!("cofleet-workspace-{}", device.simple())
}
fn volume_name(device: Uuid) -> String {
    format!("cofleet-workspace-home-{}", device.simple())
}
fn valid_image_id(s: &str) -> bool {
    s.strip_prefix("sha256:")
        .is_some_and(|s| s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit()))
}
impl Inner {
    async fn owned(&self) -> Result<Option<Owned>> {
        let output = docker(&[
            "ps",
            "-a",
            "--no-trunc",
            "--filter",
            &format!("name=^/{}$", container_name(self.desired.device)),
            "--format",
            "{{.ID}}",
        ])
        .await?;
        let Some(id) = output.lines().next().filter(|s| !s.is_empty()) else {
            return Ok(None);
        };
        let info: serde_json::Value =
            serde_json::from_str(&docker(&["inspect", "--format", "{{json .}}", id]).await?)?;
        ensure!(
            info["Config"]["Labels"][OWNER].as_str() == Some(&self.desired.device.to_string())
                && info["Config"]["Labels"][SPEC].as_str() == Some(VERSION)
                && info["Config"]["Image"].as_str() == Some(&self.desired.image),
            "workspace container ownership or image mismatch; refusing access"
        );
        let mounts = info["Mounts"]
            .as_array()
            .context("workspace mounts missing")?;
        ensure!(
            mounts.len() == 1
                && mounts[0]["Type"] == "volume"
                && mounts[0]["Name"] == volume_name(self.desired.device)
                && mounts[0]["Destination"] == "/home/coder",
            "workspace volume mismatch; refusing access"
        );
        let mut port = 0;
        if let Some(bindings) = info["NetworkSettings"]["Ports"]["8080/tcp"].as_array() {
            ensure!(
                bindings.len() == 1 && bindings[0]["HostIp"] == "127.0.0.1",
                "workspace must publish only on loopback"
            );
            port = bindings[0]["HostPort"].as_str().unwrap_or("").parse()?;
        }
        let state = info["State"]["Status"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();
        Ok(Some(Owned {
            id: id.into(),
            healthy: state == "running" && info["State"]["Health"]["Status"] == "healthy",
            state,
            port,
        }))
    }
    async fn volume(&self) -> Result<()> {
        let name = volume_name(self.desired.device);
        let exists = docker(&[
            "volume",
            "ls",
            "--filter",
            &format!("name=^{name}$"),
            "--format",
            "{{.Name}}",
        ])
        .await?;
        if exists.trim() != name {
            docker(&[
                "volume",
                "create",
                "--label",
                &format!("{OWNER}={}", self.desired.device),
                "--label",
                &format!("{SPEC}={VERSION}"),
                &name,
            ])
            .await?;
        }
        let info: serde_json::Value = serde_json::from_str(
            &docker(&["volume", "inspect", "--format", "{{json .}}", &name]).await?,
        )?;
        ensure!(
            info["Labels"][OWNER].as_str() == Some(&self.desired.device.to_string())
                && info["Labels"][SPEC] == VERSION,
            "workspace volume ownership mismatch"
        );
        Ok(())
    }
    async fn reconcile(&mut self) -> Result<()> {
        match (self.desired.running, self.owned().await?) {
            (true, None) => {
                self.volume().await?;
                docker(&[
                    "create",
                    "--pull=never",
                    "--name",
                    &container_name(self.desired.device),
                    "--label",
                    &format!("{OWNER}={}", self.desired.device),
                    "--label",
                    &format!("{SPEC}={VERSION}"),
                    "--restart=no",
                    "--init",
                    "--user=1000:1000",
                    "--cap-drop=ALL",
                    "--security-opt=no-new-privileges:true",
                    "--memory=2g",
                    "--memory-swap=2g",
                    "--cpus=2",
                    "--pids-limit=256",
                    "--publish=127.0.0.1::8080",
                    "--mount",
                    &format!(
                        "type=volume,source={},target=/home/coder",
                        volume_name(self.desired.device)
                    ),
                    "--log-driver=local",
                    "--log-opt=max-size=2m",
                    "--log-opt=max-file=2",
                    &self.desired.image,
                ])
                .await?;
                let info = self.owned().await?.context("created workspace missing")?;
                docker(&["start", &info.id]).await?;
            }
            (true, Some(info)) if info.state != "running" => {
                self.volume().await?;
                docker(&["start", &info.id]).await?;
            }
            (false, Some(info)) if info.state == "running" => {
                docker(&["stop", "--time=5", &info.id]).await?;
            }
            _ => {}
        }
        Ok(())
    }
}
fn persist(dir: &Path, state: &Desired) -> Result<()> {
    use std::io::Write;
    let tmp = dir.join(format!(".workspace-{}", Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(&serde_json::to_vec(state)?)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, dir.join("workspace-state.json"))?;
        std::fs::File::open(dir)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    result.context("workspace state persistence failed")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_immutable_local_image_ids_are_accepted() {
        assert!(valid_image_id(&format!("sha256:{}", "a".repeat(64))));
        for image in ["latest", "--privileged", "sha256:abc", "image:tag"] {
            assert!(!valid_image_id(image));
        }
    }
}
