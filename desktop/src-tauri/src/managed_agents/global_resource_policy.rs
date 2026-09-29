//! Device-local hard resource limits for Buzz-managed local agent processes.

use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
};
use tauri::{AppHandle, Manager};

use crate::{
    app_state::AppState,
    managed_agents::{
        storage::{atomic_write_json_restricted, managed_agents_base_dir},
        ManagedAgentPairRuntime, ManagedAgentRuntimeKey,
    },
};

pub const RESOURCE_POLICY_SCHEMA_VERSION: u32 = 1;
pub const MAX_RUNNING_AGENTS_LIMIT: u32 = 128;
pub const MIN_MEMORY_RESERVE_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_MEMORY_RESERVE_BYTES: u64 = 1024 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeviceMemorySnapshot {
    pub available_memory_bytes: u64,
    pub total_memory_bytes: u64,
}

pub fn device_memory_snapshot() -> DeviceMemorySnapshot {
    let mut system = sysinfo::System::new();
    system.refresh_memory();
    DeviceMemorySnapshot {
        available_memory_bytes: system.available_memory(),
        total_memory_bytes: system.total_memory(),
    }
}

/// Holds one admission slot until its process has been registered or spawn
/// fails. This bridges parallel startup restore, where child creation and
/// runtime-map registration happen in separate phases.
pub struct ManagedAgentStartReservation {
    app: AppHandle,
    key: Option<ManagedAgentRuntimeKey>,
}

impl Drop for ManagedAgentStartReservation {
    fn drop(&mut self) {
        let Some(key) = self.key.take() else {
            return;
        };
        if let Some(state) = self.app.try_state::<AppState>() {
            if let Ok(mut reservations) = state.managed_agent_start_reservations.lock() {
                reservations.remove(&key);
            }
        }
    }
}

/// Holds the global local-agent capacity reserved by active critic reviewers.
pub struct CriticWorkerProcessReservation {
    app: AppHandle,
    slots: u32,
}

impl Drop for CriticWorkerProcessReservation {
    fn drop(&mut self) {
        if let Some(state) = self.app.try_state::<AppState>() {
            state
                .critic_worker_process_reservations
                .fetch_sub(self.slots, std::sync::atomic::Ordering::AcqRel);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GlobalAgentResourcePolicy {
    pub schema_version: u32,
    /// Optional hard cap on simultaneously running Buzz-managed agents and
    /// critic reviewers. `None` preserves the legacy uncapped behavior.
    #[serde(default)]
    pub max_running_agents: Option<u32>,
    /// Optional minimum free system RAM reserve required before a new managed
    /// local agent or critic reviewer starts. This is a point-in-time admission
    /// check; it does not estimate a child process's RAM or account for GPU memory.
    #[serde(default)]
    pub min_available_memory_bytes: Option<u64>,
}

impl Default for GlobalAgentResourcePolicy {
    fn default() -> Self {
        Self {
            schema_version: RESOURCE_POLICY_SCHEMA_VERSION,
            max_running_agents: None,
            min_available_memory_bytes: None,
        }
    }
}

pub fn validate_global_agent_resource_policy(
    policy: &GlobalAgentResourcePolicy,
) -> Result<(), String> {
    if policy.schema_version != RESOURCE_POLICY_SCHEMA_VERSION {
        return Err(format!(
            "unsupported resource-policy schema version: {}",
            policy.schema_version
        ));
    }
    if let Some(limit) = policy.max_running_agents {
        if !(1..=MAX_RUNNING_AGENTS_LIMIT).contains(&limit) {
            return Err(format!(
                "maxRunningAgents must be between 1 and {MAX_RUNNING_AGENTS_LIMIT}, or null"
            ));
        }
    }
    if let Some(reserve) = policy.min_available_memory_bytes {
        if !(MIN_MEMORY_RESERVE_BYTES..=MAX_MEMORY_RESERVE_BYTES).contains(&reserve) {
            return Err(format!(
                "minAvailableMemoryBytes must be between {MIN_MEMORY_RESERVE_BYTES} and {MAX_MEMORY_RESERVE_BYTES}, or null"
            ));
        }
    }
    Ok(())
}

pub(crate) fn check_memory_reserve(minimum: Option<u64>, available: u64) -> Result<(), String> {
    if let Some(minimum) = minimum {
        if available < minimum {
            return Err(format!(
                "local process start blocked: available system RAM is {available} bytes, below the configured {minimum}-byte reserve"
            ));
        }
    }
    Ok(())
}

/// Serialize policy loading, the final RAM sample, and the OS child spawn.
/// Keeping the spawn closure inside this helper makes the admission boundary
/// directly testable and ensures concurrent starts cannot reuse one sample.
pub(crate) fn with_memory_admission<T>(
    admission_lock: &Mutex<()>,
    load_policy: impl FnOnce() -> Result<GlobalAgentResourcePolicy, String>,
    available_memory: impl FnOnce() -> u64,
    spawn: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let _guard = admission_lock
        .lock()
        .map_err(|error| format!("cannot serialize managed-agent memory checks: {error}"))?;
    let policy = load_policy()?;
    if let Some(minimum) = policy.min_available_memory_bytes {
        check_memory_reserve(Some(minimum), available_memory())?;
    }
    spawn()
}

/// Reserve one local-process slot before spawning. The caller serializes its
/// runtime map and holds the returned reservation until the spawned child is
/// registered. Even uncapped mode records the in-flight key so parallel start
/// paths cannot create duplicate children for the same runtime.
pub fn reserve_managed_agent_start(
    app: &AppHandle,
    runtimes: &mut HashMap<ManagedAgentRuntimeKey, ManagedAgentPairRuntime>,
    key: &ManagedAgentRuntimeKey,
) -> Result<ManagedAgentStartReservation, String> {
    let policy = load_global_agent_resource_policy(app)?;

    let mut active_keys = HashSet::new();
    for (runtime_key, runtime) in runtimes.iter_mut() {
        match runtime.child.try_wait() {
            Ok(None) => {
                active_keys.insert(runtime_key.clone());
            }
            Ok(Some(_)) => {}
            Err(error) => {
                return Err(format!(
                    "cannot verify whether managed agent {} is running: {error}",
                    runtime_key.pubkey
                ));
            }
        }
    }
    if active_keys.contains(key) {
        return Ok(ManagedAgentStartReservation {
            app: app.clone(),
            key: None,
        });
    }

    let state = app.state::<AppState>();
    let mut reservations = state
        .managed_agent_start_reservations
        .lock()
        .map_err(|error| error.to_string())?;
    if reservations.contains(key) {
        return Err("a start for this managed-agent runtime is already in progress".into());
    }

    if let Some(limit) = policy.max_running_agents {
        let pending = reservations
            .iter()
            .filter(|reserved_key| !active_keys.contains(*reserved_key))
            .count()
            .min(u32::MAX as usize) as u32;
        let critic_slots = state
            .critic_worker_process_reservations
            .load(std::sync::atomic::Ordering::Acquire);
        let used = used_process_slots(
            active_keys.len().min(u32::MAX as usize) as u32,
            pending,
            critic_slots,
        );
        if !process_slots_available(Some(limit), used, 1) {
            return Err(format!(
                "global managed-agent process limit reached ({used}/{limit}); stop a local agent or raise the limit before starting another"
            ));
        }
    }
    reservations.insert(key.clone());
    Ok(ManagedAgentStartReservation {
        app: app.clone(),
        key: Some(key.clone()),
    })
}

/// Reserve one global local-agent slot per critic reviewer until the critic
/// command finishes or is canceled.
pub(crate) fn reserve_critic_worker_processes(
    app: &AppHandle,
    requested: usize,
) -> Result<CriticWorkerProcessReservation, String> {
    let requested = u32::try_from(requested)
        .ok()
        .filter(|count| *count > 0)
        .ok_or_else(|| "critic round has no reviewer processes to reserve".to_string())?;
    let state = app.state::<AppState>();
    let _transition = state
        .managed_agent_runtime_transition
        .lock()
        .map_err(|error| error.to_string())?;
    let mut runtimes = state
        .managed_agent_processes
        .lock()
        .map_err(|error| error.to_string())?;
    let policy = load_global_agent_resource_policy(app)?;

    let mut active_keys = HashSet::new();
    for (key, runtime) in runtimes.iter_mut() {
        match runtime.child.try_wait() {
            Ok(None) => {
                active_keys.insert(key.clone());
            }
            Ok(Some(_)) => {}
            Err(error) => {
                return Err(format!(
                    "cannot verify whether managed agent {} is running: {error}",
                    key.pubkey
                ));
            }
        }
    }
    let reservations = state
        .managed_agent_start_reservations
        .lock()
        .map_err(|error| error.to_string())?;
    let pending = reservations
        .iter()
        .filter(|key| !active_keys.contains(*key))
        .count()
        .min(u32::MAX as usize) as u32;
    let active = active_keys.len().min(u32::MAX as usize) as u32;
    let critic_slots = state
        .critic_worker_process_reservations
        .load(std::sync::atomic::Ordering::Acquire);
    let used = used_process_slots(active, pending, critic_slots);
    if !process_slots_available(policy.max_running_agents, used, requested) {
        let limit = policy.max_running_agents.unwrap_or(u32::MAX);
        return Err(format!(
            "critic round needs {requested} local reviewer process slots, but the global limit has {used}/{limit} in use; stop local agents or raise the limit before retrying"
        ));
    }
    state
        .critic_worker_process_reservations
        .fetch_add(requested, std::sync::atomic::Ordering::AcqRel);
    Ok(CriticWorkerProcessReservation {
        app: app.clone(),
        slots: requested,
    })
}

fn process_slots_available(limit: Option<u32>, used: u32, requested: u32) -> bool {
    limit.is_none_or(|limit| used.saturating_add(requested) <= limit)
}

fn used_process_slots(active: u32, pending: u32, critic_reviewers: u32) -> u32 {
    active
        .saturating_add(pending)
        .saturating_add(critic_reviewers)
}

fn global_agent_resource_policy_path<R: tauri::Runtime>(
    app: &AppHandle<R>,
) -> Result<std::path::PathBuf, String> {
    Ok(managed_agents_base_dir(app)?.join("global-agent-resource-policy.json"))
}

pub fn load_global_agent_resource_policy<R: tauri::Runtime>(
    app: &AppHandle<R>,
) -> Result<GlobalAgentResourcePolicy, String> {
    let path = global_agent_resource_policy_path(app)?;
    if !path.exists() {
        return Ok(GlobalAgentResourcePolicy::default());
    }
    let content = std::fs::read_to_string(&path)
        .map_err(|error| format!("failed to read global agent resource policy: {error}"))?;
    let policy: GlobalAgentResourcePolicy = serde_json::from_str(&content)
        .map_err(|error| format!("failed to parse global agent resource policy: {error}"))?;
    validate_global_agent_resource_policy(&policy)?;
    Ok(policy)
}

pub fn save_global_agent_resource_policy<R: tauri::Runtime>(
    app: &AppHandle<R>,
    policy: &GlobalAgentResourcePolicy,
) -> Result<GlobalAgentResourcePolicy, String> {
    validate_global_agent_resource_policy(policy)?;
    let path = global_agent_resource_policy_path(app)?;
    let payload = serde_json::to_vec_pretty(policy)
        .map_err(|error| format!("failed to serialize global agent resource policy: {error}"))?;
    atomic_write_json_restricted(&path, &payload)?;
    Ok(policy.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_resource_policy_defaults_memory_reserve_to_disabled() {
        let policy: GlobalAgentResourcePolicy =
            serde_json::from_str(r#"{"schemaVersion":1,"maxRunningAgents":4}"#).unwrap();
        assert_eq!(policy.min_available_memory_bytes, None);
    }

    #[test]
    fn memory_reserve_bounds_are_validated() {
        let mut policy = GlobalAgentResourcePolicy::default();
        policy.min_available_memory_bytes = Some(MIN_MEMORY_RESERVE_BYTES);
        assert!(validate_global_agent_resource_policy(&policy).is_ok());
        policy.min_available_memory_bytes = Some(MIN_MEMORY_RESERVE_BYTES - 1);
        assert!(validate_global_agent_resource_policy(&policy).is_err());
        policy.min_available_memory_bytes = Some(MAX_MEMORY_RESERVE_BYTES + 1);
        assert!(validate_global_agent_resource_policy(&policy).is_err());
    }

    #[test]
    fn reserve_gate_blocks_only_below_threshold() {
        let minimum = Some(8_000);
        assert!(check_memory_reserve(minimum, 8_001).is_ok());
        assert!(check_memory_reserve(minimum, 8_000).is_ok());
        assert!(check_memory_reserve(minimum, 7_999).is_err());
        assert!(check_memory_reserve(None, 0).is_ok());
    }

    #[test]
    fn global_process_capacity_includes_critic_reviewers() {
        assert!(process_slots_available(
            Some(4),
            used_process_slots(1, 0, 1),
            2
        ));
        assert!(!process_slots_available(
            Some(4),
            used_process_slots(1, 1, 1),
            2
        ));
        assert!(process_slots_available(
            Some(1),
            used_process_slots(0, 0, 0),
            1
        ));
        assert!(!process_slots_available(
            Some(1),
            used_process_slots(1, 0, 0),
            1
        ));
        assert!(process_slots_available(None, u32::MAX, u32::MAX));
        assert_eq!(used_process_slots(u32::MAX, 1, 1), u32::MAX);
    }

    #[test]
    fn memory_admission_does_not_spawn_below_configured_reserve() {
        use std::cell::Cell;

        let spawn_called = Cell::new(false);
        let mut policy = GlobalAgentResourcePolicy::default();
        policy.min_available_memory_bytes = Some(8_000);

        let result = with_memory_admission(
            &Mutex::new(()),
            || Ok(policy),
            || 7_999,
            || {
                spawn_called.set(true);
                Ok(())
            },
        );

        assert!(result.unwrap_err().contains("below the configured"));
        assert!(!spawn_called.get());
    }

    #[test]
    fn memory_admission_spawns_at_the_configured_reserve() {
        use std::cell::Cell;

        let admission_lock = Mutex::new(());
        let spawn_called = Cell::new(false);
        let mut policy = GlobalAgentResourcePolicy::default();
        policy.min_available_memory_bytes = Some(8_000);

        let result = with_memory_admission(
            &admission_lock,
            || Ok(policy),
            || 8_000,
            || {
                assert!(matches!(
                    admission_lock.try_lock(),
                    Err(std::sync::TryLockError::WouldBlock)
                ));
                spawn_called.set(true);
                Ok("spawned")
            },
        );

        assert_eq!(result.unwrap(), "spawned");
        assert!(spawn_called.get());
    }

    #[test]
    fn device_memory_snapshot_reports_a_nonzero_capacity() {
        let snapshot = device_memory_snapshot();
        assert!(snapshot.total_memory_bytes > 0);
        assert!(snapshot.available_memory_bytes <= snapshot.total_memory_bytes);
    }
}
