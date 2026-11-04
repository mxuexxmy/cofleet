//! Persistent external-application endpoints. The database stores intent only;
//! resolved container IDs and host ports are always reconstructed by the Agent.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AccessProtocol {
    Http,
    Https,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EndpointBinding {
    Container { container_id: String },
    Compose { project: String, service: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EndpointDefinition {
    pub id: Uuid,
    pub revision: u64,
    pub device_id: Uuid,
    pub environment_id: String,
    pub binding: EndpointBinding,
    pub container_port: u16,
    pub protocol: AccessProtocol,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_server_name: Option<String>,
}
impl EndpointDefinition {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.revision > 0, "revision must be positive");
        ensure!(self.container_port > 0, "container port must be positive");
        ensure!(
            valid_short(&self.environment_id, 128),
            "invalid Docker environment ID"
        );
        match &self.binding {
            EndpointBinding::Container { container_id } => ensure!(
                container_id.len() == 64 && container_id.bytes().all(|b| b.is_ascii_hexdigit()),
                "container binding requires a full container ID"
            ),
            EndpointBinding::Compose { project, service } => {
                ensure!(
                    valid_short(project, 128) && valid_short(service, 128),
                    "invalid Compose binding"
                );
            }
        }
        match (&self.protocol, &self.tls_server_name) {
            (AccessProtocol::Http, None) => {}
            (AccessProtocol::Https, Some(name)) if valid_dns_name(name) => {}
            (AccessProtocol::Http, Some(_)) => {
                anyhow::bail!("HTTP endpoint cannot set a TLS server name")
            }
            (AccessProtocol::Https, _) => {
                anyhow::bail!("HTTPS endpoint requires a valid TLS server name")
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccessEndpoint {
    #[serde(flatten)]
    pub definition: EndpointDefinition,
    pub name: String,
    pub owner: String,
    pub enabled: bool,
    pub management_url: Option<String>,
    pub public_url: String,
    pub created_unix: u64,
    pub updated_unix: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CreateEndpoint {
    pub id: Uuid,
    pub device_id: Uuid,
    pub environment_id: String,
    pub binding: EndpointBinding,
    pub container_port: u16,
    pub protocol: AccessProtocol,
    #[serde(default)]
    pub tls_server_name: Option<String>,
    pub name: String,
    #[serde(default)]
    pub management_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PatchEndpoint {
    pub revision: u64,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub management_url: Option<Option<String>>,
}

fn valid_short(value: &str, max: usize) -> bool {
    let value = value.trim();
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
fn valid_dns_name(value: &str) -> bool {
    let value = value.trim().trim_end_matches('.');
    !value.is_empty()
        && value.len() <= 253
        && value.split('.').all(|part| {
            !part.is_empty()
                && part.len() <= 63
                && !part.starts_with('-')
                && !part.ends_with('-')
                && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}
fn valid_management_url(value: &Option<String>) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let url = reqwest::Url::parse(value).context("invalid management URL")?;
    ensure!(
        matches!(url.scheme(), "http" | "https"),
        "management URL must use HTTP or HTTPS"
    );
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "management URL cannot contain credentials"
    );
    ensure!(url.host_str().is_some(), "management URL requires a host");
    Ok(())
}
impl CreateEndpoint {
    pub fn validate(&self) -> Result<()> {
        ensure!(valid_short(&self.name, 128), "invalid endpoint name");
        valid_management_url(&self.management_url)?;
        EndpointDefinition {
            id: self.id,
            revision: 1,
            device_id: self.device_id,
            environment_id: self.environment_id.clone(),
            binding: self.binding.clone(),
            container_port: self.container_port,
            protocol: self.protocol.clone(),
            tls_server_name: self.tls_server_name.clone(),
        }
        .validate()
    }
}

#[derive(Clone)]
pub struct EndpointStore {
    path: Arc<PathBuf>,
}
impl EndpointStore {
    pub async fn open(dir: &Path) -> Result<Self> {
        crate::config::private_dir(dir)?;
        let store = Self {
            path: Arc::new(dir.join("state.sqlite3")),
        };
        store.run(|conn| {
            conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
                CREATE TABLE IF NOT EXISTS schema_version(version INTEGER NOT NULL);
                INSERT INTO schema_version(version) SELECT 1 WHERE NOT EXISTS(SELECT 1 FROM schema_version);
                CREATE TABLE IF NOT EXISTS access_endpoints(
                    id TEXT PRIMARY KEY, owner TEXT NOT NULL, device_id TEXT NOT NULL,
                    environment_id TEXT NOT NULL, binding_json TEXT NOT NULL,
                    container_port INTEGER NOT NULL, protocol TEXT NOT NULL,
                    tls_server_name TEXT, name TEXT NOT NULL, management_url TEXT,
                    enabled INTEGER NOT NULL, revision INTEGER NOT NULL,
                    public_url TEXT NOT NULL, created_unix INTEGER NOT NULL, updated_unix INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS trusted_access_endpoints(endpoint_id TEXT PRIMARY KEY, owner TEXT NOT NULL);
                CREATE INDEX IF NOT EXISTS access_endpoints_owner ON access_endpoints(owner);")?;
            let version: i64 = conn.query_row("SELECT version FROM schema_version", [], |r| r.get(0))?;
            ensure!(version == 1, "unsupported endpoint database schema");
            Ok(())
        }).await?;
        Ok(store)
    }
    async fn run<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = Connection::open(&*path).context("open endpoint database")?;
            conn.busy_timeout(Duration::from_secs(5))?;
            operation(&mut conn)
        })
        .await
        .context("endpoint database worker failed")?
    }
    pub async fn list(&self, owner: &str) -> Result<Vec<AccessEndpoint>> {
        let owner = owner.to_owned();
        self.run(move |conn| {
            let mut stmt = conn.prepare("SELECT id,owner,device_id,environment_id,binding_json,container_port,protocol,tls_server_name,name,management_url,enabled,revision,public_url,created_unix,updated_unix FROM access_endpoints WHERE owner=? ORDER BY created_unix,id")?;
            let rows = stmt.query_map([owner], row_endpoint)?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
        }).await
    }
    pub async fn get(&self, owner: &str, id: Uuid) -> Result<Option<AccessEndpoint>> {
        let owner = owner.to_owned();
        self.run(move |conn| conn.query_row("SELECT id,owner,device_id,environment_id,binding_json,container_port,protocol,tls_server_name,name,management_url,enabled,revision,public_url,created_unix,updated_unix FROM access_endpoints WHERE owner=? AND id=?", params![owner,id.to_string()], row_endpoint).optional().map_err(Into::into)).await
    }
    pub async fn find_public_host(&self, host: &str) -> Result<Option<AccessEndpoint>> {
        let expected = format!("https://{}/", host.to_ascii_lowercase());
        self.run(move |conn| conn.query_row("SELECT id,owner,device_id,environment_id,binding_json,container_port,protocol,tls_server_name,name,management_url,enabled,revision,public_url,created_unix,updated_unix FROM access_endpoints WHERE public_url=?", [expected], row_endpoint).optional().map_err(Into::into)).await
    }
    pub async fn trust(&self, owner: &str, id: Uuid) -> Result<()> {
        let owner = owner.to_owned();
        self.run(move |conn| {
            ensure!(
                conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM access_endpoints WHERE id=? AND owner=?)",
                    params![id.to_string(), owner],
                    |r| r.get::<_, bool>(0)
                )?,
                "endpoint not owned"
            );
            conn.execute(
                "INSERT OR IGNORE INTO trusted_access_endpoints(endpoint_id,owner) VALUES(?,?)",
                params![id.to_string(), owner],
            )?;
            Ok(())
        })
        .await
    }
    pub async fn is_trusted(&self, id: Uuid) -> Result<bool> {
        self.run(move |conn|conn.query_row("SELECT EXISTS(SELECT 1 FROM trusted_access_endpoints t JOIN access_endpoints e ON t.endpoint_id=e.id AND t.owner=e.owner WHERE e.id=?)",[id.to_string()],|r|r.get(0)).map_err(Into::into)).await
    }
    pub async fn create(
        &self,
        owner: &str,
        request: CreateEndpoint,
        public_url: String,
    ) -> Result<AccessEndpoint> {
        request.validate()?;
        let owner = owner.to_owned();
        self.run(move |conn| {
            let now = crate::protocol::unix_now();
            let binding = serde_json::to_string(&request.binding)?;
            let protocol = protocol_name(&request.protocol);
            let changed = conn.execute("INSERT OR IGNORE INTO access_endpoints(id,owner,device_id,environment_id,binding_json,container_port,protocol,tls_server_name,name,management_url,enabled,revision,public_url,created_unix,updated_unix) VALUES(?,?,?,?,?,?,?,?,?,?,1,1,?,?,?)",
                params![request.id.to_string(),owner,request.device_id.to_string(),request.environment_id,binding,request.container_port,protocol,request.tls_server_name,request.name.trim(),request.management_url,public_url,now,now])?;
            let endpoint = conn.query_row("SELECT id,owner,device_id,environment_id,binding_json,container_port,protocol,tls_server_name,name,management_url,enabled,revision,public_url,created_unix,updated_unix FROM access_endpoints WHERE owner=? AND id=?", params![owner,request.id.to_string()], row_endpoint).optional()?.context("endpoint ID belongs to another owner")?;
            if changed == 0 {
                ensure!(endpoint.definition.device_id == request.device_id && endpoint.definition.environment_id == request.environment_id && endpoint.definition.binding == request.binding && endpoint.definition.container_port == request.container_port && endpoint.definition.protocol == request.protocol && endpoint.definition.tls_server_name == request.tls_server_name && endpoint.name == request.name.trim() && endpoint.management_url == request.management_url, "endpoint ID already used for different content");
            }
            Ok(endpoint)
        }).await
    }
    pub async fn patch(
        &self,
        owner: &str,
        id: Uuid,
        patch: PatchEndpoint,
    ) -> Result<Option<AccessEndpoint>> {
        if let Some(name) = &patch.name {
            ensure!(valid_short(name, 128), "invalid endpoint name");
        }
        if let Some(url) = &patch.management_url {
            valid_management_url(url)?;
        }
        let owner = owner.to_owned();
        self.run(move |conn| {
            let tx = conn.transaction()?;
            let current = tx.query_row("SELECT id,owner,device_id,environment_id,binding_json,container_port,protocol,tls_server_name,name,management_url,enabled,revision,public_url,created_unix,updated_unix FROM access_endpoints WHERE owner=? AND id=?", params![owner,id.to_string()], row_endpoint).optional()?;
            let Some(mut current) = current else { return Ok(None); };
            ensure!(current.definition.revision == patch.revision, "endpoint revision conflict");
            if let Some(name) = patch.name { current.name = name.trim().to_owned(); }
            if let Some(enabled) = patch.enabled { current.enabled = enabled; }
            if let Some(url) = patch.management_url { current.management_url = url; }
            current.definition.revision += 1; current.updated_unix = crate::protocol::unix_now();
            tx.execute("UPDATE access_endpoints SET name=?,management_url=?,enabled=?,revision=?,updated_unix=? WHERE owner=? AND id=? AND revision=?", params![current.name,current.management_url,current.enabled,current.definition.revision,current.updated_unix,owner,id.to_string(),patch.revision])?;
            tx.commit()?; Ok(Some(current))
        }).await
    }
    pub async fn delete(&self, owner: &str, id: Uuid, revision: u64) -> Result<bool> {
        let owner = owner.to_owned();
        self.run(move |conn| {
            let changed = conn.execute(
                "DELETE FROM access_endpoints WHERE owner=? AND id=? AND revision=?",
                params![owner, id.to_string(), revision],
            )?;
            if changed == 0 {
                let exists: bool = conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM access_endpoints WHERE owner=? AND id=?)",
                    params![owner, id.to_string()],
                    |r| r.get(0),
                )?;
                ensure!(!exists, "endpoint revision conflict");
            }
            if changed == 1 {
                conn.execute(
                    "DELETE FROM trusted_access_endpoints WHERE endpoint_id=? AND owner=?",
                    params![id.to_string(), owner],
                )?;
            }
            Ok(changed == 1)
        })
        .await
    }
}
fn protocol_name(value: &AccessProtocol) -> &'static str {
    match value {
        AccessProtocol::Http => "http",
        AccessProtocol::Https => "https",
    }
}
fn row_endpoint(row: &rusqlite::Row<'_>) -> rusqlite::Result<AccessEndpoint> {
    let parse = |value: String| {
        Uuid::parse_str(&value).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
    };
    let binding_json: String = row.get(4)?;
    let binding = serde_json::from_str(&binding_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let protocol: String = row.get(6)?;
    let protocol = match protocol.as_str() {
        "http" => AccessProtocol::Http,
        "https" => AccessProtocol::Https,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    Ok(AccessEndpoint {
        definition: EndpointDefinition {
            id: parse(row.get(0)?)?,
            device_id: parse(row.get(2)?)?,
            environment_id: row.get(3)?,
            binding,
            container_port: row.get::<_, u16>(5)?,
            protocol,
            tls_server_name: row.get(7)?,
            revision: row.get(11)?,
        },
        owner: row.get(1)?,
        name: row.get(8)?,
        management_url: row.get(9)?,
        enabled: row.get(10)?,
        public_url: row.get(12)?,
        created_unix: row.get(13)?,
        updated_unix: row.get(14)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(id: Uuid) -> CreateEndpoint {
        CreateEndpoint {
            id,
            device_id: Uuid::new_v4(),
            environment_id: "local-test".into(),
            binding: EndpointBinding::Container {
                container_id: "a".repeat(64),
            },
            container_port: 8080,
            protocol: AccessProtocol::Http,
            tls_server_name: None,
            name: "app".into(),
            management_url: None,
        }
    }
    #[tokio::test]
    async fn trusted_consent_is_owned_persistent_and_removed_with_endpoint() {
        let dir = std::env::temp_dir().join(format!("cofleet-trust-test-{}", Uuid::new_v4()));
        let store = EndpointStore::open(&dir).await.unwrap();
        let id = Uuid::new_v4();
        store
            .create("alice", request(id), format!("https://e-{id}.example/"))
            .await
            .unwrap();
        assert!(!store.is_trusted(id).await.unwrap());
        assert!(store.trust("bob", id).await.is_err());
        store.trust("alice", id).await.unwrap();
        assert!(
            EndpointStore::open(&dir)
                .await
                .unwrap()
                .is_trusted(id)
                .await
                .unwrap()
        );
        assert!(store.delete("alice", id, 1).await.unwrap());
        assert!(!store.is_trusted(id).await.unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn sqlite_create_is_idempotent_and_revision_guarded() {
        let dir = std::env::temp_dir().join(format!("cofleet-access-{}", Uuid::new_v4()));
        let store = EndpointStore::open(&dir).await.unwrap();
        let id = Uuid::new_v4();
        let create = request(id);
        let first = store
            .create("alice", create.clone(), format!("https://{id}.example/"))
            .await
            .unwrap();
        assert_eq!(first.definition.revision, 1);
        assert_eq!(
            store
                .create("alice", create, first.public_url.clone())
                .await
                .unwrap(),
            first
        );
        assert!(store.get("bob", id).await.unwrap().is_none());
        let next = store
            .patch(
                "alice",
                id,
                PatchEndpoint {
                    revision: 1,
                    name: Some("renamed".into()),
                    enabled: Some(false),
                    management_url: None,
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(next.definition.revision, 2);
        assert!(!next.enabled);
        let reopened = EndpointStore::open(&dir).await.unwrap();
        assert_eq!(
            reopened
                .get("alice", id)
                .await
                .unwrap()
                .unwrap()
                .definition
                .revision,
            2
        );
        assert!(store.delete("alice", id, 1).await.is_err());
        assert!(store.delete("alice", id, 2).await.unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
