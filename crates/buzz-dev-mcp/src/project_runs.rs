use std::{process::Stdio, time::Duration};

use rmcp::{model::CallToolResult, model::Content, ErrorData};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::{configure_no_window_async, shell::SharedState};

const DEFAULT_LIMIT: u32 = 20;
const MAX_LIMIT: u32 = 20;
const MAX_RESULT_BYTES: usize = 64 * 1024;
const TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectCoordinatorRunListParams {
    /// NIP-MP project coordinate selector; current relay state is authoritative.
    pub project_coordinate: String,
    /// Candidate project-home channel UUID selector; current relay state is authoritative.
    pub home_channel_id: String,
    /// Candidate rows to inspect (defaults to 20; maximum 20).
    #[serde(default)]
    #[schemars(range(min = 1, max = 20))]
    pub limit: Option<u32>,
    /// Continue using both fields from the previous response's next_cursor.
    #[serde(default)]
    pub cursor_updated_at_ms: Option<i64>,
    /// Continue using both fields from the previous response's next_cursor.
    #[serde(default)]
    pub cursor_run_id: Option<String>,
}

pub async fn run(
    state: &SharedState,
    params: ProjectCoordinatorRunListParams,
) -> Result<CallToolResult, ErrorData> {
    if std::env::var_os("BUZZ_PRIVATE_KEY").is_none()
        || std::env::var_os("BUZZ_RELAY_URL").is_none()
    {
        return Err(ErrorData::invalid_params(
            "Project coordinator run listings require the configured Buzz identity and relay.",
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

    let (status, stdout) = tokio::time::timeout(TIMEOUT, async {
        let mut child = command.spawn().map_err(|error| error.to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Buzz project-run query did not expose stdout".to_string())?;
        let mut bytes = Vec::new();
        stdout
            .take(MAX_RESULT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| error.to_string())?;
        if bytes.len() > MAX_RESULT_BYTES {
            let _ = child.kill().await;
            return Err(
                "The project coordinator run page exceeded the MCP result limit.".to_string(),
            );
        }
        let status = child.wait().await.map_err(|error| error.to_string())?;
        Ok((status, bytes))
    })
    .await
    .map_err(|_| ErrorData::internal_error("Project coordinator run listing timed out.", None))?
    .map_err(|error| {
        ErrorData::internal_error(format!("Buzz project-run query failed: {error}"), None)
    })?;
    if !status.success() {
        return Err(ErrorData::internal_error(
            format!(
                "Buzz project-run listing failed (exit {}). Check relay access and project selectors.",
                status.code().unwrap_or(-1)
            ),
            None,
        ));
    }
    let page: serde_json::Value = serde_json::from_slice(&stdout).map_err(|error| {
        ErrorData::internal_error(
            format!("Buzz returned an invalid project-run page: {error}"),
            None,
        )
    })?;
    let json = serde_json::to_string(&page)
        .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
    Ok(CallToolResult::success(vec![Content::text(json)]))
}

fn build_args(params: ProjectCoordinatorRunListParams) -> Result<Vec<String>, String> {
    validate_project_coordinate(&params.project_coordinate)?;
    let home_channel_id = uuid::Uuid::parse_str(&params.home_channel_id)
        .map_err(|_| "home_channel_id must be a UUID".to_string())?
        .to_string();
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err("limit must be between 1 and 20".into());
    }
    let mut args = vec![
        "--format".into(),
        "json".into(),
        "runs".into(),
        "project-list".into(),
        "--project-coordinate".into(),
        params.project_coordinate,
        "--home-channel".into(),
        home_channel_id,
        "--limit".into(),
        limit.to_string(),
    ];
    match (params.cursor_updated_at_ms, params.cursor_run_id) {
        (Some(timestamp), Some(run_id)) if timestamp >= 0 => {
            let run_id = uuid::Uuid::parse_str(&run_id)
                .map_err(|_| "cursor_run_id must be a UUID".to_string())?
                .to_string();
            args.extend([
                "--cursor-updated-at-ms".into(),
                timestamp.to_string(),
                "--cursor-run-id".into(),
                run_id,
            ]);
        }
        (Some(_), Some(_)) => return Err("cursor_updated_at_ms must be nonnegative".into()),
        (None, None) => {}
        _ => return Err("both cursor fields must be supplied together".into()),
    }
    Ok(args)
}

fn validate_project_coordinate(value: &str) -> Result<(), String> {
    let mut parts = value.splitn(3, ':');
    let kind = parts.next();
    let owner = parts.next().unwrap_or_default();
    let slug = parts.next().unwrap_or_default();
    if kind != Some("30621")
        || owner.len() != 64
        || !owner.bytes().all(|byte| byte.is_ascii_hexdigit())
        || slug.is_empty()
        || slug.len() > 1024
        || slug.trim() != slug
        || slug.chars().any(char::is_control)
        || value.len() > 1100
    {
        return Err("project_coordinate must be a valid NIP-MP project coordinate".into());
    }
    Ok(())
}

fn invalid_params(message: String) -> ErrorData {
    ErrorData::invalid_params(message, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> ProjectCoordinatorRunListParams {
        ProjectCoordinatorRunListParams {
            project_coordinate: format!("30621:{}:demo", "a".repeat(64)),
            home_channel_id: "123e4567-e89b-12d3-a456-426614174001".into(),
            limit: None,
            cursor_updated_at_ms: None,
            cursor_run_id: None,
        }
    }

    #[test]
    fn project_run_list_args_are_bounded_and_cursors_are_paired() {
        let mut request = params();
        request.limit = Some(21);
        assert!(build_args(request).is_err());

        let mut request = params();
        request.cursor_run_id = Some(uuid::Uuid::new_v4().to_string());
        assert!(build_args(request).is_err());

        let mut request = params();
        request.cursor_updated_at_ms = Some(-1);
        request.cursor_run_id = Some(uuid::Uuid::new_v4().to_string());
        assert!(build_args(request).is_err());

        let args = build_args(params()).unwrap();
        assert_eq!(args[3], "project-list");
        assert_eq!(
            args[args.iter().position(|arg| arg == "--limit").unwrap() + 1],
            "20"
        );
    }

    #[test]
    fn project_run_list_rejects_invalid_selector_and_accepts_complete_cursor() {
        let mut request = params();
        request.project_coordinate = "30617:bad".into();
        assert!(build_args(request).is_err());

        let mut request = params();
        request.cursor_updated_at_ms = Some(42);
        request.cursor_run_id = Some("123e4567-e89b-12d3-a456-426614174000".into());
        let args = build_args(request).unwrap();
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--cursor-updated-at-ms", "42"]));
    }
}
