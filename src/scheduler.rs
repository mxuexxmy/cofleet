//! Small in-memory scheduler for stage 4. Devices are execution environments;
//! reservations are accounted against a physical host exactly once.
use crate::{protocol::Heartbeat, resources::AvailabilityPreference};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkloadKind {
    Batch,
    StatelessService,
    StatefulService,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Requirements {
    pub cpu_threads: u32,
    pub memory_mib: u64,
    pub gpu_memory_mib: u64,
    pub data_location: Option<String>,
    pub prefer_always_on: bool,
}
impl Default for Requirements {
    fn default() -> Self {
        Self {
            cpu_threads: 1,
            memory_mib: 512,
            gpu_memory_mib: 0,
            data_location: None,
            prefer_always_on: false,
        }
    }
}
impl Requirements {
    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            (1..=65536).contains(&self.cpu_threads),
            "cpu_threads must be 1..65536"
        );
        anyhow::ensure!(self.memory_mib > 0, "memory_mib must be greater than zero");
        if let Some(location) = &self.data_location {
            anyhow::ensure!(
                !location.trim().is_empty() && location.len() <= 128,
                "invalid data location"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleRequest {
    pub workload_id: Uuid,
    pub kind: WorkloadKind,
    #[serde(default)]
    pub requirements: Requirements,
}
impl ScheduleRequest {
    pub fn validate(&self) -> anyhow::Result<()> {
        self.requirements.validate()?;
        if self.kind == WorkloadKind::StatefulService {
            anyhow::ensure!(
                self.requirements.data_location.is_some(),
                "stateful workloads require a data_location"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacementAction {
    Start,
    Keep,
    Retry,
    Rebuild,
    Wait,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CandidateEvaluation {
    pub device_id: Uuid,
    pub physical_host_id: Uuid,
    pub eligible: bool,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Placement {
    pub workload_id: Uuid,
    pub kind: WorkloadKind,
    pub action: PlacementAction,
    pub attempt: u32,
    pub device_id: Option<Uuid>,
    pub physical_host_id: Option<Uuid>,
    pub reason: String,
    pub candidates: Vec<CandidateEvaluation>,
}

#[derive(Clone)]
pub struct ScheduleNode {
    pub state: Heartbeat,
    pub online: bool,
}

#[derive(Clone)]
struct Entry {
    request: ScheduleRequest,
    device_id: Option<Uuid>,
    physical_host_id: Option<Uuid>,
    attempt: u32,
}

#[derive(Default)]
pub struct Scheduler {
    entries: HashMap<(String, Uuid), Entry>,
}

impl Scheduler {
    pub fn remove(&mut self, owner: &str, id: Uuid) -> bool {
        self.entries.remove(&(owner.to_owned(), id)).is_some()
    }

    pub fn submit(
        &mut self,
        owner: &str,
        nodes: &[ScheduleNode],
        request: ScheduleRequest,
    ) -> anyhow::Result<Placement> {
        request.validate()?;
        let key = (owner.to_owned(), request.workload_id);
        if let Some(entry) = self.entries.get(&key) {
            anyhow::ensure!(
                entry.request == request,
                "workload ID already used for different requirements"
            );
        } else {
            self.entries.insert(
                key.clone(),
                Entry {
                    request,
                    device_id: None,
                    physical_host_id: None,
                    attempt: 0,
                },
            );
        }
        Ok(self.reconcile_one(owner, nodes, key.1))
    }

    pub fn list(&mut self, owner: &str, nodes: &[ScheduleNode]) -> Vec<Placement> {
        let mut ids: Vec<_> = self
            .entries
            .keys()
            .filter(|(entry_owner, _)| entry_owner == owner)
            .map(|(_, id)| *id)
            .collect();
        ids.sort();
        ids.into_iter()
            .map(|id| self.reconcile_one(owner, nodes, id))
            .collect()
    }

    fn reconcile_one(&mut self, owner: &str, nodes: &[ScheduleNode], id: Uuid) -> Placement {
        let key = (owner.to_owned(), id);
        let existing = self.entries.get(&key).expect("known workload").clone();
        if let Some(device) = existing.device_id
            && nodes
                .iter()
                .any(|node| node.state.device_id == device && node.online)
        {
            return placement(
                &existing,
                PlacementAction::Keep,
                "assigned node is online",
                Vec::new(),
            );
        }
        if existing.device_id.is_some() && existing.request.kind == WorkloadKind::StatefulService {
            return placement(
                &existing,
                PlacementAction::Wait,
                "stateful workload lost contact with its node; waiting to avoid a second writer",
                Vec::new(),
            );
        }

        let commitments = self.commitments(owner, id);
        let (selected, candidates) = choose(nodes, &existing.request, &commitments);
        let entry = self.entries.get_mut(&key).expect("known workload");
        match selected {
            Some((device, host)) => {
                let action = if entry.device_id.is_none() {
                    PlacementAction::Start
                } else if entry.request.kind == WorkloadKind::Batch {
                    PlacementAction::Retry
                } else {
                    PlacementAction::Rebuild
                };
                entry.attempt = entry.attempt.saturating_add(1);
                entry.device_id = Some(device);
                entry.physical_host_id = Some(host);
                let reason = match action {
                    PlacementAction::Start => "matching node selected",
                    PlacementAction::Retry => "previous node is offline; retry on a matching node",
                    PlacementAction::Rebuild => {
                        "previous node is offline; rebuild stateless service on a matching node"
                    }
                    _ => unreachable!(),
                };
                placement(entry, action, reason, candidates)
            }
            None => placement(
                entry,
                PlacementAction::Wait,
                &waiting_reason(&candidates),
                candidates,
            ),
        }
    }

    fn commitments(&self, owner: &str, except: Uuid) -> HashMap<Uuid, (u32, u64, u64)> {
        let mut used = HashMap::new();
        for ((entry_owner, id), entry) in &self.entries {
            if entry_owner != owner || *id == except {
                continue;
            }
            if let Some(host) = entry.physical_host_id {
                let value = used.entry(host).or_insert((0u32, 0u64, 0u64));
                value.0 = value
                    .0
                    .saturating_add(entry.request.requirements.cpu_threads);
                value.1 = value
                    .1
                    .saturating_add(entry.request.requirements.memory_mib);
                value.2 = value
                    .2
                    .saturating_add(entry.request.requirements.gpu_memory_mib);
            }
        }
        used
    }
}

fn placement(
    entry: &Entry,
    action: PlacementAction,
    reason: &str,
    candidates: Vec<CandidateEvaluation>,
) -> Placement {
    Placement {
        workload_id: entry.request.workload_id,
        kind: entry.request.kind,
        action,
        attempt: entry.attempt,
        device_id: entry.device_id,
        physical_host_id: entry.physical_host_id,
        reason: reason.into(),
        candidates,
    }
}

fn choose(
    nodes: &[ScheduleNode],
    request: &ScheduleRequest,
    commitments: &HashMap<Uuid, (u32, u64, u64)>,
) -> (Option<(Uuid, Uuid)>, Vec<CandidateEvaluation>) {
    const MIB: u64 = 1024 * 1024;
    // A physical host's capacity is the largest report from any environment on it,
    // never the sum of Windows and WSL reports.
    let mut host_capacity = HashMap::<Uuid, (u32, u64, u64)>::new();
    for node in nodes {
        if let Some(resources) = &node.state.resources {
            let host = node.state.capabilities.physical_host(node.state.device_id);
            let capacity = node.state.resource_policy.capacity(resources);
            let value = host_capacity.entry(host).or_insert((0, 0, 0));
            value.0 = value.0.max(capacity.cpu_threads);
            value.1 = value.1.max(capacity.memory_bytes / MIB);
            value.2 = value.2.max(
                node.state
                    .capabilities
                    .gpu
                    .as_ref()
                    .map_or(0, |gpu| gpu.memory_mib),
            );
        }
    }
    let mut evaluations = Vec::new();
    let mut eligible = Vec::new();
    let mut seen_devices = HashSet::new();
    for node in nodes {
        if !seen_devices.insert(node.state.device_id) {
            continue;
        }
        let host = node.state.capabilities.physical_host(node.state.device_id);
        let mut reasons = Vec::new();
        if !node.online {
            reasons.push("node is offline".into());
        }
        if !node.state.accepting_tasks {
            reasons.push("node paused new tasks".into());
        }
        if node.state.pipeline.is_none() {
            reasons.push("build/deploy executor is not enabled".into());
        }
        if node.state.resources.is_none() {
            reasons.push("resource report is unavailable".into());
        }
        let (used_cpu, used_memory, used_gpu) = commitments.get(&host).copied().unwrap_or_default();
        let (host_cpu, host_memory, host_gpu) =
            host_capacity.get(&host).copied().unwrap_or_default();
        if let Some(resources) = &node.state.resources {
            let capacity = node.state.resource_policy.capacity(resources);
            if capacity.cpu_threads < request.requirements.cpu_threads {
                reasons.push("insufficient execution-environment CPU capacity".into());
            }
            if capacity.memory_bytes / MIB < request.requirements.memory_mib {
                reasons.push("insufficient execution-environment memory capacity".into());
            }
        }
        if host_cpu.saturating_sub(used_cpu) < request.requirements.cpu_threads {
            reasons.push("insufficient physical CPU capacity".into());
        }
        if host_memory.saturating_sub(used_memory) < request.requirements.memory_mib {
            reasons.push("insufficient physical memory capacity".into());
        }
        if request.requirements.gpu_memory_mib > 0 {
            match &node.state.capabilities.gpu {
                None => reasons.push("GPU is required but not reported".into()),
                Some(gpu) if gpu.memory_mib < request.requirements.gpu_memory_mib => {
                    reasons.push("insufficient GPU memory".into())
                }
                Some(_) => {}
            }
            if host_gpu.saturating_sub(used_gpu) < request.requirements.gpu_memory_mib {
                reasons.push("insufficient uncommitted physical GPU memory".into());
            }
        }
        if let Some(location) = &request.requirements.data_location
            && !node.state.capabilities.data_locations.contains(location)
        {
            reasons.push(format!("data location {location} is not present"));
        }
        let is_eligible = reasons.is_empty();
        if is_eligible {
            let always = node.state.capabilities.availability == AvailabilityPreference::AlwaysOn;
            let data_local = request
                .requirements
                .data_location
                .as_ref()
                .is_some_and(|location| node.state.capabilities.data_locations.contains(location));
            let spare_memory = host_memory.saturating_sub(used_memory);
            eligible.push((
                data_local,
                !request.requirements.prefer_always_on || always,
                spare_memory,
                node.state.device_id,
                host,
            ));
        }
        evaluations.push(CandidateEvaluation {
            device_id: node.state.device_id,
            physical_host_id: host,
            eligible: is_eligible,
            reasons,
        });
    }
    eligible.sort_by_key(|(data, always, memory, device, _)| {
        (!*data, !*always, std::cmp::Reverse(*memory), *device)
    });
    (
        eligible
            .first()
            .map(|(_, _, _, device, host)| (*device, *host)),
        evaluations,
    )
}

fn waiting_reason(candidates: &[CandidateEvaluation]) -> String {
    if candidates.is_empty() {
        return "no nodes are registered".into();
    }
    let mut reasons: Vec<_> = candidates
        .iter()
        .flat_map(|candidate| candidate.reasons.iter().cloned())
        .collect();
    reasons.sort();
    reasons.dedup();
    format!("no matching node: {}", reasons.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        protocol::Heartbeat,
        resources::{NodeCapabilities, ResourcePolicy, ResourceSnapshot},
    };

    fn node(device: Uuid, host: Uuid, location: &str, online: bool) -> ScheduleNode {
        ScheduleNode {
            online,
            state: Heartbeat {
                docker_observe: None,
                pipeline: Some(Default::default()),
                workspace: None,
                docker: Some(Default::default()),
                device_id: device,
                name: "node".into(),
                os: "windows".into(),
                arch: "x86_64".into(),
                accepting_tasks: true,
                heartbeat_secs: 5,
                version: "test".into(),
                resources: Some(ResourceSnapshot {
                    sampled_at_unix: 1,
                    age_secs: 0,
                    logical_cpus: 8,
                    cpu_usage_percent: None,
                    total_memory_bytes: 16 * 1024 * 1024 * 1024,
                    available_memory_bytes: 8 * 1024 * 1024 * 1024,
                    storage: None,
                    os_version: "test".into(),
                    environment: "native".into(),
                    uptime_secs: 1,
                }),
                resource_policy: ResourcePolicy::default(),
                capabilities: NodeCapabilities {
                    physical_host_id: Some(host),
                    data_locations: vec![location.into()],
                    ..Default::default()
                },
            },
        }
    }

    fn request(id: Uuid, kind: WorkloadKind, memory_mib: u64) -> ScheduleRequest {
        ScheduleRequest {
            workload_id: id,
            kind,
            requirements: Requirements {
                memory_mib,
                ..Default::default()
            },
        }
    }

    #[test]
    fn windows_and_wsl_do_not_double_physical_capacity() {
        let host = Uuid::new_v4();
        let nodes = vec![
            node(Uuid::new_v4(), host, "home", true),
            node(Uuid::new_v4(), host, "home", true),
        ];
        let mut scheduler = Scheduler::default();
        let first = scheduler
            .submit(
                "owner",
                &nodes,
                request(Uuid::new_v4(), WorkloadKind::Batch, 12_000),
            )
            .unwrap();
        assert_eq!(first.action, PlacementAction::Start);
        let second = scheduler
            .submit(
                "owner",
                &nodes,
                request(Uuid::new_v4(), WorkloadKind::Batch, 8_000),
            )
            .unwrap();
        assert_eq!(second.action, PlacementAction::Wait);
        assert!(second.reason.contains("physical memory"));
    }

    #[test]
    fn stateful_waits_while_batch_retries_and_stateless_rebuilds() {
        let first_device = Uuid::from_u128(1);
        let first_host = Uuid::from_u128(11);
        let original = vec![
            node(first_device, first_host, "db-a", true),
            node(Uuid::from_u128(2), Uuid::from_u128(22), "db-a", false),
        ];
        let mut offline = original.clone();
        offline[0].online = false;
        offline[1].online = true;

        let mut scheduler = Scheduler::default();
        let stateful_id = Uuid::new_v4();
        let mut stateful = request(stateful_id, WorkloadKind::StatefulService, 512);
        stateful.requirements.data_location = Some("db-a".into());
        assert_eq!(
            scheduler
                .submit("owner", &original, stateful.clone())
                .unwrap()
                .device_id,
            Some(first_device)
        );
        let stateful_result = scheduler.submit("owner", &offline, stateful).unwrap();
        assert_eq!(stateful_result.action, PlacementAction::Wait);
        assert_eq!(stateful_result.device_id, Some(first_device));

        let mut scheduler = Scheduler::default();
        let batch = request(Uuid::new_v4(), WorkloadKind::Batch, 512);
        scheduler.submit("owner", &original, batch.clone()).unwrap();
        assert_eq!(
            scheduler.submit("owner", &offline, batch).unwrap().action,
            PlacementAction::Retry
        );

        let mut scheduler = Scheduler::default();
        let stateless = request(Uuid::new_v4(), WorkloadKind::StatelessService, 512);
        scheduler
            .submit("owner", &original, stateless.clone())
            .unwrap();
        assert_eq!(
            scheduler
                .submit("owner", &offline, stateless)
                .unwrap()
                .action,
            PlacementAction::Rebuild
        );
    }

    #[test]
    fn reports_data_and_gpu_mismatch_reasons() {
        let nodes = vec![node(Uuid::new_v4(), Uuid::new_v4(), "home", true)];
        let mut scheduler = Scheduler::default();
        let mut request = request(Uuid::new_v4(), WorkloadKind::StatefulService, 512);
        request.requirements.data_location = Some("nas-a".into());
        request.requirements.gpu_memory_mib = 4096;
        let result = scheduler.submit("owner", &nodes, request).unwrap();
        assert_eq!(result.action, PlacementAction::Wait);
        assert!(result.reason.contains("GPU"));
        assert!(result.reason.contains("nas-a"));
    }
}
