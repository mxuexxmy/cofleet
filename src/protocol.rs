use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Heartbeat {
    pub device_id: uuid::Uuid,
    pub name: String,
    pub os: String,
    pub arch: String,
    pub accepting_tasks: bool,
    pub heartbeat_secs: u64,
    pub version: String,
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
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum LocalRequest {
    Status,
    Pause,
    Resume,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LocalResponse {
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
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentMessage {
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
