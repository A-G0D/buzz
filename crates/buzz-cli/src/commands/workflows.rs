use sha2::{Digest, Sha256};

use crate::client::{
    extract_d_tag, extract_relay_response_field, normalize_write_response, print_create_response,
    BuzzClient,
};
use crate::error::CliError;
use crate::validate::{parse_uuid, read_or_stdin, sdk_err, validate_uuid};

// TODO(phase-4): Replace raw nostr::EventBuilder usage with buzz-sdk builder functions

/// List workflows in a channel — query kind:30620 workflow definition events.
pub async fn cmd_list_workflows(client: &BuzzClient, channel_id: &str) -> Result<(), CliError> {
    validate_uuid(channel_id)?;
    let filter = serde_json::json!({
        "kinds": [30620],
        "#h": [channel_id]
    });
    let resp = client.query(&filter).await?;
    let events: Vec<serde_json::Value> = serde_json::from_str(&resp).unwrap_or_default();
    let workflows: Vec<serde_json::Value> = events
        .iter()
        .map(|e| {
            serde_json::json!({
                "workflow_id": extract_d_tag(e),
                "content": e.get("content").and_then(|v| v.as_str()).unwrap_or(""),
                "created_at": e.get("created_at").and_then(|v| v.as_u64()).unwrap_or(0),
                "pubkey": e.get("pubkey").and_then(|v| v.as_str()).unwrap_or(""),
            })
        })
        .collect();
    let output = serde_json::to_string(&workflows).unwrap_or_default();
    println!("{output}");
    Ok(())
}

/// Get a single workflow definition.
pub async fn cmd_get_workflow(client: &BuzzClient, workflow_id: &str) -> Result<(), CliError> {
    validate_uuid(workflow_id)?;
    let filter = serde_json::json!({
        "kinds": [30620],
        "#d": [workflow_id]
    });
    let resp = client.query(&filter).await?;
    let events: Vec<serde_json::Value> = serde_json::from_str(&resp).unwrap_or_default();
    if let Some(e) = events.first() {
        let normalized = serde_json::json!({
            "workflow_id": extract_d_tag(e),
            "content": e.get("content").and_then(|v| v.as_str()).unwrap_or(""),
            "created_at": e.get("created_at").and_then(|v| v.as_u64()).unwrap_or(0),
            "pubkey": e.get("pubkey").and_then(|v| v.as_str()).unwrap_or(""),
        });
        println!("{normalized}");
    } else {
        println!("null");
    }
    Ok(())
}

/// Get workflow run history from the relay's authorized database read endpoint.
pub async fn cmd_get_workflow_runs(
    client: &BuzzClient,
    workflow_id: &str,
    limit: Option<u32>,
) -> Result<(), CliError> {
    validate_uuid(workflow_id)?;
    let limit = limit.unwrap_or(20).clamp(1, 100);
    let response = client
        .get_authed(&format!("/workflows/{workflow_id}/runs?limit={limit}"))
        .await?;
    let response: serde_json::Value = serde_json::from_str(&response)
        .map_err(|error| CliError::Other(format!("invalid workflow-runs response: {error}")))?;
    if !response
        .get("runs")
        .is_some_and(serde_json::Value::is_array)
    {
        return Err(CliError::Other(
            "workflow-runs response is missing its runs array".into(),
        ));
    }
    println!("{response}");
    Ok(())
}

/// The bounded workflow-history lookup used to annotate a source-backed thread brief.
#[derive(Debug)]
pub struct WorkflowRunLookup {
    pub runs: Vec<serde_json::Value>,
    pub matched_count: u32,
    pub pages_checked: u32,
    pub history_truncated: bool,
    pub matches_truncated: bool,
}

fn run_trigger_matches(
    run: &serde_json::Value,
    trigger_event_ids: &std::collections::HashSet<String>,
) -> bool {
    run.get("trigger_event_id")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|event_id| trigger_event_ids.contains(&event_id.to_ascii_lowercase()))
}

/// Find workflow runs whose trigger event appears in the selected conversation evidence.
///
/// Reads only the authorized run endpoint and keeps the brief payload small by
/// omitting execution traces and diagnostic text. Pagination is bounded so a
/// frequently-fired workflow cannot make one brief request scan unbounded history.
pub async fn find_runs_by_trigger_events(
    client: &BuzzClient,
    workflow_id: &str,
    trigger_event_ids: &[String],
    max_pages: u32,
) -> Result<WorkflowRunLookup, CliError> {
    validate_uuid(workflow_id)?;
    if !(1..=50).contains(&max_pages) {
        return Err(CliError::Usage(
            "--workflow-pages must be between 1 and 50".into(),
        ));
    }

    let trigger_event_ids = trigger_event_ids
        .iter()
        .map(|event_id| event_id.to_ascii_lowercase())
        .collect::<std::collections::HashSet<_>>();
    if trigger_event_ids.is_empty() {
        return Ok(WorkflowRunLookup {
            runs: Vec::new(),
            matched_count: 0,
            pages_checked: 0,
            history_truncated: false,
            matches_truncated: false,
        });
    }

    const PAGE_SIZE: u32 = 100;
    const MAX_SAVED_MATCHES: usize = 50;
    let mut cursor: Option<(String, String)> = None;
    let mut runs = Vec::new();
    let mut seen_run_ids = std::collections::HashSet::new();
    let mut matched_count = 0_u32;
    let mut pages_checked = 0_u32;
    let mut history_truncated = false;

    for page in 0..max_pages {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("limit", &PAGE_SIZE.to_string());
        if let Some((before, before_id)) = &cursor {
            query.append_pair("before", before);
            query.append_pair("before_id", before_id);
        }
        let path = format!("/workflows/{workflow_id}/runs?{}", query.finish());
        let raw = client.get_authed(&path).await?;
        let response: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|error| CliError::Other(format!("invalid workflow-runs response: {error}")))?;
        let page_runs = response
            .get("runs")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                CliError::Other("workflow-runs response is missing its runs array".into())
            })?;

        pages_checked += 1;
        for run in page_runs {
            if !run_trigger_matches(run, &trigger_event_ids) {
                continue;
            }
            let trigger_event_id = run["trigger_event_id"]
                .as_str()
                .expect("matched run has a trigger event id");
            let run_id = run
                .get("id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| CliError::Other("workflow run response is missing its id".into()))?;
            if !seen_run_ids.insert(run_id.to_string()) {
                continue;
            }
            matched_count = matched_count.saturating_add(1);
            if runs.len() < MAX_SAVED_MATCHES {
                runs.push(serde_json::json!({
                    "id": run.get("id"),
                    "workflow_id": run.get("workflow_id"),
                    "trigger_event_id": trigger_event_id,
                    "status": run.get("status"),
                    "current_step": run.get("current_step"),
                    "started_at": run.get("started_at"),
                    "completed_at": run.get("completed_at"),
                    "error_code": run.get("error_code"),
                    "created_at": run.get("created_at"),
                }));
            }
        }

        let Some(next) = response.get("next").filter(|value| !value.is_null()) else {
            break;
        };
        let before = next
            .get("before")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| CliError::Other("workflow-runs next cursor is missing before".into()))?;
        let before_id = next
            .get("before_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                CliError::Other("workflow-runs next cursor is missing before_id".into())
            })?;
        cursor = Some((before.to_string(), before_id.to_string()));
        if page + 1 == max_pages {
            history_truncated = true;
        }
    }

    Ok(WorkflowRunLookup {
        runs,
        matched_count,
        pages_checked,
        history_truncated,
        matches_truncated: matched_count as usize > MAX_SAVED_MATCHES,
    })
}

#[cfg(test)]
mod brief_lookup_tests {
    use super::run_trigger_matches;
    use serde_json::json;
    use std::collections::HashSet;

    #[test]
    fn trigger_join_requires_an_exact_source_event_match() {
        let source_ids = HashSet::from(["ab".repeat(32)]);
        assert!(run_trigger_matches(
            &json!({"trigger_event_id": "ABABABABABABABABABABABABABABABABABABABABABABABABABABABABABABABAB"}),
            &source_ids,
        ));
        assert!(!run_trigger_matches(
            &json!({"trigger_event_id": null}),
            &source_ids
        ));
        assert!(!run_trigger_matches(
            &json!({"trigger_event_id": "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd"}),
            &source_ids,
        ));
    }
}

/// Create a workflow — sign and submit a kind:30620 event.
pub async fn cmd_create_workflow(
    client: &BuzzClient,
    channel_id: &str,
    yaml: &str,
) -> Result<(), CliError> {
    let channel_uuid = parse_uuid(channel_id)?;
    let yaml_definition = read_or_stdin(yaml)?;

    let workflow_id = uuid::Uuid::new_v4();
    let builder = buzz_sdk::build_workflow_def(channel_uuid, workflow_id, &yaml_definition)
        .map_err(sdk_err)?;
    let event = client.sign_event(builder)?;

    let resp = client.submit_event(event).await?;
    let final_workflow_id = extract_relay_response_field(&resp, "workflow_id")
        .unwrap_or_else(|| workflow_id.to_string());
    print_create_response(&resp, "workflow_id", &final_workflow_id);
    Ok(())
}

/// Update a workflow — sign and submit an updated kind:30620 event with same d-tag.
pub async fn cmd_update_workflow(
    client: &BuzzClient,
    channel_id: &str,
    workflow_id: &str,
    yaml: &str,
) -> Result<(), CliError> {
    let channel_uuid = parse_uuid(channel_id)?;
    let wf_uuid = parse_uuid(workflow_id)?;
    let yaml_definition = read_or_stdin(yaml)?;

    let filter = serde_json::json!({
        "kinds": [30620],
        "#d": [workflow_id]
    });
    let resp = client.query(&filter).await?;
    let events: Vec<serde_json::Value> = serde_json::from_str(&resp).unwrap_or_default();
    let expected_revision = events
        .first()
        .and_then(|event| event.get("id"))
        .and_then(|id| id.as_str())
        .ok_or_else(|| CliError::NotFound(format!("workflow {workflow_id} not found")))?;

    let builder =
        buzz_sdk::build_workflow_update(channel_uuid, wf_uuid, &yaml_definition, expected_revision)
            .map_err(sdk_err)?;
    let event = client.sign_event(builder)?;

    let resp = client.submit_event(event).await?;
    println!("{}", normalize_write_response(&resp));
    Ok(())
}

/// Delete a workflow — sign and submit a kind:5 deletion event.
pub async fn cmd_delete_workflow(client: &BuzzClient, workflow_id: &str) -> Result<(), CliError> {
    let wf_uuid = parse_uuid(workflow_id)?;
    let keys = client.keys();

    let builder =
        buzz_sdk::build_workflow_delete(&keys.public_key().to_hex(), wf_uuid).map_err(sdk_err)?;
    let event = client.sign_event(builder)?;

    let resp = client.submit_event(event).await?;
    println!("{}", normalize_write_response(&resp));
    Ok(())
}

/// Trigger a workflow — sign and submit a kind:46020 event.
///
/// When `inputs` is provided, it is parsed as a JSON object and used as the
/// event content (MCP parity). When omitted, the event content is `{}`.
pub async fn cmd_trigger_workflow(
    client: &BuzzClient,
    workflow_id: &str,
    inputs: Option<&str>,
) -> Result<(), CliError> {
    let wf_uuid = parse_uuid(workflow_id)?;

    if let Some(raw) = inputs {
        // Parse and validate it is a JSON object, then build the event manually
        // so we can embed the inputs as the event content.
        let parsed: serde_json::Value = serde_json::from_str(raw)
            .map_err(|e| CliError::Usage(format!("--inputs is not valid JSON: {e}")))?;
        if !parsed.is_object() {
            return Err(CliError::Usage("--inputs must be a JSON object".into()));
        }
        let content = serde_json::to_string(&parsed).unwrap_or_default();
        use nostr::{EventBuilder, Kind, Tag};
        let tags = vec![Tag::parse(["d", &wf_uuid.to_string()])
            .map_err(|e| CliError::Other(format!("tag error: {e}")))?];
        let builder = EventBuilder::new(
            Kind::Custom(buzz_sdk::kind::KIND_WORKFLOW_TRIGGER as u16),
            &content,
        )
        .tags(tags);
        let event = client.sign_event(builder)?;
        let resp = client.submit_event(event).await?;
        println!("{}", normalize_write_response(&resp));
    } else {
        let builder = buzz_sdk::build_workflow_trigger(wf_uuid).map_err(sdk_err)?;
        let event = client.sign_event(builder)?;
        let resp = client.submit_event(event).await?;
        println!("{}", normalize_write_response(&resp));
    }
    Ok(())
}

/// Approve or deny a workflow step — sign and submit a kind:46030 (grant) or 46031 (deny) event.
pub async fn cmd_approve_step(
    client: &BuzzClient,
    approval_token: &str,
    approved: bool,
    note: Option<&str>,
) -> Result<(), CliError> {
    validate_uuid(approval_token)?;

    let content = note.unwrap_or("");

    // The relay expects d-tag = hex(SHA256(token)), not the raw token UUID.
    let token_hash = hex::encode(Sha256::digest(approval_token.as_bytes()));
    let builder =
        buzz_sdk::build_workflow_approval(&token_hash, approved, content).map_err(sdk_err)?;
    let event = client.sign_event(builder)?;

    let resp = client.submit_event(event).await?;
    println!("{}", normalize_write_response(&resp));
    Ok(())
}

pub async fn dispatch(cmd: crate::WorkflowsCmd, client: &BuzzClient) -> Result<(), CliError> {
    use crate::WorkflowsCmd;
    match cmd {
        WorkflowsCmd::List { channel } => cmd_list_workflows(client, &channel).await,
        WorkflowsCmd::Get { workflow } => cmd_get_workflow(client, &workflow).await,
        WorkflowsCmd::Create { channel, yaml } => {
            cmd_create_workflow(client, &channel, &yaml).await
        }
        WorkflowsCmd::Update {
            channel,
            workflow,
            yaml,
        } => cmd_update_workflow(client, &channel, &workflow, &yaml).await,
        WorkflowsCmd::Delete { workflow } => cmd_delete_workflow(client, &workflow).await,
        WorkflowsCmd::Trigger { workflow, inputs } => {
            cmd_trigger_workflow(client, &workflow, inputs.as_deref()).await
        }
        WorkflowsCmd::Runs { workflow, limit } => {
            cmd_get_workflow_runs(client, &workflow, limit).await
        }
        WorkflowsCmd::Approve {
            token,
            approved,
            note,
        } => {
            // approved is already a bool — no parse_bool_flag needed
            cmd_approve_step(client, &token, approved, note.as_deref()).await
        }
    }
}
