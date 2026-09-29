//! Deterministic, source-linked projection for a Buzz message-thread brief.
//!
//! This module contains no relay access or model calls so CLI and Desktop can
//! build the same evidence shape from their own authorized thread reads.

use serde_json::{json, Value};
use std::collections::HashSet;

/// Message kinds used for a thread brief. Aux events are fetched separately
/// through the relay's `include_aux` extension.
pub const THREAD_BRIEF_KINDS: [u32; 5] = [
    crate::kind::KIND_STREAM_MESSAGE,
    crate::kind::KIND_STREAM_MESSAGE_V2,
    crate::kind::KIND_STREAM_MESSAGE_DIFF,
    crate::kind::KIND_FORUM_POST,
    crate::kind::KIND_FORUM_COMMENT,
];

/// Message kinds that advance the reply cursor. Auxiliary events never advance
/// the keyset, even when they appear in `progress_events` as evidence.
pub const THREAD_BRIEF_CURSOR_KINDS: [u32; 4] = [
    crate::kind::KIND_STREAM_MESSAGE,
    crate::kind::KIND_STREAM_MESSAGE_V2,
    crate::kind::KIND_STREAM_MESSAGE_DIFF,
    crate::kind::KIND_FORUM_COMMENT,
];

/// Auxiliary events returned with a thread page for edits, reactions, and
/// deletions. They are useful evidence but are not progress replies and never
/// advance the reply cursor.
pub const THREAD_BRIEF_AUX_KINDS: [u32; 4] = [
    crate::kind::KIND_DELETION,
    crate::kind::KIND_REACTION,
    crate::kind::KIND_NIP29_DELETE_EVENT,
    crate::kind::KIND_STREAM_MESSAGE_EDIT,
];

/// Confirm that supplied relay results contain the source event(s) for one
/// local run on the selected channel. This checks IDs, kinds, channel tags,
/// and NIP-10 ancestry, but not signatures; callers must use a validated relay
/// event path and must not expose these events as run-list message content.
pub fn has_readable_canonical_run_source(
    original_intent: &Value,
    thread_root: Option<&Value>,
    expected_intent_id: &str,
    expected_root_id: Option<&str>,
    channel_id: &str,
) -> bool {
    let Some(intent_id) = valid_event_id(expected_intent_id) else {
        return false;
    };
    let Some(intent) = readable_source_event(original_intent, &intent_id, channel_id) else {
        return false;
    };
    let intent_root = canonical_thread_root(intent);
    match (expected_root_id, thread_root) {
        (Some(expected), Some(root)) => {
            let Some(expected) = valid_event_id(expected) else {
                return false;
            };
            readable_source_event(root, &expected, channel_id).is_some()
                && canonical_thread_root(root).eq_ignore_ascii_case(&expected)
                && intent_root.eq_ignore_ascii_case(&expected)
        }
        (None, None) => intent_root.eq_ignore_ascii_case(&intent_id),
        _ => false,
    }
}

fn readable_source_event<'a>(
    event: &'a Value,
    expected_id: &str,
    channel_id: &str,
) -> Option<&'a Value> {
    let id = event.get("id")?.as_str()?;
    if !id.eq_ignore_ascii_case(expected_id)
        || !event
            .get("kind")
            .and_then(Value::as_u64)
            .and_then(|kind| u32::try_from(kind).ok())
            .is_some_and(|kind| THREAD_BRIEF_KINDS.contains(&kind))
        || !has_channel_tag(event, channel_id)
    {
        return None;
    }
    Some(event)
}

fn valid_event_id(value: &str) -> Option<String> {
    (value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| value.to_ascii_lowercase())
}

fn has_channel_tag(event: &Value, channel_id: &str) -> bool {
    event
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

fn canonical_thread_root(event: &Value) -> String {
    let id = event.get("id").and_then(Value::as_str).unwrap_or_default();
    let tags = event
        .get("tags")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_array)
        .map(|tag| {
            tag.iter()
                .map(|part| part.as_str().unwrap_or_default().to_string())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    crate::nip10::parse_thread_markers_from_parts(tags.iter().map(Vec::as_slice))
        .resolve()
        .map(|(root, _)| root)
        .unwrap_or_else(|| id.to_ascii_lowercase())
}

/// Build a deterministic brief from a root event and the events returned for
/// its thread page. A missing root is an error; task completion remains
/// unknown because message activity alone is not lifecycle evidence.
pub fn make_thread_brief(
    root_event_id: &str,
    events: &[Value],
    limit: u32,
    requested_depth_limit: Option<u32>,
) -> Result<Value, String> {
    let mut ordered_events = events
        .iter()
        .map(normalize_thread_event)
        .collect::<Vec<_>>();
    ordered_events.sort_by(|left, right| {
        left.get("created_at")
            .and_then(Value::as_u64)
            .cmp(&right.get("created_at").and_then(Value::as_u64))
            .then_with(|| {
                left.get("id")
                    .and_then(Value::as_str)
                    .cmp(&right.get("id").and_then(Value::as_str))
            })
    });

    let root = ordered_events
        .iter()
        .find(|event| event.get("id").and_then(Value::as_str) == Some(root_event_id))
        .cloned()
        .ok_or_else(|| format!("thread root {root_event_id} not found"))?;

    let mut seen_ids = HashSet::new();
    let observed = ordered_events
        .into_iter()
        .filter(|event| event.get("id").and_then(Value::as_str) != Some(root_event_id))
        .filter(|event| {
            event
                .get("id")
                .and_then(Value::as_str)
                .is_none_or(|event_id| seen_ids.insert(event_id.to_string()))
        })
        .filter(|event| {
            event
                .get("kind")
                .and_then(Value::as_u64)
                .and_then(|kind| u32::try_from(kind).ok())
                .is_some_and(|kind| {
                    THREAD_BRIEF_KINDS.contains(&kind) || THREAD_BRIEF_AUX_KINDS.contains(&kind)
                })
        })
        .collect::<Vec<_>>();
    let replies = observed
        .iter()
        .filter(|event| {
            event
                .get("kind")
                .and_then(Value::as_u64)
                .and_then(|kind| u32::try_from(kind).ok())
                .is_some_and(|kind| THREAD_BRIEF_KINDS.contains(&kind))
        })
        .cloned()
        .collect::<Vec<_>>();
    let auxiliary_events = observed
        .iter()
        .filter(|event| {
            event
                .get("kind")
                .and_then(Value::as_u64)
                .and_then(|kind| u32::try_from(kind).ok())
                .is_some_and(|kind| THREAD_BRIEF_AUX_KINDS.contains(&kind))
        })
        .cloned()
        .collect::<Vec<_>>();
    let latest = observed.last();
    let reply_count = replies.len();
    let latest_activity = latest.map(|event| {
        json!({
            "event_id": event.get("id"),
            "created_at": event.get("created_at"),
        })
    });
    let cursor_replies = replies
        .iter()
        .filter(|event| {
            event
                .get("kind")
                .and_then(Value::as_u64)
                .and_then(|kind| u32::try_from(kind).ok())
                .is_some_and(|kind| THREAD_BRIEF_CURSOR_KINDS.contains(&kind))
        })
        .collect::<Vec<_>>();
    let next_cursor = if cursor_replies.len() >= limit.max(1) as usize {
        cursor_replies.last().and_then(|event| {
            let created_at = event.get("created_at")?.as_u64()?;
            let event_id = event.get("id")?.as_str()?;
            Some(json!({
                "created_at": created_at,
                "event_id": event_id,
            }))
        })
    } else {
        None
    };
    let applied_depth_limit = requested_depth_limit.unwrap_or(64);
    let depth_limit_may_truncate = applied_depth_limit < i32::MAX as u32;
    let possibly_truncated = next_cursor.is_some() || depth_limit_may_truncate;
    let summary = if let Some(latest) = latest {
        format!(
            "{} reply event(s) observed; latest returned activity is at {}. Task completion is unknown.",
            reply_count,
            latest
                .get("created_at")
                .and_then(Value::as_u64)
                .unwrap_or_default()
        )
    } else {
        "No reply events were returned. Task completion is unknown.".to_string()
    };

    let mut source_ids = HashSet::new();
    let source_event_ids = std::iter::once(&root)
        .chain(replies.iter())
        .chain(auxiliary_events.iter())
        .filter_map(|event| event.get("id"))
        .filter(|event_id| {
            event_id
                .as_str()
                .is_none_or(|id| source_ids.insert(id.to_string()))
        })
        .cloned()
        .collect::<Vec<_>>();

    Ok(json!({
        "thread_root_id": root_event_id,
        "original_intent": root,
        "summary": {
            "text": summary,
            "method": "deterministic",
            "task_completion": "unknown",
        },
        "progress_events": replies,
        "auxiliary_events": auxiliary_events,
        "status": {
            "task_state": "unknown",
            "reply_event_count": reply_count,
            "latest_activity": latest_activity,
            "next_cursor": next_cursor,
            "requested_depth_limit": requested_depth_limit,
            "applied_depth_limit": applied_depth_limit,
            "depth_limit_may_truncate": depth_limit_may_truncate,
            "possibly_truncated": possibly_truncated,
        },
        "source_event_ids": source_event_ids,
    }))
}

/// Attach the local query completion time to a brief. This is not a relay
/// event timestamp and does not imply an atomic relay snapshot.
pub fn add_observed_at(brief: &mut Value, observed_at_ms: u64) {
    brief["status"]["observed_at_ms"] = json!(observed_at_ms);
}

/// Attach the shared, bounded local managed-turn projection to a deterministic
/// thread brief. Storage and authorization stay in the caller's adapter.
pub fn add_managed_turn_history(
    brief: &mut Value,
    managed_turns: Value,
    has_more_turns: bool,
    capture_gap_count: u64,
) {
    brief["status"]["steering_controls"] = Value::Array(project_steering_controls(&managed_turns));
    brief["status"]["managed_turns"] = managed_turns;
    brief["status"]["managed_turn_lookup"] = json!({
        "source": "local_acp_attempt_journal",
        "scope": "caller-readable exact thread",
        "has_more_turns": has_more_turns,
        "capture_reliability": "best_effort",
        "capture_gap_count": capture_gap_count,
        "capture_gap_scope": "current_relay_owner_journal",
        "task_state": "unknown",
        "liveness": "unknown",
    });
}

/// Project exact-thread steering receipts into one machine-readable list for
/// the CLI, Desktop, and agent callers. ACP acknowledgement never means that
/// the model observed the guidance; that stage stays explicitly unknown.
fn project_steering_controls(managed_turns: &Value) -> Vec<Value> {
    let Some(turns) = managed_turns.as_array() else {
        return Vec::new();
    };
    let mut controls = Vec::new();
    for attempt in turns {
        let Some(turn) = attempt.get("turn") else {
            continue;
        };
        let Some(turn_id) = turn.get("turn_id").and_then(Value::as_str) else {
            continue;
        };
        let agent_index = turn.get("agent_index").cloned().unwrap_or(Value::Null);
        let history_truncated = attempt
            .get("event_history_may_be_truncated")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let mut by_source = std::collections::BTreeMap::<String, Value>::new();
        for event in attempt
            .get("recent_events")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let kind = event
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if kind != "steer_submitted" && kind != "steer_outcome" {
                continue;
            }
            let Some(details) = event.get("details") else {
                continue;
            };
            let Some(source_id) = details
                .get("source_event_id")
                .and_then(Value::as_str)
                .filter(|id| id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
            else {
                continue;
            };
            let occurred_at = event.get("occurred_at_ms").cloned().unwrap_or(Value::Null);
            let entry = by_source
                .entry(source_id.to_ascii_lowercase())
                .or_insert_with(|| {
                    json!({
                        "turn_id": turn_id,
                        "agent_index": agent_index,
                        "source_event_id": source_id.to_ascii_lowercase(),
                        "submission_recorded": false,
                        "submitted_at_ms": null,
                        "adapter_outcome": null,
                        "outcome_at_ms": null,
                        "agent_observed": "unknown",
                        "event_history_may_be_truncated": history_truncated,
                    })
                });
            if kind == "steer_submitted" {
                entry["submission_recorded"] = json!(true);
                entry["submitted_at_ms"] = occurred_at;
            } else {
                let outcome = details
                    .get("outcome")
                    .and_then(Value::as_str)
                    .filter(|outcome| {
                        matches!(
                            *outcome,
                            "adapter_acknowledged"
                                | "adapter_rejected"
                                | "attempt_failed"
                                | "delivery_unknown"
                        )
                    })
                    .unwrap_or("unknown");
                entry["adapter_outcome"] = json!(outcome);
                entry["outcome_at_ms"] = occurred_at;
            }
        }
        for (_, mut control) in by_source {
            let outcome = control
                .get("adapter_outcome")
                .and_then(Value::as_str)
                .unwrap_or("pending");
            control["state"] = json!(if outcome == "pending" {
                "submitted"
            } else {
                outcome
            });
            controls.push(control);
        }
    }
    controls.sort_by(|left, right| {
        left.get("submitted_at_ms")
            .and_then(Value::as_i64)
            .cmp(&right.get("submitted_at_ms").and_then(Value::as_i64))
            .then_with(|| {
                left.get("turn_id")
                    .and_then(Value::as_str)
                    .cmp(&right.get("turn_id").and_then(Value::as_str))
            })
            .then_with(|| {
                left.get("source_event_id")
                    .and_then(Value::as_str)
                    .cmp(&right.get("source_event_id").and_then(Value::as_str))
            })
    });
    controls
}

/// Attach bounded coordinator-run identities and their attempt/source links.
/// Callers must first prove access to the exact message thread.
pub fn add_coordinator_run_history(
    brief: &mut Value,
    runs: Value,
    has_more_runs: bool,
    capture_gap_count: u64,
) {
    brief["status"]["coordinator_runs"] = runs;
    brief["status"]["coordinator_run_lookup"] = json!({
        "source": "local_coordinator_run_journal",
        "scope": "caller-readable exact thread",
        "has_more_runs": has_more_runs,
        "capture_reliability": "best_effort",
        "capture_gap_count": capture_gap_count,
        "capture_gap_scope": "current_relay_owner_journal",
        "task_state": "unknown",
    });
}

fn normalize_thread_event(event: &Value) -> Value {
    let mut normalized = json!({
        "id": event.get("id").and_then(Value::as_str).unwrap_or(""),
        "pubkey": event.get("pubkey").and_then(Value::as_str).unwrap_or(""),
        "kind": event.get("kind").and_then(Value::as_u64).unwrap_or(0),
        "content": event.get("content").and_then(Value::as_str).unwrap_or(""),
        "created_at": event.get("created_at").and_then(Value::as_u64).unwrap_or(0),
        "tags": event.get("tags").cloned().unwrap_or_else(|| json!([])),
    });
    if let Some(signature) = event.get("sig").and_then(Value::as_str) {
        normalized["sig"] = json!(signature);
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::{
        add_coordinator_run_history, add_managed_turn_history, add_observed_at,
        has_readable_canonical_run_source, make_thread_brief,
    };
    use serde_json::json;

    const ROOT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const REPLY: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn preserves_root_evidence_and_does_not_infer_completion() {
        let events = vec![
            json!({"id": REPLY, "kind": 45003, "content": "Progress", "created_at": 20}),
            json!({"id": ROOT, "kind": 45001, "content": "Original request", "created_at": 10}),
        ];
        let brief = make_thread_brief(ROOT, &events, 1, None).unwrap();

        assert_eq!(brief["original_intent"]["content"], "Original request");
        assert_eq!(brief["progress_events"][0]["content"], "Progress");
        assert_eq!(brief["source_event_ids"].as_array().unwrap().len(), 2);
        assert_eq!(brief["status"]["next_cursor"]["event_id"], REPLY);
        assert_eq!(brief["status"]["task_state"], "unknown");
        assert_eq!(brief["summary"]["task_completion"], "unknown");
    }

    #[test]
    fn records_query_observation_time_separately_from_event_time() {
        let mut brief = make_thread_brief(
            ROOT,
            &[json!({"id": ROOT, "kind": 45001, "content": "Request", "created_at": 10})],
            10,
            None,
        )
        .unwrap();

        add_observed_at(&mut brief, 1_780_000_000_123);

        assert_eq!(brief["status"]["observed_at_ms"], 1_780_000_000_123_u64);
        assert_eq!(brief["original_intent"]["created_at"], 10);
        assert_eq!(brief["status"]["task_state"], "unknown");
    }

    #[test]
    fn rejects_a_missing_root_and_deduplicates_reply_events() {
        assert!(make_thread_brief(ROOT, &[], 10, None).is_err());
        let reply = json!({"id": REPLY, "kind": 45003, "content": "Progress", "created_at": 20});
        let brief = make_thread_brief(
            ROOT,
            &[
                json!({"id": ROOT, "kind": 45001, "content": "Original request", "created_at": 10}),
                reply.clone(),
                reply,
            ],
            10,
            None,
        )
        .unwrap();
        assert_eq!(brief["progress_events"].as_array().unwrap().len(), 1);
        assert_eq!(brief["source_event_ids"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn separates_auxiliary_evidence_and_ignores_unrelated_thread_kinds() {
        let edit_kind = crate::kind::KIND_STREAM_MESSAGE_EDIT;
        let unrelated_kind = crate::kind::KIND_JOB_PROGRESS;
        let brief = make_thread_brief(
            ROOT,
            &[
                json!({"id": ROOT, "kind": 45001, "content": "Request", "created_at": 10}),
                json!({"id": REPLY, "kind": 45003, "content": "Progress", "created_at": 20}),
                json!({"id": "c".repeat(64), "kind": edit_kind, "content": "Edit", "created_at": 30}),
                json!({"id": "d".repeat(64), "kind": unrelated_kind, "content": "Not a thread update", "created_at": 40}),
            ],
            1,
            None,
        )
        .unwrap();

        assert_eq!(brief["status"]["reply_event_count"], 1);
        assert_eq!(brief["status"]["latest_activity"]["created_at"], 30);
        assert_eq!(brief["status"]["next_cursor"]["event_id"], REPLY);
        assert_eq!(brief["progress_events"].as_array().unwrap().len(), 1);
        assert_eq!(brief["auxiliary_events"].as_array().unwrap().len(), 1);
        assert_eq!(brief["source_event_ids"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn managed_turn_projection_keeps_completion_and_liveness_unknown() {
        let mut brief = make_thread_brief(
            ROOT,
            &[json!({"id": ROOT, "kind": 45001, "content": "Request", "created_at": 10})],
            10,
            None,
        )
        .unwrap();
        add_managed_turn_history(&mut brief, json!([]), false, 2);

        assert_eq!(brief["status"]["managed_turns"], json!([]));
        assert_eq!(brief["status"]["steering_controls"], json!([]));
        assert_eq!(
            brief["status"]["managed_turn_lookup"]["source"],
            "local_acp_attempt_journal"
        );
        assert_eq!(
            brief["status"]["managed_turn_lookup"]["task_state"],
            "unknown"
        );
        assert_eq!(
            brief["status"]["managed_turn_lookup"]["liveness"],
            "unknown"
        );
        assert_eq!(
            brief["status"]["managed_turn_lookup"]["capture_reliability"],
            "best_effort"
        );
        assert_eq!(
            brief["status"]["managed_turn_lookup"]["capture_gap_count"],
            2
        );
    }

    #[test]
    fn steering_control_projection_separates_submission_ack_and_observation() {
        let mut brief = make_thread_brief(
            ROOT,
            &[json!({"id": ROOT, "kind": 45001, "content": "Request", "created_at": 10})],
            10,
            None,
        )
        .unwrap();
        let event_id = "b".repeat(64);
        add_managed_turn_history(
            &mut brief,
            json!([{
                "turn": {"turn_id": "turn-1", "agent_index": 2},
                "recent_events": [
                    {"sequence": 1, "kind": "steer_submitted", "occurred_at_ms": 10, "details": {"source_event_id": event_id}},
                    {"sequence": 2, "kind": "steer_outcome", "occurred_at_ms": 20, "details": {"source_event_id": event_id, "outcome": "adapter_acknowledged"}}
                ],
                "event_history_may_be_truncated": false
            }]),
            false,
            0,
        );

        let control = &brief["status"]["steering_controls"][0];
        assert_eq!(control["state"], "adapter_acknowledged");
        assert_eq!(control["submission_recorded"], true);
        assert_eq!(control["submitted_at_ms"], 10);
        assert_eq!(control["outcome_at_ms"], 20);
        assert_eq!(control["agent_observed"], "unknown");
        assert_eq!(control["event_history_may_be_truncated"], false);
    }

    #[test]
    fn pending_steer_projection_keeps_truncation_and_does_not_claim_delivery() {
        let mut brief = make_thread_brief(
            ROOT,
            &[json!({"id": ROOT, "kind": 45001, "content": "Request", "created_at": 10})],
            10,
            None,
        )
        .unwrap();
        let event_id = "c".repeat(64);
        add_managed_turn_history(
            &mut brief,
            json!([{
                "turn": {"turn_id": "turn-2", "agent_index": 0},
                "recent_events": [
                    {"sequence": 1, "kind": "steer_submitted", "occurred_at_ms": 30, "details": {"source_event_id": event_id}}
                ],
                "event_history_may_be_truncated": true
            }]),
            true,
            1,
        );

        let control = &brief["status"]["steering_controls"][0];
        assert_eq!(control["state"], "submitted");
        assert_eq!(control["adapter_outcome"], serde_json::Value::Null);
        assert_eq!(control["agent_observed"], "unknown");
        assert_eq!(control["event_history_may_be_truncated"], true);
        assert_eq!(
            brief["status"]["managed_turn_lookup"]["capture_gap_count"],
            1
        );
    }

    #[test]
    fn coordinator_run_projection_keeps_task_state_unknown() {
        let mut brief = make_thread_brief(
            ROOT,
            &[json!({"id": ROOT, "kind": 45001, "content": "Request", "created_at": 10})],
            10,
            None,
        )
        .unwrap();
        add_coordinator_run_history(&mut brief, json!([]), true, 3);
        assert_eq!(brief["status"]["coordinator_runs"], json!([]));
        assert_eq!(
            brief["status"]["coordinator_run_lookup"]["source"],
            "local_coordinator_run_journal"
        );
        assert_eq!(
            brief["status"]["coordinator_run_lookup"]["task_state"],
            "unknown"
        );
        assert_eq!(
            brief["status"]["coordinator_run_lookup"]["capture_reliability"],
            "best_effort"
        );
        assert_eq!(
            brief["status"]["coordinator_run_lookup"]["capture_gap_count"],
            3
        );
        assert_eq!(brief["summary"]["task_completion"], "unknown");
    }

    #[test]
    fn project_run_source_requires_readable_intent_and_canonical_root() {
        let channel = "123e4567-e89b-12d3-a456-426614174001";
        let root_id = "a".repeat(64);
        let intent_id = "b".repeat(64);
        let root = json!({"id": root_id, "kind": 9, "tags": [["h", channel]]});
        let intent = json!({"id": intent_id, "kind": 9, "tags": [
            ["h", channel], ["e", root_id, "", "reply"]
        ]});
        assert!(has_readable_canonical_run_source(
            &intent,
            Some(&root),
            &intent_id,
            Some(&root_id),
            channel,
        ));
        assert!(!has_readable_canonical_run_source(
            &intent,
            None,
            &intent_id,
            Some(&root_id),
            channel,
        ));

        let wrong_channel = "123e4567-e89b-12d3-a456-426614174099";
        assert!(!has_readable_canonical_run_source(
            &intent,
            Some(&root),
            &intent_id,
            Some(&root_id),
            wrong_channel,
        ));
        let unreadable_intent = json!({"id": intent_id, "kind": 9,
            "tags": [["h", wrong_channel], ["e", root_id, "", "reply"]]});
        assert!(!has_readable_canonical_run_source(
            &unreadable_intent,
            Some(&root),
            &intent_id,
            Some(&root_id),
            channel,
        ));

        let top_level = json!({"id": intent_id, "kind": 9, "tags": [["h", channel]]});
        assert!(has_readable_canonical_run_source(
            &top_level, None, &intent_id, None, channel,
        ));
        assert!(!has_readable_canonical_run_source(
            &intent, None, &intent_id, None, channel,
        ));
    }
}
