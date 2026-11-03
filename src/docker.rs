//! Opt-in, fixed-spec Docker test workload. No caller supplied commands or mounts.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command, sync::Mutex};
use uuid::Uuid;

pub const TEST_IMAGE: &str =
    "alpine@sha256:5291449c3df73caf6ed85e649dec1b9e818b39a5d8c871e97afc13e9cd5e8fa8";
const OWNER: &str = "xyz.mxue.cofleet.device";
const SPEC: &str = "xyz.mxue.cofleet.spec";
const SPEC_VERSION: &str = "test-v1";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum DockerRequest {
    List,
    SetTest { request_id: Uuid, running: bool },
    TestLogs,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    pub state: String,
    pub managed: bool,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DockerSnapshot {
    pub sampled_at_unix: u64,
    pub desired_running: bool,
    pub generation: u64,
    pub containers: Vec<Container>,
    pub truncated: bool,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DockerReply {
    pub snapshot: Option<DockerSnapshot>,
    pub logs: Option<String>,
    pub duplicate: bool,
    pub error: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Receipt {
    id: Uuid,
    running: bool,
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct Desired {
    device: Option<Uuid>,
    running: bool,
    generation: u64,
    receipts: Vec<Receipt>,
}
impl Desired {
    fn apply(&mut self, id: Uuid, running: bool) -> Result<bool> {
        if let Some(previous) = self.receipts.iter().find(|r| r.id == id) {
            ensure!(
                previous.running == running,
                "request ID already used for a different intent"
            );
            return Ok(true);
        }
        // Never evict receipts: a delayed retry must not revive an explicitly stopped workload.
        ensure!(
            self.receipts.len() < 4096,
            "request journal full; no new intent accepted"
        );
        self.receipts.push(Receipt { id, running });
        self.running = running;
        self.generation += 1;
        Ok(false)
    }
}
struct Inner {
    dir: PathBuf,
    device: Uuid,
    desired: Desired,
}
#[derive(Clone)]
pub struct DockerController {
    inner: Arc<Mutex<Inner>>,
    snapshot: Arc<std::sync::RwLock<DockerSnapshot>>,
}
impl DockerController {
    pub fn open(dir: &Path, device: Uuid) -> Result<Self> {
        ensure!(
            cfg!(target_os = "linux"),
            "Docker execution is currently supported only on native Linux"
        );
        let path = dir.join("docker-state.json");
        let desired: Desired = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .context("invalid Docker desired state; refusing to reset")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Desired {
                device: Some(device),
                ..Default::default()
            },
            Err(e) => return Err(e.into()),
        };
        ensure!(
            desired.device == Some(device),
            "Docker desired state belongs to another device"
        );
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                dir: dir.into(),
                device,
                desired,
            })),
            snapshot: Default::default(),
        })
    }
    pub fn snapshot(&self) -> DockerSnapshot {
        self.snapshot.read().unwrap().clone()
    }
    pub async fn execute(&self, request: DockerRequest, accepting: bool) -> DockerReply {
        let mut inner = self.inner.lock().await;
        let mut reply = DockerReply::default();
        let result: Result<()> = async {
            match request {
                DockerRequest::SetTest {
                    request_id,
                    running,
                } => {
                    let mut next = inner.desired.clone();
                    reply.duplicate = next.apply(request_id, running)?;
                    ensure!(
                        !running || accepting || reply.duplicate,
                        "node paused; new start intent rejected"
                    );
                    if !reply.duplicate {
                        persist(&inner.dir, &next)?;
                        inner.desired = next;
                    }
                    // Retry reconciles the latest desired state, never the old request's state.
                    inner.reconcile().await?;
                }
                DockerRequest::TestLogs => {
                    let id = inner
                        .owned()
                        .await?
                        .context("test container does not exist")?
                        .0;
                    let output = docker(&["logs", "--tail", "40", "--timestamps", &id]).await?;
                    reply.logs = Some(output[..output.floor_char_boundary(4096)].to_owned());
                }
                DockerRequest::List => {}
            }
            Ok(())
        }
        .await;
        let mut snapshot = inner.collect().await.unwrap_or_else(|_| DockerSnapshot {
            desired_running: inner.desired.running,
            generation: inner.desired.generation,
            sampled_at_unix: crate::protocol::unix_now(),
            error: Some("Docker unavailable; check local socket and daemon".into()),
            ..Default::default()
        });
        if let Err(error) = result {
            reply.error = Some(error.to_string());
        }
        if reply.error.is_some() {
            snapshot.error.clone_from(&reply.error);
        }
        *self.snapshot.write().unwrap() = snapshot.clone();
        reply.snapshot = Some(snapshot);
        reply
    }
    pub async fn reconcile(&self) {
        let mut inner = self.inner.lock().await;
        let error = inner.reconcile().await.err().map(|e| e.to_string());
        let mut snapshot = inner.collect().await.unwrap_or_else(|_| DockerSnapshot {
            error: Some("Docker unavailable; check local socket and daemon".into()),
            ..Default::default()
        });
        snapshot.desired_running = inner.desired.running;
        snapshot.generation = inner.desired.generation;
        snapshot.sampled_at_unix = crate::protocol::unix_now();
        if error.is_some() {
            snapshot.error = error;
        }
        *self.snapshot.write().unwrap() = snapshot;
    }
}
impl Inner {
    fn name(&self) -> String {
        format!("cofleet-test-{}", self.device.simple())
    }
    async fn owned(&self) -> Result<Option<(String, bool)>> {
        // Inspect by deterministic name, but mutate only the inspected immutable ID.
        let output = docker(&[
            "ps",
            "-a",
            "--no-trunc",
            "--filter",
            &format!("name=^/{}$", self.name()),
            "--format",
            "{{.ID}}",
        ])
        .await?;
        let Some(id) = output.lines().next().filter(|s| !s.is_empty()) else {
            return Ok(None);
        };
        let data = docker(&["inspect", "--format", "{{json .}}", id]).await?;
        let info: serde_json::Value = serde_json::from_str(&data)?;
        ensure!(
            info["Config"]["Labels"][OWNER].as_str() == Some(&self.device.to_string())
                && info["Config"]["Labels"][SPEC].as_str() == Some(SPEC_VERSION),
            "container name occupied by an unmanaged workload; refusing mutation"
        );
        ensure!(
            info["Config"]["Image"].as_str() == Some(TEST_IMAGE),
            "managed container image changed; refusing mutation"
        );
        Ok(Some((
            id.to_string(),
            info["State"]["Running"].as_bool().unwrap_or(false),
        )))
    }
    async fn reconcile(&mut self) -> Result<()> {
        let existing = self.owned().await?;
        match (self.desired.running, existing) {
            (true, None) => {
                // Agent owns recovery; Docker does not independently restart explicitly stopped work.
                docker(&["create", "--pull=never", "--name", &self.name(), "--label", &format!("{OWNER}={}",self.device), "--label", &format!("{SPEC}={SPEC_VERSION}"), "--restart=no", "--network=none", "--read-only", "--cap-drop=ALL", "--security-opt=no-new-privileges:true", "--user=65534:65534", "--memory=32m", "--memory-swap=32m", "--cpus=0.25", "--pids-limit=16", "--log-driver=local", "--log-opt=max-size=1m", "--log-opt=max-file=2", TEST_IMAGE, "/bin/sh", "-c", "trap 'exit 0' TERM INT; echo 'CoFleet test container started'; while :; do echo 'CoFleet heartbeat'; sleep 5 & wait $!; done"]).await?;
                let (id, _) = self
                    .owned()
                    .await?
                    .context("created test container missing")?;
                docker(&["start", &id]).await?;
            }
            (true, Some((id, false))) => {
                docker(&["start", &id]).await?;
            }
            (false, Some((id, true))) => {
                docker(&["stop", "--time=3", &id]).await?;
            }
            _ => {}
        }
        Ok(())
    }
    async fn collect(&self) -> Result<DockerSnapshot> {
        let output = docker(&["ps", "-a", "--no-trunc", "--format", "{{json .}}"]).await?;
        let mut containers = Vec::new();
        let mut truncated = false;
        for line in output.lines() {
            let v: serde_json::Value = serde_json::from_str(line)?;
            let short = |key: &str| {
                v[key]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(128)
                    .collect::<String>()
            };
            let name = short("Names");
            let labels = v["Labels"].as_str().unwrap_or("");
            containers.push(Container {
                id: short("ID"),
                managed: name == self.name()
                    && labels
                        .split(',')
                        .any(|l| l == format!("{OWNER}={}", self.device))
                    && labels
                        .split(',')
                        .any(|l| l == format!("{SPEC}={SPEC_VERSION}")),
                name,
                image: short("Image"),
                state: short("State"),
            });
        }
        containers.sort_by_key(|c| !c.managed);
        if containers.len() > 32 {
            containers.truncate(32);
            truncated = true;
        }
        while serde_json::to_vec(&containers)?.len() > 8192 {
            containers.pop();
            truncated = true;
        }
        Ok(DockerSnapshot {
            sampled_at_unix: crate::protocol::unix_now(),
            desired_running: self.desired.running,
            generation: self.desired.generation,
            containers,
            truncated,
            error: None,
        })
    }
}
fn persist(dir: &Path, desired: &Desired) -> Result<()> {
    use std::io::Write;
    let tmp = dir.join(format!(".docker-{}", Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(&serde_json::to_vec(desired)?)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, dir.join("docker-state.json"))?;
        std::fs::File::open(dir)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    result.context("Docker desired state persistence failed")
}
async fn docker(args: &[&str]) -> Result<String> {
    // Ignore remote contexts/DOCKER_HOST. This feature targets the native local Linux engine.
    let mut child = Command::new("docker")
        .args(["--host", "unix:///var/run/docker.sock"])
        .args(args)
        .env_remove("DOCKER_CONTEXT")
        .env_remove("DOCKER_HOST")
        .env_remove("DOCKER_TLS_VERIFY")
        .env_remove("DOCKER_CERT_PATH")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("Docker CLI unavailable")?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut out_reader = stdout.take(262145);
        let mut err_reader = stderr.take(8193);
        let (a, b, status) = tokio::join!(
            out_reader.read_to_end(&mut out),
            err_reader.read_to_end(&mut err),
            child.wait()
        );
        a?;
        b?;
        let status = status?;
        ensure!(
            out.len() <= 262144 && err.len() <= 8192,
            "Docker response too large"
        );
        ensure!(
            status.success(),
            "Docker operation failed; check daemon, local image, or container state"
        );
        if args.first() == Some(&"logs") {
            out.extend_from_slice(&err);
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    })
    .await
    .context("Docker operation timed out; retry using the same request ID")?
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delayed_retry_cannot_undo_a_later_stop_and_conflicts_are_rejected() {
        let mut desired = Desired::default();
        let start = Uuid::new_v4();
        let stop = Uuid::new_v4();
        assert!(!desired.apply(start, true).unwrap());
        assert!(!desired.apply(stop, false).unwrap());
        assert!(desired.apply(start, true).unwrap());
        assert!(!desired.running);
        assert_eq!(desired.generation, 2);
        assert!(desired.apply(start, false).is_err());
        let restored: Desired =
            serde_json::from_slice(&serde_json::to_vec(&desired).unwrap()).unwrap();
        assert!(!restored.running);
        assert_eq!(restored.receipts.len(), 2);
    }
    #[test]
    fn corrupt_state_is_not_silently_reset() {
        let dir = std::env::temp_dir().join(format!("cofleet-docker-{}", Uuid::new_v4()));
        crate::config::private_dir(&dir).unwrap();
        std::fs::write(dir.join("docker-state.json"), "broken").unwrap();
        assert!(DockerController::open(&dir, Uuid::new_v4()).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
