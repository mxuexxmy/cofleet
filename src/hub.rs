//! CoFleet control hub. Only bounded, typed control messages are accepted.
use crate::protocol::{
    AgentMessage, Heartbeat, HubMessage, MAX_FRAME_BYTES, PROTOCOL_VERSION, unix_now,
};
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{RwLock, mpsc};
use uuid::Uuid;

#[derive(Clone, Default)]
pub struct Hub {
    records: Arc<RwLock<HashMap<(String, Uuid), Record>>>,
}
struct Session {
    id: Uuid,
    sender: mpsc::Sender<HubMessage>,
}
struct Record {
    state: Heartbeat,
    seen_at: Instant,
    last_seen: u64,
    session: Option<Session>,
    last_report: Option<Uuid>,
    pending_reports: Vec<Uuid>,
}
#[derive(Serialize)]
pub struct Device {
    #[serde(flatten)]
    pub state: Heartbeat,
    pub last_seen_unix: u64,
    pub online: bool,
    pub last_report_id: Option<Uuid>,
}
impl Hub {
    pub async fn devices(&self, user: &str) -> Vec<Device> {
        let records = self.records.read().await;
        let mut devices: Vec<_> = records
            .iter()
            .filter(|((owner, _), _)| owner == user)
            .map(|(_, r)| Device {
                state: r.state.clone(),
                last_seen_unix: r.last_seen,
                online: r.session.is_some()
                    && r.seen_at.elapsed().as_secs() < (r.state.heartbeat_secs * 3).max(15),
                last_report_id: r.last_report,
            })
            .collect();
        devices.sort_by_key(|v| v.state.device_id);
        devices
    }
    pub async fn request_report(
        &self,
        user: &str,
        device: Uuid,
    ) -> Result<Uuid, axum::http::StatusCode> {
        use axum::http::StatusCode;
        let mut records = self.records.write().await;
        let record = records
            .get_mut(&(user.into(), device))
            .ok_or(StatusCode::NOT_FOUND)?;
        let session = record.session.as_ref().ok_or(StatusCode::CONFLICT)?;
        if record.pending_reports.len() >= 8 {
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
        let request_id = Uuid::new_v4();
        session
            .sender
            .try_send(HubMessage::RequestReport { request_id })
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        record.pending_reports.push(request_id);
        Ok(request_id)
    }
    async fn attach(
        &self,
        user: &str,
        state: Heartbeat,
        sender: mpsc::Sender<HubMessage>,
    ) -> Option<Uuid> {
        let mut records = self.records.write().await;
        records.retain(|_, r| r.session.is_some() || r.seen_at.elapsed().as_secs() < 86400);
        let key = (user.into(), state.device_id);
        if !records.contains_key(&key)
            && (records.len() >= 4096
                || records.keys().filter(|(owner, _)| owner == user).count() >= 256)
        {
            return None;
        }
        let id = Uuid::new_v4();
        // Replacing the sender closes the previous session's receiver. Generation checks
        // ensure its eventual disconnect cannot mark this newer connection offline.
        records.insert(
            key,
            Record {
                state,
                seen_at: Instant::now(),
                last_seen: unix_now(),
                session: Some(Session { id, sender }),
                last_report: None,
                pending_reports: Vec::new(),
            },
        );
        Some(id)
    }
    async fn update(&self, user: &str, id: Uuid, state: Heartbeat, report: Option<Uuid>) -> bool {
        if !state.valid() {
            return false;
        }
        let mut records = self.records.write().await;
        let Some(record) = records.get_mut(&(user.into(), state.device_id)) else {
            return false;
        };
        if record.session.as_ref().map(|s| s.id) != Some(id) {
            return false;
        }
        if let Some(report) = report {
            let Some(position) = record.pending_reports.iter().position(|v| *v == report) else {
                return false;
            };
            record.pending_reports.remove(position);
            record.last_report = Some(report);
        }
        record.state = state;
        record.seen_at = Instant::now();
        record.last_seen = unix_now();
        true
    }
    async fn detach(&self, user: &str, device: Uuid, session: Uuid) {
        let mut records = self.records.write().await;
        if let Some(record) = records.get_mut(&(user.into(), device))
            && record.session.as_ref().map(|s| s.id) == Some(session)
        {
            record.session = None;
            record.pending_reports.clear();
        }
    }
    pub async fn serve(self, user: String, mut socket: WebSocket) {
        let first = tokio::time::timeout(Duration::from_secs(5), socket.recv()).await;
        let Ok(Some(Ok(Message::Text(text)))) = first else {
            return;
        };
        let Ok(AgentMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            state,
        }) = serde_json::from_str::<AgentMessage>(&text)
        else {
            return;
        };
        if !state.valid() {
            return;
        }
        let device = state.device_id;
        let mut idle_secs = (state.heartbeat_secs * 3).max(15);
        let (sender, mut receiver) = mpsc::channel(8);
        let Some(session) = self.attach(&user, state, sender).await else {
            return;
        };
        let (mut sink, mut stream) = socket.split();
        let run = async {
            let welcome = HubMessage::Welcome {
                protocol_version: PROTOCOL_VERSION,
                session_id: session,
            };
            send(&mut sink, &welcome).await?;
            let idle = tokio::time::sleep(Duration::from_secs(idle_secs));
            tokio::pin!(idle);
            loop {
                tokio::select! {
                    message = stream.next() => {
                        let Some(Ok(frame)) = message else { break; };
                        match frame {
                            Message::Text(text) if text.len() <= MAX_FRAME_BYTES => {
                                let Ok(message) = serde_json::from_str::<AgentMessage>(&text) else { break; };
                                let (state, report) = match message {
                                    AgentMessage::Heartbeat { state } => (state,None),
                                    AgentMessage::Report { request_id, state } => (state,Some(request_id)),
                                    _ => break,
                                };
                                if state.device_id != device || !state.valid() { break; }
                                idle_secs = (state.heartbeat_secs * 3).max(15);
                                if !self.update(&user, session, state, report).await { break; }
                                idle.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(idle_secs));
                                send(&mut sink, &HubMessage::Ack { last_seen_unix: unix_now() }).await?;
                            }
                            Message::Ping(data) => { tokio::time::timeout(Duration::from_secs(5), sink.send(Message::Pong(data))).await??; },
                            Message::Pong(_) => {},
                            _ => break,
                        }
                    }
                    command = receiver.recv() => {
                        let Some(command) = command else { break; };
                        send(&mut sink, &command).await?;
                    }
                    _ = &mut idle => break,
                }
            }
            Ok::<(), anyhow::Error>(())
        };
        let _ = run.await;
        self.detach(&user, device, session).await;
        // Dropping the sink closes the transport even when the other side is stalled.
    }
}
async fn send(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    value: &HubMessage,
) -> anyhow::Result<()> {
    tokio::time::timeout(
        Duration::from_secs(5),
        sink.send(Message::Text(serde_json::to_string(value)?.into())),
    )
    .await??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state(id: Uuid) -> Heartbeat {
        Heartbeat {
            device_id: id,
            name: "node".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            accepting_tasks: true,
            heartbeat_secs: 5,
            version: "test".into(),
        }
    }
    #[tokio::test]
    async fn rooms_are_isolated_and_old_disconnect_does_not_remove_new_session() {
        let hub = Hub::default();
        let device = Uuid::new_v4();
        let (tx, mut old_rx) = mpsc::channel(8);
        let old = hub.attach("alice", state(device), tx).await.unwrap();
        let (tx, _rx) = mpsc::channel(8);
        let current = hub.attach("alice", state(device), tx).await.unwrap();
        assert!(old_rx.recv().await.is_none());
        hub.detach("alice", device, old).await;
        assert!(hub.devices("alice").await[0].online);
        assert!(hub.devices("bob").await.is_empty());
        assert_eq!(
            hub.request_report("bob", device).await,
            Err(axum::http::StatusCode::NOT_FOUND)
        );
        assert!(!hub.update("alice", old, state(device), None).await);
        assert!(
            !hub.update("alice", current, state(device), Some(Uuid::new_v4()))
                .await
        );
        hub.detach("alice", device, current).await;
        assert!(!hub.devices("alice").await[0].online);
    }
}
