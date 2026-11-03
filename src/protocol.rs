use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Heartbeat {
    #[serde(default)]
    pub workspace: Option<crate::workspace::WorkspaceSnapshot>,
    #[serde(default)]
    pub docker: Option<crate::docker::DockerSnapshot>,
    pub device_id: uuid::Uuid,
    pub name: String,
    pub os: String,
    pub arch: String,
    pub accepting_tasks: bool,
    pub heartbeat_secs: u64,
    pub version: String,
    #[serde(default)]
    pub resources: Option<crate::resources::ResourceSnapshot>,
    #[serde(default)]
    pub resource_policy: crate::resources::ResourcePolicy,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentStatus {
    pub device_id: uuid::Uuid,
    pub name: String,
    pub manager_url: String,
    pub accepting_tasks: bool,
    pub connected: bool,
    pub last_success_unix: Option<u64>,
    pub last_error: Option<String>,
    #[serde(default)]
    pub resources: Option<crate::resources::ResourceSnapshot>,
    #[serde(default)]
    pub resource_policy: crate::resources::ResourcePolicy,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum LocalRequest {
    Status,
    Docker {
        request: crate::docker::DockerRequest,
    },
    Pause,
    Resume,
    SetResourcePolicy {
        policy: crate::resources::ResourcePolicy,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LocalResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docker: Option<crate::docker::DockerReply>,
    pub status: Option<AgentStatus>,
    pub error: Option<String>,
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 16 * 1024;

impl Heartbeat {
    pub fn valid(&self) -> bool {
        (1..=300).contains(&self.heartbeat_secs)
            && !self.name.trim().is_empty()
            && self.name.len() <= 128
            && self.os.len() <= 32
            && self.arch.len() <= 32
            && self.version.len() <= 64
            && self.resource_policy.validate().is_ok()
            && self.resources.as_ref().is_none_or(|r| r.valid())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentMessage {
    DockerResult {
        call_id: uuid::Uuid,
        result: crate::docker::DockerReply,
    },
    Hello {
        protocol_version: u32,
        state: Heartbeat,
    },
    Heartbeat {
        state: Heartbeat,
    },
    Report {
        request_id: uuid::Uuid,
        state: Heartbeat,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HubMessage {
    WorkspaceTunnel {
        tunnel_id: uuid::Uuid,
    },
    DockerCommand {
        call_id: uuid::Uuid,
        request: crate::docker::DockerRequest,
    },
    Welcome {
        protocol_version: u32,
        session_id: uuid::Uuid,
    },
    Ack {
        last_seen_unix: u64,
    },
    RequestReport {
        request_id: uuid::Uuid,
    },
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;
    #[test]
    fn legacy_v1_heartbeat_needs_no_resource_fields() {
        let state: Heartbeat = serde_json::from_value(serde_json::json!({
            "device_id": "00000000-0000-4000-8000-000000000001",
            "name": "legacy", "os": "linux", "arch": "x86_64",
            "accepting_tasks": true, "heartbeat_secs": 5, "version": "0.1.0"
        }))
        .unwrap();
        assert!(state.valid());
        assert!(state.resources.is_none());
        assert_eq!(state.resource_policy, Default::default());
    }
}
