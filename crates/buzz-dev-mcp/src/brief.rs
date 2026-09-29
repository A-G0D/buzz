use std::{process::Stdio, time::Duration};

use rmcp::{model::CallToolResult, model::Content, ErrorData};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::process::Command;

use crate::{configure_no_window_async, shell::SharedState};

const DEFAULT_LIMIT: u32 = 10;
const MAX_LIMIT: u32 = 20;
const DEFAULT_DEPTH_LIMIT: u32 = 32;
const MAX_DEPTH_LIMIT: u32 = 64;
const MAX_RESULT_BYTES: usize = 128 * 1024;
const TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ThreadBriefParams {
    /// Buzz channel UUID containing the message thread.
    pub channel_id: String,
    /// A message event ID in the thread, or its canonical root event ID.
    pub event_id: String,
    /// Maximum reply events (defaults to 10; capped at 20).
    #[serde(default)]
    #[schemars(range(min = 1, max = 20))]
    pub limit: Option<u32>,
    /// Maximum reply nesting depth (defaults to 32; capped at 64).
    #[serde(default)]
    #[schemars(range(min = 1, max = 64))]
    pub depth_limit: Option<u32>,
    /// Continue using both values from the previous brief's status.next_cursor.
    #[serde(default)]
    pub cursor_created_at: Option<u64>,
    /// Continue using both values from the previous brief's status.next_cursor.
    #[serde(default)]
    pub cursor_event_id: Option<String>,
}

pub async fn run(
    state: &SharedState,
    params: ThreadBriefParams,
) -> Result<CallToolResult, ErrorData> {
    if std::env::var_os("BUZZ_PRIVATE_KEY").is_none()
        || std::env::var_os("BUZZ_RELAY_URL").is_none()
    {
        return Err(ErrorData::invalid_params(
            "Buzz thread briefs require the configured Buzz identity and relay.",
            None,
        ));
    }
    let args = build_args(params).map_err(invalid_params)?;
    let mut command = Command::new(state.shim.buzz_cli_path());
    command
        .args(args)
        .env("PATH", &state.shim.path_env)
        .current_dir(&state.cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    configure_no_window_async(&mut command);

    let output = tokio::time::timeout(TIMEOUT, command.output())
        .await
        .map_err(|_| ErrorData::internal_error("Buzz thread brief timed out.", None))?
        .map_err(|error| {
            ErrorData::internal_error(format!("could not start Buzz brief command: {error}"), None)
        })?;
    if !output.status.success() {
        return Err(ErrorData::internal_error(
            format!(
                "Buzz thread brief failed (exit {}). Check relay access and event identifiers.",
                output.status.code().unwrap_or(-1)
            ),
            None,
        ));
    }
    if output.stdout.len() > MAX_RESULT_BYTES {
        return Err(ErrorData::internal_error(
            "The thread brief exceeded the MCP result limit. Retry with a smaller limit or a narrower thread page.",
            None,
        ));
    }
    let brief: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        ErrorData::internal_error(format!("Buzz returned an invalid brief: {error}"), None)
    })?;
    let json = serde_json::to_string(&brief)
        .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
    Ok(CallToolResult::success(vec![Content::text(json)]))
}

fn build_args(params: ThreadBriefParams) -> Result<Vec<String>, String> {
    let channel_id = uuid::Uuid::parse_str(&params.channel_id)
        .map_err(|_| "channel_id must be a UUID".to_string())?
        .to_string();
    let event_id = normalize_event_id(&params.event_id)?;
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let depth_limit = params
        .depth_limit
        .unwrap_or(DEFAULT_DEPTH_LIMIT)
        .clamp(1, MAX_DEPTH_LIMIT);

    let mut args = vec![
        "--format".into(),
        "json".into(),
        "messages".into(),
        "brief".into(),
        "--channel".into(),
        channel_id,
        "--event".into(),
        event_id,
        "--limit".into(),
        limit.to_string(),
        "--depth-limit".into(),
        depth_limit.to_string(),
        "--managed-turns".into(),
    ];
    match (params.cursor_created_at, params.cursor_event_id) {
        (Some(created_at), Some(event_id)) => {
            let created_at = i64::try_from(created_at).map_err(|_| {
                "cursor_created_at exceeds the supported timestamp range".to_string()
            })?;
            args.extend([
                "--cursor-created-at".into(),
                created_at.to_string(),
                "--cursor-event-id".into(),
                normalize_event_id(&event_id)?,
            ]);
        }
        (None, None) => {}
        _ => return Err("cursor_created_at and cursor_event_id must be supplied together".into()),
    }
    Ok(args)
}

fn normalize_event_id(value: &str) -> Result<String, String> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("event identifiers must be 64 hexadecimal characters".into());
    }
    Ok(value.to_ascii_lowercase())
}

fn invalid_params(message: String) -> ErrorData {
    ErrorData::invalid_params(message, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> ThreadBriefParams {
        ThreadBriefParams {
            channel_id: "123e4567-e89b-12d3-a456-426614174000".into(),
            event_id: "A".repeat(64),
            limit: None,
            depth_limit: None,
            cursor_created_at: None,
            cursor_event_id: None,
        }
    }

    #[test]
    fn brief_arguments_are_bounded_and_request_managed_receipts() {
        let args = build_args(ThreadBriefParams {
            limit: Some(u32::MAX),
            depth_limit: Some(1000),
            ..params()
        })
        .unwrap();
        assert!(args.windows(2).any(|pair| pair == ["--limit", "20"]));
        assert!(args.windows(2).any(|pair| pair == ["--depth-limit", "64"]));
        assert!(args.iter().any(|arg| arg == "--managed-turns"));
        assert!(args.iter().any(|arg| arg == &"a".repeat(64)));
    }

    #[test]
    fn brief_arguments_reject_bad_targets_and_incomplete_cursors() {
        let mut bad_channel = params();
        bad_channel.channel_id = "not-a-uuid".into();
        assert!(build_args(bad_channel).is_err());

        let mut bad_event = params();
        bad_event.event_id = "../inject".into();
        assert!(build_args(bad_event).is_err());

        let mut incomplete_cursor = params();
        incomplete_cursor.cursor_created_at = Some(1);
        assert!(build_args(incomplete_cursor).is_err());

        let args = build_args(ThreadBriefParams {
            cursor_created_at: Some(12),
            cursor_event_id: Some("B".repeat(64)),
            ..params()
        })
        .unwrap();
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--cursor-created-at", "12"]));
        assert!(args.iter().any(|arg| arg == &"b".repeat(64)));
    }
}
