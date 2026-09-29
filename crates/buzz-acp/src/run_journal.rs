//! Best-effort, ordered local writes for managed ACP turn history.

use std::{
    env,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use buzz_run_journal::{
    JournalCommand, RouteDecisionRecord, RouteThroughputSample, RunJournal, StartRecord,
    SteerOutcome,
};

static JOURNAL: OnceLock<Mutex<JournalState>> = OnceLock::new();

#[derive(Default)]
struct JournalState {
    journal: Option<RunJournal>,
    pending_failed_events: u64,
}

fn send(command: JournalCommand) {
    if env::var_os("BUZZ_NEST_DIR").is_none()
        || env::var_os("BUZZ_RELAY_URL").is_none()
        || env::var_os("BUZZ_PRIVATE_KEY").is_none()
    {
        tracing::debug!("managed run journal scope is unavailable; journal disabled");
        return;
    }

    let started = Instant::now();
    let journal_state = JOURNAL.get_or_init(|| Mutex::new(JournalState::default()));
    let mut state = journal_state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Err(error) = commit_command(&mut state, command, RunJournal::open_default_scoped) {
        tracing::warn!(%error, "local run journal event was not committed");
    } else if started.elapsed() >= Duration::from_millis(100) {
        tracing::warn!(
            commit_wait_ms = started.elapsed().as_millis(),
            "local run journal commit was slow"
        );
    }
}

fn commit_command(
    state: &mut JournalState,
    command: JournalCommand,
    open: impl FnOnce() -> Result<RunJournal, String>,
) -> Result<(), String> {
    if state.journal.is_none() {
        match open() {
            Ok(opened) => state.journal = Some(opened),
            Err(error) => {
                state.pending_failed_events = state.pending_failed_events.saturating_add(1);
                return Err(error);
            }
        }
    }

    if state.pending_failed_events > 0 {
        let pending = state.pending_failed_events;
        let gap_result = state
            .journal
            .as_ref()
            .expect("journal was initialized above")
            .record_capture_gap(pending);
        if let Err(error) = gap_result {
            state.journal = None;
            // The current command has not been attempted because the earlier
            // gap must be committed first.
            state.pending_failed_events = pending.saturating_add(1);
            return Err(error);
        }
        state.pending_failed_events = 0;
    }

    let result = state
        .journal
        .as_ref()
        .expect("journal was initialized above")
        .apply(command);
    if let Err(error) = result {
        state.journal = None;
        state.pending_failed_events = state.pending_failed_events.saturating_add(1);
        return Err(error);
    }
    Ok(())
}

pub(crate) fn record_started(mut record: StartRecord) {
    if record.managed_worker_generation_nonce.is_none() {
        record.managed_worker_generation_nonce = env::var("BUZZ_MANAGED_AGENT_START_NONCE")
            .ok()
            .filter(|nonce| {
                uuid::Uuid::parse_str(nonce)
                    .is_ok_and(|parsed| parsed.simple().to_string() == *nonce)
            });
    }
    send(JournalCommand::Started(record));
}

pub(crate) fn record_project_linked(turn_id: &str, project_coordinate: &str) {
    send(JournalCommand::ProjectLinked {
        turn_id: turn_id.to_owned(),
        project_coordinate: project_coordinate.to_owned(),
    });
}

pub(crate) fn record_session_resolved(turn_id: &str, session_id: &str) {
    send(JournalCommand::SessionResolved {
        turn_id: turn_id.to_owned(),
        session_id: session_id.to_owned(),
    });
}

pub(crate) fn record_route_decision(turn_id: &str, decision: RouteDecisionRecord) {
    send(JournalCommand::RouteDecision {
        turn_id: turn_id.to_owned(),
        decision,
    });
}

pub(crate) fn record_route_throughput_sample(turn_id: &str, sample: RouteThroughputSample) {
    send(JournalCommand::RouteThroughputSample {
        turn_id: turn_id.to_owned(),
        sample,
    });
}

pub(crate) fn record_prompt_call_started(turn_id: &str) {
    send(JournalCommand::PromptCallStarted {
        turn_id: turn_id.to_owned(),
    });
}

pub(crate) fn record_steer_submitted(turn_id: &str, source_event_id: &str) {
    send(JournalCommand::SteerSubmitted {
        turn_id: turn_id.to_owned(),
        source_event_id: source_event_id.to_owned(),
    });
}

pub(crate) fn record_steer_outcome(turn_id: &str, source_event_id: &str, outcome: SteerOutcome) {
    send(JournalCommand::SteerOutcome {
        turn_id: turn_id.to_owned(),
        source_event_id: source_event_id.to_owned(),
        outcome,
    });
}

pub(crate) fn record_returned(turn_id: &str, outcome: &'static str) {
    send(JournalCommand::Returned {
        turn_id: turn_id.to_owned(),
        outcome: outcome.to_owned(),
    });
}

pub(crate) fn record_worker_crashed(turn_id: &str) {
    send(JournalCommand::WorkerCrashed {
        turn_id: turn_id.to_owned(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovered_write_failure_is_persisted_as_a_capture_gap() {
        let nest = tempfile::tempdir().unwrap();
        let open = || RunJournal::open_scoped(nest.path(), "wss://relay.example", &"c".repeat(64));
        let mut state = JournalState {
            journal: Some(open().unwrap()),
            pending_failed_events: 0,
        };
        let turn_id = "123e4567-e89b-12d3-a456-426614174000";
        let record = |session_scope: &str| StartRecord {
            turn_id: turn_id.into(),
            managed_worker_generation_nonce: None,
            adapter_child_generation_id: None,
            channel_id: Some("123e4567-e89b-12d3-a456-426614174001".into()),
            session_scope: session_scope.into(),
            thread_root_event_id: Some("a".repeat(64)),
            batch_trigger_event_ids: vec!["b".repeat(64)],
            merged_cancelled_event_ids: Vec::new(),
            agent_index: 0,
            configured_worker_pool_slots: 1,
            idle_timeout_secs: 60,
            max_turn_duration_secs: 120,
            agent_profile: buzz_run_journal::AgentProfileSnapshot {
                harness_id: "goose".into(),
                provider_id: None,
                model_id: None,
                agent_prompt_sha256: None,
                execution_profile_id: None,
                execution_profile_version: None,
                prompt_profile_id: None,
                prompt_profile_version: None,
                prompt_profile_hash: None,
                route_profile_id: None,
                route_profile_version: None,
                route_profile_hash: None,
            },
        };

        assert!(commit_command(
            &mut state,
            JournalCommand::Started(record("invalid_scope")),
            open,
        )
        .is_err());
        assert_eq!(state.pending_failed_events, 1);

        commit_command(&mut state, JournalCommand::Started(record("thread")), open).unwrap();
        let journal = state.journal.as_ref().unwrap();
        assert!(journal.get(turn_id).unwrap().is_some());
        assert_eq!(journal.capture_gap_count().unwrap(), 1);
    }
}
