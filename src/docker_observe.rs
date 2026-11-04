//! Explicitly enabled, read-only Docker discovery and external endpoint authorization.
//! Docker is always addressed through the fixed local engine; callers never supply a host port.
use crate::access::{EndpointBinding, EndpointDefinition};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObserveCapability {
    pub environment_id: String,
    pub catalog_page_limit: u16,
    pub log_line_limit: u16,
    pub endpoint_tunnel: bool,
}
impl ObserveCapability {
    pub fn valid(&self) -> bool {
        !self.environment_id.is_empty()
            && self.environment_id.len() <= 128
            && (1..=50).contains(&self.catalog_page_limit)
            && (1..=500).contains(&self.log_line_limit)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublishedPort {
    pub container_port: u16,
    pub host_ip: String,
    pub host_port: u16,
    pub supported: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObservedContainer {
    pub id: String,
    pub name: String,
    pub image: String,
    pub state: String,
    pub health: Option<String>,
    pub compose_project: Option<String>,
    pub compose_service: Option<String>,
    pub compose_oneoff: bool,
    pub ports: Vec<PublishedPort>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogPage {
    pub environment_id: String,
    pub snapshot_id: Uuid,
    pub sampled_at_unix: u64,
    pub containers: Vec<ObservedContainer>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContainerLogs {
    pub container_id: String,
    pub collected_at_unix: u64,
    pub logs: String,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccessResolution {
    pub container_id: String,
    pub host: String,
    pub port: u16,
    pub rebound: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ObserveRequest {
    Catalog { cursor: Option<String>, limit: u16 },
    Logs { container_id: String, tail: u16 },
    Authorize { definition: EndpointDefinition },
    Revoke { endpoint_id: Uuid, revision: u64 },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ObserveReply {
    Catalog { page: CatalogPage },
    Logs { value: ContainerLogs },
    Authorized { resolution: AccessResolution },
    Revoked,
    Error { message: String },
}
impl ObserveReply {
    pub fn error(error: impl std::fmt::Display) -> Self {
        Self::Error {
            message: error.to_string(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Persisted {
    device_id: Uuid,
    environment_id: String,
    definitions: HashMap<Uuid, EndpointDefinition>,
}
struct Cache {
    id: Uuid,
    created: Instant,
    sampled_at_unix: u64,
    containers: Vec<ObservedContainer>,
}
struct Inner {
    persisted: Persisted,
    cache: Option<Cache>,
}

#[derive(Clone)]
pub struct DockerObserver {
    dir: PathBuf,
    environment_id: String,
    inner: Arc<Mutex<Inner>>,
}
impl DockerObserver {
    pub fn open(dir: &Path, device_id: Uuid) -> Result<Self> {
        ensure!(
            cfg!(any(target_os = "linux", target_os = "windows")),
            "Docker observation is supported on Linux and Windows"
        );
        let environment_id = environment_id(device_id);
        let path = dir.join("docker-observe-state.json");
        let persisted = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Persisted>(&bytes)
                .context("invalid Docker observation state; refusing to reset")?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Persisted {
                device_id,
                environment_id: environment_id.clone(),
                definitions: HashMap::new(),
            },
            Err(error) => return Err(error.into()),
        };
        ensure!(
            persisted.device_id == device_id && persisted.environment_id == environment_id,
            "Docker observation state belongs to another device or environment"
        );
        Ok(Self {
            dir: dir.into(),
            environment_id,
            inner: Arc::new(Mutex::new(Inner {
                persisted,
                cache: None,
            })),
        })
    }
    pub fn capability(&self) -> ObserveCapability {
        ObserveCapability {
            environment_id: self.environment_id.clone(),
            catalog_page_limit: 50,
            log_line_limit: 500,
            endpoint_tunnel: true,
        }
    }
    pub async fn execute(&self, request: ObserveRequest) -> ObserveReply {
        match self.execute_inner(request).await {
            Ok(reply) => reply,
            Err(error) => ObserveReply::error(error),
        }
    }
    async fn execute_inner(&self, request: ObserveRequest) -> Result<ObserveReply> {
        match request {
            ObserveRequest::Catalog { cursor, limit } => Ok(ObserveReply::Catalog {
                page: self.catalog(cursor, limit).await?,
            }),
            ObserveRequest::Logs { container_id, tail } => Ok(ObserveReply::Logs {
                value: self.logs(&container_id, tail).await?,
            }),
            ObserveRequest::Authorize { definition } => {
                definition.validate()?;
                let resolution = self.resolve_definition(&definition).await?;
                let mut inner = self.inner.lock().await;
                ensure!(
                    definition.device_id == inner.persisted.device_id
                        && definition.environment_id == inner.persisted.environment_id,
                    "endpoint belongs to another device or Docker environment"
                );
                if let Some(old) = inner.persisted.definitions.get(&definition.id) {
                    ensure!(
                        definition.revision >= old.revision,
                        "stale endpoint revision"
                    );
                }
                inner
                    .persisted
                    .definitions
                    .insert(definition.id, definition);
                persist(&self.dir, &inner.persisted)?;
                Ok(ObserveReply::Authorized { resolution })
            }
            ObserveRequest::Revoke {
                endpoint_id,
                revision,
            } => {
                let mut inner = self.inner.lock().await;
                if let Some(old) = inner.persisted.definitions.get(&endpoint_id) {
                    ensure!(revision >= old.revision, "stale endpoint revision");
                }
                inner.persisted.definitions.remove(&endpoint_id);
                persist(&self.dir, &inner.persisted)?;
                Ok(ObserveReply::Revoked)
            }
        }
    }
    async fn catalog(&self, cursor: Option<String>, limit: u16) -> Result<CatalogPage> {
        let limit = usize::from(limit.clamp(1, 50));
        let (snapshot, offset) = if let Some(cursor) = cursor {
            let (id, offset) = cursor.split_once(':').context("invalid catalog cursor")?;
            (
                Uuid::parse_str(id).context("invalid catalog cursor")?,
                offset.parse::<usize>().context("invalid catalog cursor")?,
            )
        } else {
            (Uuid::nil(), 0)
        };
        let mut inner = self.inner.lock().await;
        let refresh = snapshot.is_nil()
            || inner.cache.as_ref().is_none_or(|cache| {
                cache.id != snapshot || cache.created.elapsed() > Duration::from_secs(15)
            });
        if refresh {
            ensure!(
                snapshot.is_nil(),
                "catalog snapshot expired; restart pagination"
            );
            let containers = collect().await?;
            inner.cache = Some(Cache {
                id: Uuid::new_v4(),
                created: Instant::now(),
                sampled_at_unix: crate::protocol::unix_now(),
                containers,
            });
        }
        let cache = inner.cache.as_ref().unwrap();
        ensure!(
            offset <= cache.containers.len(),
            "invalid catalog cursor offset"
        );
        let mut end = offset;
        while end < (offset + limit).min(cache.containers.len()) {
            let bytes = serde_json::to_vec(&cache.containers[offset..=end])?.len();
            if bytes > 80 * 1024 {
                break;
            }
            end += 1;
        }
        ensure!(
            end > offset || offset == cache.containers.len(),
            "container metadata exceeds catalog page limit"
        );
        let next_cursor = (end < cache.containers.len()).then(|| format!("{}:{end}", cache.id));
        Ok(CatalogPage {
            environment_id: inner.persisted.environment_id.clone(),
            snapshot_id: cache.id,
            sampled_at_unix: cache.sampled_at_unix,
            containers: cache.containers[offset..end].to_vec(),
            next_cursor,
        })
    }
    async fn logs(&self, id: &str, tail: u16) -> Result<ContainerLogs> {
        validate_id(id)?;
        let tail = tail.clamp(1, 500).to_string();
        inspect_one(id).await?;
        let (logs, truncated) =
            crate::docker::docker_log_tail(&["logs", "--tail", &tail, "--timestamps", id]).await?;
        Ok(ContainerLogs {
            container_id: id.into(),
            collected_at_unix: crate::protocol::unix_now(),
            logs,
            truncated,
        })
    }
    async fn resolve_definition(
        &self,
        definition: &EndpointDefinition,
    ) -> Result<AccessResolution> {
        let capability = self.capability();
        ensure!(
            definition.environment_id == capability.environment_id,
            "Docker environment changed"
        );
        let all = collect().await?;
        resolve_from(&all, definition)
    }
    pub async fn resolve_authorized(
        &self,
        endpoint: Uuid,
        revision: u64,
    ) -> Result<AccessResolution> {
        let definition = {
            let inner = self.inner.lock().await;
            inner
                .persisted
                .definitions
                .get(&endpoint)
                .filter(|d| d.revision == revision)
                .cloned()
                .context("endpoint revision is not authorized")?
        };
        self.resolve_definition(&definition).await
    }
    pub async fn reset_authorizations(&self) -> Result<()> {
        let mut inner = self.inner.lock().await;
        if !inner.persisted.definitions.is_empty() {
            inner.persisted.definitions.clear();
            persist(&self.dir, &inner.persisted)?;
        }
        Ok(())
    }
}

fn environment_id(device: Uuid) -> String {
    let mut hash = Sha256::new();
    hash.update(b"cofleet-docker-observe-v1\0");
    hash.update(device.as_bytes());
    hash.update(std::env::consts::OS.as_bytes());
    hash.update(local_host().as_bytes());
    format!("local-{:x}", hash.finalize())
}
fn local_host() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "unix:///var/run/docker.sock"
    }
    #[cfg(target_os = "windows")]
    {
        "npipe:////./pipe/dockerDesktopLinuxEngine"
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        "unsupported"
    }
}
fn validate_id(id: &str) -> Result<()> {
    ensure!(
        id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "full container ID required"
    );
    Ok(())
}
async fn inspect_one(id: &str) -> Result<serde_json::Value> {
    validate_id(id)?;
    // Select fields at the engine; never retrieve environment variables or health log output.
    const FORMAT: &str = r#"{
      "Id":{{json .Id}}, "Name":{{json .Name}},
      "Config":{"Image":{{json .Config.Image}},"Labels":{
        "com.docker.compose.project":{{json (index .Config.Labels "com.docker.compose.project")}},
        "com.docker.compose.service":{{json (index .Config.Labels "com.docker.compose.service")}},
        "com.docker.compose.oneoff":{{json (index .Config.Labels "com.docker.compose.oneoff")}}
      }},
      "State":{"Status":{{json .State.Status}},"Health":{"Status":{{with (index .State "Health")}}{{json .Status}}{{else}}null{{end}}}},
      "HostConfig":{"PortBindings":{{json .HostConfig.PortBindings}}},
      "NetworkSettings":{"Ports":{{json .NetworkSettings.Ports}}}
    }"#;
    let output = crate::docker::docker(&["inspect", "--format", FORMAT, id]).await?;
    serde_json::from_str(output.trim()).context("invalid Docker inspect response")
}
async fn collect() -> Result<Vec<ObservedContainer>> {
    let output = crate::docker::docker(&["ps", "-a", "--no-trunc", "--format", "{{.ID}}"]).await?;
    let mut result = Vec::new();
    ensure!(
        output.lines().count() <= 500,
        "Docker catalog exceeds 500 containers; narrow the local Docker environment"
    );
    for id in output.lines() {
        if !id.is_empty() {
            result.push(observed(inspect_one(id).await?)?);
        }
    }
    result.sort_by(|a, b| {
        a.compose_project
            .cmp(&b.compose_project)
            .then(a.compose_service.cmp(&b.compose_service))
            .then(a.name.cmp(&b.name))
    });
    Ok(result)
}
fn observed(value: serde_json::Value) -> Result<ObservedContainer> {
    let text = |path: &[&str]| -> Option<String> {
        let mut v = &value;
        for key in path {
            v = &v[*key];
        }
        v.as_str().map(|s| s.chars().take(256).collect())
    };
    let id = text(&["Id"]).context("Docker inspect missing ID")?;
    validate_id(&id)?;
    let labels = &value["Config"]["Labels"];
    let label = |name: &str| {
        labels[name]
            .as_str()
            .filter(|v| !v.is_empty())
            .map(|v| v.chars().take(128).collect())
    };
    let mut ports = Vec::new();
    // HostConfig contains an empty HostPort for dynamically allocated mappings.
    // Only NetworkSettings reflects the engine's currently assigned listening port.
    let bindings = if value["State"]["Status"].as_str() == Some("running") {
        &value["NetworkSettings"]["Ports"]
    } else {
        &value["HostConfig"]["PortBindings"]
    };
    if let Some(bindings) = bindings.as_object() {
        for (key, mappings) in bindings {
            let Some((port, protocol)) = key.split_once('/') else {
                continue;
            };
            if protocol != "tcp" {
                continue;
            }
            let Ok(container_port) = port.parse::<u16>() else {
                continue;
            };
            for mapping in mappings.as_array().into_iter().flatten() {
                let host_ip = mapping["HostIp"].as_str().unwrap_or("");
                let Ok(host_port) = mapping["HostPort"].as_str().unwrap_or("").parse::<u16>()
                else {
                    continue;
                };
                let (supported, normalized, reason) = match host_ip {
                    "" | "0.0.0.0" | "127.0.0.1" => (true, "127.0.0.1", None),
                    "::" | "::1" => (true, "::1", None),
                    _ => (false, host_ip, Some("specific_interface_binding".into())),
                };
                ports.push(PublishedPort {
                    container_port,
                    host_ip: normalized.into(),
                    host_port,
                    supported,
                    reason,
                });
            }
        }
    }
    ports.sort_by_key(|p| (p.container_port, p.host_ip.clone(), p.host_port));
    Ok(ObservedContainer {
        id,
        name: text(&["Name"])
            .unwrap_or_default()
            .trim_start_matches('/')
            .to_owned(),
        image: text(&["Config", "Image"]).unwrap_or_default(),
        state: text(&["State", "Status"]).unwrap_or_default(),
        health: text(&["State", "Health", "Status"]),
        compose_project: label("com.docker.compose.project"),
        compose_service: label("com.docker.compose.service"),
        compose_oneoff: label("com.docker.compose.oneoff")
            .is_some_and(|v| v.eq_ignore_ascii_case("true")),
        ports,
    })
}
fn resolve_from(
    all: &[ObservedContainer],
    definition: &EndpointDefinition,
) -> Result<AccessResolution> {
    let original = match &definition.binding {
        EndpointBinding::Container { container_id } => Some(container_id.as_str()),
        _ => None,
    };
    let candidates: Vec<_> = all
        .iter()
        .filter(|container| {
            container.state == "running"
                && match &definition.binding {
                    EndpointBinding::Container { container_id } => &container.id == container_id,
                    EndpointBinding::Compose { project, service } => {
                        !container.compose_oneoff
                            && container.compose_project.as_deref() == Some(project)
                            && container.compose_service.as_deref() == Some(service)
                    }
                }
        })
        .collect();
    ensure!(
        candidates.len() == 1,
        if candidates.is_empty() {
            "no running container matches endpoint"
        } else {
            "multiple running containers match endpoint"
        }
    );
    let container = candidates[0];
    let mappings: Vec<_> = container
        .ports
        .iter()
        .filter(|p| p.container_port == definition.container_port && p.supported)
        .collect();
    ensure!(
        mappings.len() == 1,
        if mappings.is_empty() {
            "approved container port is not published on a supported loopback mapping"
        } else {
            "approved container port has multiple mappings"
        }
    );
    let mapping = mappings[0];
    Ok(AccessResolution {
        container_id: container.id.clone(),
        host: mapping.host_ip.clone(),
        port: mapping.host_port,
        rebound: original.is_none(),
    })
}
fn persist(dir: &Path, value: &Persisted) -> Result<()> {
    use std::io::Write;
    let tmp = dir.join(format!(".docker-observe-{}", Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(&serde_json::to_vec(value)?)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, dir.join("docker-observe-state.json"))?;
        std::fs::File::open(dir)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    result.context("Docker observation state persistence failed")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dynamically_assigned_ports_come_from_runtime_bindings() {
        let value = serde_json::json!({"Id":"a".repeat(64),"Name":"/fixture","Config":{"Image":"fixture"},"State":{"Status":"running"},"HostConfig":{"PortBindings":{"8800/tcp":[{"HostIp":"127.0.0.1","HostPort":""}]}},"NetworkSettings":{"Ports":{"8800/tcp":[{"HostIp":"127.0.0.1","HostPort":"45321"}]}}});
        let container = observed(value).unwrap();
        assert_eq!(container.ports.len(), 1);
        assert_eq!(container.ports[0].host_port, 45321);
        assert!(container.ports[0].supported);
    }

    fn container(
        id: char,
        project: Option<&str>,
        service: Option<&str>,
        port: u16,
    ) -> ObservedContainer {
        ObservedContainer {
            id: id.to_string().repeat(64),
            name: id.to_string(),
            image: "image".into(),
            state: "running".into(),
            health: None,
            compose_project: project.map(Into::into),
            compose_service: service.map(Into::into),
            compose_oneoff: false,
            ports: vec![PublishedPort {
                container_port: 8080,
                host_ip: "127.0.0.1".into(),
                host_port: port,
                supported: true,
                reason: None,
            }],
        }
    }
    #[test]
    fn compose_rebind_requires_one_candidate() {
        let id = Uuid::new_v4();
        let d = EndpointDefinition {
            id,
            revision: 1,
            device_id: Uuid::new_v4(),
            environment_id: "e".into(),
            binding: EndpointBinding::Compose {
                project: "p".into(),
                service: "s".into(),
            },
            container_port: 8080,
            protocol: crate::access::AccessProtocol::Http,
            tls_server_name: None,
        };
        assert_eq!(
            resolve_from(&[container('a', Some("p"), Some("s"), 3000)], &d)
                .unwrap()
                .port,
            3000
        );
        assert!(
            resolve_from(
                &[
                    container('a', Some("p"), Some("s"), 3000),
                    container('b', Some("p"), Some("s"), 3001)
                ],
                &d
            )
            .is_err()
        );
    }
    #[test]
    fn exact_binding_never_follows_same_name() {
        let c = container('a', None, None, 3000);
        let d = EndpointDefinition {
            id: Uuid::new_v4(),
            revision: 1,
            device_id: Uuid::new_v4(),
            environment_id: "e".into(),
            binding: EndpointBinding::Container {
                container_id: "b".repeat(64),
            },
            container_port: 8080,
            protocol: crate::access::AccessProtocol::Http,
            tls_server_name: None,
        };
        assert!(resolve_from(&[c], &d).is_err());
    }
}
