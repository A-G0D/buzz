use std::{process::Stdio, time::Duration};

use rmcp::{model::CallToolResult, model::Content, ErrorData};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::{configure_no_window_async, shell::SharedState};

const MAX_RESULT_BYTES: usize = 128 * 1024;
const TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CoordinatorRunStatusParams {
    /// Buzz channel UUID containing the run's source thread.
    pub channel_id: String,
    /// Canonical root event ID used to authorize and scope the status lookup.
    pub thread_root_event_id: String,
    /// Stable coordinator-run UUID returned by `thread_brief` or `buzz runs list`.
    pub run_id: String,
}

pub async fn run(
    state: &SharedState,
    params: CoordinatorRunStatusParams,
) -> Result<CallToolResult, ErrorData> {
    if std::env::var_os("BUZZ_PRIVATE_KEY").is_none()
        || std::env::var_os("BUZZ_RELAY_URL").is_none()
    {
        return Err(ErrorData::invalid_params(
            "Coordinator run status requires the configured Buzz identity and relay.",
            None,
        ));
    }

    let channel_id = uuid::Uuid::parse_str(&params.channel_id)
        .map_err(|_| invalid_params("channel_id must be a UUID".into()))?
        .to_string();
    let thread_root_event_id =
        normalize_event_id(&params.thread_root_event_id).map_err(invalid_params)?;
    let run_id = uuid::Uuid::parse_str(&params.run_id)
        .map_err(|_| invalid_params("run_id must be a UUID".into()))?
        .to_string();

    let mut command = Command::new(state.shim.buzz_cli_path());
    command
        .args([
            "--format",
            "json",
            "runs",
            "show",
            "--run-id",
            &run_id,
            "--channel",
            &channel_id,
            "--thread-root",
            &thread_root_event_id,
        ])
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
            .ok_or_else(|| "Buzz run query did not expose stdout".to_string())?;
        let mut bytes = Vec::new();
        stdout
            .take(MAX_RESULT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| error.to_string())?;
        if bytes.len() > MAX_RESULT_BYTES {
            let _ = child.kill().await;
            return Err("The coordinator run record exceeded the MCP result limit.".to_string());
        }
        let status = child.wait().await.map_err(|error| error.to_string())?;
        Ok((status, bytes))
    })
    .await
    .map_err(|_| ErrorData::internal_error("Coordinator run status timed out.", None))?
    .map_err(|error| ErrorData::internal_error(format!("Buzz run query failed: {error}"), None))?;
    if !status.success() {
        return Err(ErrorData::internal_error(
            format!(
                "Buzz run query failed (exit {}). Check relay access and run identifiers.",
                status.code().unwrap_or(-1)
            ),
            None,
        ));
    }
    let status: serde_json::Value = serde_json::from_slice(&stdout).map_err(|error| {
        ErrorData::internal_error(format!("Buzz returned invalid run status: {error}"), None)
    })?;
    let json = serde_json::to_string(&status)
        .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
    Ok(CallToolResult::success(vec![Content::text(json)]))
}

fn normalize_event_id(value: &str) -> Result<String, String> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("thread_root_event_id must be 64 hexadecimal characters".into());
    }
    Ok(value.to_ascii_lowercase())
}

fn invalid_params(message: String) -> ErrorData {
    ErrorData::invalid_params(message, None)
}
