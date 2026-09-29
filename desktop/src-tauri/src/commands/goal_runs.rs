//! Local goal-run commands.
//!
//! These commands persist a source-linked, bounded task graph after the goal
//! message has been accepted by the relay. They do not claim a worker was
//! launched or that a task is complete.

use tauri::{AppHandle, State};

use crate::{app_state::AppState, managed_agents::nest_dir};

fn open_goal_journal(state: &AppState) -> Result<buzz_run_journal::RunJournal, String> {
    let viewer = state
        .keys
        .lock()
        .map_err(|error| format!("lock workspace identity: {error}"))?
        .public_key()
        .to_hex();
    let relay_url = crate::relay::relay_ws_url_with_override(state);
    let workspace = nest_dir().ok_or("cannot resolve Buzz workspace")?;
    buzz_run_journal::RunJournal::open_scoped(workspace, &relay_url, &viewer)
}

/// Persist a new or retried local goal plan for one relay-accepted message.
#[tauri::command]
pub fn create_goal_run_from_message(
    input: buzz_run_journal::GoalRunSpec,
    state: State<'_, AppState>,
) -> Result<buzz_run_journal::GoalRunRecord, String> {
    open_goal_journal(&state)?.create_goal_run(input)
}

/// Read local goal runs for the current owner/relay and one channel.
#[tauri::command]
pub fn list_goal_runs_for_channel(
    channel_id: String,
    limit: Option<u32>,
    state: State<'_, AppState>,
) -> Result<Vec<buzz_run_journal::GoalRunRecord>, String> {
    open_goal_journal(&state)?.recent_goal_runs(&channel_id, limit.unwrap_or(20).min(50) as usize)
}

/// Read one local goal run by stable ID in the current owner/relay scope.
#[tauri::command]
pub fn get_goal_run(
    goal_run_id: String,
    state: State<'_, AppState>,
) -> Result<Option<buzz_run_journal::GoalRunRecord>, String> {
    open_goal_journal(&state)?.goal_run(&goal_run_id)
}

/// Apply a bounded task graph from the exact worker assigned to the root
/// planning task. This does not accept the planner task or start its children.
#[tauri::command]
pub fn append_goal_plan(
    goal_run_id: String,
    planner_task_id: String,
    generation: u32,
    reporter_pubkey: String,
    tasks: Vec<buzz_run_journal::GoalTaskSpec>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<buzz_run_journal::GoalRunRecord, String> {
    let active = crate::managed_agents::active_managed_agent_pubkeys(&app)?;
    if !active
        .iter()
        .any(|pubkey| pubkey.eq_ignore_ascii_case(reporter_pubkey.trim()))
    {
        return Err("goal plan sender is not an active managed agent".into());
    }
    open_goal_journal(&state)?.append_goal_plan(
        &goal_run_id,
        &planner_task_id,
        generation,
        &reporter_pubkey,
        &tasks,
    )
}

/// Mark a task running only after its task-scoped relay message was accepted.
/// Worker output still cannot complete the task without separate evidence.
#[tauri::command]
pub fn start_goal_task(
    goal_run_id: String,
    task_id: String,
    generation: u32,
    state: State<'_, AppState>,
) -> Result<buzz_run_journal::GoalRunRecord, String> {
    open_goal_journal(&state)?.transition_goal_task(
        &goal_run_id,
        &task_id,
        generation,
        buzz_run_journal::GoalTaskState::Running,
    )
}

/// Bind the relay-accepted assignment message before the worker can consume it.
#[tauri::command]
pub fn bind_goal_task_assignment(
    goal_run_id: String,
    task_id: String,
    generation: u32,
    source_event_id: String,
    assigned_agent_pubkey: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    open_goal_journal(&state)?.bind_goal_task_assignment_source(
        &goal_run_id,
        &task_id,
        generation,
        &source_event_id,
        &assigned_agent_pubkey,
    )
}

/// Ingest a bounded task report only from its persisted assigned agent.
#[tauri::command]
pub fn ingest_goal_task_report(
    goal_run_id: String,
    task_id: String,
    generation: u32,
    reporter_pubkey: String,
    report_event_id: String,
    state: buzz_run_journal::GoalTaskState,
    evidence: Vec<String>,
    app: AppHandle,
    app_state: State<'_, AppState>,
) -> Result<buzz_run_journal::GoalRunRecord, String> {
    let active = crate::managed_agents::active_managed_agent_pubkeys(&app)?;
    if !active
        .iter()
        .any(|pubkey| pubkey.eq_ignore_ascii_case(reporter_pubkey.trim()))
    {
        return Err("goal report sender is not an active managed agent".into());
    }
    open_goal_journal(&app_state)?.ingest_goal_task_report(
        &goal_run_id,
        &task_id,
        generation,
        &reporter_pubkey,
        &report_event_id,
        state,
        &evidence,
    )
}

/// Accept a task only after its report/evidence is visible for review.
#[tauri::command]
pub fn accept_goal_task(
    goal_run_id: String,
    task_id: String,
    generation: u32,
    state: State<'_, AppState>,
) -> Result<buzz_run_journal::GoalRunRecord, String> {
    open_goal_journal(&state)?.transition_goal_task(
        &goal_run_id,
        &task_id,
        generation,
        buzz_run_journal::GoalTaskState::Accepted,
    )
}

/// Select an active local managed agent that is still a member of the target
/// channel. Explicitly mentioned agents preserve their mention order; a goal
/// with no agent mention falls back to the first verified active member.
///
/// Directory lookup is deliberately bounded to local active candidates. It
/// verifies live channel membership before the coordinator can be mentioned,
/// and it never reads agent key material or starts a process.
async fn eligible_goal_coordinators(
    candidate_pubkeys: Vec<String>,
    channel_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
    limit: u8,
) -> Result<Vec<String>, String> {
    if candidate_pubkeys.len() > 128 {
        return Err("too many coordinator candidates".into());
    }
    if !(1..=3).contains(&limit) {
        return Err("goal coordinator limit must be between 1 and 3".into());
    }
    let channel_id = channel_id.trim();
    if channel_id.is_empty() || channel_id.len() > 512 {
        return Err("goal coordinator needs a valid channel ID".into());
    }
    let active = crate::managed_agents::active_managed_agent_pubkeys(&app)?;
    let candidates = if candidate_pubkeys.is_empty() {
        active.clone()
    } else {
        candidate_pubkeys
            .into_iter()
            .map(|candidate| candidate.trim().to_ascii_lowercase())
            .filter(|candidate| active.iter().any(|pubkey| pubkey == candidate))
            .collect()
    };
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let verified = crate::commands::revalidate_relay_agents(
        candidates.clone(),
        Some(channel_id.to_string()),
        state,
    )
    .await?;
    let mut selected = Vec::new();
    for candidate in candidates {
        if verified.iter().any(|agent| {
            agent.pubkey.eq_ignore_ascii_case(&candidate)
                && agent.channel_ids.iter().any(|id| id == channel_id)
        }) {
            selected.push(candidate);
            if selected.len() == limit as usize {
                break;
            }
        }
    }
    Ok(selected)
}

#[tauri::command]
pub async fn select_goal_coordinator(
    candidate_pubkeys: Vec<String>,
    channel_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<String>, String> {
    Ok(eligible_goal_coordinators(candidate_pubkeys, channel_id, app, state, 1)
        .await?
        .into_iter()
        .next())
}

/// Return up to three verified active managed agents in current channel order.
/// The controller uses this only for durable task assignment, never as proof
/// that a task is suitable or complete.
#[tauri::command]
pub async fn select_goal_coordinators(
    candidate_pubkeys: Vec<String>,
    channel_id: String,
    limit: Option<u8>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    eligible_goal_coordinators(
        candidate_pubkeys,
        channel_id,
        app,
        state,
        limit.unwrap_or(3),
    )
    .await
}
