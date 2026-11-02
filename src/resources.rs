//! Lightweight machine snapshots. No process list, network addresses or mount paths
//! are collected. The storage counter covers only the Agent data filesystem.
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{
        Arc, RwLock,
        mpsc::{self, RecvTimeoutError},
    },
    time::{Duration, Instant},
};
use sysinfo::System;

const MIB: u64 = 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ResourcePolicy {
    pub reserved_cpu_threads: u32,
    pub reserved_memory_mib: u64,
}
impl ResourcePolicy {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.reserved_cpu_threads <= 65536,
            "reserved CPU threads exceed supported range"
        );
        anyhow::ensure!(
            self.reserved_memory_mib <= u64::MAX / MIB,
            "reserved memory exceeds supported range"
        );
        Ok(())
    }
    /// Planning capacity, not a process limit or proof that this memory is idle.
    pub fn capacity(&self, resources: &ResourceSnapshot) -> PlanningCapacity {
        PlanningCapacity {
            cpu_threads: resources
                .logical_cpus
                .saturating_sub(self.reserved_cpu_threads),
            memory_bytes: resources
                .total_memory_bytes
                .saturating_sub(self.reserved_memory_mib.saturating_mul(MIB)),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanningCapacity {
    pub cpu_threads: u32,
    pub memory_bytes: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StorageSnapshot {
    pub total_bytes: u64,
    pub available_bytes: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResourceSnapshot {
    pub sampled_at_unix: u64,
    #[serde(default)]
    pub age_secs: u64,
    pub logical_cpus: u32,
    pub cpu_usage_percent: Option<f32>,
    pub total_memory_bytes: u64,
    pub available_memory_bytes: u64,
    pub storage: Option<StorageSnapshot>,
    pub os_version: String,
    /// WSL is an execution environment, not an independent physical resource pool.
    pub environment: String,
    pub uptime_secs: u64,
}
impl ResourceSnapshot {
    pub fn valid(&self) -> bool {
        self.logical_cpus > 0
            && self.logical_cpus <= 65536
            && self.total_memory_bytes > 0
            && self.available_memory_bytes <= self.total_memory_bytes
            && self
                .cpu_usage_percent
                .is_none_or(|v| v.is_finite() && (0.0..=100.0).contains(&v))
            && self
                .storage
                .as_ref()
                .is_none_or(|d| d.total_bytes > 0 && d.available_bytes <= d.total_bytes)
            && self.os_version.len() <= 256
            && matches!(self.environment.as_str(), "native" | "wsl")
    }
}

pub struct ResourceMonitor {
    snapshot: Arc<RwLock<Option<(Instant, ResourceSnapshot)>>>,
    _shutdown: mpsc::Sender<()>,
}
impl ResourceMonitor {
    pub fn start(data_dir: PathBuf) -> std::io::Result<Self> {
        let snapshot = Arc::new(RwLock::new(None));
        let (shutdown, shutdown_rx) = mpsc::channel();
        let latest = snapshot.clone();
        std::thread::Builder::new()
            .name("cofleet-resources".into())
            .spawn(move || {
                let mut system = System::new();
                let os_version: String = System::long_os_version()
                    .unwrap_or_default()
                    .chars()
                    .take(64)
                    .collect();
                let environment = if std::env::var_os("WSL_DISTRO_NAME").is_some()
                    || std::fs::read_to_string("/proc/sys/kernel/osrelease")
                        .is_ok_and(|v| v.to_lowercase().contains("microsoft"))
                {
                    "wsl"
                } else {
                    "native"
                };
                let mut first = true;
                loop {
                    system.refresh_cpu_usage();
                    system.refresh_memory();
                    let storage = fs2::total_space(&data_dir)
                        .ok()
                        .zip(fs2::available_space(&data_dir).ok())
                        .filter(|(total, available)| *total > 0 && available <= total)
                        .map(|(total_bytes, available_bytes)| StorageSnapshot {
                            total_bytes,
                            available_bytes,
                        });
                    let sample = ResourceSnapshot {
                        sampled_at_unix: crate::protocol::unix_now(),
                        age_secs: 0,
                        logical_cpus: system.cpus().len() as u32,
                        cpu_usage_percent: (!first)
                            .then(|| system.global_cpu_usage().clamp(0.0, 100.0)),
                        total_memory_bytes: system.total_memory(),
                        available_memory_bytes: system.available_memory(),
                        storage,
                        os_version: os_version.clone(),
                        environment: environment.into(),
                        uptime_secs: System::uptime(),
                    };
                    if sample.valid() {
                        *latest.write().expect("resource cache") = Some((Instant::now(), sample));
                    }
                    first = false;
                    if !matches!(
                        shutdown_rx.recv_timeout(Duration::from_secs(5)),
                        Err(RecvTimeoutError::Timeout)
                    ) {
                        break;
                    }
                }
            })?;
        Ok(Self {
            snapshot,
            _shutdown: shutdown,
        })
    }
    pub fn snapshot(&self) -> Option<ResourceSnapshot> {
        let cache = self.snapshot.read().ok()?;
        let (collected, snapshot) = cache.as_ref()?;
        let mut snapshot = snapshot.clone();
        snapshot.age_secs = collected.elapsed().as_secs();
        Some(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> ResourceSnapshot {
        ResourceSnapshot {
            sampled_at_unix: 1,
            age_secs: 0,
            logical_cpus: 8,
            cpu_usage_percent: Some(25.0),
            total_memory_bytes: 16 * 1024 * MIB,
            available_memory_bytes: 8 * 1024 * MIB,
            storage: None,
            os_version: "test".into(),
            environment: "native".into(),
            uptime_secs: 1,
        }
    }
    #[test]
    fn budgets_saturate_and_reject_overflow() {
        let mut policy = ResourcePolicy {
            reserved_cpu_threads: 2,
            reserved_memory_mib: 4096,
        };
        let capacity = policy.capacity(&sample());
        assert_eq!(capacity.cpu_threads, 6);
        assert_eq!(capacity.memory_bytes, 12 * 1024 * MIB);
        policy.reserved_cpu_threads = 99;
        policy.reserved_memory_mib = 999999;
        assert_eq!(policy.capacity(&sample()).cpu_threads, 0);
        assert_eq!(policy.capacity(&sample()).memory_bytes, 0);
        policy.reserved_memory_mib = u64::MAX;
        assert!(policy.validate().is_err());
    }
    #[test]
    fn partial_local_policy_defaults_unspecified_values() {
        let policy: ResourcePolicy = toml::from_str("reserved_cpu_threads = 2").unwrap();
        assert_eq!(policy.reserved_memory_mib, 0);
        assert_eq!(policy.reserved_cpu_threads, 2);
    }
    #[test]
    fn snapshots_reject_invalid_measurements() {
        let mut s = sample();
        assert!(s.valid());
        s.cpu_usage_percent = Some(f32::NAN);
        assert!(!s.valid());
        s.cpu_usage_percent = Some(25.0);
        s.available_memory_bytes = u64::MAX;
        assert!(!s.valid());
    }
    #[test]
    fn collects_actual_resources_without_process_enumeration() {
        let monitor = ResourceMonitor::start(std::env::temp_dir()).unwrap();
        for _ in 0..100 {
            if let Some(sample) = monitor.snapshot() {
                assert!(sample.valid());
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("no resource sample");
    }
}
