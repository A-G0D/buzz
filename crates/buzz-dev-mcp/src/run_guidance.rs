use std::{process::Stdio, time::Duration};

use rmcp::{model::CallToolResult, model::Content, ErrorData};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::{configure_no_window_async, run_status, shell::SharedState};

const MAX_GUIDANCE_BYTES: usize = 8 * 1024;
const MAX_RESULT_BYTES: usize = 32 * 1024;
const TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CoordinatorRunGuidanceParams {
    /// Buzz channel UUID containing the run's source thread.
    pub channel_id: String,
    /// Canonical root event ID used to authorize and scope the run.
    pub thread_root_event_id: String,
    /// Stable coordinator-run UUID returned by `thread_brief` or `buzz runs list`.
    pub run_id: String,
    /// Guidance to post as one reply in the run's source thread (maximum 8 KiB).
    pub guidance: String,
}

pub async fn run(
    state: &SharedState,
    params: CoordinatorRunGuidanceParams,
) -> Result<CallToolResult, ErrorData> {
    let guidance = params.guidance.trim();
    if guidance.is_empty() || guidance.len() > MAX_GUIDANCE_BYTES {
        return Err(ErrorData::invalid_params(
            "guidance must contain 1–8192 UTF-8 bytes after trimming",
            None,
        ));
    }
    let channel_id = uuid::Uuid::parse_str(&params.channel_id)
        .map_err(|_| ErrorData::invalid_params("channel_id must be a UUID", None))?
        .to_string();
    let run_id = uuid::Uuid::parse_str(&params.run_id)
        .map_err(|_| ErrorData::invalid_params("run_id must be a UUID", None))?
        .to_string();
    let thread_root_event_id = normalize_event_id(&params.thread_root_event_id)?;

    // Verify the exact run/thread scope before allowing this tool to publish.
    run_status::run(
        state,
        run_status::CoordinatorRunStatusParams {
            channel_id: channel_id.clone(),
            thread_root_event_id: thread_root_event_id.clone(),
            run_id: run_id.clone(),
        },
    )
    .await?;

    if std::env::var_os("BUZZ_PRIVATE_KEY").is_none()
        || std::env::var_os("BUZZ_RELAY_URL").is_none()
    {
        return Err(ErrorData::invalid_params(
            "Coordinator run guidance requires the configured Buzz identity and relay.",
            None,
        ));
    }

    let mut command = Command::new(state.shim.buzz_cli_path());
    command
        .args([
            "--format",
            "json",
            "messages",
            "send",
            "--channel",
            &channel_id,
            "--reply-to",
            &thread_root_event_id,
            "--content",
            "-",
        ])
        .env("PATH", &state.shim.path_env)
        .current_dir(&state.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    configure_no_window_async(&mut command);

    let output = tokio::time::timeout(TIMEOUT, async {
        let mut child = command.spawn().map_err(|error| error.to_string())?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Buzz message command did not expose stdin".to_string())?;
        stdin
            .write_all(guidance.as_bytes())
            .await
            .map_err(|error| error.to_string())?;
        drop(stdin);

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Buzz message command did not expose stdout".to_string())?;
        let mut bytes = Vec::new();
        stdout
            .take(MAX_RESULT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| error.to_string())?;
        if bytes.len() > MAX_RESULT_BYTES {
            let _ = child.kill().await;
            return Err(
                "The Buzz publish response exceeded the MCP result limit. Inspect the thread before retrying."
                    .to_string(),
            );
        }
        let status = child.wait().await.map_err(|error| error.to_string())?;
        Ok((status, bytes))
    })
    .await
    .map_err(|_| {
        ErrorData::internal_error(
            "Coordinator run guidance timed out. Inspect the thread before retrying because the publish may have succeeded.",
            None,
        )
    })?
    .map_err(|error| {
        ErrorData::internal_error(
            format!("Buzz guidance command failed: {error}. Inspect the thread before retrying."),
            None,
        )
    })?;

    let (status, stdout) = output;
    if !status.success() {
        return Err(ErrorData::internal_error(
            format!(
                "Buzz guidance publish failed (exit {}). Inspect the thread before retrying because the publish may have succeeded.",
                status.code().unwrap_or(-1)
            ),
            None,
        ));
    }
    let publish = parse_accepted_guidance_response(&stdout)
        .map_err(|error| ErrorData::internal_error(error, None))?;
    let json = serialize_guidance_receipt(&run_id, &channel_id, &thread_root_event_id, &publish)
        .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
    Ok(CallToolResult::success(vec![Content::text(json)]))
}

fn serialize_guidance_receipt(
    run_id: &str,
    channel_id: &str,
    thread_root_event_id: &str,
    publish: &AcceptedGuidancePublish,
) -> Result<String, serde_json::Error> {
    serde_json::to_string(&serde_json::json!({
        "run_id": run_id,
        "channel_id": channel_id,
        "thread_root_event_id": thread_root_event_id,
        "target_scope": "source_thread_subscribers",
        "delivery_stage": "relay_accepted",
        "relay_accepted": true,
        "event_id": publish.event_id,
        "agent_delivery": "unknown",
        "agent_observed": "unknown",
        "agent_applied": "unknown",
        "note": "The relay accepted one reply to the run's source thread. It may reach every subscribed worker in that thread; this does not target one worker or prove delivery.",
        "relay_response": publish.relay_response
    }))
}

fn normalize_event_id(value: &str) -> Result<String, ErrorData> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ErrorData::invalid_params(
            "thread_root_event_id must be 64 hexadecimal characters",
            None,
        ));
    }
    Ok(value.to_ascii_lowercase())
}

#[derive(Debug)]
struct AcceptedGuidancePublish {
    event_id: String,
    relay_response: serde_json::Value,
}

fn parse_accepted_guidance_response(bytes: &[u8]) -> Result<AcceptedGuidancePublish, String> {
    let relay_response: serde_json::Value = serde_json::from_slice(bytes).map_err(|error| {
        format!("Buzz returned invalid publish JSON: {error}. Check the thread before retrying.")
    })?;
    let accepted = relay_response
        .get("accepted")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if !accepted {
        return Err(
            "The relay did not confirm guidance acceptance. Check the thread before retrying."
                .into(),
        );
    }
    let event_id = relay_response
        .get("event_id")
        .and_then(serde_json::Value::as_str)
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| {
            "The relay accepted guidance but returned no valid event ID. Check the thread before retrying."
                .to_string()
        })?
        .to_ascii_lowercase();

    Ok(AcceptedGuidancePublish {
        event_id,
        relay_response,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_publish_requires_a_valid_event_id() {
        let event_id = "aB".repeat(32);
        let bytes = serde_json::to_vec(&serde_json::json!({
            "accepted": true,
            "event_id": event_id
        }))
        .unwrap();

        let publish = parse_accepted_guidance_response(&bytes).unwrap();
        assert_eq!(publish.event_id, "ab".repeat(32));
        assert_eq!(publish.relay_response["accepted"], true);
    }

    #[test]
    fn rejected_or_unconfirmed_publish_is_not_success() {
        for bytes in [br#"{"accepted":false}"#.as_slice(), br#"{}"#.as_slice()] {
            let error = parse_accepted_guidance_response(bytes).unwrap_err();
            assert!(error.contains("did not confirm guidance acceptance"));
            assert!(error.contains("Check the thread before retrying"));
        }
    }

    #[test]
    fn malformed_publish_event_id_is_not_success() {
        let bytes = br#"{"accepted":true,"event_id":"short"}"#;
        let error = parse_accepted_guidance_response(bytes).unwrap_err();

        assert!(error.contains("returned no valid event ID"));
        assert!(error.contains("Check the thread before retrying"));
    }

    #[test]
    fn guidance_receipt_serializes_relay_acceptance_without_claiming_application() {
        let event_id = "ab".repeat(32);
        let publish = AcceptedGuidancePublish {
            event_id: event_id.clone(),
            relay_response: serde_json::json!({ "accepted": true, "event_id": event_id }),
        };
        let serialized =
            serialize_guidance_receipt("run-id", "channel-id", &"cd".repeat(32), &publish).unwrap();
        let receipt: serde_json::Value = serde_json::from_str(&serialized).unwrap();

        assert_eq!(receipt["target_scope"], "source_thread_subscribers");
        assert_eq!(receipt["delivery_stage"], "relay_accepted");
        assert_eq!(receipt["relay_accepted"], true);
        assert_eq!(receipt["event_id"], publish.event_id);
        assert_eq!(receipt["agent_delivery"], "unknown");
        assert_eq!(receipt["agent_observed"], "unknown");
        assert_eq!(receipt["agent_applied"], "unknown");
        assert_eq!(
            receipt["note"],
            "The relay accepted one reply to the run's source thread. It may reach every subscribed worker in that thread; this does not target one worker or prove delivery."
        );
        assert_eq!(receipt["relay_response"], publish.relay_response);
    }
}
