//! Host-bound sessions for the explicitly enabled trusted-application mode.
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use uuid::Uuid;

pub const MANAGER_COOKIE: &str = "__Host-cofleet_manager";
pub const ACCESS_COOKIE: &str = "__Host-cofleet_access";
pub const BOOTSTRAP_COOKIE: &str = "__Host-cofleet_bootstrap";
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub owner: String,
    pub host: String,
    pub endpoint: Uuid,
    pub revision: u64,
    pub agent_session: Uuid,
}
struct Record<T> {
    value: T,
    expires: Instant,
}
#[derive(Clone)]
struct Grant {
    binding: Binding,
    manager: String,
    bootstrap: String,
}
#[derive(Default)]
struct Data {
    managers: HashMap<String, Record<String>>,
    bootstraps: HashMap<String, Record<(Uuid, String)>>,
    grants: HashMap<String, Record<Grant>>,
    sessions: HashMap<String, Record<Grant>>,
}
impl Data {
    fn clean(&mut self) {
        let now = Instant::now();
        self.managers.retain(|_, v| v.expires > now);
        self.bootstraps.retain(|_, v| v.expires > now);
        self.grants
            .retain(|_, v| v.expires > now && self.managers.contains_key(&v.value.manager));
        self.sessions
            .retain(|_, v| v.expires > now && self.managers.contains_key(&v.value.manager));
    }
}
#[derive(Clone, Default)]
pub struct Sessions(Arc<Mutex<Data>>);
fn hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
fn secret() -> String {
    hash(&format!(
        "{}{}{}",
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4()
    ))
}
fn record<T>(value: T, seconds: u64) -> Record<T> {
    Record {
        value,
        expires: Instant::now() + Duration::from_secs(seconds),
    }
}
pub fn cookie(headers: &axum::http::HeaderMap, name: &str) -> Option<String> {
    let mut found = headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|v| v.trim().split_once('='))
        .filter(|(key, _)| *key == name)
        .map(|(_, v)| v.to_owned());
    let value = found.next()?;
    (found.next().is_none() && value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()))
        .then_some(value)
}
pub fn set_cookie(name: &str, value: &str, max_age: u64) -> String {
    format!("{name}={value}; Secure; HttpOnly; Path=/; SameSite=Lax; Max-Age={max_age}")
}
impl Sessions {
    pub fn manager_valid(&self, token: &str, owner: &str) -> bool {
        let mut data = self.0.lock().unwrap();
        data.clean();
        data.managers
            .get(&hash(token))
            .is_some_and(|v| v.value == owner)
    }
    pub fn manager(&self, owner: &str) -> Option<String> {
        let mut data = self.0.lock().unwrap();
        data.clean();
        if data.managers.len() >= 256 {
            return None;
        }
        let token = secret();
        data.managers
            .insert(hash(&token), record(owner.to_owned(), 8 * 3600));
        Some(token)
    }
    pub fn begin(&self, endpoint: Uuid, host: &str) -> Option<(String, String)> {
        let mut data = self.0.lock().unwrap();
        data.clean();
        if data.bootstraps.len() >= 1024 {
            return None;
        }
        let raw = secret();
        let challenge = hash(&raw);
        data.bootstraps
            .insert(challenge.clone(), record((endpoint, host.into()), 60));
        Some((raw, challenge))
    }
    pub fn challenge(&self, challenge: &str) -> Option<(Uuid, String)> {
        let mut data = self.0.lock().unwrap();
        data.clean();
        data.bootstraps.get(challenge).map(|v| v.value.clone())
    }
    pub fn issue(&self, challenge: &str, manager: &str, binding: Binding) -> Option<String> {
        let mut data = self.0.lock().unwrap();
        data.clean();
        let manager = hash(manager);
        if data.grants.len() >= 1024
            || !data
                .managers
                .get(&manager)
                .is_some_and(|v| v.value == binding.owner)
        {
            return None;
        }
        let pending = data.bootstraps.get(challenge)?;
        if pending.value != (binding.endpoint, binding.host.clone())
            || data.grants.values().any(|v| v.value.bootstrap == challenge)
        {
            return None;
        }
        let token = secret();
        data.grants.insert(
            hash(&token),
            record(
                Grant {
                    binding,
                    manager,
                    bootstrap: challenge.into(),
                },
                30,
            ),
        );
        Some(token)
    }
    pub fn redeem(&self, ticket: &str, bootstrap: &str, binding: &Binding) -> Option<String> {
        let mut data = self.0.lock().unwrap();
        data.clean();
        if data.sessions.len() >= 2048 {
            return None;
        }
        let grant = data.grants.remove(&hash(ticket))?.value;
        if &grant.binding != binding
            || grant.bootstrap != hash(bootstrap)
            || !data
                .managers
                .get(&grant.manager)
                .is_some_and(|v| v.value == binding.owner)
            || !data.bootstraps.contains_key(&grant.bootstrap)
        {
            return None;
        }
        data.bootstraps.remove(&grant.bootstrap);
        let token = secret();
        data.sessions.insert(hash(&token), record(grant, 15 * 60));
        Some(token)
    }
    pub fn valid(&self, token: &str, binding: &Binding) -> bool {
        let mut data = self.0.lock().unwrap();
        data.clean();
        data.sessions.get(&hash(token)).is_some_and(|v| {
            v.value.binding == *binding
                && data
                    .managers
                    .get(&v.value.manager)
                    .is_some_and(|m| m.value == binding.owner)
        })
    }
    pub fn logout(&self, manager: &str) {
        let mut d = self.0.lock().unwrap();
        d.managers.remove(&hash(manager));
        d.clean();
    }
    pub fn revoke(&self, endpoint: Uuid) {
        let mut d = self.0.lock().unwrap();
        d.bootstraps.retain(|_, v| v.value.0 != endpoint);
        d.grants.retain(|_, v| v.value.binding.endpoint != endpoint);
        d.sessions
            .retain(|_, v| v.value.binding.endpoint != endpoint);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn binding() -> Binding {
        Binding {
            owner: "alice".into(),
            host: "e-one.example".into(),
            endpoint: Uuid::new_v4(),
            revision: 1,
            agent_session: Uuid::new_v4(),
        }
    }
    #[test]
    fn grants_bind_browser_owner_host_revision_and_agent_and_are_single_use() {
        let s = Sessions::default();
        let b = binding();
        let m = s.manager("alice").unwrap();
        let (boot, c) = s.begin(b.endpoint, &b.host).unwrap();
        assert!(s.issue(&c, &s.manager("bob").unwrap(), b.clone()).is_none());
        let grant = s.issue(&c, &m, b.clone()).unwrap();
        assert!(s.issue(&c, &m, b.clone()).is_none());
        assert!(s.redeem(&grant, "wrong", &b).is_none());
        assert!(s.redeem(&grant, &boot, &b).is_none());
        let (_, c) = s.begin(b.endpoint, &b.host).unwrap();
        let grant = s.issue(&c, &m, b.clone()).unwrap();
        assert!(s.redeem(&grant, &boot, &b).is_none());
        let (boot, c) = s.begin(b.endpoint, &b.host).unwrap();
        let grant = s.issue(&c, &m, b.clone()).unwrap();
        let access = s.redeem(&grant, &boot, &b).unwrap();
        assert!(s.valid(&access, &b));
        assert!(s.redeem(&grant, &boot, &b).is_none());
        let mut changed = b.clone();
        changed.host = "other.example".into();
        assert!(!s.valid(&access, &changed));
        changed = b.clone();
        changed.revision += 1;
        assert!(!s.valid(&access, &changed));
        changed = b.clone();
        changed.agent_session = Uuid::new_v4();
        assert!(!s.valid(&access, &changed));
        s.logout(&m);
        assert!(!s.valid(&access, &b));
    }
    #[test]
    fn revocation_and_expiry_fail_closed() {
        let s = Sessions::default();
        let b = binding();
        let m = s.manager("alice").unwrap();
        let (boot, c) = s.begin(b.endpoint, &b.host).unwrap();
        let g = s.issue(&c, &m, b.clone()).unwrap();
        s.revoke(b.endpoint);
        assert!(s.redeem(&g, &boot, &b).is_none());
        let (_, c) = s.begin(b.endpoint, &b.host).unwrap();
        s.0.lock().unwrap().bootstraps.get_mut(&c).unwrap().expires = Instant::now();
        assert!(s.issue(&c, &m, b).is_none());
    }
}
