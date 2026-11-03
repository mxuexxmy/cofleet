//! Bounded native-node build jobs and health-gated stateless application releases.
use crate::{
    docker::{DockerReply, docker},
    workspace::WorkspaceController,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU16, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{Mutex, Notify, Semaphore},
};
use uuid::Uuid;
const OWNER: &str = "xyz.mxue.cofleet.device";
const SPEC: &str = "xyz.mxue.cofleet.spec";
const VERSION: &str = "application-v1";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PipelineRequest {
    Status,
    Logs {
        job_id: Uuid,
    },
    Build {
        request_id: Uuid,
        project_dir: String,
    },
    Deploy {
        request_id: Uuid,
        build_id: Uuid,
        expected_current: Option<Uuid>,
    },
    Rollback {
        request_id: Uuid,
        expected_current: Uuid,
    },
    Stop {
        request_id: Uuid,
        expected_current: Option<Uuid>,
    },
}
impl PipelineRequest {
    fn id(&self) -> Option<Uuid> {
        match self {
            Self::Build { request_id, .. }
            | Self::Deploy { request_id, .. }
            | Self::Rollback { request_id, .. }
            | Self::Stop { request_id, .. } => Some(*request_id),
            _ => None,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseSpec {
    data_policy: String,
    #[serde(default)]
    migration: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub id: Uuid,
    pub request: PipelineRequest,
    pub status: String,
    pub created_at: u64,
    pub input_digest: Option<String>,
    pub image_id: Option<String>,
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    spec: Option<ReleaseSpec>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Deployment {
    pub id: Uuid,
    pub build_id: Uuid,
    pub image_id: String,
    pub input_digest: String,
    pub container_id: String,
    pub healthy: bool,
    pub activated_at: u64,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PipelineSnapshot {
    pub jobs: Vec<Job>,
    pub current: Option<Deployment>,
    pub previous: Option<Deployment>,
    pub desired_running: bool,
    pub error: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct State {
    device: Uuid,
    jobs: Vec<Job>,
    current: Option<Deployment>,
    previous: Option<Deployment>,
    desired_running: bool,
}
#[derive(Clone)]
pub struct PipelineController {
    dir: PathBuf,
    state: Arc<Mutex<State>>,
    snapshot: Arc<std::sync::RwLock<PipelineSnapshot>>,
    port: Arc<AtomicU16>,
    notify: Arc<Notify>,
    captures: Arc<Semaphore>,
    activation: Arc<Mutex<()>>,
    workspace: WorkspaceController,
}
impl PipelineController {
    pub fn open(dir: &Path, device: Uuid, workspace: WorkspaceController) -> Result<Self> {
        ensure!(
            cfg!(target_os = "linux"),
            "build/deploy currently requires native Linux"
        );
        let dir = dir.join("pipeline");
        crate::config::private_dir(&dir)?;
        let mut state: State = match std::fs::read(dir.join("state.json")) {
            Ok(v) => {
                serde_json::from_slice(&v).context("invalid pipeline state; refusing reset")?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State {
                device,
                jobs: Vec::new(),
                current: None,
                previous: None,
                desired_running: false,
            },
            Err(e) => return Err(e.into()),
        };
        ensure!(
            state.device == device,
            "pipeline state belongs to another device"
        );
        if let Some(current) = &mut state.current {
            current.healthy = false;
        }
        for job in &mut state.jobs {
            if matches!(job.status.as_str(), "running" | "capturing") {
                job.status = "interrupted".into();
                job.error = Some(
                    "Agent interrupted; inspect current deployment and submit a new operation ID"
                        .into(),
                );
            }
        }
        persist(&dir, &state)?;
        let controller = Self {
            dir,
            state: Arc::new(Mutex::new(state.clone())),
            snapshot: Default::default(),
            port: Default::default(),
            notify: Default::default(),
            captures: Arc::new(Semaphore::new(2)),
            activation: Default::default(),
            workspace,
        };
        controller.publish(&state, None);
        Ok(controller)
    }
    pub fn snapshot(&self) -> PipelineSnapshot {
        self.snapshot.read().unwrap().clone()
    }
    pub fn port(&self) -> Option<u16> {
        let p = self.port.load(Ordering::Acquire);
        (p != 0).then_some(p)
    }
    fn job_dir(&self, id: Uuid) -> PathBuf {
        self.dir.join(id.to_string())
    }
    fn publish(&self, state: &State, error: Option<String>) {
        let mut value = PipelineSnapshot {
            jobs: state.jobs.iter().rev().take(8).cloned().collect(),
            current: state.current.clone(),
            previous: state.previous.clone(),
            desired_running: state.desired_running,
            error,
        };
        while serde_json::to_vec(&value).unwrap().len() > 4096 && !value.jobs.is_empty() {
            value.jobs.pop();
        }
        *self.snapshot.write().unwrap() = value;
    }
    async fn edit(&self, change: impl FnOnce(&mut State) -> Result<()>) -> Result<()> {
        let mut state = self.state.lock().await;
        let mut next = state.clone();
        change(&mut next)?;
        persist(&self.dir, &next)?;
        *state = next;
        self.publish(&state, None);
        Ok(())
    }
    pub async fn execute(&self, request: PipelineRequest, accepting: bool) -> DockerReply {
        let mut reply = DockerReply::default();
        let result: Result<()> = async {
            if let PipelineRequest::Logs { job_id } = &request {
                ensure!(
                    self.state.lock().await.jobs.iter().any(|j| j.id == *job_id),
                    "unknown pipeline job"
                );
                let path = self.job_dir(*job_id).join("build.log");
                reply.logs = Some(match tokio::fs::read(path).await {
                    Ok(bytes) => {
                        let text =
                            String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(4096)..])
                                .into_owned();
                        text[..text.floor_char_boundary(4096)].into()
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                    Err(e) => return Err(e.into()),
                });
                return Ok(());
            }
            let Some(id) = request.id() else {
                return Ok(());
            };
            let mut state = self.state.lock().await;
            if let Some(job) = state.jobs.iter().find(|j| j.id == id) {
                ensure!(
                    job.request == request,
                    "request ID already used for different pipeline input"
                );
                reply.duplicate = true;
                return Ok(());
            }
            ensure!(
                state.jobs.len() < 128,
                "pipeline journal full; new operation refused"
            );
            let capture = if let PipelineRequest::Build { project_dir, .. } = &request {
                ensure!(accepting, "node paused; new build rejected");
                ensure!(valid_project(project_dir), "invalid project path");
                ensure!(
                    state
                        .jobs
                        .iter()
                        .filter(|j| matches!(j.request, PipelineRequest::Build { .. }))
                        .count()
                        < 32,
                    "build archive limit reached"
                );
                Some(
                    self.captures
                        .clone()
                        .try_acquire_owned()
                        .context("snapshot capacity busy; retry this request ID")?,
                )
            } else {
                if matches!(
                    request,
                    PipelineRequest::Deploy { .. } | PipelineRequest::Rollback { .. }
                ) {
                    ensure!(accepting, "node paused; new deployment rejected");
                }
                None
            };
            let folder = self.job_dir(id);
            crate::config::private_dir(&folder)?;
            let mut next = state.clone();
            next.jobs.push(Job {
                id,
                request: request.clone(),
                status: if capture.is_some() {
                    "capturing"
                } else {
                    "queued"
                }
                .into(),
                created_at: crate::protocol::unix_now(),
                input_digest: None,
                image_id: None,
                error: None,
                spec: None,
            });
            persist(&self.dir, &next)?;
            *state = next;
            self.publish(&state, None);
            drop(state);
            if let Some(permit) = capture {
                let this = self.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let result = this.capture(id).await;
                    if let Err(e) = result {
                        let _ = this.finish(id, Err(e)).await;
                    }
                    this.notify.notify_one();
                });
            } else {
                self.notify.notify_one();
            }
            Ok(())
        }
        .await;
        reply.error = result.err().map(|e| e.to_string());
        reply.pipeline = Some(self.snapshot());
        reply
    }
    async fn capture(&self, id: Uuid) -> Result<()> {
        let job = self
            .state
            .lock()
            .await
            .jobs
            .iter()
            .find(|j| j.id == id)
            .cloned()
            .context("unknown build")?;
        let PipelineRequest::Build { project_dir, .. } = job.request else {
            anyhow::bail!("not a build")
        };
        let container = self.workspace.source_container().await?;
        let mut child = command()
            .args([
                "exec",
                "--user=1000:1000",
                &container,
                "/usr/bin/python3",
                "-c",
                include_str!("pipeline/snapshot.py"),
                &project_dir,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut stdout = child.stdout.take().unwrap();
        let tmp = self.job_dir(id).join("input.tmp");
        let mut output = private_file(&tmp)?;
        let mut digest = Sha256::new();
        tokio::time::timeout(Duration::from_secs(20),async {
            let mut buffer=vec![0;65536];let mut total=0usize;
            loop {let n=stdout.read(&mut buffer).await?;if n==0 {break;}total+=n;ensure!(total<=32*1024*1024,"snapshot archive too large");std::io::Write::write_all(&mut output,&buffer[..n])?;digest.update(&buffer[..n]);}
            ensure!(child.wait().await?.success(),"source snapshot rejected; check source links, size, Dockerfile and release manifest");Ok::<(),anyhow::Error>(())
        }).await.context("snapshot timed out")??;
        output.sync_all()?;
        drop(output);
        let archive = self.job_dir(id).join("input.tar");
        std::fs::rename(tmp, &archive)?;
        std::fs::File::open(self.job_dir(id))?.sync_all()?;
        let mut tar = tar::Archive::new(std::fs::File::open(&archive)?);
        let mut spec = None;
        for entry in tar.entries()? {
            let mut entry = entry?;
            if entry.path()?.as_ref() == Path::new(".cofleet-release.json") {
                ensure!(entry.size() <= 4096, "release manifest too large");
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut entry, &mut bytes)?;
                spec = Some(
                    serde_json::from_slice::<ReleaseSpec>(&bytes)
                        .context("invalid release manifest")?,
                );
            }
        }
        let spec = spec.context("release manifest missing")?;
        ensure!(
            matches!(spec.data_policy.as_str(), "stateless" | "persistent"),
            "unknown data policy"
        );
        let input_digest = format!("sha256:{:x}", digest.finalize());
        self.edit(|s| {
            let j = s.jobs.iter_mut().find(|j| j.id == id).unwrap();
            j.input_digest = Some(input_digest);
            j.spec = Some(spec);
            j.status = "queued".into();
            Ok(())
        })
        .await
    }
    pub async fn worker(&self) {
        loop {
            let next = self
                .state
                .lock()
                .await
                .jobs
                .iter()
                .find(|j| j.status == "queued")
                .cloned();
            if let Some(job) = next
                && self
                    .edit(|s| {
                        s.jobs.iter_mut().find(|j| j.id == job.id).unwrap().status =
                            "running".into();
                        Ok(())
                    })
                    .await
                    .is_ok()
            {
                let result = match job.request {
                    PipelineRequest::Build { .. } => self.build(&job).await,
                    PipelineRequest::Deploy {
                        build_id,
                        expected_current,
                        ..
                    } => self.deploy(&job, build_id, expected_current).await,
                    PipelineRequest::Rollback {
                        expected_current, ..
                    } => {
                        let previous = self.state.lock().await.previous.clone();
                        match previous {
                            Some(previous) => {
                                self.deploy(&job, previous.build_id, Some(expected_current))
                                    .await
                            }
                            None => Err(anyhow::anyhow!("no previous release")),
                        }
                    }
                    PipelineRequest::Stop {
                        expected_current, ..
                    } => self.stop(expected_current).await,
                    _ => Err(anyhow::anyhow!("invalid queued operation")),
                };
                let _ = self.finish(job.id, result).await;
            }
            self.reconcile().await;
            if self
                .state
                .lock()
                .await
                .jobs
                .iter()
                .any(|j| j.status == "queued")
            {
                continue;
            }
            tokio::select! {_=self.notify.notified()=>{},_=tokio::time::sleep(Duration::from_secs(10))=>{}}
        }
    }
    async fn finish(&self, id: Uuid, result: Result<()>) -> Result<()> {
        let error = result
            .err()
            .map(|e| e.to_string().chars().take(200).collect::<String>());
        self.edit(|s| {
            let j = s
                .jobs
                .iter_mut()
                .find(|j| j.id == id)
                .context("unknown job")?;
            j.status = if error.is_none() {
                "succeeded"
            } else {
                "failed"
            }
            .into();
            j.error = error;
            Ok(())
        })
        .await
    }
    async fn build(&self, job: &Job) -> Result<()> {
        let folder = self.job_dir(job.id);
        let archive = folder.join("input.tar");
        ensure!(job.input_digest.is_some(), "build has no frozen input");
        let mut source = tokio::fs::File::open(&archive).await?;
        let mut digest = Sha256::new();
        let mut buffer = vec![0; 65536];
        loop {
            let n = source.read(&mut buffer).await?;
            if n == 0 {
                break;
            }
            digest.update(&buffer[..n]);
        }
        let actual = format!("sha256:{:x}", digest.finalize());
        ensure!(
            Some(&actual) == job.input_digest.as_ref(),
            "input snapshot checksum changed"
        );
        let device = self.state.lock().await.device;
        let iid = folder.join("image.id");
        let tag = format!("cofleet-build-{}:{}", device.simple(), job.id.simple());
        let mut child = command()
            .args([
                "build",
                "--load",
                "--pull=false",
                "--network=none",
                "--resource=memory=1g",
                "--resource=cpu-quota=100000",
                "--progress=plain",
                "--iidfile",
            ])
            .arg(&iid)
            .args([
                "--label",
                &format!("{OWNER}={device}"),
                "--label",
                &format!("xyz.mxue.cofleet.build={}", job.id),
                "--label",
                &format!("xyz.mxue.cofleet.input={actual}"),
                "--tag",
                &tag,
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Docker build unavailable")?;
        let mut input = child.stdin.take().unwrap();
        let out = child.stdout.take().unwrap();
        let err = child.stderr.take().unwrap();
        let sink = Arc::new(Mutex::new((
            tokio::fs::File::from_std(private_file(&folder.join("build.log"))?),
            0usize,
        )));
        let result = tokio::time::timeout(Duration::from_secs(300), async {
            let mut source = tokio::fs::File::open(archive).await?;
            let send = async move {
                tokio::io::copy(&mut source, &mut input).await?;
                input.shutdown().await?;
                drop(input);
                Ok::<(), std::io::Error>(())
            };
            let (sent, out, err, status) = tokio::join!(
                send,
                drain(out, sink.clone()),
                drain(err, sink),
                child.wait()
            );
            sent?;
            out?;
            err?;
            ensure!(
                status?.success(),
                "Docker build failed; inspect saved build log"
            );
            Ok::<(), anyhow::Error>(())
        })
        .await
        .context("build exceeded 300 second limit")?;
        result?;
        let image = tokio::fs::read_to_string(iid).await?.trim().to_string();
        ensure!(image_id(&image), "builder returned invalid image ID");
        self.edit(|s| {
            s.jobs.iter_mut().find(|j| j.id == job.id).unwrap().image_id = Some(image);
            Ok(())
        })
        .await
    }
    async fn deploy(&self, job: &Job, build_id: Uuid, expected: Option<Uuid>) -> Result<()> {
        let _activation = self.activation.lock().await;
        let (device, build, old) = {
            let s = self.state.lock().await;
            ensure!(
                s.current.as_ref().map(|d| d.id) == expected,
                "current release changed; refresh before deploying"
            );
            let build = s
                .jobs
                .iter()
                .find(|j| {
                    j.id == build_id
                        && matches!(j.request, PipelineRequest::Build { .. })
                        && j.status == "succeeded"
                })
                .cloned()
                .context("build not successful")?;
            (s.device, build, s.current.clone())
        };
        let spec = build.spec.as_ref().context("release contract missing")?;
        ensure!(
            spec.data_policy == "stateless" && !spec.migration,
            "persistent data or migration declared; automatic image deployment/rollback refused"
        );
        let image = build.image_id.as_ref().context("build image missing")?;
        let info: serde_json::Value = serde_json::from_str(
            &docker(&["image", "inspect", "--format", "{{json .}}", image]).await?,
        )?;
        ensure!(
            info["Config"]["Labels"][OWNER].as_str() == Some(&device.to_string())
                && info["Config"]["Labels"]["xyz.mxue.cofleet.build"].as_str()
                    == Some(&build_id.to_string()),
            "artifact ownership mismatch"
        );
        ensure!(
            info["Config"]["Volumes"].is_null()
                || info["Config"]["Volumes"]
                    .as_object()
                    .is_some_and(|m| m.is_empty()),
            "image requests persistent volumes; deployment refused"
        );
        let name = app_name(device, job.id);
        let candidate = async {
            if lookup(&name).await?.is_none() {
                docker(&[
                    "create",
                    "--pull=never",
                    "--name",
                    &name,
                    "--label",
                    &format!("{OWNER}={device}"),
                    "--label",
                    &format!("{SPEC}={VERSION}"),
                    "--label",
                    &format!("xyz.mxue.cofleet.release={}", job.id),
                    "--restart=no",
                    "--init",
                    "--user=1000:1000",
                    "--read-only",
                    "--tmpfs=/tmp:rw,noexec,nosuid,size=16m",
                    "--cap-drop=ALL",
                    "--security-opt=no-new-privileges:true",
                    "--memory=256m",
                    "--memory-swap=256m",
                    "--cpus=1",
                    "--pids-limit=128",
                    "--publish=127.0.0.1::8080",
                    "--log-driver=local",
                    "--log-opt=max-size=1m",
                    "--log-opt=max-file=2",
                    image,
                ])
                .await?;
            }
            let (id, _, running) = owned(&name, device, image)
                .await?
                .context("candidate missing")?;
            if !running {
                docker(&["start", &id]).await?;
            }
            let (_, port, _) = owned(&name, device, image)
                .await?
                .context("candidate missing")?;
            let mut healthy = false;
            for _ in 0..30 {
                if health(port).await {
                    healthy = true;
                    break;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            ensure!(
                healthy,
                "candidate failed /health; current release retained"
            );
            let deployment = Deployment {
                id: job.id,
                build_id,
                image_id: image.clone(),
                input_digest: build.input_digest.clone().unwrap(),
                container_id: id,
                healthy: true,
                activated_at: crate::protocol::unix_now(),
            };
            self.edit(|s| {
                ensure!(
                    s.current.as_ref().map(|d| d.id) == expected,
                    "release changed during health check"
                );
                s.previous = s.current.clone();
                s.current = Some(deployment);
                s.desired_running = true;
                Ok(())
            })
            .await?;
            self.port.store(port, Ordering::Release);
            Ok::<(), anyhow::Error>(())
        }
        .await;
        if candidate.is_err() {
            if let Ok(Some((id, _, true))) = owned(&name, device, image).await {
                let _ = docker(&["stop", "--time=3", &id]).await;
            }
            return candidate;
        }
        if let Some(old) = old
            && let Ok(Some((id, _, true))) =
                owned(&app_name(device, old.id), device, &old.image_id).await
        {
            let _ = docker(&["stop", "--time=3", &id]).await;
        }
        Ok(())
    }
    async fn stop(&self, expected: Option<Uuid>) -> Result<()> {
        let _activation = self.activation.lock().await;
        self.edit(|s| {
            ensure!(
                s.current.as_ref().map(|d| d.id) == expected,
                "current release changed"
            );
            s.desired_running = false;
            if let Some(current) = &mut s.current {
                current.healthy = false;
            }
            Ok(())
        })
        .await?;
        self.port.store(0, Ordering::Release);
        let s = self.state.lock().await.clone();
        if let Some(current) = s.current
            && let Some((id, _, true)) =
                owned(&app_name(s.device, current.id), s.device, &current.image_id).await?
        {
            docker(&["stop", "--time=3", &id]).await?;
        }
        Ok(())
    }
    pub async fn monitor(&self) {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        loop {
            interval.tick().await;
            self.reconcile().await;
        }
    }
    async fn reconcile(&self) {
        let Ok(_activation) = self.activation.try_lock() else {
            return;
        };
        let snapshot = self.state.lock().await.clone();
        let mut active = 0;
        let mut healthy = false;
        let mut error = None;
        if let Some(current) = &snapshot.current {
            let result = async {
                let (id, _, running) = owned(
                    &app_name(snapshot.device, current.id),
                    snapshot.device,
                    &current.image_id,
                )
                .await?
                .context("active application missing; explicit redeploy required")?;
                if snapshot.desired_running && !running {
                    docker(&["start", &id]).await?;
                }
                if !snapshot.desired_running && running {
                    docker(&["stop", "--time=3", &id]).await?;
                }
                if snapshot.desired_running {
                    let (_, port, _) = owned(
                        &app_name(snapshot.device, current.id),
                        snapshot.device,
                        &current.image_id,
                    )
                    .await?
                    .context("active application missing")?;
                    healthy = health(port).await;
                    if healthy {
                        active = port;
                    }
                }
                Ok::<(), anyhow::Error>(())
            }
            .await;
            error = result.err().map(|e| e.to_string());
        }
        // Retire previous releases and any uncommitted candidate left by an interrupted Agent.
        for job in &snapshot.jobs {
            if matches!(
                job.request,
                PipelineRequest::Deploy { .. } | PipelineRequest::Rollback { .. }
            ) && snapshot.current.as_ref().map(|d| d.id) != Some(job.id)
                && let Ok(Some(id)) = lookup(&app_name(snapshot.device, job.id)).await
            {
                let data = docker(&["inspect", "--format", "{{json .}}", &id]).await;
                if let Ok(data) = data
                    && let Ok(v) = serde_json::from_str::<serde_json::Value>(&data)
                    && v["Config"]["Labels"][OWNER].as_str() == Some(&snapshot.device.to_string())
                    && v["Config"]["Labels"][SPEC] == VERSION
                    && v["State"]["Running"] == true
                {
                    let _ = docker(&["stop", "--time=3", &id]).await;
                }
            }
        }
        self.port.store(active, Ordering::Release);
        let mut state = self.state.lock().await;
        if let Some(current) = &mut state.current {
            current.healthy = healthy;
        }
        self.publish(&state, error);
    }
}
async fn drain(
    mut input: impl tokio::io::AsyncRead + Unpin,
    sink: Arc<Mutex<(tokio::fs::File, usize)>>,
) -> Result<()> {
    let mut b = vec![0; 8192];
    loop {
        let n = input.read(&mut b).await?;
        if n == 0 {
            break;
        }
        let mut sink = sink.lock().await;
        let count = n.min((2 * 1024 * 1024usize).saturating_sub(sink.1));
        if count > 0 {
            sink.0.write_all(&b[..count]).await?;
            sink.1 += count;
        }
    }
    let mut sink = sink.lock().await;
    sink.0.flush().await?;
    sink.0.sync_all().await?;
    Ok(())
}
fn command() -> tokio::process::Command {
    let mut c = tokio::process::Command::new("docker");
    c.stdin(Stdio::null())
        .args(["--host", "unix:///var/run/docker.sock"])
        .env_remove("DOCKER_CONTEXT")
        .env_remove("DOCKER_HOST")
        .env_remove("DOCKER_TLS_VERIFY")
        .env_remove("DOCKER_CERT_PATH")
        .kill_on_drop(true);
    c
}
fn private_file(path: &Path) -> Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    Ok(o.open(path)?)
}
fn persist(dir: &Path, state: &State) -> Result<()> {
    use std::io::Write;
    let p = dir.join(format!(".state-{}", Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut f = private_file(&p)?;
        f.write_all(&serde_json::to_vec(state)?)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&p, dir.join("state.json"))?;
        std::fs::File::open(dir)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(p);
    }
    result
}
fn valid_project(s: &str) -> bool {
    s == "."
        || (!s.is_empty()
            && s.len() <= 128
            && s.split('/').all(|p| {
                !p.is_empty()
                    && p != "."
                    && p != ".."
                    && p.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_ .".contains(&b))
                    && !p.contains(' ')
            }))
}
fn image_id(s: &str) -> bool {
    s.strip_prefix("sha256:")
        .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
}
fn app_name(device: Uuid, id: Uuid) -> String {
    format!("cofleet-app-{}-{}", device.simple(), id.simple())
}
async fn lookup(name: &str) -> Result<Option<String>> {
    Ok(docker(&[
        "ps",
        "-a",
        "--no-trunc",
        "--filter",
        &format!("name=^/{name}$"),
        "--format",
        "{{.ID}}",
    ])
    .await?
    .lines()
    .next()
    .filter(|s| !s.is_empty())
    .map(str::to_string))
}
async fn owned(name: &str, device: Uuid, image: &str) -> Result<Option<(String, u16, bool)>> {
    let Some(id) = lookup(name).await? else {
        return Ok(None);
    };
    let v: serde_json::Value =
        serde_json::from_str(&docker(&["inspect", "--format", "{{json .}}", &id]).await?)?;
    ensure!(
        v["Config"]["Labels"][OWNER].as_str() == Some(&device.to_string())
            && v["Config"]["Labels"][SPEC] == VERSION
            && v["Config"]["Image"].as_str() == Some(image),
        "application ownership/image mismatch"
    );
    ensure!(
        v["Mounts"].as_array().is_some_and(|m| m
            .iter()
            .all(|v| v["Type"] == "tmpfs" && v["Destination"] == "/tmp")),
        "application mounts forbidden"
    );
    let bindings = &v["NetworkSettings"]["Ports"]["8080/tcp"];
    let port = if let Some(a) = bindings.as_array() {
        ensure!(
            a.len() == 1 && a[0]["HostIp"] == "127.0.0.1",
            "application must publish only on loopback"
        );
        a[0]["HostPort"].as_str().unwrap_or("").parse()?
    } else {
        0
    };
    Ok(Some((id, port, v["State"]["Running"] == true)))
}
async fn health(port: u16) -> bool {
    if port == 0 {
        return false;
    }
    match reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(c) => c
            .get(format!("http://127.0.0.1:{port}/health"))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success()),
        Err(_) => false,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_paths_cannot_escape() {
        for path in ["../x", "/etc", "a/../b", "a//b", "a;id", "a\\b", ""] {
            assert!(!valid_project(path));
        }
        for path in [".", "cofleet-example/release-demo", "app-v1"] {
            assert!(valid_project(path));
        }
    }
}
