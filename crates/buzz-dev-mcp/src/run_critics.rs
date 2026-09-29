//! Local, bounded dispatch for explicit critic requests.

use std::{
    collections::{HashMap, HashSet},
    process::Stdio,
    time::Duration,
};

use rmcp::{model::CallToolResult, model::Content, ErrorData};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    configure_no_window_async,
    shell::{KillGroup, SharedState},
};
use buzz_run_journal::{
    CriticReviewerRecord, CriticRoundSettings, CriticRouteProfileRef, RunJournal,
};

const MAX_REVIEWERS: usize = 3;
const MAX_OBJECTIVE_BYTES: usize = 4 * 1024;
const MAX_SCOPE_BYTES: usize = 2 * 1024;
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024;
const MAX_COORDINATOR_REQUEST_BYTES: usize = 96 * 1024;
const MAX_COORDINATOR_RESPONSE_BYTES: usize = 96 * 1024;
const MAX_RESULT_BYTES: usize = 96 * 1024;
const MIN_OUTPUT_TOKENS: u32 = 64;
const DEFAULT_OUTPUT_TOKENS: u32 = 2048;
const MAX_OUTPUT_TOKENS: u32 = 2048;
const MIN_TIME_LIMIT_SECONDS: u64 = 15;
const DEFAULT_TIME_LIMIT_SECONDS: u64 = 120;
const MAX_TIME_LIMIT_SECONDS: u64 = 120;
const WORKER_STARTUP_GRACE_SECONDS: u64 = 15;
const MAX_ESTIMATED_ROUND_COST_MICROUSD: u64 = 1_000_000_000_000;
const ROUTE_PROFILE_ENV: &str = "BUZZ_AGENT_ROUTE_PROFILE_JSON";
const ROUTE_COST_BUDGET_ENV: &str = "BUZZ_AGENT_ROUTE_COST_BUDGET_MICROUSD";

#[derive(Debug, Deserialize)]
struct CriticBudgetRouteProfile {
    version: u16,
    max_turn_cost_microusd: Option<u64>,
    profile_id: Option<String>,
    profile_version: Option<u32>,
    profile_hash: Option<String>,
    candidates: Vec<CriticBudgetRouteCandidate>,
}

#[derive(Debug, Deserialize)]
struct CriticBudgetRouteCandidate {
    data_location: String,
    input_cost_microusd_per_million_tokens: Option<u64>,
    output_cost_microusd_per_million_tokens: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunCriticsParams {
    /// The original user objective the reviewers should evaluate against.
    pub objective: String,
    /// What to inspect and which questions the review should answer.
    pub scope: String,
    /// Frozen text snapshot, such as a patch, design, or selected files.
    pub snapshot: String,
    /// One to three roles: correctness, security, architecture, ui_accessibility, performance, product.
    pub roles: Vec<String>,
    /// Requested response-token cap per reviewer, from 64 through 2048. Defaults to 2048.
    pub max_output_tokens: Option<u32>,
    /// Requested model-turn wall-clock cap per reviewer, from 15 through 120 seconds. Defaults to 120.
    pub time_limit_seconds: Option<u64>,
    /// Optional requested reasoning effort. A provider may reject, clamp, or ignore it.
    pub thinking_effort: Option<CriticThinkingEffort>,
    /// Optional estimated aggregate round ceiling, as plain USD text with up to six decimals.
    /// Requires a configured Local route profile with price rates for every candidate.
    pub estimated_round_cost_budget_usd: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CriticThinkingEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
enum CriticRole {
    Correctness,
    Security,
    Architecture,
    UiAccessibility,
    Performance,
    Product,
}

impl CriticRole {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "correctness" => Some(Self::Correctness),
            "security" => Some(Self::Security),
            "architecture" => Some(Self::Architecture),
            "ui_accessibility" => Some(Self::UiAccessibility),
            "performance" => Some(Self::Performance),
            "product" => Some(Self::Product),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Correctness => "correctness",
            Self::Security => "security",
            Self::Architecture => "architecture",
            Self::UiAccessibility => "ui_accessibility",
            Self::Performance => "performance",
            Self::Product => "product",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct CriticCoordinatorRequest {
    version: u8,
    objective: String,
    scope: String,
    snapshot: String,
    roles: Vec<CriticRole>,
    max_output_tokens: Option<u32>,
    time_limit_seconds: Option<u64>,
    thinking_effort: Option<CriticThinkingEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    estimated_round_cost_budget_microusd: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    reviewer_cost_limits: Vec<CriticReviewerCostLimit>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
struct CriticReviewerCostLimit {
    role: CriticRole,
    estimated_cost_limit_microusd: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CriticCoordinatorResponse {
    version: u8,
    snapshot_sha256: String,
    limits: CriticCoordinatorLimits,
    reviewers: Vec<CriticResult>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CriticCoordinatorLimits {
    maximum_reviewers: usize,
    output_tokens_per_reviewer: u32,
    time_limit_seconds_per_reviewer: u64,
    thinking_effort_requested: Option<CriticThinkingEffort>,
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
    #[serde(default)]
    route_profile: Option<CriticRouteProfileRef>,
    estimated_cost_limit_microusd: Option<u64>,
    elapsed_ms: Option<u64>,
    error_code: Option<String>,
}

#[derive(Debug, Serialize)]
struct CriticRoundResult {
    round_id: Option<String>,
    ledger_status: &'static str,
    ledger_error_code: Option<&'static str>,
    snapshot_sha256: String,
    execution: &'static str,
    data_boundary: &'static str,
    independence: &'static str,
    limits: CriticLimits,
    reviewers: Vec<CriticResult>,
}

#[derive(Debug, Serialize)]
struct CriticLimits {
    maximum_reviewers: usize,
    output_tokens_per_reviewer: u32,
    time_limit_seconds_per_reviewer: u64,
    thinking_effort_requested: Option<CriticThinkingEffort>,
    estimated_round_cost_budget_microusd: Option<u64>,
    thinking_effort_note: &'static str,
}

pub async fn run(
    state: &SharedState,
    params: RunCriticsParams,
    cancellation: CancellationToken,
) -> Result<CallToolResult, ErrorData> {
    run_with_journal_opener(state, params, cancellation, RunJournal::open_default_scoped).await
}

async fn run_with_journal_opener<F>(
    state: &SharedState,
    params: RunCriticsParams,
    cancellation: CancellationToken,
    open_journal: F,
) -> Result<CallToolResult, ErrorData>
where
    F: FnOnce() -> Result<RunJournal, String>,
{
    let roles = validate(&params)?;
    let estimated_round_cost_budget_microusd =
        parse_estimated_round_cost_budget(params.estimated_round_cost_budget_usd.as_deref())
            .map_err(|message| ErrorData::invalid_params(message, None))?;
    let (reviewer_cost_limits, route_profile) = if let Some(budget_microusd) =
        estimated_round_cost_budget_microusd
    {
        let profile_json = std::env::var(ROUTE_PROFILE_ENV).map_err(|_| {
            ErrorData::invalid_params(
                "An estimated critic budget requires a configured Local route profile with pricing.",
                None,
            )
        })?;
        let inherited_budget = match std::env::var(ROUTE_COST_BUDGET_ENV) {
            Ok(value) => Some(value),
            Err(std::env::VarError::NotPresent) => None,
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(ErrorData::invalid_params(
                    "The inherited critic route cost ceiling is not valid text.",
                    None,
                ));
            }
        };
        allocate_reviewer_cost_limits(
            budget_microusd,
            &roles,
            &profile_json,
            inherited_budget.as_deref(),
        )
        .map_err(|message| ErrorData::invalid_params(message, None))?
    } else {
        (Vec::new(), current_route_profile_ref())
    };
    let max_output_tokens = params.max_output_tokens.unwrap_or(DEFAULT_OUTPUT_TOKENS);
    let time_limit_seconds = params
        .time_limit_seconds
        .unwrap_or(DEFAULT_TIME_LIMIT_SECONDS);
    let worker = state.shim.resolve_executable("buzz-acp").ok_or_else(|| {
        ErrorData::internal_error(
            "The bundled Buzz ACP coordinator is unavailable on the configured PATH.",
            None,
        )
    })?;
    let request = CriticCoordinatorRequest {
        version: 1,
        objective: params.objective.clone(),
        scope: params.scope.clone(),
        snapshot: params.snapshot.clone(),
        roles: roles.clone(),
        max_output_tokens: params.max_output_tokens,
        time_limit_seconds: params.time_limit_seconds,
        thinking_effort: params.thinking_effort,
        estimated_round_cost_budget_microusd,
        reviewer_cost_limits,
    };
    let expected_reviewer_cost_limits = request.reviewer_cost_limits.clone();
    let response = run_coordinator(
        worker,
        state.cwd.clone(),
        state.shim.path_env.clone(),
        request,
        time_limit_seconds,
        cancellation,
    )
    .await
    .map_err(|_| ErrorData::internal_error("Critic coordinator failed.", None))?;
    let snapshot_sha256 = hex::encode(Sha256::digest(params.snapshot.as_bytes()));
    validate_coordinator_response(
        &response,
        &roles,
        &snapshot_sha256,
        max_output_tokens,
        time_limit_seconds,
        params.thinking_effort,
        estimated_round_cost_budget_microusd,
        &expected_reviewer_cost_limits,
    )?;
    let mut reviewers = response.reviewers;
    for reviewer in &mut reviewers {
        reviewer.route_profile = route_profile.clone();
    }

    let (round_id, ledger_status, ledger_error_code) = persist_round(
        &params,
        &snapshot_sha256,
        max_output_tokens,
        time_limit_seconds,
        &reviewers,
        estimated_round_cost_budget_microusd,
        route_profile.clone(),
        open_journal,
    );

    let result = CriticRoundResult {
        round_id,
        ledger_status,
        ledger_error_code,
        snapshot_sha256,
        execution: "loopback-only Buzz Agent review mode",
        data_boundary: "Buzz connects only to configured loopback endpoints; forwarding or egress by that local service is not inspected",
        independence: "prompt-separated passes; shared model configuration, no cross-model independence claim",
        limits: CriticLimits {
            maximum_reviewers: MAX_REVIEWERS,
            output_tokens_per_reviewer: max_output_tokens,
            time_limit_seconds_per_reviewer: time_limit_seconds,
            thinking_effort_requested: params.thinking_effort,
            estimated_round_cost_budget_microusd,
            thinking_effort_note: "requested setting; provider support and effective level may differ",
        },
        reviewers,
    };
    let body = serde_json::to_string(&result)
        .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
    if body.len() > MAX_RESULT_BYTES {
        return Err(ErrorData::internal_error(
            "Critic results exceeded the response limit; narrow the snapshot or reviewer set.",
            None,
        ));
    }
    Ok(CallToolResult::success(vec![Content::text(body)]))
}

#[allow(clippy::too_many_arguments)] // persists the independently bounded critic-round fields
fn persist_round<F>(
    params: &RunCriticsParams,
    snapshot_sha256: &str,
    max_output_tokens: u32,
    time_limit_seconds: u64,
    reviewers: &[CriticResult],
    estimated_round_cost_budget_microusd: Option<u64>,
    route_profile: Option<CriticRouteProfileRef>,
    open_journal: F,
) -> (Option<String>, &'static str, Option<&'static str>)
where
    F: FnOnce() -> Result<RunJournal, String>,
{
    let journal = match open_journal() {
        Ok(journal) => journal,
        Err(_) => {
            return (
                None,
                "not_saved",
                Some("local_identity_or_storage_unavailable"),
            );
        }
    };
    let round_id = Uuid::new_v4().to_string();
    let stored_reviewers = reviewers
        .iter()
        .map(|reviewer| CriticReviewerRecord {
            role: reviewer.role.clone(),
            status: reviewer.status.clone(),
            output: reviewer.output.clone(),
            output_truncated: reviewer.output_truncated,
            stop_reason: reviewer.stop_reason.clone(),
            candidate_id: reviewer.candidate_id.clone(),
            provider_id: reviewer.provider_id.clone(),
            model_id: reviewer.model_id.clone(),
            route_profile: route_profile.clone(),
            estimated_cost_limit_microusd: reviewer.estimated_cost_limit_microusd,
            elapsed_ms: reviewer.elapsed_ms,
            error_code: reviewer.error_code.clone(),
        })
        .collect();
    let settings = CriticRoundSettings {
        max_output_tokens,
        time_limit_seconds,
        thinking_effort_requested: params.thinking_effort.map(|effort| {
            match effort {
                CriticThinkingEffort::None => "none",
                CriticThinkingEffort::Minimal => "minimal",
                CriticThinkingEffort::Low => "low",
                CriticThinkingEffort::Medium => "medium",
                CriticThinkingEffort::High => "high",
                CriticThinkingEffort::XHigh => "xhigh",
                CriticThinkingEffort::Max => "max",
            }
            .to_owned()
        }),
        estimated_round_cost_budget_microusd,
        route_profile,
        coordinator_guide: None,
    };
    let objective_sha256 = hex::encode(Sha256::digest(params.objective.as_bytes()));
    let scope_sha256 = hex::encode(Sha256::digest(params.scope.as_bytes()));
    match journal.record_critic_round(
        &round_id,
        snapshot_sha256,
        &objective_sha256,
        &scope_sha256,
        settings,
        stored_reviewers,
    ) {
        Ok(record) => (Some(record.round_id), "saved", None),
        Err(_) => (None, "not_saved", Some("critic_round_not_saved")),
    }
}

fn validate(params: &RunCriticsParams) -> Result<Vec<CriticRole>, ErrorData> {
    if params.objective.trim().is_empty()
        || params.scope.trim().is_empty()
        || params.snapshot.trim().is_empty()
        || params.objective.len() > MAX_OBJECTIVE_BYTES
        || params.scope.len() > MAX_SCOPE_BYTES
        || params.snapshot.len() > MAX_SNAPSHOT_BYTES
        || params.objective.contains('\0')
        || params.scope.contains('\0')
        || params.snapshot.contains('\0')
    {
        return Err(ErrorData::invalid_params(
            "Objective, scope, and snapshot must be non-empty text within their size limits.",
            None,
        ));
    }
    if params
        .max_output_tokens
        .is_some_and(|value| !(MIN_OUTPUT_TOKENS..=MAX_OUTPUT_TOKENS).contains(&value))
        || params.time_limit_seconds.is_some_and(|value| {
            !(MIN_TIME_LIMIT_SECONDS..=MAX_TIME_LIMIT_SECONDS).contains(&value)
        })
    {
        return Err(ErrorData::invalid_params(
            "Critic limits must be 64–2048 output tokens and 15–120 seconds per reviewer.",
            None,
        ));
    }
    if let Some(raw) = params.estimated_round_cost_budget_usd.as_deref() {
        parse_estimated_round_cost_budget(Some(raw))
            .map_err(|message| ErrorData::invalid_params(message, None))?;
    }
    if params.roles.is_empty() || params.roles.len() > MAX_REVIEWERS {
        return Err(ErrorData::invalid_params(
            format!("Choose between one and {MAX_REVIEWERS} critic roles."),
            None,
        ));
    }
    let mut seen = HashSet::new();
    let mut roles = Vec::with_capacity(params.roles.len());
    for raw in &params.roles {
        let role = CriticRole::parse(raw).ok_or_else(|| {
            ErrorData::invalid_params(
                format!("Unsupported critic role '{raw}'. Choose correctness, security, architecture, ui_accessibility, performance, or product."),
                None,
            )
        })?;
        if !seen.insert(role) {
            return Err(ErrorData::invalid_params(
                format!(
                    "Critic role '{}' was selected more than once.",
                    role.label()
                ),
                None,
            ));
        }
        roles.push(role);
    }
    Ok(roles)
}

fn parse_estimated_round_cost_budget(raw: Option<&str>) -> Result<Option<u64>, &'static str> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let (whole_text, fractional_text) = raw.split_once('.').unwrap_or((raw, ""));
    if whole_text.is_empty() && fractional_text.is_empty()
        || whole_text.bytes().any(|byte| !byte.is_ascii_digit())
        || fractional_text.len() > 6
        || fractional_text.bytes().any(|byte| !byte.is_ascii_digit())
    {
        return Err(
            "estimated_round_cost_budget_usd must be a non-negative decimal with up to six fractional digits.",
        );
    }
    let whole = if whole_text.is_empty() {
        0
    } else {
        whole_text
            .parse::<u64>()
            .map_err(|_| "estimated critic round budget is outside the supported range.")?
    };
    let fractional = if fractional_text.is_empty() {
        0
    } else {
        let padded = format!("{fractional_text:0<6}");
        padded
            .parse::<u64>()
            .map_err(|_| "estimated critic round budget is invalid.")?
    };
    let budget = whole
        .checked_mul(1_000_000)
        .and_then(|value| value.checked_add(fractional))
        .filter(|value| *value <= MAX_ESTIMATED_ROUND_COST_MICROUSD)
        .ok_or("estimated critic round budget must be between $0 and $1,000,000.")?;
    Ok(Some(budget))
}

fn allocate_reviewer_cost_limits(
    budget_microusd: u64,
    roles: &[CriticRole],
    profile_json: &str,
    inherited_budget: Option<&str>,
) -> Result<(Vec<CriticReviewerCostLimit>, Option<CriticRouteProfileRef>), &'static str> {
    if budget_microusd > MAX_ESTIMATED_ROUND_COST_MICROUSD
        || roles.is_empty()
        || roles.len() > MAX_REVIEWERS
    {
        return Err("Estimated critic budget or reviewer count is outside the supported range.");
    }
    let profile: CriticBudgetRouteProfile = serde_json::from_str(profile_json).map_err(|_| {
        "An estimated critic budget requires a valid Buzz Local route profile with complete pricing."
    })?;
    if profile.version != 1
        || profile.candidates.is_empty()
        || profile.candidates.iter().any(|candidate| {
            candidate.data_location != "local"
                || candidate.input_cost_microusd_per_million_tokens.is_none()
                || candidate.output_cost_microusd_per_million_tokens.is_none()
        })
    {
        return Err(
            "An estimated critic budget requires a Local route profile with input and output prices for every candidate.",
        );
    }
    let inherited_budget = inherited_budget
        .map(|value| {
            value
                .parse::<u64>()
                .ok()
                .filter(|value| *value <= MAX_ESTIMATED_ROUND_COST_MICROUSD)
                .ok_or("The inherited critic route cost ceiling is invalid.")
        })
        .transpose()?;
    let mut sorted_roles = roles.to_vec();
    sorted_roles.sort_by_key(|role| role.label());
    let role_count =
        u64::try_from(sorted_roles.len()).map_err(|_| "Critic role count is invalid.")?;
    let base = budget_microusd / role_count;
    let remainder = usize::try_from(budget_microusd % role_count)
        .map_err(|_| "Critic budget allocation is invalid.")?;
    let limits = sorted_roles
        .into_iter()
        .enumerate()
        .map(|(index, role)| {
            let allocated = base + u64::from(index < remainder);
            let mut effective = profile
                .max_turn_cost_microusd
                .map_or(allocated, |saved| saved.min(allocated));
            if let Some(inherited) = inherited_budget {
                effective = effective.min(inherited);
            }
            CriticReviewerCostLimit {
                role,
                estimated_cost_limit_microusd: effective,
            }
        })
        .collect();
    Ok((limits, route_profile_ref(&profile)))
}

fn current_route_profile_ref() -> Option<CriticRouteProfileRef> {
    let raw = std::env::var(ROUTE_PROFILE_ENV).ok()?;
    let profile: CriticBudgetRouteProfile = serde_json::from_str(&raw).ok()?;
    route_profile_ref(&profile)
}

fn route_profile_ref(profile: &CriticBudgetRouteProfile) -> Option<CriticRouteProfileRef> {
    let hash = profile.profile_hash.as_deref()?;
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(CriticRouteProfileRef {
        id: profile.profile_id.clone().filter(|id| !id.is_empty())?,
        version: profile.profile_version?,
        hash: hash.to_ascii_lowercase(),
    })
}

async fn run_coordinator(
    worker: std::path::PathBuf,
    workdir: std::path::PathBuf,
    path_env: String,
    request: CriticCoordinatorRequest,
    time_limit_seconds: u64,
    cancellation: CancellationToken,
) -> Result<CriticCoordinatorResponse, String> {
    let input = encode_coordinator_request(&request)?;
    let mut command = Command::new(worker);
    command
        .arg("critic-round")
        .current_dir(workdir)
        .env("PATH", path_env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    crate::shell::set_process_group(&mut command);
    configure_no_window_async(&mut command);
    let mut child = command
        .spawn()
        .map_err(|_| "worker_start_failed".to_owned())?;
    let mut kill_group = KillGroup::new(&child, child.id());

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "worker_stdin_unavailable".to_owned())?;
    tokio::time::timeout(Duration::from_secs(5), stdin.write_all(&input))
        .await
        .map_err(|_| "worker_request_timeout".to_owned())?
        .map_err(|_| "worker_request_failed".to_owned())?;
    drop(stdin);

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "worker_stdout_unavailable".to_owned())?;
    let output_task = tokio::spawn(async move {
        let mut output = Vec::new();
        stdout
            .take((MAX_COORDINATOR_RESPONSE_BYTES + 1) as u64)
            .read_to_end(&mut output)
            .await
            .map(|_| output)
    });

    let status = tokio::select! {
        biased;
        _ = cancellation.cancelled() => {
            kill_group.kill_immediate();
            let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
            output_task.abort();
            return Err("cancelled".into());
        }
        result = tokio::time::timeout(
            Duration::from_secs(time_limit_seconds + WORKER_STARTUP_GRACE_SECONDS),
            child.wait(),
        ) => match result {
            Ok(Ok(status)) => status,
            Ok(Err(_)) => {
                kill_group.kill_immediate();
                output_task.abort();
                return Err("worker_wait_failed".into());
            }
            Err(_) => {
                kill_group.kill_graceful().await;
                let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
                output_task.abort();
                return Err("worker_timeout".into());
            }
        }
    };
    kill_group.kill_graceful().await;
    kill_group.disarm();
    if !status.success() {
        output_task.abort();
        return Err("worker_failed".into());
    }
    let bytes = output_task
        .await
        .map_err(|_| "worker_output_failed".to_owned())?
        .map_err(|_| "worker_output_failed".to_owned())?;
    if bytes.len() > MAX_COORDINATOR_RESPONSE_BYTES {
        return Err("coordinator_response_too_large".into());
    }
    let response: CriticCoordinatorResponse =
        serde_json::from_slice(&bytes).map_err(|_| "coordinator_response_invalid".to_owned())?;
    Ok(response)
}

fn encode_coordinator_request(request: &CriticCoordinatorRequest) -> Result<Vec<u8>, String> {
    let input = serde_json::to_vec(request).map_err(|_| "request_encode_failed".to_owned())?;
    if input.len() > MAX_COORDINATOR_REQUEST_BYTES {
        return Err("coordinator_request_too_large".into());
    }
    Ok(input)
}

#[allow(clippy::too_many_arguments)] // validates the complete frozen round contract
fn validate_coordinator_response(
    response: &CriticCoordinatorResponse,
    roles: &[CriticRole],
    snapshot_sha256: &str,
    max_output_tokens: u32,
    time_limit_seconds: u64,
    thinking_effort: Option<CriticThinkingEffort>,
    estimated_round_cost_budget_microusd: Option<u64>,
    expected_cost_limits: &[CriticReviewerCostLimit],
) -> Result<(), ErrorData> {
    let expected_roles = roles
        .iter()
        .map(|role| role.label())
        .collect::<HashSet<_>>();
    let actual_roles = response
        .reviewers
        .iter()
        .map(|reviewer| reviewer.role.as_str())
        .collect::<HashSet<_>>();
    let expected_cost_limits = expected_cost_limits
        .iter()
        .map(|limit| (limit.role.label(), limit.estimated_cost_limit_microusd))
        .collect::<HashMap<_, _>>();
    if response.version != 1
        || response.snapshot_sha256 != snapshot_sha256
        || response.limits.maximum_reviewers != MAX_REVIEWERS
        || response.limits.output_tokens_per_reviewer != max_output_tokens
        || response.limits.time_limit_seconds_per_reviewer != time_limit_seconds
        || response.limits.thinking_effort_requested != thinking_effort
        || response.limits.estimated_round_cost_budget_microusd
            != estimated_round_cost_budget_microusd
        || expected_roles != actual_roles
        || response.reviewers.len() != roles.len()
        || response.reviewers.iter().any(|reviewer| {
            !matches!(reviewer.status.as_str(), "completed" | "failed")
                || reviewer
                    .output
                    .as_ref()
                    .is_some_and(|output| output.len() > 24 * 1024)
                || reviewer.estimated_cost_limit_microusd
                    != expected_cost_limits.get(reviewer.role.as_str()).copied()
        })
    {
        return Err(ErrorData::internal_error(
            "Critic coordinator response did not match the submitted round.",
            None,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(roles: &[&str]) -> RunCriticsParams {
        RunCriticsParams {
            objective: "Improve the route UI".into(),
            scope: "Inspect the changed screen".into(),
            snapshot: "diff --git a/a b/a\n+color: blue\n".into(),
            roles: roles.iter().map(|role| (*role).into()).collect(),
            max_output_tokens: None,
            time_limit_seconds: None,
            thinking_effort: None,
            estimated_round_cost_budget_usd: None,
        }
    }

    #[test]
    fn accepts_unique_known_roles_and_caps_reviewers() {
        assert_eq!(
            validate(&params(&["ui_accessibility", "product"]))
                .unwrap()
                .len(),
            2
        );
        assert!(validate(&params(&[
            "correctness",
            "security",
            "product",
            "performance"
        ]))
        .is_err());
        assert!(validate(&params(&["critic"])).is_err());
        assert!(validate(&params(&["security", "security"])).is_err());
    }

    #[test]
    fn parses_usd_budget_text_without_rounding() {
        assert_eq!(
            parse_estimated_round_cost_budget(Some(" 1.234567 ")),
            Ok(Some(1_234_567))
        );
        assert_eq!(
            parse_estimated_round_cost_budget(Some(".5")),
            Ok(Some(500_000))
        );
        assert_eq!(
            parse_estimated_round_cost_budget(Some("1.")),
            Ok(Some(1_000_000))
        );
        assert_eq!(parse_estimated_round_cost_budget(Some("")), Ok(None));
        assert!(parse_estimated_round_cost_budget(Some("1.0000001")).is_err());
        assert!(parse_estimated_round_cost_budget(Some("-1")).is_err());
        assert!(parse_estimated_round_cost_budget(Some("$1")).is_err());
        assert!(parse_estimated_round_cost_budget(Some("1000000.000001")).is_err());
    }

    #[test]
    fn aggregate_budget_is_split_and_clamped_to_route_and_inherited_limits() {
        let profile = serde_json::json!({
            "version": 1,
            "preference_order": ["local"],
            "max_turn_cost_microusd": 40,
            "profile_id": "local-profile",
            "profile_version": 1,
            "profile_hash": "a".repeat(64),
            "candidates": [{
                "id": "local",
                "provider": "openai",
                "model": "local-model",
                "data_location": "local",
                "input_cost_microusd_per_million_tokens": 1_000_000,
                "output_cost_microusd_per_million_tokens": 1_000_000
            }]
        })
        .to_string();
        let (limits, profile_ref) = allocate_reviewer_cost_limits(
            101,
            &[CriticRole::Security, CriticRole::Correctness],
            &profile,
            Some("30"),
        )
        .unwrap();
        assert_eq!(
            profile_ref,
            Some(CriticRouteProfileRef {
                id: "local-profile".into(),
                version: 1,
                hash: "a".repeat(64),
            })
        );
        assert_eq!(
            limits,
            vec![
                CriticReviewerCostLimit {
                    role: CriticRole::Correctness,
                    estimated_cost_limit_microusd: 30,
                },
                CriticReviewerCostLimit {
                    role: CriticRole::Security,
                    estimated_cost_limit_microusd: 30,
                },
            ]
        );
    }

    #[test]
    fn aggregate_budget_rejects_unpriced_or_nonlocal_routes() {
        let priced = serde_json::json!({
            "version": 1,
            "preference_order": ["local"],
            "candidates": [{
                "id": "local",
                "provider": "openai",
                "model": "local-model",
                "data_location": "local",
                "input_cost_microusd_per_million_tokens": 1,
                "output_cost_microusd_per_million_tokens": 1
            }]
        });
        let hosted = serde_json::json!({
            "version": 1,
            "preference_order": ["local"],
            "candidates": [{
                "id": "local",
                "provider": "openai",
                "model": "local-model",
                "data_location": "hosted",
                "input_cost_microusd_per_million_tokens": 1,
                "output_cost_microusd_per_million_tokens": 1
            }]
        });
        let unpriced = serde_json::json!({
            "version": 1,
            "preference_order": ["local"],
            "candidates": [{
                "id": "local",
                "provider": "openai",
                "model": "local-model",
                "data_location": "local"
            }]
        });
        for profile in [hosted, unpriced] {
            assert!(allocate_reviewer_cost_limits(
                10,
                &[CriticRole::Correctness],
                &profile.to_string(),
                None,
            )
            .is_err());
        }
        assert!(allocate_reviewer_cost_limits(
            10,
            &[CriticRole::Correctness],
            &priced.to_string(),
            Some("not-a-budget"),
        )
        .is_err());
    }

    #[test]
    fn accepts_bounded_runtime_controls_and_rejects_values_outside_hard_caps() {
        let mut p = params(&["correctness"]);
        p.max_output_tokens = Some(64);
        p.time_limit_seconds = Some(15);
        p.thinking_effort = Some(CriticThinkingEffort::High);
        assert!(validate(&p).is_ok());

        p.max_output_tokens = Some(63);
        assert!(validate(&p).is_err());
        p.max_output_tokens = Some(2049);
        assert!(validate(&p).is_err());
        p.max_output_tokens = None;
        p.time_limit_seconds = Some(14);
        assert!(validate(&p).is_err());
        p.time_limit_seconds = Some(121);
        assert!(validate(&p).is_err());
    }

    #[test]
    fn omitted_runtime_controls_preserve_the_existing_defaults() {
        let p = params(&["correctness"]);
        assert_eq!(p.max_output_tokens.unwrap_or(DEFAULT_OUTPUT_TOKENS), 2048);
        assert_eq!(
            p.time_limit_seconds.unwrap_or(DEFAULT_TIME_LIMIT_SECONDS),
            120
        );
        assert_eq!(p.thinking_effort, None);
    }

    #[test]
    fn rejects_empty_or_oversized_frozen_snapshots() {
        let mut p = params(&["correctness"]);
        p.snapshot = "  ".into();
        assert!(validate(&p).is_err());
        p.snapshot = "x".repeat(MAX_SNAPSHOT_BYTES + 1);
        assert!(validate(&p).is_err());
    }

    #[test]
    fn rejects_serialized_requests_that_expand_past_the_coordinator_limit() {
        let snapshot = "\u{1}".repeat(20 * 1024);
        let request = CriticCoordinatorRequest {
            version: 1,
            objective: "Review input validation".into(),
            scope: "Inspect the diff".into(),
            snapshot,
            roles: vec![CriticRole::Security],
            max_output_tokens: None,
            time_limit_seconds: None,
            thinking_effort: None,
            estimated_round_cost_budget_microusd: None,
            reviewer_cost_limits: Vec::new(),
        };
        assert_eq!(
            encode_coordinator_request(&request).unwrap_err(),
            "coordinator_request_too_large"
        );
    }

    #[test]
    fn stores_hashes_and_findings_in_the_supplied_local_journal() {
        let nest = tempfile::tempdir().unwrap();
        let journal =
            RunJournal::open_scoped(nest.path(), "ws://localhost:3000", &"d".repeat(64)).unwrap();
        let params = RunCriticsParams {
            objective: "original private objective".into(),
            scope: "private review scope".into(),
            snapshot: "private frozen source snapshot".into(),
            roles: vec!["security".into()],
            max_output_tokens: Some(512),
            time_limit_seconds: Some(45),
            thinking_effort: Some(CriticThinkingEffort::Medium),
            estimated_round_cost_budget_usd: Some("0.000010".into()),
        };
        let snapshot_sha256 = hex::encode(Sha256::digest(params.snapshot.as_bytes()));
        let profile_ref = Some(CriticRouteProfileRef {
            id: "local-profile".into(),
            version: 1,
            hash: "a".repeat(64),
        });
        let reviewers = vec![CriticResult {
            role: "security".into(),
            status: "completed".into(),
            output: Some("Review result stored locally.".into()),
            output_truncated: false,
            stop_reason: Some("end_turn".into()),
            candidate_id: Some("local-review".into()),
            provider_id: Some("openai".into()),
            model_id: Some("local-model".into()),
            route_profile: profile_ref.clone(),
            estimated_cost_limit_microusd: Some(10),
            elapsed_ms: Some(1_500),
            error_code: None,
        }];

        let (round_id, status, error_code) = persist_round(
            &params,
            &snapshot_sha256,
            512,
            45,
            &reviewers,
            Some(10),
            profile_ref,
            || Ok(journal),
        );
        assert_eq!(status, "saved");
        assert_eq!(error_code, None);

        let journal =
            RunJournal::open_scoped(nest.path(), "ws://localhost:3000", &"d".repeat(64)).unwrap();
        let record = journal
            .critic_round(round_id.as_deref().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(record.snapshot_sha256, snapshot_sha256);
        assert_eq!(
            record.objective_sha256,
            hex::encode(Sha256::digest(params.objective.as_bytes()))
        );
        assert_eq!(
            record.scope_sha256,
            hex::encode(Sha256::digest(params.scope.as_bytes()))
        );
        assert_eq!(
            record.settings.thinking_effort_requested.as_deref(),
            Some("medium")
        );
        assert_eq!(
            record.reviewers[0].output.as_deref(),
            Some("Review result stored locally.")
        );
        assert_eq!(
            record.settings.estimated_round_cost_budget_microusd,
            Some(10)
        );
        assert_eq!(
            record.settings.route_profile.as_ref().unwrap().id,
            "local-profile"
        );
        assert_eq!(
            record.reviewers[0].route_profile.as_ref().unwrap().id,
            "local-profile"
        );
        assert_eq!(record.reviewers[0].estimated_cost_limit_microusd, Some(10));
        let stored = serde_json::to_string(&record).unwrap();
        assert!(!stored.contains(&params.objective));
        assert!(!stored.contains(&params.scope));
        assert!(!stored.contains(&params.snapshot));
    }

    #[test]
    fn validates_coordinator_result_against_requested_roles_digest_and_limits() {
        let role = CriticRole::Security;
        let digest = "a".repeat(64);
        let mut response = CriticCoordinatorResponse {
            version: 1,
            snapshot_sha256: digest.clone(),
            limits: CriticCoordinatorLimits {
                maximum_reviewers: MAX_REVIEWERS,
                output_tokens_per_reviewer: 512,
                time_limit_seconds_per_reviewer: 45,
                thinking_effort_requested: Some(CriticThinkingEffort::High),
                estimated_round_cost_budget_microusd: None,
            },
            reviewers: vec![CriticResult {
                role: "security".into(),
                status: "completed".into(),
                output: Some("No findings.".into()),
                output_truncated: false,
                stop_reason: Some("end_turn".into()),
                candidate_id: Some("local".into()),
                provider_id: Some("openai".into()),
                model_id: Some("local-model".into()),
                route_profile: None,
                estimated_cost_limit_microusd: None,
                elapsed_ms: Some(4),
                error_code: None,
            }],
        };
        validate_coordinator_response(
            &response,
            &[role],
            &digest,
            512,
            45,
            Some(CriticThinkingEffort::High),
            None,
            &[],
        )
        .unwrap();
        assert!(validate_coordinator_response(
            &response,
            &[CriticRole::Product],
            &digest,
            512,
            45,
            Some(CriticThinkingEffort::High),
            None,
            &[],
        )
        .is_err());
        response.snapshot_sha256 = "b".repeat(64);
        assert!(validate_coordinator_response(
            &response,
            &[role],
            &digest,
            512,
            45,
            Some(CriticThinkingEffort::High),
            None,
            &[],
        )
        .is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dispatches_the_frozen_snapshot_and_requested_limits_to_the_resolved_worker() {
        use crate::{shell::SharedState, shim::Shim};
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let worker = directory.path().join("buzz-acp");
        std::fs::write(
            &worker,
            r##"#!/usr/bin/env python3
import hashlib
import json
import sys

assert sys.argv[1:] == ["critic-round"]
request = json.load(sys.stdin)
assert request["version"] == 1
assert request["max_output_tokens"] == 64
assert request["time_limit_seconds"] == 15
assert request["thinking_effort"] == "high"
assert "estimated_round_cost_budget_microusd" not in request
assert "reviewer_cost_limits" not in request
assert request["snapshot"] == "diff --git a/a b/a\n+color: blue\n"
role = request["roles"][0]
print(json.dumps({
    "version": 1,
    "snapshot_sha256": hashlib.sha256(request["snapshot"].encode()).hexdigest(),
    "limits": {
        "maximum_reviewers": 3,
        "output_tokens_per_reviewer": 64,
        "time_limit_seconds_per_reviewer": 15,
        "thinking_effort_requested": "high",
    },
    "reviewers": [{
        "role": role,
        "status": "completed",
        "output": "No verified defects.",
        "output_truncated": False,
        "stop_reason": "end_turn",
        "candidate_id": "mock-local",
        "provider_id": "openai",
        "model_id": "mock-local-model",
        "elapsed_ms": 3,
        "error_code": None,
    }],
}))
"##,
        )
        .unwrap();
        std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o700)).unwrap();

        let mut shim = Shim::install().unwrap();
        shim.path_env = std::env::join_paths(
            std::iter::once(directory.path().to_path_buf())
                .chain(std::env::split_paths(std::ffi::OsStr::new(&shim.path_env))),
        )
        .unwrap()
        .to_string_lossy()
        .into_owned();
        let state = SharedState::new(directory.path().to_path_buf(), shim).unwrap();
        let mut p = params(&["correctness"]);
        p.max_output_tokens = Some(64);
        p.time_limit_seconds = Some(15);
        p.thinking_effort = Some(CriticThinkingEffort::High);

        let result = run_with_journal_opener(&state, p, CancellationToken::new(), || {
            Err("journal disabled for unit test".into())
        })
        .await
        .unwrap();
        let text = result.content[0].as_text().unwrap().text.clone();
        let body: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            body["snapshot_sha256"],
            hex::encode(Sha256::digest("diff --git a/a b/a\n+color: blue\n"))
        );
        assert_eq!(body["limits"]["output_tokens_per_reviewer"], 64);
        assert_eq!(body["limits"]["time_limit_seconds_per_reviewer"], 15);
        assert_eq!(body["limits"]["thinking_effort_requested"], "high");
        assert_eq!(body["reviewers"][0]["status"], "completed");
        assert_eq!(body["reviewers"][0]["model_id"], "mock-local-model");
    }
}
