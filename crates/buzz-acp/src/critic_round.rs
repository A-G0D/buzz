//! Bounded local coordinator for independent, tool-free critic passes.

use std::{collections::HashSet, time::Duration};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    task::JoinSet,
};

use crate::critic_worker::{
    CriticRole, CriticThinkingEffort, CriticWorkerRequest, CriticWorkerResponse,
};

const MAX_REVIEWERS: usize = 3;
const MAX_OBJECTIVE_BYTES: usize = 4 * 1024;
const MAX_SCOPE_BYTES: usize = 2 * 1024;
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024;
const MAX_ROUND_REQUEST_BYTES: usize = 96 * 1024;
const MAX_ROUND_RESPONSE_BYTES: usize = 96 * 1024;
const MIN_OUTPUT_TOKENS: u32 = 64;
const DEFAULT_OUTPUT_TOKENS: u32 = 2048;
const MAX_OUTPUT_TOKENS: u32 = 2048;
const MIN_TIME_LIMIT_SECONDS: u64 = 15;
const DEFAULT_TIME_LIMIT_SECONDS: u64 = 120;
const MAX_TIME_LIMIT_SECONDS: u64 = 120;
const WORKER_STARTUP_GRACE_SECONDS: u64 = 15;
const MAX_ESTIMATED_ROUND_COST_MICROUSD: u64 = 1_000_000_000_000;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CriticRoundRequest {
    version: u8,
    objective: String,
    scope: String,
    snapshot: String,
    roles: Vec<CriticRole>,
    max_output_tokens: Option<u32>,
    time_limit_seconds: Option<u64>,
    thinking_effort: Option<CriticThinkingEffort>,
    estimated_round_cost_budget_microusd: Option<u64>,
    #[serde(default)]
    reviewer_cost_limits: Vec<CriticReviewerCostLimit>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CriticReviewerCostLimit {
    role: CriticRole,
    estimated_cost_limit_microusd: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CriticRoundResponse {
    version: u8,
    snapshot_sha256: String,
    limits: CriticLimits,
    reviewers: Vec<CriticResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CriticLimits {
    maximum_reviewers: usize,
    output_tokens_per_reviewer: u32,
    time_limit_seconds_per_reviewer: u64,
    thinking_effort_requested: Option<CriticThinkingEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    estimated_round_cost_budget_microusd: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CriticResult {
    role: String,
    status: String,
    output: Option<String>,
    output_truncated: bool,
    stop_reason: Option<String>,
    candidate_id: Option<String>,
    provider_id: Option<String>,
    model_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    estimated_cost_limit_microusd: Option<u64>,
    elapsed_ms: Option<u64>,
    error_code: Option<String>,
}

/// Read a versioned bounded critic-round request from stdin and emit one JSON response.
pub(crate) async fn run_from_stdio() -> Result<()> {
    let mut input = Vec::new();
    tokio::io::stdin()
        .take((MAX_ROUND_REQUEST_BYTES + 1) as u64)
        .read_to_end(&mut input)
        .await
        .context("read critic round request")?;
    if input.len() > MAX_ROUND_REQUEST_BYTES {
        bail!("critic round request exceeds the size limit");
    }

    let request: CriticRoundRequest =
        serde_json::from_slice(&input).context("decode critic round request")?;
    let response = run_round(request, "buzz-agent").await?;
    let output = serde_json::to_vec(&response).context("encode critic round response")?;
    if output.len() > MAX_ROUND_RESPONSE_BYTES {
        bail!("critic round response exceeds the size limit");
    }
    let mut stdout = tokio::io::stdout();
    stdout.write_all(&output).await?;
    stdout.write_all(b"\n").await?;
    stdout.flush().await?;
    Ok(())
}

async fn run_round(
    request: CriticRoundRequest,
    agent_command: &str,
) -> Result<CriticRoundResponse> {
    validate_request(&request)?;
    let max_output_tokens = request.max_output_tokens.unwrap_or(DEFAULT_OUTPUT_TOKENS);
    let time_limit_seconds = request
        .time_limit_seconds
        .unwrap_or(DEFAULT_TIME_LIMIT_SECONDS);
    let snapshot_sha256 = hex::encode(Sha256::digest(request.snapshot.as_bytes()));
    let mut tasks = JoinSet::new();

    for role in request.roles.iter().copied() {
        let worker_request = CriticWorkerRequest {
            version: 2,
            role,
            objective: request.objective.clone(),
            scope: request.scope.clone(),
            snapshot: request.snapshot.clone(),
            snapshot_sha256: snapshot_sha256.clone(),
            max_output_tokens,
            time_limit_seconds,
            thinking_effort: request.thinking_effort,
            estimated_cost_limit_microusd: request
                .reviewer_cost_limits
                .iter()
                .find(|limit| limit.role == role)
                .map(|limit| limit.estimated_cost_limit_microusd),
        };
        let estimated_cost_limit_microusd = worker_request.estimated_cost_limit_microusd;
        let agent_command = agent_command.to_owned();
        tasks.spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(
                    worker_request.time_limit_seconds + WORKER_STARTUP_GRACE_SECONDS,
                ),
                crate::critic_worker::run_one_with_agent_command(worker_request, &agent_command),
            )
            .await
            .unwrap_or(Err("worker_timeout"));
            (role, result, estimated_cost_limit_microusd)
        });
    }

    let mut reviewers = Vec::with_capacity(request.roles.len());
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((role, Ok(response), _)) => reviewers.push(response_into_result(response, role)),
            Ok((role, Err(code), estimated_cost_limit_microusd)) => {
                reviewers.push(failed_result(role, code, estimated_cost_limit_microusd))
            }
            Err(_) => bail!("critic reviewer task failed unexpectedly"),
        }
    }
    reviewers.sort_by(|left, right| left.role.cmp(&right.role));

    Ok(CriticRoundResponse {
        version: 1,
        snapshot_sha256,
        limits: CriticLimits {
            maximum_reviewers: MAX_REVIEWERS,
            output_tokens_per_reviewer: max_output_tokens,
            time_limit_seconds_per_reviewer: time_limit_seconds,
            thinking_effort_requested: request.thinking_effort,
            estimated_round_cost_budget_microusd: request.estimated_round_cost_budget_microusd,
        },
        reviewers,
    })
}

fn validate_request(request: &CriticRoundRequest) -> Result<()> {
    if request.version != 1
        || request.objective.trim().is_empty()
        || request.scope.trim().is_empty()
        || request.snapshot.trim().is_empty()
        || request.objective.len() > MAX_OBJECTIVE_BYTES
        || request.scope.len() > MAX_SCOPE_BYTES
        || request.snapshot.len() > MAX_SNAPSHOT_BYTES
        || request.objective.contains('\0')
        || request.scope.contains('\0')
        || request.snapshot.contains('\0')
    {
        bail!("critic round text is empty, invalid, or exceeds its size limit");
    }
    if request
        .max_output_tokens
        .is_some_and(|value| !(MIN_OUTPUT_TOKENS..=MAX_OUTPUT_TOKENS).contains(&value))
        || request.time_limit_seconds.is_some_and(|value| {
            !(MIN_TIME_LIMIT_SECONDS..=MAX_TIME_LIMIT_SECONDS).contains(&value)
        })
    {
        bail!("critic round limits exceed the supported range");
    }
    if request.roles.is_empty() || request.roles.len() > MAX_REVIEWERS {
        bail!("critic rounds support one to three reviewers");
    }
    let mut seen = HashSet::new();
    if !request.roles.iter().all(|role| seen.insert(*role)) {
        bail!("critic role is duplicated");
    }
    validate_cost_limits(request)?;
    Ok(())
}

fn validate_cost_limits(request: &CriticRoundRequest) -> Result<()> {
    let Some(round_budget) = request.estimated_round_cost_budget_microusd else {
        if !request.reviewer_cost_limits.is_empty() {
            bail!("reviewer cost limits require an aggregate critic budget");
        }
        return Ok(());
    };
    if round_budget > MAX_ESTIMATED_ROUND_COST_MICROUSD
        || request.reviewer_cost_limits.len() != request.roles.len()
    {
        bail!("critic round cost limits are invalid");
    }
    let mut sorted_roles = request.roles.clone();
    sorted_roles.sort_by_key(|role| role.label());
    let role_count = u64::try_from(sorted_roles.len()).context("critic role count")?;
    let base = round_budget / role_count;
    let remainder =
        usize::try_from(round_budget % role_count).context("critic budget remainder")?;
    let mut limits = HashSet::new();
    let mut total = 0u64;
    for (index, role) in sorted_roles.into_iter().enumerate() {
        let Some(limit) = request
            .reviewer_cost_limits
            .iter()
            .find(|limit| limit.role == role)
        else {
            bail!("critic reviewer cost limit is missing");
        };
        let maximum = base + u64::from(index < remainder);
        if limit.estimated_cost_limit_microusd > maximum || !limits.insert(limit.role) {
            bail!("critic reviewer cost limit exceeds its aggregate allocation");
        }
        total = total
            .checked_add(limit.estimated_cost_limit_microusd)
            .context("critic reviewer cost sum overflow")?;
    }
    if total > round_budget {
        bail!("critic reviewer cost limits exceed the aggregate budget");
    }
    Ok(())
}

fn response_into_result(
    response: CriticWorkerResponse,
    requested_role: CriticRole,
) -> CriticResult {
    CriticResult {
        role: response
            .role
            .map(CriticRole::label)
            .unwrap_or(requested_role.label())
            .into(),
        status: if response.ok { "completed" } else { "failed" }.into(),
        output: response.output,
        output_truncated: response.output_truncated,
        stop_reason: response.stop_reason,
        candidate_id: response.candidate_id,
        provider_id: response.provider_id,
        model_id: response.model_id,
        estimated_cost_limit_microusd: response.estimated_cost_limit_microusd,
        elapsed_ms: Some(response.elapsed_ms),
        error_code: response.error_code,
    }
}

fn failed_result(
    role: CriticRole,
    code: &'static str,
    estimated_cost_limit_microusd: Option<u64>,
) -> CriticResult {
    CriticResult {
        role: role.label().into(),
        status: "failed".into(),
        output: None,
        output_truncated: false,
        stop_reason: None,
        candidate_id: None,
        provider_id: None,
        model_id: None,
        estimated_cost_limit_microusd,
        elapsed_ms: None,
        error_code: Some(code.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(roles: &[CriticRole]) -> CriticRoundRequest {
        CriticRoundRequest {
            version: 1,
            objective: "Improve route safety".into(),
            scope: "Inspect the route changes".into(),
            snapshot: "diff --git a/a b/a\n+safe: true\n".into(),
            roles: roles.to_vec(),
            max_output_tokens: Some(64),
            time_limit_seconds: Some(15),
            thinking_effort: Some(CriticThinkingEffort::High),
            estimated_round_cost_budget_microusd: None,
            reviewer_cost_limits: Vec::new(),
        }
    }

    #[test]
    fn validates_nonempty_unique_roles_and_resource_caps() {
        assert!(validate_request(&request(&[CriticRole::Security])).is_ok());
        assert!(validate_request(&request(&[])).is_err());
        assert!(validate_request(&request(&[CriticRole::Security, CriticRole::Security])).is_err());
        assert!(validate_request(&request(&[
            CriticRole::Security,
            CriticRole::Correctness,
            CriticRole::Product,
            CriticRole::Architecture,
        ]))
        .is_err());

        let mut invalid = request(&[CriticRole::Security]);
        invalid.max_output_tokens = Some(MAX_OUTPUT_TOKENS + 1);
        assert!(validate_request(&invalid).is_err());
        invalid.max_output_tokens = Some(MIN_OUTPUT_TOKENS);
        invalid.time_limit_seconds = Some(MIN_TIME_LIMIT_SECONDS - 1);
        assert!(validate_request(&invalid).is_err());
    }

    #[test]
    fn accepts_only_role_limits_bounded_by_the_aggregate_split() {
        let mut request = request(&[CriticRole::Security, CriticRole::Correctness]);
        request.estimated_round_cost_budget_microusd = Some(101);
        request.reviewer_cost_limits = vec![
            CriticReviewerCostLimit {
                role: CriticRole::Correctness,
                estimated_cost_limit_microusd: 51,
            },
            CriticReviewerCostLimit {
                role: CriticRole::Security,
                estimated_cost_limit_microusd: 50,
            },
        ];
        assert!(validate_request(&request).is_ok());
        request.reviewer_cost_limits[0].estimated_cost_limit_microusd = 52;
        assert!(validate_request(&request).is_err());
        request.estimated_round_cost_budget_microusd = None;
        assert!(validate_request(&request).is_err());
    }
}
