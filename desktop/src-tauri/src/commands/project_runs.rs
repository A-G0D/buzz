use nostr::Event;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use tauri::State;

use crate::{
    app_state::AppState,
    commands::messages::{event_has_channel, root_thread_id},
    relay::query_relay,
};

const PROJECT_KIND: u32 = 30621;
const REPOSITORY_KIND: u32 = 30617;
const DELETION_KIND: u32 = 5;
const PROJECT_EVENT_LIMIT: usize = 100;
const PROJECT_RUN_PAGE_MAX: u32 = 100;

#[derive(Serialize)]
pub struct ProjectCoordinatorRunsResponse {
    project_coordinate: String,
    runs: Vec<buzz_run_journal::ProjectCoordinatorRunEvidence>,
    attempt_evidence_may_be_truncated: bool,
    has_more_candidates: bool,
    next_cursor: Option<buzz_run_journal::CoordinatorRunCursor>,
}

/// Read coordinator-run evidence for a current project home.
///
/// The caller's coordinate is only a selector. This command re-resolves the
/// authoritative project home on the active relay, then reads every candidate
/// source event in the home channel before returning its local journal data.
#[tauri::command]
pub async fn get_project_coordinator_runs(
    project_coordinate: String,
    home_channel_id: String,
    limit: Option<u32>,
    cursor: Option<buzz_run_journal::CoordinatorRunCursor>,
    state: State<'_, AppState>,
) -> Result<ProjectCoordinatorRunsResponse, String> {
    uuid::Uuid::parse_str(&home_channel_id).map_err(|_| "invalid project home channel ID")?;
    let (owner, slug) = parse_project_coordinate(&project_coordinate)?;

    let project_events = query_relay(
        &state,
        &[serde_json::json!({
            "kinds": [PROJECT_KIND],
            "authors": [owner],
            "#d": [slug],
            "limit": PROJECT_EVENT_LIMIT,
        })],
    )
    .await?;
    if project_events.len() >= PROJECT_EVENT_LIMIT {
        return Err("project home lookup reached its safety bound; refresh and retry".into());
    }

    let referenced_repositories = project_events
        .iter()
        .flat_map(event_address_tags)
        .filter_map(parse_repository_coordinate)
        .collect::<std::collections::BTreeSet<_>>();
    if referenced_repositories.len() > PROJECT_EVENT_LIMIT {
        return Err("project home has too many member repositories to verify safely".into());
    }
    let repository_filters = referenced_repositories
        .iter()
        .map(|(repo_owner, repo_id)| {
            serde_json::json!({
                "kinds": [REPOSITORY_KIND],
                "authors": [repo_owner],
                "#d": [repo_id],
                "limit": 1,
            })
        })
        .collect::<Vec<_>>();
    let repository_events = if repository_filters.is_empty() {
        Vec::new()
    } else {
        query_relay(&state, &repository_filters).await?
    };

    let coordinates = std::iter::once(project_coordinate.clone())
        .chain(
            referenced_repositories
                .iter()
                .map(|(owner, id)| format!("{REPOSITORY_KIND}:{owner}:{id}")),
        )
        .collect::<Vec<_>>();
    let deletion_events = query_relay(
        &state,
        &[serde_json::json!({
            "kinds": [DELETION_KIND],
            "#a": coordinates,
            "limit": PROJECT_EVENT_LIMIT,
        })],
    )
    .await?;
    if deletion_events.len() >= PROJECT_EVENT_LIMIT {
        return Err("project deletion lookup reached its safety bound; refresh and retry".into());
    }
    let project_values = to_json_events(&project_events)?;
    let repository_values = to_json_events(&repository_events)?;
    let deletion_values = to_json_events(&deletion_events)?;
    let home = buzz_core_pkg::project_home::pick_current_authoritative_project_home(
        &project_values,
        &repository_values,
        &deletion_values,
        &home_channel_id,
    )
    .filter(|home| home.coordinate == project_coordinate)
    .ok_or_else(|| {
        "selected project is not the current authoritative home for this channel".to_string()
    })?;

    let viewer = state
        .keys
        .lock()
        .map_err(|error| format!("lock workspace identity: {error}"))?
        .public_key()
        .to_hex();
    if project_events.iter().any(|event| {
        event_has_tag(event, "buzz-visibility", "unlisted") && event.pubkey.to_hex() != viewer
    }) {
        return Err("selected project is not listed for this workspace identity".into());
    }
    if home.default_repo_owner.is_none() || home.default_repo_id.is_none() {
        return Err("project home has no authoritative member repository".into());
    }

    let relay_url = crate::relay::relay_ws_url_with_override(&state);
    let nest_dir = crate::managed_agents::nest_dir().ok_or("cannot resolve Buzz workspace")?;
    let journal = buzz_run_journal::RunJournal::open_scoped(nest_dir, &relay_url, &viewer)?;
    let candidates = journal.project_coordinator_runs(
        &project_coordinate,
        &home_channel_id,
        limit.unwrap_or(20).min(PROJECT_RUN_PAGE_MAX) as usize,
        cursor.as_ref(),
    )?;
    let attempt_targets = buzz_run_journal::project_run_attempt_source_targets(&candidates.runs);
    let readable_attempt_sources =
        read_candidate_attempt_sources(&state, &attempt_targets.targets).await?;
    let readable_events =
        read_candidate_sources(&state, &home_channel_id, &candidates.runs).await?;
    let mut remaining_attempt_limit = buzz_run_journal::PROJECT_RUN_ATTEMPT_EVIDENCE_LIMIT;
    let mut attempt_evidence_may_be_truncated = attempt_targets.may_be_truncated;
    let runs = candidates
        .runs
        .iter()
        .filter(|run| has_readable_canonical_source(run, &readable_events, &home_channel_id))
        .map(|run| {
            let evidence = buzz_run_journal::project_run_evidence(
                run,
                &readable_attempt_sources,
                remaining_attempt_limit,
            );
            remaining_attempt_limit =
                remaining_attempt_limit.saturating_sub(evidence.attempt_turns.len());
            attempt_evidence_may_be_truncated |= evidence.attempt_evidence_may_be_truncated;
            evidence
        })
        .collect();

    Ok(ProjectCoordinatorRunsResponse {
        project_coordinate,
        runs,
        attempt_evidence_may_be_truncated,
        has_more_candidates: candidates.has_more_candidates,
        next_cursor: candidates.next_cursor,
    })
}

async fn read_candidate_sources(
    state: &AppState,
    channel_id: &str,
    runs: &[buzz_run_journal::CoordinatorRunSummary],
) -> Result<Vec<Event>, String> {
    let ids = runs
        .iter()
        .flat_map(|run| {
            run.thread_root_event_id
                .iter()
                .chain(std::iter::once(&run.original_intent_event_id))
        })
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut events = Vec::new();
    for chunk in ids.into_iter().collect::<Vec<_>>().chunks(200) {
        events.extend(
            query_relay(
                state,
                &[serde_json::json!({
                    "ids": chunk,
                    "#h": [channel_id],
                    "kinds": buzz_core_pkg::thread_brief::THREAD_BRIEF_KINDS,
                    "limit": chunk.len(),
                })],
            )
            .await?,
        );
    }
    Ok(events)
}

async fn read_candidate_attempt_sources(
    state: &AppState,
    targets: &BTreeSet<(String, String)>,
) -> Result<BTreeSet<(String, String)>, String> {
    let mut by_channel = BTreeMap::<String, Vec<String>>::new();
    for (channel_id, event_id) in targets {
        by_channel
            .entry(channel_id.clone())
            .or_default()
            .push(event_id.clone());
    }
    let mut readable = BTreeSet::new();
    for (channel_id, ids) in by_channel {
        for chunk in ids.chunks(200) {
            let expected = chunk.iter().cloned().collect::<HashSet<_>>();
            let events = query_relay(
                state,
                &[serde_json::json!({
                    "ids": chunk,
                    "#h": [channel_id],
                    "kinds": buzz_core_pkg::thread_brief::THREAD_BRIEF_KINDS,
                    "limit": chunk.len(),
                })],
            )
            .await?;
            if events.len() > chunk.len() {
                return Err("project attempt source lookup exceeded its ID bound".into());
            }
            for event in events {
                if readable_attempt_source(&event, &channel_id, &expected) {
                    readable.insert((channel_id.clone(), event.id.to_hex()));
                }
            }
        }
    }
    Ok(readable)
}

fn readable_attempt_source(
    event: &Event,
    channel_id: &str,
    expected_ids: &HashSet<String>,
) -> bool {
    expected_ids.contains(&event.id.to_hex()) && readable_project_message_event(event, channel_id)
}

fn readable_project_message_event(event: &Event, channel_id: &str) -> bool {
    buzz_core_pkg::thread_brief::THREAD_BRIEF_KINDS.contains(&(event.kind.as_u16() as u32))
        && event_has_channel(event, channel_id)
}

fn has_readable_canonical_source(
    run: &buzz_run_journal::CoordinatorRunSummary,
    events: &[Event],
    channel_id: &str,
) -> bool {
    let Some(intent) = events.iter().find(|event| {
        event.id.to_hex() == run.original_intent_event_id
            && readable_project_message_event(event, channel_id)
    }) else {
        return false;
    };
    match run.thread_root_event_id.as_deref() {
        Some(root_id) => events.iter().any(|root| {
            root.id.to_hex() == root_id
                && readable_project_message_event(root, channel_id)
                && root_thread_id(root) == root_id
                && root_thread_id(intent) == root_id
        }),
        None => root_thread_id(intent) == intent.id.to_hex(),
    }
}

fn parse_project_coordinate(coordinate: &str) -> Result<(String, String), String> {
    let mut parts = coordinate.splitn(3, ':');
    let kind = parts.next();
    let owner = parts.next().unwrap_or_default().to_ascii_lowercase();
    let slug = parts.next().unwrap_or_default();
    if kind != Some("30621")
        || owner.len() != 64
        || !owner.bytes().all(|byte| byte.is_ascii_hexdigit())
        || slug.is_empty()
        || slug.len() > 1024
        || slug.chars().any(char::is_control)
        || coordinate.len() > 1100
    {
        return Err("invalid NIP-MP project coordinate".into());
    }
    Ok((owner, slug.to_string()))
}

fn parse_repository_coordinate(coordinate: &str) -> Option<(String, String)> {
    let mut parts = coordinate.splitn(3, ':');
    if parts.next()? != "30617" {
        return None;
    }
    let owner = parts.next()?.to_ascii_lowercase();
    let id = parts.next()?;
    if owner.len() != 64
        || !owner.bytes().all(|byte| byte.is_ascii_hexdigit())
        || id.is_empty()
        || id.len() > 1024
        || id.chars().any(char::is_control)
    {
        return None;
    }
    Some((owner, id.to_string()))
}

fn event_address_tags(event: &Event) -> impl Iterator<Item = &str> {
    event.tags.iter().filter_map(|tag| {
        let parts = tag.as_slice();
        (parts.first().map(String::as_str) == Some("a"))
            .then(|| parts.get(1).map(String::as_str))
            .flatten()
    })
}

fn event_has_tag(event: &Event, name: &str, value: &str) -> bool {
    event.tags.iter().any(|tag| {
        let parts = tag.as_slice();
        parts.first().map(String::as_str) == Some(name)
            && parts.get(1).map(String::as_str) == Some(value)
    })
}

fn to_json_events(events: &[Event]) -> Result<Vec<serde_json::Value>, String> {
    events
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("serialize project metadata: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::{EventBuilder, Keys, Kind, Tag};

    fn signed_event(kind: u16, tags: Vec<Vec<&str>>, created_at: u64) -> Event {
        EventBuilder::new(Kind::from_u16(kind), "test")
            .tags(
                tags.into_iter()
                    .map(|tag| Tag::parse(tag).unwrap())
                    .collect::<Vec<_>>(),
            )
            .custom_created_at(nostr::Timestamp::from(created_at))
            .sign_with_keys(&Keys::generate())
            .unwrap()
    }

    #[test]
    fn project_coordinate_parser_rejects_invalid_values() {
        assert!(parse_project_coordinate("30617:not-a-project").is_err());
        assert!(parse_project_coordinate(&format!("30621:{}:x", "g".repeat(64))).is_err());
        assert!(parse_project_coordinate(&format!("30621:{}:x\n", "a".repeat(64))).is_err());
        assert_eq!(
            parse_project_coordinate(&format!("30621:{}:project:slug", "A".repeat(64))).unwrap(),
            ("a".repeat(64), "project:slug".into())
        );
    }

    #[test]
    fn project_run_page_serializes_attempt_limit_disclosure() {
        let response = ProjectCoordinatorRunsResponse {
            project_coordinate: "30621:owner:project".into(),
            runs: Vec::new(),
            attempt_evidence_may_be_truncated: true,
            has_more_candidates: false,
            next_cursor: None,
        };
        let serialized = serde_json::to_value(response).unwrap();
        assert_eq!(serialized["attempt_evidence_may_be_truncated"], true);
    }

    #[test]
    fn local_run_evidence_requires_readable_canonical_root_and_source() {
        let channel = "123e4567-e89b-12d3-a456-426614174001";
        let root = signed_event(9, vec![vec!["h", channel]], 100);
        let root_id = root.id.to_hex();
        let intent = signed_event(
            9,
            vec![vec!["h", channel], vec!["e", root_id.as_str(), "", "reply"]],
            101,
        );
        let run = buzz_run_journal::CoordinatorRunSummary {
            run_id: "run".into(),
            channel_id: channel.into(),
            session_scope: "thread".into(),
            thread_root_event_id: Some(root_id.clone()),
            original_intent_event_id: intent.id.to_hex(),
            project_coordinate: None,
            project_link_conflict: false,
            attempt_turns: vec![buzz_run_journal::TurnSummary {
                turn_id: "223e4567-e89b-12d3-a456-426614174000".into(),
                channel_id: Some(channel.into()),
                session_scope: "thread".into(),
                thread_root_event_id: Some(root_id.clone()),
                batch_trigger_event_ids: vec![intent.id.to_hex()],
                merged_cancelled_event_ids: Vec::new(),
                agent_index: 0,
                acp_session_id: Some("captured-session".into()),
                status: "returned".into(),
                liveness: "unknown".into(),
                task_state: "unknown".into(),
                started_at_ms: 100,
                updated_at_ms: 101,
            }],
            attempt_history_may_be_truncated: false,
            recent_events: Vec::new(),
            event_history_may_be_truncated: false,
            created_at_ms: 0,
            updated_at_ms: 0,
            task_state: "unknown".into(),
        };

        assert!(has_readable_canonical_source(
            &run,
            &[root.clone(), intent.clone()],
            channel
        ));
        assert!(!has_readable_canonical_source(&run, &[intent], channel));
        assert!(!has_readable_canonical_source(
            &run,
            &[root.clone()],
            "123e4567-e89b-12d3-a456-426614174099"
        ));

        let wrong_kind_intent = signed_event(
            5,
            vec![vec!["h", channel], vec!["e", root_id.as_str(), "", "reply"]],
            102,
        );
        let mut wrong_intent_candidate = run.clone();
        wrong_intent_candidate.original_intent_event_id = wrong_kind_intent.id.to_hex();
        wrong_intent_candidate.attempt_turns[0].batch_trigger_event_ids =
            vec![wrong_kind_intent.id.to_hex()];
        let candidate_attempts = [wrong_intent_candidate]
            .into_iter()
            .filter(|candidate| {
                has_readable_canonical_source(
                    candidate,
                    &[root.clone(), wrong_kind_intent.clone()],
                    channel,
                )
            })
            .map(|candidate| {
                buzz_run_journal::project_run_evidence(
                    &candidate,
                    &BTreeSet::from([
                        (channel.into(), root_id.clone()),
                        (channel.into(), wrong_kind_intent.id.to_hex()),
                    ]),
                    10,
                )
                .attempt_turns
                .len()
            })
            .sum::<usize>();
        assert_eq!(
            candidate_attempts, 0,
            "wrong-kind intent must withhold attempt"
        );

        let wrong_kind_root = signed_event(5, vec![vec!["h", channel]], 103);
        let wrong_root_id = wrong_kind_root.id.to_hex();
        let intent_on_wrong_kind_root = signed_event(
            9,
            vec![
                vec!["h", channel],
                vec!["e", wrong_root_id.as_str(), "", "reply"],
            ],
            104,
        );
        let mut wrong_root_candidate = run;
        wrong_root_candidate.thread_root_event_id = Some(wrong_root_id.clone());
        wrong_root_candidate.original_intent_event_id = intent_on_wrong_kind_root.id.to_hex();
        wrong_root_candidate.attempt_turns[0].thread_root_event_id = Some(wrong_root_id.clone());
        wrong_root_candidate.attempt_turns[0].batch_trigger_event_ids =
            vec![intent_on_wrong_kind_root.id.to_hex()];
        let candidate_attempts = [wrong_root_candidate]
            .into_iter()
            .filter(|candidate| {
                has_readable_canonical_source(
                    candidate,
                    &[wrong_kind_root.clone(), intent_on_wrong_kind_root.clone()],
                    channel,
                )
            })
            .map(|candidate| {
                buzz_run_journal::project_run_evidence(
                    &candidate,
                    &BTreeSet::from([
                        (channel.into(), wrong_root_id.clone()),
                        (channel.into(), intent_on_wrong_kind_root.id.to_hex()),
                    ]),
                    10,
                )
                .attempt_turns
                .len()
            })
            .sum::<usize>();
        assert_eq!(
            candidate_attempts, 0,
            "wrong-kind root must withhold attempt"
        );
    }

    #[test]
    fn project_attempt_source_requires_exact_id_channel_and_allowed_kind() {
        let channel = "123e4567-e89b-12d3-a456-426614174001";
        let event = signed_event(9, vec![vec!["h", channel]], 100);
        let expected = HashSet::from([event.id.to_hex()]);
        assert!(readable_attempt_source(&event, channel, &expected));
        assert!(!readable_attempt_source(
            &event,
            "123e4567-e89b-12d3-a456-426614174099",
            &expected,
        ));
        assert!(!readable_attempt_source(
            &event,
            channel,
            &HashSet::from(["a".repeat(64)]),
        ));
        let wrong_kind = signed_event(5, vec![vec!["h", channel]], 101);
        assert!(!readable_attempt_source(
            &wrong_kind,
            channel,
            &HashSet::from([wrong_kind.id.to_hex()]),
        ));
    }
}
