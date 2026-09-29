use crate::{client::BuzzClient, commands::messages::load_thread_events, error::CliError, RunsCmd};
use buzz_run_journal::{
    nest_dir_from_env_or_default, project_run_attempt_source_targets, project_run_evidence,
    CoordinatorRunCursor, CoordinatorRunSummary, ProjectAttemptEvidence,
    ProjectCoordinatorRunEvidence, RunJournal, TurnSummary, PROJECT_RUN_ATTEMPT_EVIDENCE_LIMIT,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};

const PROJECT_EVENT_LIMIT: usize = 100;
const PROJECT_REPOSITORY_LIMIT: usize = 100;
const PROJECT_RUN_PAGE_MAX: u32 = 20;
const PROJECT_QUERY_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const PROJECT_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Serialize)]
struct ProjectRunItem {
    run_id: String,
    project_coordinate: String,
    channel_id: String,
    session_scope: String,
    thread_root_event_id: Option<String>,
    original_intent_event_id: String,
    project_link_conflict: bool,
    attempt_turns: Vec<ProjectAttemptEvidence>,
    attempt_history_may_be_truncated: bool,
    event_history_may_be_truncated: bool,
    created_at_ms: i64,
    updated_at_ms: i64,
    task_state: &'static str,
    liveness: &'static str,
    history_reliability: &'static str,
    history_completeness: &'static str,
}

pub(crate) async fn dispatch(command: RunsCmd, client: &BuzzClient) -> Result<(), CliError> {
    let command = match command {
        RunsCmd::ProjectList {
            project_coordinate,
            home_channel,
            limit,
            cursor_updated_at_ms,
            cursor_run_id,
        } => {
            let output = project_list(
                client,
                &project_coordinate,
                &home_channel,
                limit,
                cursor_updated_at_ms,
                cursor_run_id.as_deref(),
            )
            .await?;
            let encoded = encode_project_output(&output)?;
            println!(
                "{}",
                String::from_utf8(encoded).map_err(|error| CliError::Other(error.to_string()))?
            );
            return Ok(());
        }
        command => command,
    };
    let (channel_id, thread_root_id) = match &command {
        RunsCmd::List {
            channel,
            thread_root,
            ..
        }
        | RunsCmd::Show {
            channel,
            thread_root,
            ..
        } => (channel.clone(), thread_root.clone()),
        RunsCmd::ProjectList { .. } => unreachable!("handled above"),
    };
    let channel_id = uuid::Uuid::parse_str(&channel_id)
        .map_err(|_| CliError::Usage("invalid --channel UUID".into()))?
        .to_string();
    let thread_root_id = thread_root_id.to_ascii_lowercase();

    // Prove read access to this exact source thread through the relay before
    // consulting the local journal. Only history linked to returned source
    // events is included in the result.
    let (canonical_root, source_events) = load_thread_events(
        client,
        &channel_id,
        &thread_root_id,
        Some(&thread_root_id),
        Some(500),
        Some(64),
        None,
    )
    .await?;
    let source_event_ids = source_events
        .iter()
        .filter_map(|event| event.get("id").and_then(serde_json::Value::as_str))
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let source_ids = source_event_ids
        .iter()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    let owner_pubkey = client.keys().public_key().to_hex();
    let journal = RunJournal::open_scoped(
        nest_dir_from_env_or_default().map_err(CliError::Other)?,
        client.relay_url(),
        &owner_pubkey,
    )
    .map_err(CliError::Other)?;

    let output = match command {
        RunsCmd::List { limit, .. } => {
            let turns = journal
                .list_recent_for_thread(&channel_id, &canonical_root, &source_event_ids, limit)
                .map_err(CliError::Other)?;
            let coordinator_runs = journal
                .thread_coordinator_runs(&channel_id, &canonical_root, &source_event_ids)
                .map_err(CliError::Other)?;
            serde_json::json!({
                "channel_id": channel_id,
                "thread_root_event_id": canonical_root,
                "managed_turns": turns,
                "coordinator_runs": coordinator_runs.runs,
                "has_more_coordinator_runs": coordinator_runs.has_more_runs,
                "task_state": "unknown",
                "liveness": "unknown",
            })
        }
        RunsCmd::Show {
            turn_id: Some(turn_id),
            run_id: None,
            ..
        } => {
            let turn = journal
                .get(&turn_id)
                .map_err(CliError::Other)?
                .filter(|turn| matches_thread(turn, &channel_id, &canonical_root, &source_ids))
                .ok_or_else(|| {
                    CliError::NotFound(format!("no managed turn {turn_id} in this thread"))
                })?;
            serde_json::json!({
                "channel_id": channel_id,
                "thread_root_event_id": canonical_root,
                "turn": turn,
                "events": journal.events(&turn_id, 200).map_err(CliError::Other)?,
                "task_state": "unknown",
                "liveness": "unknown",
            })
        }
        RunsCmd::Show {
            turn_id: None,
            run_id: Some(run_id),
            ..
        } => {
            let run = journal
                .coordinator_run_in_thread(&run_id, &channel_id, &canonical_root, &source_event_ids)
                .map_err(CliError::Other)?
                .ok_or_else(|| {
                    CliError::NotFound(format!("no coordinator run {run_id} in this thread"))
                })?;
            serde_json::json!({
                "channel_id": channel_id,
                "thread_root_event_id": canonical_root,
                "run": run,
                "task_state": "unknown",
                "liveness": "unknown",
            })
        }
        RunsCmd::Show {
            turn_id: None,
            run_id: None,
            ..
        } => {
            return Err(CliError::Usage(
                "runs show requires either --turn-id or --run-id".into(),
            ));
        }
        RunsCmd::Show {
            turn_id: Some(_),
            run_id: Some(_),
            ..
        } => {
            return Err(CliError::Usage(
                "runs show accepts only one of --turn-id or --run-id".into(),
            ));
        }
        RunsCmd::ProjectList { .. } => unreachable!("handled above"),
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&output)
            .map_err(|error| CliError::Other(error.to_string()))?
    );
    Ok(())
}

async fn project_list(
    client: &BuzzClient,
    project_coordinate: &str,
    home_channel_id: &str,
    limit: u32,
    cursor_updated_at_ms: Option<i64>,
    cursor_run_id: Option<&str>,
) -> Result<Value, CliError> {
    let (owner, slug) = parse_project_coordinate(project_coordinate)?;
    let home_channel_id = uuid::Uuid::parse_str(home_channel_id)
        .map_err(|_| CliError::Usage("invalid --home-channel UUID".into()))?
        .to_string();
    if !(1..=PROJECT_RUN_PAGE_MAX).contains(&limit) {
        return Err(CliError::Usage(format!(
            "--limit must be between 1 and {PROJECT_RUN_PAGE_MAX}"
        )));
    }
    let cursor = parse_project_cursor(cursor_updated_at_ms, cursor_run_id)?;

    // Query the current addressable project head without filtering on its
    // channel. Filtering by the caller's channel could otherwise hide a newer
    // head that moved the project to another home.
    let project_events = query_events(
        client,
        &json!({
            "kinds": [30621],
            "authors": [owner],
            "#d": [slug],
            "limit": PROJECT_EVENT_LIMIT,
        }),
    )
    .await?;
    if project_events.len() >= PROJECT_EVENT_LIMIT {
        return Err(CliError::Other(
            "project home lookup reached its safety bound; refresh and retry".into(),
        ));
    }
    let project_events = project_events
        .into_iter()
        .filter(|event| has_addressable_identity(event, 30621, &owner, &slug))
        .collect::<Vec<_>>();
    let referenced_repositories = project_events
        .iter()
        .flat_map(repository_address_tags)
        .filter_map(parse_repository_coordinate)
        .collect::<BTreeSet<_>>();
    if referenced_repositories.len() > PROJECT_REPOSITORY_LIMIT {
        return Err(CliError::Other(
            "project home has too many member repositories to verify safely".into(),
        ));
    }
    let repository_filters = referenced_repositories
        .iter()
        .map(|(repo_owner, repo_id)| {
            json!({
                "kinds": [30617],
                "authors": [repo_owner],
                "#d": [repo_id],
                "limit": 1,
            })
        })
        .collect::<Vec<_>>();
    let repository_events = if repository_filters.is_empty() {
        Vec::new()
    } else {
        parse_query_events(
            &client
                .query_multi_bounded(&repository_filters, PROJECT_QUERY_RESPONSE_BYTES)
                .await?,
        )?
    };
    if repository_events.len() > repository_filters.len() {
        return Err(CliError::Other(
            "repository-head lookup exceeded its filter bound".into(),
        ));
    }
    let repository_events = repository_events
        .into_iter()
        .filter(|event| {
            referenced_repositories.iter().any(|(repo_owner, repo_id)| {
                has_addressable_identity(event, 30617, repo_owner, repo_id)
            })
        })
        .collect::<Vec<_>>();

    let project_coordinate = format!("30621:{owner}:{slug}");
    let coordinates = std::iter::once(project_coordinate.clone())
        .chain(
            referenced_repositories
                .iter()
                .map(|(repo_owner, repo_id)| format!("30617:{repo_owner}:{repo_id}")),
        )
        .collect::<Vec<_>>();
    let deletion_events = query_events(
        client,
        &json!({
            "kinds": [5],
            "#a": coordinates,
            "limit": PROJECT_EVENT_LIMIT,
        }),
    )
    .await?;
    if deletion_events.len() >= PROJECT_EVENT_LIMIT {
        return Err(CliError::Other(
            "project deletion lookup reached its safety bound; refresh and retry".into(),
        ));
    }
    let project_home = buzz_core::project_home::pick_current_authoritative_project_home(
        &project_events,
        &repository_events,
        &deletion_events,
        &home_channel_id,
    )
    .filter(|home| home.coordinate == project_coordinate)
    .ok_or_else(|| {
        CliError::NotFound(
            "selected project is not the current authoritative listed home for this channel".into(),
        )
    })?;
    if project_home.default_repo_owner.is_none() || project_home.default_repo_id.is_none() {
        return Err(CliError::NotFound(
            "project home has no authoritative member repository".into(),
        ));
    }

    let owner_pubkey = client.keys().public_key().to_hex();
    let journal = RunJournal::open_scoped(
        nest_dir_from_env_or_default().map_err(CliError::Other)?,
        client.relay_url(),
        &owner_pubkey,
    )
    .map_err(CliError::Other)?;
    let candidates = journal
        .project_coordinator_runs(
            &project_coordinate,
            &home_channel_id,
            limit as usize,
            cursor.as_ref(),
        )
        .map_err(CliError::Other)?;

    let source_ids = candidates
        .runs
        .iter()
        .flat_map(|run| {
            run.thread_root_event_id
                .iter()
                .chain(std::iter::once(&run.original_intent_event_id))
        })
        .cloned()
        .collect::<BTreeSet<_>>();
    let source_events = if source_ids.is_empty() {
        Vec::new()
    } else {
        let ids = source_ids.iter().cloned().collect::<Vec<_>>();
        let raw = client
            .query_bounded(
                &json!({
                    "ids": ids,
                    "#h": [home_channel_id],
                    "kinds": buzz_core::thread_brief::THREAD_BRIEF_KINDS,
                    "limit": source_ids.len(),
                }),
                PROJECT_QUERY_RESPONSE_BYTES,
            )
            .await?;
        let events = parse_query_events(&raw)?;
        if events.len() > source_ids.len() {
            return Err(CliError::Other(
                "project run source lookup exceeded its ID bound".into(),
            ));
        }
        events
    };
    let source_by_id = source_events
        .iter()
        .filter_map(|event| {
            event
                .get("id")
                .and_then(Value::as_str)
                .map(|id| (id.to_ascii_lowercase(), event))
        })
        .collect::<HashMap<_, _>>();
    let attempt_targets = project_run_attempt_source_targets(&candidates.runs);
    let readable_attempt_sources =
        read_project_attempt_sources(client, &attempt_targets.targets).await?;
    let mut remaining_attempt_limit = PROJECT_RUN_ATTEMPT_EVIDENCE_LIMIT;
    let mut attempt_evidence_may_be_truncated = attempt_targets.may_be_truncated;
    let runs = candidates
        .runs
        .iter()
        .filter(|run| readable_run_source(run, &source_by_id, &home_channel_id))
        .map(|run| {
            let evidence =
                project_run_evidence(run, &readable_attempt_sources, remaining_attempt_limit);
            remaining_attempt_limit =
                remaining_attempt_limit.saturating_sub(evidence.attempt_turns.len());
            attempt_evidence_may_be_truncated |= evidence.attempt_evidence_may_be_truncated;
            project_run_item(run, evidence, &project_coordinate)
        })
        .collect::<Vec<_>>();
    let next_cursor = serde_json::to_value(&candidates.next_cursor)
        .map_err(|error| CliError::Other(format!("serialize project run cursor: {error}")))?;
    Ok(json!({
        "project_coordinate": project_coordinate,
        "home_channel_id": home_channel_id,
        "runs": runs,
        "attempt_evidence_may_be_truncated": attempt_evidence_may_be_truncated,
        "has_more_candidates": candidates.has_more_candidates,
        "next_cursor": next_cursor,
        "task_state": "unknown",
        "liveness": "unknown",
    }))
}

fn project_run_item(
    run: &CoordinatorRunSummary,
    evidence: ProjectCoordinatorRunEvidence,
    project_coordinate: &str,
) -> ProjectRunItem {
    ProjectRunItem {
        run_id: run.run_id.clone(),
        project_coordinate: project_coordinate.to_string(),
        channel_id: run.channel_id.clone(),
        session_scope: run.session_scope.clone(),
        thread_root_event_id: run.thread_root_event_id.clone(),
        original_intent_event_id: run.original_intent_event_id.clone(),
        project_link_conflict: run.project_link_conflict,
        attempt_turns: evidence.attempt_turns,
        attempt_history_may_be_truncated: run.attempt_history_may_be_truncated,
        event_history_may_be_truncated: run.event_history_may_be_truncated,
        created_at_ms: run.created_at_ms,
        updated_at_ms: run.updated_at_ms,
        task_state: "unknown",
        liveness: "unknown",
        history_reliability: evidence.history_reliability,
        history_completeness: evidence.history_completeness,
    }
}

async fn read_project_attempt_sources(
    client: &BuzzClient,
    targets: &BTreeSet<(String, String)>,
) -> Result<BTreeSet<(String, String)>, CliError> {
    let mut by_channel = std::collections::BTreeMap::<String, Vec<String>>::new();
    for (channel_id, event_id) in targets {
        by_channel
            .entry(channel_id.clone())
            .or_default()
            .push(event_id.clone());
    }
    let mut readable = BTreeSet::new();
    for (channel_id, ids) in by_channel {
        for chunk in ids.chunks(200) {
            let expected = chunk
                .iter()
                .cloned()
                .collect::<std::collections::HashSet<_>>();
            let raw = client
                .query_bounded(
                    &json!({
                        "ids": chunk,
                        "#h": [channel_id],
                        "kinds": buzz_core::thread_brief::THREAD_BRIEF_KINDS,
                        "limit": chunk.len(),
                    }),
                    PROJECT_QUERY_RESPONSE_BYTES,
                )
                .await?;
            let events = parse_query_events(&raw)?;
            if events.len() > chunk.len() {
                return Err(CliError::Other(
                    "project attempt source lookup exceeded its ID bound".into(),
                ));
            }
            for event in events {
                if readable_project_attempt_source(&event, &channel_id, &expected) {
                    let event_id = event["id"].as_str().expect("validated event ID");
                    readable.insert((channel_id.clone(), event_id.to_ascii_lowercase()));
                }
            }
        }
    }
    Ok(readable)
}

fn readable_project_attempt_source(
    event: &Value,
    channel_id: &str,
    expected_ids: &std::collections::HashSet<String>,
) -> bool {
    let Some(event_id) = event.get("id").and_then(Value::as_str) else {
        return false;
    };
    let Some(kind) = event
        .get("kind")
        .and_then(Value::as_u64)
        .and_then(|kind| u32::try_from(kind).ok())
    else {
        return false;
    };
    event_id.len() == 64
        && event_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        && expected_ids.contains(&event_id.to_ascii_lowercase())
        && buzz_core::thread_brief::THREAD_BRIEF_KINDS.contains(&kind)
        && event
            .get("tags")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_array)
            .any(|tag| {
                tag.first().and_then(Value::as_str) == Some("h")
                    && tag.get(1).and_then(Value::as_str) == Some(channel_id)
            })
}

fn readable_run_source(
    run: &CoordinatorRunSummary,
    source_events: &HashMap<String, &Value>,
    channel_id: &str,
) -> bool {
    let Some(intent) = source_events.get(&run.original_intent_event_id.to_ascii_lowercase()) else {
        return false;
    };
    let root = run
        .thread_root_event_id
        .as_ref()
        .and_then(|id| source_events.get(&id.to_ascii_lowercase()).copied());
    buzz_core::thread_brief::has_readable_canonical_run_source(
        intent,
        root,
        &run.original_intent_event_id,
        run.thread_root_event_id.as_deref(),
        channel_id,
    )
}

async fn query_events(client: &BuzzClient, filter: &Value) -> Result<Vec<Value>, CliError> {
    let raw = client
        .query_bounded(filter, PROJECT_QUERY_RESPONSE_BYTES)
        .await?;
    parse_query_events(&raw)
}

fn parse_query_events(raw: &str) -> Result<Vec<Value>, CliError> {
    if raw.len() > PROJECT_QUERY_RESPONSE_BYTES {
        return Err(CliError::Other(
            "relay query response exceeded the 2 MiB project-run bound".into(),
        ));
    }
    serde_json::from_str(raw)
        .map_err(|error| CliError::Other(format!("failed to parse relay query response: {error}")))
}

fn encode_project_output(output: &Value) -> Result<Vec<u8>, CliError> {
    let encoded = serde_json::to_vec(output)
        .map_err(|error| CliError::Other(format!("serialize project run page: {error}")))?;
    if encoded.len().saturating_add(1) > PROJECT_OUTPUT_BYTES {
        return Err(CliError::Other(
            "project run page exceeded the 64 KiB output limit".into(),
        ));
    }
    Ok(encoded)
}

fn has_addressable_identity(event: &Value, kind: u32, owner: &str, slug: &str) -> bool {
    event.get("kind").and_then(Value::as_u64) == Some(u64::from(kind))
        && event
            .get("pubkey")
            .and_then(Value::as_str)
            .is_some_and(|pubkey| pubkey.eq_ignore_ascii_case(owner))
        && event
            .get("tags")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_array)
            .any(|tag| {
                tag.first().and_then(Value::as_str) == Some("d")
                    && tag.get(1).and_then(Value::as_str) == Some(slug)
            })
}

fn repository_address_tags(event: &Value) -> impl Iterator<Item = &str> {
    event
        .get("tags")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_array)
        .filter(|tag| tag.first().and_then(Value::as_str) == Some("a"))
        .filter_map(|tag| tag.get(1).and_then(Value::as_str))
}

fn parse_repository_coordinate(coordinate: &str) -> Option<(String, String)> {
    let mut parts = coordinate.splitn(3, ':');
    if parts.next()? != "30617" {
        return None;
    }
    let owner = parts.next()?.to_ascii_lowercase();
    let slug = parts.next()?;
    if owner.len() != 64
        || !owner.bytes().all(|byte| byte.is_ascii_hexdigit())
        || slug.is_empty()
        || slug.len() > 1024
        || slug.chars().any(char::is_control)
    {
        return None;
    }
    Some((owner, slug.to_string()))
}

fn parse_project_coordinate(coordinate: &str) -> Result<(String, String), CliError> {
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
        || slug.trim() != slug
        || coordinate.len() > 1100
    {
        return Err(CliError::Usage("invalid NIP-MP project coordinate".into()));
    }
    Ok((owner, slug.to_string()))
}

fn parse_project_cursor(
    updated_at_ms: Option<i64>,
    run_id: Option<&str>,
) -> Result<Option<CoordinatorRunCursor>, CliError> {
    match (updated_at_ms, run_id) {
        (Some(updated_at_ms), Some(run_id)) if updated_at_ms >= 0 => {
            let run_id = uuid::Uuid::parse_str(run_id)
                .map_err(|_| CliError::Usage("--cursor-run-id must be a UUID".into()))?
                .to_string();
            Ok(Some(CoordinatorRunCursor {
                updated_at_ms,
                run_id,
            }))
        }
        (Some(_), Some(_)) => Err(CliError::Usage(
            "--cursor-updated-at-ms must be nonnegative".into(),
        )),
        (None, None) => Ok(None),
        _ => Err(CliError::Usage(
            "--cursor-updated-at-ms and --cursor-run-id must be supplied together".into(),
        )),
    }
}

fn matches_thread(
    turn: &TurnSummary,
    channel_id: &str,
    root_event_id: &str,
    source_ids: &std::collections::HashSet<String>,
) -> bool {
    turn.channel_id.as_deref() == Some(channel_id)
        && (turn.thread_root_event_id.as_deref() == Some(root_event_id)
            || turn
                .batch_trigger_event_ids
                .iter()
                .chain(&turn.merged_cancelled_event_ids)
                .any(|event_id| source_ids.contains(&event_id.to_ascii_lowercase())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn turn(channel_id: &str, root: &str, trigger: &str) -> TurnSummary {
        TurnSummary {
            turn_id: "123e4567-e89b-12d3-a456-426614174000".into(),
            channel_id: Some(channel_id.into()),
            session_scope: "thread".into(),
            thread_root_event_id: Some(root.into()),
            batch_trigger_event_ids: vec![trigger.into()],
            merged_cancelled_event_ids: Vec::new(),
            agent_index: 0,
            acp_session_id: None,
            status: "returned".into(),
            liveness: "unknown".into(),
            task_state: "unknown".into(),
            started_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    #[test]
    fn run_history_filter_requires_exact_channel_and_thread_evidence() {
        let channel = "123e4567-e89b-12d3-a456-426614174001";
        let root = "a".repeat(64);
        let trigger = "b".repeat(64);
        let unrelated = "c".repeat(64);
        let turn = turn(channel, &root, &trigger);
        let readable_source_ids = HashSet::from([trigger.clone()]);

        assert!(matches_thread(&turn, channel, &root, &readable_source_ids));
        assert!(matches_thread(
            &turn,
            channel,
            &unrelated,
            &readable_source_ids
        ));
        assert!(!matches_thread(
            &turn,
            "123e4567-e89b-12d3-a456-426614174002",
            &root,
            &readable_source_ids
        ));
        assert!(!matches_thread(
            &turn,
            channel,
            &unrelated,
            &HashSet::from(["d".repeat(64)])
        ));
    }

    #[test]
    fn project_coordinate_and_cursor_validators_reject_unbounded_or_partial_inputs() {
        let coordinate = format!("30621:{}:project:slug", "A".repeat(64));
        assert_eq!(
            parse_project_coordinate(&coordinate).unwrap(),
            ("a".repeat(64), "project:slug".into())
        );
        assert!(parse_project_coordinate("30617:not-a-project").is_err());
        assert!(parse_project_coordinate(&format!("30621:{}:bad\n", "a".repeat(64))).is_err());

        assert!(
            parse_project_cursor(Some(-1), Some("123e4567-e89b-12d3-a456-426614174000")).is_err()
        );
        assert!(parse_project_cursor(Some(1), None).is_err());
        assert!(parse_project_cursor(None, Some("not-a-uuid")).is_err());
        assert!(
            parse_project_cursor(Some(1), Some("123e4567-e89b-12d3-a456-426614174000"))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn project_run_response_bounds_query_bytes_and_excludes_message_bodies() {
        assert!(parse_query_events(&" ".repeat(PROJECT_QUERY_RESPONSE_BYTES + 1)).is_err());
        assert!(
            encode_project_output(&json!({"content": "x".repeat(PROJECT_OUTPUT_BYTES)})).is_err()
        );
        let run = CoordinatorRunSummary {
            run_id: "123e4567-e89b-12d3-a456-426614174000".into(),
            channel_id: "123e4567-e89b-12d3-a456-426614174001".into(),
            session_scope: "thread".into(),
            thread_root_event_id: None,
            original_intent_event_id: "a".repeat(64),
            project_coordinate: None,
            project_link_conflict: false,
            attempt_turns: Vec::new(),
            attempt_history_may_be_truncated: false,
            recent_events: vec![],
            event_history_may_be_truncated: false,
            created_at_ms: 10,
            updated_at_ms: 20,
            task_state: "unknown".into(),
        };
        let evidence = project_run_evidence(&run, &BTreeSet::new(), 10);
        let output =
            serde_json::to_value(project_run_item(&run, evidence, "30621:owner:project")).unwrap();
        assert_eq!(output["task_state"], "unknown");
        assert_eq!(output["liveness"], "unknown");
        assert_eq!(output["history_reliability"], "best_effort");
        assert_eq!(output["history_completeness"], "unknown");
        assert!(output["attempt_turns"].as_array().unwrap().is_empty());
        assert!(output.get("recent_events").is_none());
        assert!(output.get("original_intent").is_none());
    }

    #[test]
    fn project_attempt_source_requires_exact_id_channel_and_allowed_kind() {
        let channel = "123e4567-e89b-12d3-a456-426614174001";
        let event_id = "a".repeat(64);
        let expected = HashSet::from([event_id.clone()]);
        let event = json!({
            "id": event_id,
            "kind": 9,
            "tags": [["h", channel]],
            "content": "source body must not be projected",
        });
        assert!(readable_project_attempt_source(&event, channel, &expected));
        assert!(!readable_project_attempt_source(
            &event,
            "123e4567-e89b-12d3-a456-426614174099",
            &expected,
        ));
        assert!(!readable_project_attempt_source(
            &event,
            channel,
            &HashSet::from(["b".repeat(64)]),
        ));
        let wrong_kind = json!({
            "id": "c".repeat(64),
            "kind": 5,
            "tags": [["h", channel]],
        });
        assert!(!readable_project_attempt_source(
            &wrong_kind,
            channel,
            &HashSet::from(["c".repeat(64)]),
        ));
    }
}
