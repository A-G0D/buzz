//! Local, identity-scoped critic execution and review-history access.

use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager, State};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use buzz_agent_pkg::route_preview::{
    RouteProfileDataPolicy, RouteProfileLocation, RoutePromptProfileRef,
};
use buzz_agent_pkg::SYNTHETIC_PROBE_MAX_OUTPUT_TOKENS;

use crate::{
    app_state::AppState,
    managed_agents::{
        agent_route_profile::{
            resolve_saved_profile, ResolvedAgentRouteProfile, ROUTE_COST_BUDGET_ENV,
            ROUTE_PROFILE_JSON_ENV, ROUTE_PROFILE_PROVENANCE_HASH_ENV,
            ROUTE_PROFILE_PROVENANCE_ID_ENV, ROUTE_PROFILE_PROVENANCE_VERSION_ENV,
        },
        load_global_agent_config, nest_dir, reserve_critic_worker_processes, GlobalAgentConfig,
    },
};

const MAX_REVIEWERS: usize = 3;
const CRITIC_GUIDE_RELATIVE_PATH: &str = "AGENT_GUIDES/CRITICS.md";
const MAX_CRITIC_GUIDE_BYTES: usize = 32 * 1024;
const MAX_OBJECTIVE_BYTES: usize = 4 * 1024;
const MAX_SCOPE_BYTES: usize = 2 * 1024;
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024;
const MAX_COORDINATOR_REQUEST_BYTES: usize = 96 * 1024;
const MAX_COORDINATOR_RESPONSE_BYTES: usize = 96 * 1024;
const MAX_REVIEWER_OUTPUT_BYTES: usize = 24 * 1024;
const MIN_OUTPUT_TOKENS: u32 = 64;
const DEFAULT_OUTPUT_TOKENS: u32 = 2048;
const MAX_OUTPUT_TOKENS: u32 = 2048;
const MIN_TIME_LIMIT_SECONDS: u64 = 15;
const DEFAULT_TIME_LIMIT_SECONDS: u64 = 120;
const MAX_TIME_LIMIT_SECONDS: u64 = 120;
const WORKER_STARTUP_GRACE_SECONDS: u64 = 15;
const MAX_ESTIMATED_ROUND_COST_MICROUSD: u64 = 1_000_000_000_000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunCriticsParams {
    /// The original user objective reviewers should evaluate against.
    pub objective: String,
    /// What to inspect and which questions the review should answer.
    pub scope: String,
    /// Frozen text snapshot, such as a patch or selected files.
    pub snapshot: String,
    /// One to three roles: correctness, security, architecture, ui_accessibility, performance, product.
    pub roles: Vec<String>,
    /// Requested response-token cap per reviewer, from 64 through 2048.
    pub max_output_tokens: Option<u32>,
    /// Requested model-turn wall-clock cap per reviewer, from 15 through 120 seconds.
    pub time_limit_seconds: Option<u64>,
    /// Requested reasoning effort. Provider support and effective effort may differ.
    pub thinking_effort: Option<CriticThinkingEffort>,
    /// Exact resolved route-profile identity shown for each selected role.
    pub route_profiles: BTreeMap<String, buzz_run_journal::CriticRouteProfileRef>,
    /// Optional total estimated USD ceiling, in micro-USD, split across roles.
    pub estimated_round_cost_budget_microusd: Option<u64>,
    /// Hash of the exact local coordinator guide version shown in the dialog.
    /// The guide itself is never sent to the coordinator or worker processes.
    pub coordinator_guide_sha256: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CriticCoordinatorGuidePreview {
    pub path: String,
    pub sha256: String,
    pub byte_length: usize,
    pub text: String,
}

fn read_critic_coordinator_guide(root: &Path) -> Result<CriticCoordinatorGuidePreview, String> {
    let root_metadata = fs::symlink_metadata(root)
        .map_err(|_| "The active Buzz nest is unavailable.".to_string())?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err("The active Buzz nest is not a real directory.".into());
    }
    let canonical_root = fs::canonicalize(root)
        .map_err(|_| "The active Buzz nest could not be resolved safely.".to_string())?;
    let guide_dir = canonical_root.join("AGENT_GUIDES");
    let guide_path = guide_dir.join("CRITICS.md");
    let guide_dir_metadata = fs::symlink_metadata(&guide_dir)
        .map_err(|_| "The coordinator guide is missing from the active Buzz nest.".to_string())?;
    if guide_dir_metadata.file_type().is_symlink() || !guide_dir_metadata.is_dir() {
        return Err("The coordinator guide path is not a real directory.".into());
    }
    let guide_metadata = fs::symlink_metadata(&guide_path)
        .map_err(|_| "The coordinator guide is missing from the active Buzz nest.".to_string())?;
    if guide_metadata.file_type().is_symlink() || !guide_metadata.is_file() {
        return Err("The coordinator guide must be a regular, non-symlink file.".into());
    }
    let expected_path = canonical_root.join(CRITIC_GUIDE_RELATIVE_PATH);
    if fs::canonicalize(&guide_path).ok().as_deref() != Some(expected_path.as_path()) {
        return Err("The coordinator guide resolved to an unexpected path.".into());
    }
    if guide_metadata.len() > MAX_CRITIC_GUIDE_BYTES as u64 {
        return Err("The coordinator guide is too large to preview safely.".into());
    }

    let mut file = fs::File::open(&guide_path)
        .map_err(|_| "The coordinator guide could not be read.".to_string())?;
    let opened_metadata = file
        .metadata()
        .map_err(|_| "The coordinator guide could not be verified.".to_string())?;
    if !opened_metadata.is_file() || !same_file_metadata(&guide_metadata, &opened_metadata) {
        return Err("The coordinator guide changed target while being opened.".into());
    }
    if opened_metadata.len() > MAX_CRITIC_GUIDE_BYTES as u64 {
        return Err("The coordinator guide is too large to preview safely.".into());
    }
    let bytes = read_complete_critic_guide(&mut file, opened_metadata.len())?;
    let after_read = fs::symlink_metadata(&guide_path)
        .map_err(|_| "The coordinator guide changed while being read.".to_string())?;
    if after_read.file_type().is_symlink()
        || !after_read.is_file()
        || !same_file_metadata(&opened_metadata, &after_read)
        || fs::canonicalize(&guide_path).ok().as_deref() != Some(expected_path.as_path())
    {
        return Err("The coordinator guide path changed while being read.".into());
    }
    let byte_length = bytes.len();
    let sha256 = hex::encode(Sha256::digest(&bytes));
    let text = String::from_utf8(bytes)
        .map_err(|_| "The coordinator guide is not valid UTF-8.".to_string())?;
    Ok(CriticCoordinatorGuidePreview {
        path: CRITIC_GUIDE_RELATIVE_PATH.into(),
        sha256,
        byte_length,
        text,
    })
}

#[cfg(unix)]
fn same_file_metadata(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(windows)]
fn same_file_metadata(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    left.volume_serial_number().is_some()
        && left.volume_serial_number() == right.volume_serial_number()
        && left.file_index().is_some()
        && left.file_index() == right.file_index()
}

#[cfg(not(any(unix, windows)))]
fn same_file_metadata(_left: &fs::Metadata, _right: &fs::Metadata) -> bool {
    false
}

fn read_complete_critic_guide(reader: impl Read, expected_length: u64) -> Result<Vec<u8>, String> {
    if expected_length > MAX_CRITIC_GUIDE_BYTES as u64 {
        return Err("The coordinator guide is too large to preview safely.".into());
    }
    let mut bytes = Vec::with_capacity(expected_length as usize);
    reader
        .take((MAX_CRITIC_GUIDE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "The coordinator guide could not be read completely.".to_string())?;
    if bytes.len() > MAX_CRITIC_GUIDE_BYTES || bytes.len() as u64 != expected_length {
        return Err("The coordinator guide changed or was truncated while being read.".into());
    }
    Ok(bytes)
}

fn verify_critic_coordinator_guide(
    root: &Path,
    displayed_sha256: &str,
) -> Result<CriticCoordinatorGuidePreview, String> {
    if !is_lower_sha256(displayed_sha256) {
        return Err("Refresh the coordinator guide preview before running review.".into());
    }
    let current = read_critic_coordinator_guide(root)?;
    if current.sha256 != displayed_sha256 {
        return Err(
            "The coordinator guide changed after preview. Refresh it before running review.".into(),
        );
    }
    Ok(current)
}

fn is_lower_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CriticRouteProfilePreview {
    pub profile: buzz_run_journal::CriticRouteProfileRef,
    pub candidates: Vec<CriticRouteCandidatePreview>,
    pub estimated_cost_limit_microusd: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CriticRouteCandidatePreview {
    pub id: String,
    pub provider: String,
    pub model: String,
    pub configured: bool,
    pub cost_pricing_available: bool,
    pub prompt_profile: Option<RoutePromptProfileRef>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRouteCandidateTestReceipt {
    pub profile_id: String,
    pub profile_version: u32,
    pub profile_document_hash: String,
    pub resolved_profile_hash: String,
    pub candidate_id: String,
    pub provider_id: String,
    pub runtime_provider_id: Option<String>,
    pub requested_model_id: String,
    pub data_location: &'static str,
    pub endpoint_origin: Option<String>,
    pub status: &'static str,
    pub started_at: String,
    pub elapsed_ms: u64,
    pub output_token_cap: u32,
    pub timeout_seconds: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub response_marker_matched: Option<bool>,
    pub model_identity_observed: bool,
    pub identity_evidence: &'static str,
    pub target_prompt_profile_included: bool,
    pub fallback_count: u8,
    pub failure_class: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SyntheticProbeChildReceipt {
    status: String,
    provider: Option<String>,
    requested_model: Option<String>,
    response_marker_matched: Option<bool>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    total_tokens: Option<u64>,
    failure_class: Option<String>,
}

fn validate_candidate_test_destination(
    location: RouteProfileLocation,
    data_policy: RouteProfileDataPolicy,
    endpoint: &str,
    confirm_hosted: bool,
) -> Result<(url::Url, bool), String> {
    if location == RouteProfileLocation::Hosted
        && data_policy != RouteProfileDataPolicy::AllowHosted
    {
        return Err("Hosted candidate tests require an allow-hosted profile.".into());
    }
    let parsed = url::Url::parse(endpoint)
        .map_err(|_| "The selected provider endpoint is invalid.".to_string())?;
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err("The selected provider endpoint contains unsupported URL fields.".into());
    }
    match location {
        RouteProfileLocation::Local if !is_loopback_endpoint(endpoint) => {
            Err("A Local candidate must use a loopback endpoint.".into())
        }
        RouteProfileLocation::Hosted if parsed.scheme() != "https" => {
            Err("Hosted candidate tests require an HTTPS provider endpoint.".into())
        }
        RouteProfileLocation::Hosted => Ok((parsed, !confirm_hosted)),
        RouteProfileLocation::Local => Ok((parsed, false)),
    }
}

#[derive(Debug)]
struct ResolvedCriticRoute {
    preview: CriticRouteProfilePreview,
    agent_env: BTreeMap<String, String>,
    effective_cost_limit_microusd: Option<u64>,
}

type CriticProcessSpawner =
    Arc<dyn Fn(&mut Command) -> Result<tokio::process::Child, String> + Send + Sync>;

fn admitted_critic_process_spawner(app: AppHandle) -> CriticProcessSpawner {
    Arc::new(move |command| {
        let state = app.state::<AppState>();
        crate::managed_agents::with_memory_admission(
            &state.managed_agent_memory_admission,
            || crate::managed_agents::load_global_agent_resource_policy(&app),
            || crate::managed_agents::device_memory_snapshot().available_memory_bytes,
            || {
                command
                    .spawn()
                    .map_err(|_| "Critic coordinator could not be started.".to_string())
            },
        )
    })
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
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

impl CriticThinkingEffort {
    fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct CriticCoordinatorRequest {
    version: u8,
    objective: String,
    scope: String,
    snapshot: String,
    roles: Vec<CriticRole>,
    max_output_tokens: Option<u32>,
    time_limit_seconds: Option<u64>,
    thinking_effort: Option<CriticThinkingEffort>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CriticCoordinatorResponse {
    version: u8,
    snapshot_sha256: String,
    limits: CriticCoordinatorLimits,
    reviewers: Vec<CriticCoordinatorReviewer>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CriticCoordinatorLimits {
    maximum_reviewers: usize,
    output_tokens_per_reviewer: u32,
    time_limit_seconds_per_reviewer: u64,
    thinking_effort_requested: Option<CriticThinkingEffort>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CriticCoordinatorReviewer {
    role: String,
    status: String,
    output: Option<String>,
    output_truncated: bool,
    stop_reason: Option<String>,
    candidate_id: Option<String>,
    provider_id: Option<String>,
    model_id: Option<String>,
    elapsed_ms: Option<u64>,
    error_code: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CriticRunResult {
    round_id: Option<String>,
    ledger_status: &'static str,
    ledger_error_code: Option<&'static str>,
    snapshot_sha256: String,
    route_profiles: BTreeMap<String, buzz_run_journal::CriticRouteProfileRef>,
    execution: &'static str,
    data_boundary: &'static str,
    independence: &'static str,
    limits: CriticRunLimits,
    reviewers: Vec<buzz_run_journal::CriticReviewerRecord>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CriticRunLimits {
    maximum_reviewers: usize,
    output_tokens_per_reviewer: u32,
    time_limit_seconds_per_reviewer: u64,
    thinking_effort_requested: Option<CriticThinkingEffort>,
    thinking_effort_note: &'static str,
    estimated_round_cost_budget_microusd: Option<u64>,
}

#[derive(Debug, Clone)]
struct CriticJournalScope {
    nest_dir: PathBuf,
    relay_url: String,
    viewer: String,
}

impl CriticJournalScope {
    fn capture(state: &AppState) -> Result<Self, String> {
        let viewer = state
            .keys
            .lock()
            .map_err(|_| "cannot read local workspace identity".to_string())?
            .public_key()
            .to_hex();
        let relay_url = crate::relay::relay_ws_url_with_override(state);
        let nest_dir = nest_dir().ok_or("cannot resolve Buzz workspace")?;
        Ok(Self {
            nest_dir,
            relay_url,
            viewer,
        })
    }

    fn open(&self) -> Result<buzz_run_journal::RunJournal, String> {
        buzz_run_journal::RunJournal::open_scoped(&self.nest_dir, &self.relay_url, &self.viewer)
    }
}

fn open_critic_journal(state: &AppState) -> Result<buzz_run_journal::RunJournal, String> {
    CriticJournalScope::capture(state)?.open()
}

/// Read a small recent-history page without returning reviewer finding text.
#[tauri::command]
pub fn get_recent_critic_rounds(
    limit: Option<u32>,
    state: State<'_, AppState>,
) -> Result<Vec<buzz_run_journal::CriticRoundSummary>, String> {
    let limit = limit.unwrap_or(20).clamp(1, 50) as usize;
    open_critic_journal(&state)?.recent_critic_rounds(limit)
}

/// Read one complete critic record from the current identity's local journal.
#[tauri::command]
pub fn get_critic_round(
    round_id: String,
    state: State<'_, AppState>,
) -> Result<Option<buzz_run_journal::CriticRoundRecord>, String> {
    open_critic_journal(&state)?.critic_round(&round_id)
}

/// Resolve and validate an explicitly selected local critic route. The
/// preview contains only route identity and non-secret candidate metadata.
#[tauri::command]
pub fn preview_critic_route_profile(
    profile_id: String,
    app: AppHandle,
) -> Result<CriticRouteProfilePreview, String> {
    let config = load_global_agent_config(&app)?;
    Ok(resolve_critic_route_profile(&profile_id, &config)?.preview)
}

/// Send one fixed, source-free prompt through exactly one explicitly selected
/// saved route candidate. The child returns no model text or credential data.
#[tauri::command]
pub async fn test_agent_route_candidate(
    profile_id: String,
    candidate_id: String,
    expected_profile_version: u32,
    expected_profile_document_hash: String,
    confirm_hosted: bool,
    app: AppHandle,
) -> Result<AgentRouteCandidateTestReceipt, String> {
    if !is_lower_sha256(&expected_profile_document_hash) {
        return Err("Refresh this routing profile before testing a candidate.".into());
    }
    let profile = resolve_saved_profile(&profile_id)?;
    let document_hash = profile
        .document
        .profile_hash
        .as_deref()
        .ok_or_else(|| "This routing profile has no saved document identity.".to_string())?;
    if profile.identity.version != expected_profile_version
        || document_hash != expected_profile_document_hash
    {
        return Err("This routing profile changed. Reload it before testing a candidate.".into());
    }
    let candidate = profile
        .document
        .candidates
        .iter()
        .find(|candidate| candidate.id == candidate_id)
        .ok_or_else(|| {
            "The selected candidate is no longer in this routing profile.".to_string()
        })?;
    let data_location = match candidate.data_location {
        RouteProfileLocation::Local => "local",
        RouteProfileLocation::Hosted => "hosted",
    };
    let global = load_global_agent_config(&app)?;
    let provider_id = normalized_provider(&candidate.provider)?;
    let (endpoint, credentials, provider_env) = provider_settings(&provider_id, &global)?;
    if !credentials.iter().all(|key| {
        global
            .env_vars
            .get(*key)
            .is_some_and(|value| !value.trim().is_empty())
    }) {
        return Err("The selected provider is not configured in Global Agent Settings.".into());
    }
    let (parsed_endpoint, confirmation_required) = validate_candidate_test_destination(
        candidate.data_location,
        profile.document.data_policy,
        &endpoint,
        confirm_hosted,
    )?;
    let endpoint_origin = parsed_endpoint.origin().ascii_serialization();
    let document_hash = document_hash.to_owned();
    if confirmation_required {
        return Ok(AgentRouteCandidateTestReceipt {
            profile_id: profile.identity.id,
            profile_version: profile.identity.version,
            profile_document_hash: document_hash,
            resolved_profile_hash: profile.identity.hash,
            candidate_id: candidate.id.clone(),
            provider_id: candidate.provider.clone(),
            runtime_provider_id: Some(provider_id),
            requested_model_id: candidate.model.clone(),
            data_location,
            endpoint_origin: Some(endpoint_origin),
            status: "confirmation_required",
            started_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            elapsed_ms: 0,
            output_token_cap: SYNTHETIC_PROBE_MAX_OUTPUT_TOKENS,
            timeout_seconds: 30,
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            response_marker_matched: None,
            model_identity_observed: false,
            identity_evidence: "requested_configuration_only",
            target_prompt_profile_included: false,
            fallback_count: 0,
            failure_class: None,
        });
    }

    let worker = crate::managed_agents::resolve_command("buzz-agent")
        .ok_or_else(|| "The bundled Buzz Agent model-test runtime is unavailable.".to_string())?;
    let path_env = crate::managed_agents::readiness::cli_probe::augmented_path()
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    let started_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let started = std::time::Instant::now();
    let mut command = Command::new(worker);
    command
        .arg("synthetic-probe")
        .env_clear()
        .env("PATH", path_env)
        .env("BUZZ_AGENT_PROVIDER", &provider_id)
        .env("BUZZ_AGENT_MODEL", &candidate.model)
        .env(
            "BUZZ_AGENT_MAX_OUTPUT_TOKENS",
            SYNTHETIC_PROBE_MAX_OUTPUT_TOKENS.to_string(),
        )
        .env("BUZZ_AGENT_MAX_TOKEN_RECOVERIES", "0")
        .env("BUZZ_AGENT_LLM_TIMEOUT_SECS", "30")
        .env(
            "BUZZ_AGENT_PROBE_LOCAL",
            if candidate.data_location == RouteProfileLocation::Local {
                "1"
            } else {
                "0"
            },
        )
        .envs(provider_env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let child_result = tokio::time::timeout(Duration::from_secs(45), async {
        let mut child = command
            .spawn()
            .map_err(|_| "Buzz could not start the synthetic model test.".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Buzz could not read the synthetic model test receipt.".to_string())?;
        let mut receipt = Vec::new();
        stdout
            .take(4097)
            .read_to_end(&mut receipt)
            .await
            .map_err(|_| "Buzz could not read the synthetic model test receipt.".to_string())?;
        if receipt.len() > 4096 {
            return Err("The synthetic model test returned an oversized receipt.".to_string());
        }
        let status = child
            .wait()
            .await
            .map_err(|_| "Buzz could not finish the synthetic model test.".to_string())?;
        Ok::<_, String>((receipt, status))
    })
    .await
    .map_err(|_| "The synthetic model test exceeded its 45-second process limit.".to_string())??;
    let (output, status) = child_result;
    if !status.success() {
        return Err("The synthetic model test runtime failed.".into());
    }
    let child_receipt: SyntheticProbeChildReceipt = serde_json::from_slice(&output)
        .map_err(|_| "The synthetic model test returned an invalid receipt.".to_string())?;
    if child_receipt.provider.as_deref() != Some(provider_id.as_str())
        || child_receipt.requested_model.as_deref() != Some(candidate.model.as_str())
    {
        return Err("The model-test runtime identity did not match the selected candidate.".into());
    }
    let status = if child_receipt.status == "responded" {
        "responded"
    } else {
        "failed"
    };
    Ok(AgentRouteCandidateTestReceipt {
        profile_id: profile.identity.id,
        profile_version: profile.identity.version,
        profile_document_hash: document_hash,
        resolved_profile_hash: profile.identity.hash,
        candidate_id: candidate.id.clone(),
        provider_id: candidate.provider.clone(),
        runtime_provider_id: Some(provider_id),
        requested_model_id: candidate.model.clone(),
        data_location,
        endpoint_origin: Some(endpoint_origin),
        status,
        started_at,
        elapsed_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        output_token_cap: SYNTHETIC_PROBE_MAX_OUTPUT_TOKENS,
        timeout_seconds: 30,
        input_tokens: child_receipt.input_tokens,
        output_tokens: child_receipt.output_tokens,
        total_tokens: child_receipt.total_tokens,
        response_marker_matched: child_receipt.response_marker_matched,
        model_identity_observed: false,
        identity_evidence: "requested_configuration_only",
        target_prompt_profile_included: false,
        fallback_count: 0,
        failure_class: child_receipt.failure_class,
    })
}

/// Read the bounded canonical critic guide from the active Buzz nest.
#[tauri::command]
pub fn preview_critic_coordinator_guide() -> Result<CriticCoordinatorGuidePreview, String> {
    let root = nest_dir().ok_or_else(|| "The active Buzz nest is unavailable.".to_string())?;
    read_critic_coordinator_guide(&root)
}

/// Run a user-requested bounded review against one caller-frozen text snapshot.
#[tauri::command]
pub async fn run_critic_round(
    request_id: String,
    params: RunCriticsParams,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<CriticRunResult, String> {
    let request_id = validate_request_id(&request_id)?;
    let roles = validate(&params)?;
    let guide_root =
        nest_dir().ok_or_else(|| "The active Buzz nest is unavailable.".to_string())?;
    verify_critic_coordinator_guide(&guide_root, &params.coordinator_guide_sha256)?;
    let config = load_global_agent_config(&app)?;
    let mut routes = resolve_role_routes(&params.route_profiles, &roles, &config)?;
    apply_critic_round_cost_budget(
        params.estimated_round_cost_budget_microusd,
        &roles,
        &mut routes,
    )?;
    let route_profiles = routes
        .iter()
        .map(|(role, route)| (role.label().to_owned(), route.preview.profile.clone()))
        .collect::<BTreeMap<_, _>>();
    let max_output_tokens = params.max_output_tokens.unwrap_or(DEFAULT_OUTPUT_TOKENS);
    let time_limit_seconds = params
        .time_limit_seconds
        .unwrap_or(DEFAULT_TIME_LIMIT_SECONDS);
    let snapshot_sha256 = hex::encode(Sha256::digest(params.snapshot.as_bytes()));
    let identity_scope = CriticJournalScope::capture(&state)?;
    let worker = crate::managed_agents::resolve_command("buzz-acp")
        .ok_or_else(|| "Buzz ACP critic coordinator is unavailable.".to_string())?;
    let path_env = crate::managed_agents::readiness::cli_probe::augmented_path()
        .or_else(|| std::env::var("PATH").ok())
        .unwrap_or_default();
    let cancellation = CancellationToken::new();
    {
        let mut active = state
            .active_critic_rounds
            .lock()
            .map_err(|_| "critic run state is unavailable".to_string())?;
        if !active.is_empty() {
            return Err("A critic round is already running in Buzz.".into());
        }
        active.insert(request_id.clone(), cancellation.clone());
    }
    let _process_reservation = match reserve_critic_worker_processes(&app, roles.len()) {
        Ok(reservation) => reservation,
        Err(error) => {
            if let Ok(mut active) = state.active_critic_rounds.lock() {
                active.remove(&request_id);
            }
            return Err(error);
        }
    };

    let execution = run_critic_roles(
        admitted_critic_process_spawner(app.clone()),
        worker,
        identity_scope.nest_dir.clone(),
        path_env,
        &params,
        roles,
        &snapshot_sha256,
        &mut routes,
        time_limit_seconds,
        cancellation.clone(),
    )
    .await;
    if let Ok(mut active) = state.active_critic_rounds.lock() {
        active.remove(&request_id);
    }
    let reviewers = execution?
        .into_iter()
        .map(
            |(_role, route_profile, estimated_cost_limit_microusd, reviewer)| {
                buzz_run_journal::CriticReviewerRecord {
                    role: reviewer.role,
                    status: reviewer.status,
                    output: reviewer.output,
                    output_truncated: reviewer.output_truncated,
                    stop_reason: reviewer.stop_reason,
                    candidate_id: reviewer.candidate_id,
                    provider_id: reviewer.provider_id,
                    model_id: reviewer.model_id,
                    route_profile: Some(route_profile),
                    estimated_cost_limit_microusd,
                    elapsed_ms: reviewer.elapsed_ms,
                    error_code: reviewer.error_code,
                }
            },
        )
        .collect::<Vec<_>>();
    let (round_id, ledger_status, ledger_error_code) = persist_round(
        &identity_scope,
        &params,
        &snapshot_sha256,
        max_output_tokens,
        time_limit_seconds,
        &reviewers,
    );

    Ok(CriticRunResult {
        round_id,
        ledger_status,
        ledger_error_code,
        snapshot_sha256,
        route_profiles,
        execution: "separate loopback-only Buzz Agent review processes",
        data_boundary: "The snapshot is sent to each role's selected loopback endpoint. Forwarding or egress by local inference services is not inspected.",
        independence: "Each role has an independently selected route. Shared providers, services, or model weights can still make reviews correlated.",
        limits: CriticRunLimits {
            maximum_reviewers: MAX_REVIEWERS,
            output_tokens_per_reviewer: max_output_tokens,
            time_limit_seconds_per_reviewer: time_limit_seconds,
            thinking_effort_requested: params.thinking_effort,
            thinking_effort_note: "Requested setting; provider support and effective level may differ.",
            estimated_round_cost_budget_microusd: params.estimated_round_cost_budget_microusd,
        },
        reviewers,
    })
}

fn resolve_critic_route_profile(
    profile_id: &str,
    config: &GlobalAgentConfig,
) -> Result<ResolvedCriticRoute, String> {
    let profile = resolve_saved_profile(profile_id)?;
    build_critic_route_profile(profile, config)
}

fn resolve_role_routes(
    requested: &BTreeMap<String, buzz_run_journal::CriticRouteProfileRef>,
    roles: &[CriticRole],
    config: &GlobalAgentConfig,
) -> Result<BTreeMap<CriticRole, ResolvedCriticRoute>, String> {
    validate_route_assignment_keys(requested, roles)?;

    let mut routes = BTreeMap::new();
    for role in roles {
        let selected = requested
            .get(role.label())
            .ok_or_else(|| format!("Choose a Local route for the {} reviewer.", role.label()))?;
        let route = resolve_critic_route_profile(&selected.id, config)?;
        if route.preview.profile != *selected {
            return Err(format!(
                "The {} review route or prompt pack changed; refresh its preview and try again.",
                role.label()
            ));
        }
        routes.insert(*role, route);
    }
    Ok(routes)
}

fn validate_route_assignment_keys(
    requested: &BTreeMap<String, buzz_run_journal::CriticRouteProfileRef>,
    roles: &[CriticRole],
) -> Result<(), String> {
    if requested.len() != roles.len()
        || requested
            .keys()
            .any(|selected| !roles.iter().any(|role| role.label() == selected))
    {
        return Err("Choose exactly one route profile for every selected critic role.".into());
    }
    Ok(())
}

async fn run_critic_roles(
    spawner: CriticProcessSpawner,
    worker: PathBuf,
    workdir: PathBuf,
    path_env: String,
    params: &RunCriticsParams,
    roles: Vec<CriticRole>,
    snapshot_sha256: &str,
    routes: &mut BTreeMap<CriticRole, ResolvedCriticRoute>,
    time_limit_seconds: u64,
    cancellation: CancellationToken,
) -> Result<
    Vec<(
        CriticRole,
        buzz_run_journal::CriticRouteProfileRef,
        Option<u64>,
        CriticCoordinatorReviewer,
    )>,
    String,
> {
    let mut tasks = JoinSet::new();
    for role in roles {
        let Some(route) = routes.remove(&role) else {
            cancellation.cancel();
            tasks.abort_all();
            return Err(format!("The {} review route is unavailable.", role.label()));
        };
        let route_profile = route.preview.profile;
        let effective_cost_limit_microusd = route.effective_cost_limit_microusd;
        let request = CriticCoordinatorRequest {
            version: 1,
            objective: params.objective.clone(),
            scope: params.scope.clone(),
            snapshot: params.snapshot.clone(),
            roles: vec![role],
            max_output_tokens: params.max_output_tokens,
            time_limit_seconds: params.time_limit_seconds,
            thinking_effort: params.thinking_effort,
        };
        let input = encode_request(&request)?;
        let worker = worker.clone();
        let spawner = spawner.clone();
        let workdir = workdir.clone();
        let path_env = path_env.clone();
        let agent_env = route.agent_env;
        let snapshot_sha256 = snapshot_sha256.to_owned();
        let max_output_tokens = params.max_output_tokens.unwrap_or(DEFAULT_OUTPUT_TOKENS);
        let thinking_effort = params.thinking_effort;
        let cancellation = cancellation.clone();
        tasks.spawn(async move {
            let response = run_coordinator(
                spawner,
                worker,
                workdir,
                path_env,
                agent_env,
                input,
                time_limit_seconds,
                cancellation,
            )
            .await?;
            validate_response(
                &response,
                &[role],
                &snapshot_sha256,
                max_output_tokens,
                time_limit_seconds,
                thinking_effort,
            )?;
            let reviewer = response
                .reviewers
                .into_iter()
                .next()
                .ok_or_else(|| "Critic coordinator returned no reviewer result.".to_string())?;
            Ok::<_, String>((role, route_profile, effective_cost_limit_microusd, reviewer))
        });
    }

    let mut reviewers = Vec::with_capacity(tasks.len());
    let mut failure = None;
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok(Ok(reviewer)) => reviewers.push(reviewer),
            Ok(Err(error)) => {
                failure = Some(error);
                break;
            }
            Err(_) => {
                failure = Some("A critic reviewer process stopped unexpectedly.".into());
                break;
            }
        }
    }
    if let Some(error) = failure {
        cancellation.cancel();
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        return Err(error);
    }
    reviewers.sort_by_key(|(role, _, _, _)| *role);
    Ok(reviewers)
}

fn build_critic_route_profile(
    profile: ResolvedAgentRouteProfile,
    config: &GlobalAgentConfig,
) -> Result<ResolvedCriticRoute, String> {
    if profile.document.data_policy != RouteProfileDataPolicy::LocalOnly {
        return Err("Critic reviews require a Local only route profile.".into());
    }
    if profile
        .document
        .candidates
        .iter()
        .any(|candidate| candidate.data_location != RouteProfileLocation::Local)
    {
        return Err("Every critic route candidate must be declared Local.".into());
    }

    let mut candidates = Vec::with_capacity(profile.document.candidates.len());
    let mut candidate_env = BTreeMap::new();
    let mut configured_candidates = HashSet::new();
    for candidate in &profile.document.candidates {
        let provider = normalized_provider(&candidate.provider)?;
        let (endpoint, credentials, provider_env) = provider_settings(&provider, config)?;
        let configured = !credentials.is_empty()
            && credentials.iter().all(|key| {
                config
                    .env_vars
                    .get(*key)
                    .is_some_and(|value| !value.trim().is_empty())
            });
        if configured {
            if !is_loopback_endpoint(&endpoint) {
                return Err(format!(
                    "Critic route candidate '{}' has a configured endpoint that is not loopback-only.",
                    candidate.id
                ));
            }
            for (key, value) in provider_env {
                candidate_env.insert(key, value);
            }
            configured_candidates.insert(candidate.id.as_str());
        }
        candidates.push(CriticRouteCandidatePreview {
            id: candidate.id.clone(),
            provider,
            model: candidate.model.clone(),
            configured,
            cost_pricing_available: candidate.input_cost_microusd_per_million_tokens.is_some()
                && candidate.output_cost_microusd_per_million_tokens.is_some(),
            prompt_profile: candidate.prompt_profile.clone(),
        });
    }

    let selected = profile
        .document
        .preference_order
        .iter()
        .find_map(|candidate_id| {
            profile
                .document
                .candidates
                .iter()
                .find(|candidate| {
                    candidate.id == *candidate_id
                        && configured_candidates.contains(candidate.id.as_str())
                })
        })
        .ok_or_else(|| {
            "No configured Local route candidate is available. Add its provider settings and a loopback endpoint, then retry.".to_string()
        })?;
    let selected_provider = normalized_provider(&selected.provider)?;

    let mut agent_env = BTreeMap::new();
    agent_env.insert("BUZZ_AGENT_PROVIDER".into(), selected_provider);
    agent_env.insert("BUZZ_AGENT_MODEL".into(), selected.model.clone());
    agent_env.extend(candidate_env);
    agent_env.insert(
        ROUTE_PROFILE_JSON_ENV.into(),
        profile.serialized_document.clone(),
    );
    agent_env.insert(
        ROUTE_PROFILE_PROVENANCE_ID_ENV.into(),
        profile.identity.id.clone(),
    );
    agent_env.insert(
        ROUTE_PROFILE_PROVENANCE_VERSION_ENV.into(),
        profile.identity.version.to_string(),
    );
    agent_env.insert(
        ROUTE_PROFILE_PROVENANCE_HASH_ENV.into(),
        profile.identity.hash.clone(),
    );

    Ok(ResolvedCriticRoute {
        preview: CriticRouteProfilePreview {
            profile: buzz_run_journal::CriticRouteProfileRef {
                id: profile.identity.id.clone(),
                version: profile.identity.version,
                hash: profile.identity.hash.clone(),
            },
            candidates,
            estimated_cost_limit_microusd: profile.document.max_turn_cost_microusd,
        },
        agent_env,
        effective_cost_limit_microusd: profile.document.max_turn_cost_microusd,
    })
}

fn allocate_critic_round_cost_budget(
    budget_microusd: u64,
    roles: &[CriticRole],
) -> Result<BTreeMap<CriticRole, u64>, String> {
    if budget_microusd > MAX_ESTIMATED_ROUND_COST_MICROUSD || roles.is_empty() {
        return Err("Estimated critic budget is outside the supported range.".into());
    }
    let mut sorted_roles = roles.to_vec();
    sorted_roles.sort_by_key(|role| role.label());
    let role_count = u64::try_from(sorted_roles.len())
        .map_err(|_| "Critic role count is invalid.".to_string())?;
    let base = budget_microusd / role_count;
    let remainder = usize::try_from(budget_microusd % role_count)
        .map_err(|_| "Critic budget allocation is invalid.".to_string())?;
    Ok(sorted_roles
        .into_iter()
        .enumerate()
        .map(|(index, role)| (role, base + u64::from(index < remainder)))
        .collect())
}

fn apply_critic_round_cost_budget(
    budget_microusd: Option<u64>,
    roles: &[CriticRole],
    routes: &mut BTreeMap<CriticRole, ResolvedCriticRoute>,
) -> Result<(), String> {
    let Some(budget_microusd) = budget_microusd else {
        return Ok(());
    };
    let allocations = allocate_critic_round_cost_budget(budget_microusd, roles)?;
    for role in roles {
        let route = routes
            .get_mut(role)
            .ok_or_else(|| format!("The {} critic route is unavailable.", role.label()))?;
        if !route
            .preview
            .candidates
            .iter()
            .any(|candidate| candidate.configured && candidate.cost_pricing_available)
        {
            return Err(format!(
                "The {} route needs input and output prices on a configured Local candidate before an estimated budget can be applied.",
                role.label()
            ));
        }
        let allocated = *allocations
            .get(role)
            .ok_or_else(|| "Critic budget allocation is incomplete.".to_string())?;
        let effective = route
            .preview
            .estimated_cost_limit_microusd
            .map_or(allocated, |configured| configured.min(allocated));
        route
            .agent_env
            .insert(ROUTE_COST_BUDGET_ENV.into(), effective.to_string());
        route.effective_cost_limit_microusd = Some(effective);
    }
    Ok(())
}

fn normalized_provider(provider: &str) -> Result<String, String> {
    match provider.trim().to_ascii_lowercase().as_str() {
        "anthropic" => Ok("anthropic".into()),
        "openai" | "openai-compat" => Ok("openai".into()),
        "databricks" => Ok("databricks".into()),
        "databricks-v2" | "databricks_v2" => Ok("databricks_v2".into()),
        "openrouter" => Ok("openrouter".into()),
        "deepseek" => Ok("deepseek".into()),
        _ => Err("Critic route profile contains an unsupported provider.".into()),
    }
}

fn provider_settings(
    provider: &str,
    config: &GlobalAgentConfig,
) -> Result<(String, Vec<&'static str>, Vec<(String, String)>), String> {
    let value = |key: &'static str| config.env_vars.get(key).cloned();
    let (endpoint_key, default_endpoint, credentials) = match provider {
        "anthropic" => (
            "ANTHROPIC_BASE_URL",
            "https://api.anthropic.com",
            vec!["ANTHROPIC_API_KEY"],
        ),
        "openai" => (
            "OPENAI_COMPAT_BASE_URL",
            "https://api.openai.com/v1",
            vec!["OPENAI_COMPAT_API_KEY"],
        ),
        "openrouter" => (
            "OPENROUTER_BASE_URL",
            "https://openrouter.ai/api/v1",
            vec!["OPENROUTER_API_KEY"],
        ),
        "deepseek" => (
            "DEEPSEEK_BASE_URL",
            "https://api.deepseek.com",
            vec!["DEEPSEEK_API_KEY"],
        ),
        "databricks" | "databricks_v2" => (
            "DATABRICKS_HOST",
            "",
            vec!["DATABRICKS_HOST", "DATABRICKS_TOKEN"],
        ),
        _ => return Err("Critic route profile contains an unsupported provider.".into()),
    };
    let endpoint = value(endpoint_key).unwrap_or_else(|| default_endpoint.to_owned());
    let configured = credentials.iter().all(|key| {
        config
            .env_vars
            .get(*key)
            .is_some_and(|credential| !credential.trim().is_empty())
    });
    let mut provider_env = Vec::new();
    if configured {
        for key in &credentials {
            if *key != endpoint_key {
                if let Some(secret) = config.env_vars.get(*key) {
                    provider_env.push(((*key).to_owned(), secret.clone()));
                }
            }
        }
        provider_env.push((endpoint_key.to_owned(), endpoint.clone()));
        if provider == "openai" {
            if let Some(api) = value("OPENAI_COMPAT_API") {
                provider_env.push(("OPENAI_COMPAT_API".into(), api));
            }
        }
    }
    Ok((endpoint, credentials, provider_env))
}

fn is_loopback_endpoint(base_url: &str) -> bool {
    use std::net::{Ipv4Addr, Ipv6Addr};
    use url::Host;

    let Ok(url) = url::Url::parse(base_url) else {
        return false;
    };
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() {
        return false;
    }
    match url.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address == Ipv6Addr::LOCALHOST,
        Some(Host::Domain(domain)) => {
            domain.eq_ignore_ascii_case("localhost")
                || domain.to_ascii_lowercase().ends_with(".localhost")
                || domain.parse::<Ipv4Addr>().is_ok_and(|ip| ip.is_loopback())
        }
        None => false,
    }
}

/// Cancel an active critic request by its UI-generated request ID.
#[tauri::command]
pub fn cancel_critic_round(request_id: String, state: State<'_, AppState>) -> Result<bool, String> {
    let request_id = validate_request_id(&request_id)?;
    let active = state
        .active_critic_rounds
        .lock()
        .map_err(|_| "critic run state is unavailable".to_string())?;
    let Some(cancellation) = active.get(&request_id) else {
        return Ok(false);
    };
    cancellation.cancel();
    Ok(true)
}

fn validate_request_id(raw: &str) -> Result<String, String> {
    let parsed = Uuid::parse_str(raw).map_err(|_| "invalid critic request ID")?;
    if parsed.to_string() != raw {
        return Err("critic request ID must be a canonical UUID".into());
    }
    Ok(raw.to_owned())
}

fn validate(params: &RunCriticsParams) -> Result<Vec<CriticRole>, String> {
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
        return Err(
            "Objective, scope, and snapshot must be non-empty text within their size limits."
                .into(),
        );
    }
    if params
        .max_output_tokens
        .is_some_and(|value| !(MIN_OUTPUT_TOKENS..=MAX_OUTPUT_TOKENS).contains(&value))
        || params.time_limit_seconds.is_some_and(|value| {
            !(MIN_TIME_LIMIT_SECONDS..=MAX_TIME_LIMIT_SECONDS).contains(&value)
        })
        || params
            .estimated_round_cost_budget_microusd
            .is_some_and(|value| value > MAX_ESTIMATED_ROUND_COST_MICROUSD)
    {
        return Err(
                "Critic limits must be 64–2048 output tokens and 15–120 seconds per reviewer; estimated round ceilings may not exceed $1,000,000.".into(),
        );
    }
    if !is_lower_sha256(&params.coordinator_guide_sha256) {
        return Err("coordinator guide SHA-256 must be 64 lowercase hexadecimal characters".into());
    }
    if params.roles.is_empty() || params.roles.len() > MAX_REVIEWERS {
        return Err(format!(
            "Choose between one and {MAX_REVIEWERS} critic roles."
        ));
    }
    let mut seen = HashSet::new();
    let mut roles = Vec::with_capacity(params.roles.len());
    for raw in &params.roles {
        let role = CriticRole::parse(raw).ok_or_else(|| {
            format!("Unsupported critic role '{raw}'. Choose correctness, security, architecture, ui_accessibility, performance, or product.")
        })?;
        if !seen.insert(role) {
            return Err(format!(
                "Critic role '{}' was selected more than once.",
                role.label()
            ));
        }
        roles.push(role);
    }
    validate_route_assignment_keys(&params.route_profiles, &roles)?;
    Ok(roles)
}

fn encode_request(request: &CriticCoordinatorRequest) -> Result<Vec<u8>, String> {
    let input = serde_json::to_vec(request).map_err(|_| "critic request could not be encoded")?;
    if input.len() > MAX_COORDINATOR_REQUEST_BYTES {
        return Err("Critic request exceeds the coordinator size limit.".into());
    }
    Ok(input)
}

fn validate_response(
    response: &CriticCoordinatorResponse,
    roles: &[CriticRole],
    snapshot_sha256: &str,
    max_output_tokens: u32,
    time_limit_seconds: u64,
    thinking_effort: Option<CriticThinkingEffort>,
) -> Result<(), String> {
    let expected_roles = roles
        .iter()
        .map(|role| role.label())
        .collect::<HashSet<_>>();
    let actual_roles = response
        .reviewers
        .iter()
        .map(|reviewer| reviewer.role.as_str())
        .collect::<HashSet<_>>();
    if response.version != 1
        || response.snapshot_sha256 != snapshot_sha256
        || response.limits.maximum_reviewers != MAX_REVIEWERS
        || response.limits.output_tokens_per_reviewer != max_output_tokens
        || response.limits.time_limit_seconds_per_reviewer != time_limit_seconds
        || response.limits.thinking_effort_requested != thinking_effort
        || expected_roles != actual_roles
        || response.reviewers.len() != roles.len()
        || response.reviewers.iter().any(|reviewer| {
            !matches!(reviewer.status.as_str(), "completed" | "failed")
                || reviewer.output.as_ref().is_some_and(|output| {
                    output.len() > MAX_REVIEWER_OUTPUT_BYTES || output.contains('\0')
                })
                || (reviewer.status == "completed"
                    && reviewer.output.as_deref().is_none_or(str::is_empty))
                || [
                    reviewer.stop_reason.as_deref(),
                    reviewer.candidate_id.as_deref(),
                    reviewer.provider_id.as_deref(),
                    reviewer.model_id.as_deref(),
                    reviewer.error_code.as_deref(),
                ]
                .into_iter()
                .flatten()
                .any(|value| value.len() > 256 || value.chars().any(char::is_control))
        })
    {
        return Err(
            "Critic coordinator returned a result that did not match the requested round.".into(),
        );
    }
    Ok(())
}

fn persist_round(
    scope: &CriticJournalScope,
    params: &RunCriticsParams,
    snapshot_sha256: &str,
    max_output_tokens: u32,
    time_limit_seconds: u64,
    reviewers: &[buzz_run_journal::CriticReviewerRecord],
) -> (Option<String>, &'static str, Option<&'static str>) {
    let Ok(journal) = scope.open() else {
        return (
            None,
            "not_saved",
            Some("local_identity_or_storage_unavailable"),
        );
    };
    let round_id = Uuid::new_v4().to_string();
    let settings = buzz_run_journal::CriticRoundSettings {
        max_output_tokens,
        time_limit_seconds,
        thinking_effort_requested: params
            .thinking_effort
            .map(|effort| effort.label().to_owned()),
        estimated_round_cost_budget_microusd: params.estimated_round_cost_budget_microusd,
        route_profile: None,
        coordinator_guide: Some(buzz_run_journal::CriticGuideRef {
            path: CRITIC_GUIDE_RELATIVE_PATH.into(),
            sha256: params.coordinator_guide_sha256.clone(),
        }),
    };
    let objective_sha256 = hex::encode(Sha256::digest(params.objective.as_bytes()));
    let scope_sha256 = hex::encode(Sha256::digest(params.scope.as_bytes()));
    match journal.record_critic_round(
        &round_id,
        snapshot_sha256,
        &objective_sha256,
        &scope_sha256,
        settings,
        reviewers.to_vec(),
    ) {
        Ok(record) => (Some(record.round_id), "saved", None),
        Err(_) => (None, "not_saved", Some("critic_round_not_saved")),
    }
}

async fn run_coordinator(
    spawner: CriticProcessSpawner,
    worker: PathBuf,
    workdir: PathBuf,
    path_env: String,
    agent_env: BTreeMap<String, String>,
    input: Vec<u8>,
    time_limit_seconds: u64,
    cancellation: CancellationToken,
) -> Result<CriticCoordinatorResponse, String> {
    let mut command = Command::new(worker);
    command
        .arg("critic-round")
        .current_dir(workdir)
        .env_clear()
        .env("PATH", path_env)
        .envs(agent_env)
        .env_remove("BUZZ_PRIVATE_KEY")
        .env_remove("BUZZ_ACP_PRIVATE_KEY")
        .env_remove("NOSTR_PRIVATE_KEY")
        .env_remove("BUZZ_RELAY_URL")
        .env_remove("BUZZ_AUTH_TAG")
        .env_remove("MCP_HOOK_SERVERS")
        .env_remove("BUZZ_AGENT_SYSTEM_PROMPT")
        .env_remove("BUZZ_AGENT_SYSTEM_PROMPT_FILE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    for key in ["HOME", "TMPDIR", "TMP", "TEMP"] {
        if let Ok(value) = std::env::var(key) {
            command.env(key, value);
        }
    }
    configure_process_tree(&mut command);
    crate::util::configure_no_window(command.as_std_mut());
    let mut child = spawner(&mut command)?;
    let pid = child
        .id()
        .ok_or_else(|| "Critic coordinator process ID is unavailable.".to_string())?;
    let mut tree_guard = CriticProcessTreeGuard::new(pid);

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "Critic coordinator input is unavailable.".to_string())?;
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => return Err("Critic round was canceled.".into()),
        result = tokio::time::timeout(Duration::from_secs(5), stdin.write_all(&input)) => {
            result.map_err(|_| "Critic coordinator request timed out.".to_string())?
                .map_err(|_| "Critic coordinator request could not be sent.".to_string())?;
        }
    }
    drop(stdin);

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Critic coordinator output is unavailable.".to_string())?;
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
            tree_guard.terminate_graceful().await;
            let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
            output_task.abort();
            return Err("Critic round was canceled.".into());
        }
        result = tokio::time::timeout(
            Duration::from_secs(time_limit_seconds + WORKER_STARTUP_GRACE_SECONDS),
            child.wait(),
        ) => match result {
            Ok(Ok(status)) => status,
            Ok(Err(_)) => {
                tree_guard.terminate_graceful().await;
                output_task.abort();
                return Err("Critic coordinator could not be monitored.".into());
            }
            Err(_) => {
                tree_guard.terminate_graceful().await;
                let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
                output_task.abort();
                return Err("Critic round exceeded its time limit.".into());
            }
        }
    };
    if !status.success() {
        tree_guard.terminate_graceful().await;
        output_task.abort();
        return Err("Critic coordinator failed.".into());
    }
    let bytes = output_task
        .await
        .map_err(|_| "Critic coordinator output could not be collected.".to_string())?
        .map_err(|_| "Critic coordinator output could not be read.".to_string())?;
    if bytes.len() > MAX_COORDINATOR_RESPONSE_BYTES {
        tree_guard.terminate_graceful().await;
        return Err("Critic coordinator response exceeded its size limit.".into());
    }
    let response: CriticCoordinatorResponse = serde_json::from_slice(&bytes)
        .map_err(|_| "Critic coordinator returned invalid JSON.".to_string())?;
    tree_guard.terminate_graceful().await;
    Ok(response)
}

#[cfg(unix)]
fn configure_process_tree(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    command.as_std_mut().process_group(0);
}

#[cfg(not(unix))]
fn configure_process_tree(_command: &mut Command) {}

struct CriticProcessTreeGuard {
    pid: u32,
    armed: bool,
}

impl CriticProcessTreeGuard {
    fn new(pid: u32) -> Self {
        Self { pid, armed: true }
    }

    async fn terminate_graceful(&mut self) {
        if !self.armed {
            return;
        }
        #[cfg(unix)]
        {
            use nix::sys::signal::{killpg, Signal};
            use nix::unistd::Pid;
            if let Ok(pid) = i32::try_from(self.pid) {
                let process_group = Pid::from_raw(pid);
                let _ = killpg(process_group, Signal::SIGTERM);
                tokio::time::sleep(Duration::from_millis(200)).await;
                let _ = killpg(process_group, Signal::SIGKILL);
            }
            self.armed = false;
        }
        #[cfg(windows)]
        {
            let _ = crate::managed_agents::terminate_process(self.pid);
            self.armed = false;
        }
    }

    fn terminate_immediate(&mut self) {
        if !self.armed {
            return;
        }
        self.armed = false;
        #[cfg(unix)]
        {
            use nix::sys::signal::{killpg, Signal};
            use nix::unistd::Pid;
            if let Ok(pid) = i32::try_from(self.pid) {
                let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
            }
        }
        #[cfg(windows)]
        {
            let _ = crate::managed_agents::terminate_process(self.pid);
        }
    }
}

impl Drop for CriticProcessTreeGuard {
    fn drop(&mut self) {
        if self.armed {
            self.terminate_immediate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managed_agents::agent_route_profile::AgentRouteProfileIdentity;

    fn local_profile(data_policy: &str, data_location: &str) -> ResolvedAgentRouteProfile {
        let document = serde_json::from_value(serde_json::json!({
            "version": 1,
            "data_policy": data_policy,
            "preference_order": ["loopback"],
            "max_turn_cost_microusd": 500000,
            "candidates": [{
                "id": "loopback",
                "provider": "openai-compat",
                "model": "small-local",
                "data_location": data_location,
                "input_cost_microusd_per_million_tokens": 5000000,
                "output_cost_microusd_per_million_tokens": 10000000,
                "prompt_addendum": "Review carefully.",
                "prompt_profile": {
                    "id": "local-coding",
                    "version": 1,
                    "prompt_hash": "d".repeat(64)
                }
            }],
            "profile_id": "local-critic",
            "profile_version": 2,
            "profile_hash": "b".repeat(64)
        }))
        .unwrap();
        ResolvedAgentRouteProfile {
            identity: AgentRouteProfileIdentity {
                id: "local-critic".into(),
                version: 2,
                hash: "c".repeat(64),
            },
            serialized_document: serde_json::to_string(&document).unwrap(),
            document,
        }
    }

    fn local_openai_config(endpoint: &str) -> GlobalAgentConfig {
        GlobalAgentConfig {
            env_vars: BTreeMap::from([
                ("OPENAI_COMPAT_API_KEY".into(), "test-only-secret".into()),
                ("OPENAI_COMPAT_BASE_URL".into(), endpoint.into()),
                ("OPENAI_COMPAT_API".into(), "chat".into()),
                ("MCP_HOOK_SERVERS".into(), "must-not-pass".into()),
            ]),
            ..GlobalAgentConfig::default()
        }
    }

    fn write_critic_guide(root: &Path, text: &str) {
        let guide_dir = root.join("AGENT_GUIDES");
        std::fs::create_dir_all(&guide_dir).unwrap();
        std::fs::write(guide_dir.join("CRITICS.md"), text).unwrap();
    }

    #[test]
    fn critic_guide_preview_returns_exact_bounded_text_and_provenance() {
        let root = tempfile::tempdir().unwrap();
        let text = "Coordinator guidance for a human.\n";
        write_critic_guide(root.path(), text);

        let preview = read_critic_coordinator_guide(root.path()).unwrap();
        assert_eq!(preview.path, CRITIC_GUIDE_RELATIVE_PATH);
        assert_eq!(preview.text, text);
        assert_eq!(preview.byte_length, text.len());
        assert_eq!(preview.sha256, hex::encode(Sha256::digest(text.as_bytes())));
    }

    #[test]
    fn critic_guide_preview_rejects_missing_and_oversized_files_without_truncating() {
        let missing = tempfile::tempdir().unwrap();
        assert!(read_critic_coordinator_guide(missing.path())
            .unwrap_err()
            .contains("missing"));

        let oversized = tempfile::tempdir().unwrap();
        let text = "x".repeat(MAX_CRITIC_GUIDE_BYTES + 1);
        write_critic_guide(oversized.path(), &text);
        let error = read_critic_coordinator_guide(oversized.path()).unwrap_err();
        assert!(error.contains("too large"));
    }

    #[test]
    fn critic_guide_reader_rejects_a_short_read_instead_of_returning_truncated_text() {
        let error = read_complete_critic_guide(std::io::Cursor::new(b"short"), 10).unwrap_err();
        assert!(error.contains("truncated"));
    }

    #[test]
    fn critic_guide_run_revalidation_rejects_a_stale_displayed_hash() {
        let root = tempfile::tempdir().unwrap();
        write_critic_guide(root.path(), "first version\n");
        let preview = read_critic_coordinator_guide(root.path()).unwrap();
        write_critic_guide(root.path(), "second version\n");

        let error = verify_critic_coordinator_guide(root.path(), &preview.sha256).unwrap_err();
        assert!(error.contains("changed after preview"));
    }

    #[cfg(unix)]
    #[test]
    fn critic_guide_preview_rejects_wrong_target_and_escaped_symlinks() {
        use std::os::unix::fs::symlink;

        let wrong_target = tempfile::tempdir().unwrap();
        let guide_dir = wrong_target.path().join("AGENT_GUIDES");
        std::fs::create_dir_all(&guide_dir).unwrap();
        std::fs::write(guide_dir.join("OTHER.md"), "not the canonical guide").unwrap();
        symlink(guide_dir.join("OTHER.md"), guide_dir.join("CRITICS.md")).unwrap();
        assert!(read_critic_coordinator_guide(wrong_target.path()).is_err());

        let escaped = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let guide_dir = escaped.path().join("AGENT_GUIDES");
        std::fs::create_dir_all(&guide_dir).unwrap();
        std::fs::write(outside.path().join("CRITICS.md"), "outside the nest").unwrap();
        symlink(
            outside.path().join("CRITICS.md"),
            guide_dir.join("CRITICS.md"),
        )
        .unwrap();
        assert!(read_critic_coordinator_guide(escaped.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn critic_guide_preview_rejects_symlinked_nest_and_guide_directory() {
        use std::os::unix::fs::symlink;

        let target = tempfile::tempdir().unwrap();
        write_critic_guide(target.path(), "canonical guide\n");
        let link_parent = tempfile::tempdir().unwrap();
        let linked_root = link_parent.path().join("nest-link");
        symlink(target.path(), &linked_root).unwrap();
        assert!(read_critic_coordinator_guide(&linked_root).is_err());

        let nested = tempfile::tempdir().unwrap();
        let nested_target = tempfile::tempdir().unwrap();
        std::fs::write(nested_target.path().join("CRITICS.md"), "alternate guide").unwrap();
        symlink(nested_target.path(), nested.path().join("AGENT_GUIDES")).unwrap();
        assert!(read_critic_coordinator_guide(nested.path()).is_err());
    }

    #[test]
    fn aggregate_critic_budget_splits_deterministically_and_keeps_lower_profile_caps() {
        let roles = [
            CriticRole::Security,
            CriticRole::Correctness,
            CriticRole::Architecture,
        ];
        let allocations = allocate_critic_round_cost_budget(10, &roles).unwrap();
        assert_eq!(allocations[&CriticRole::Architecture], 4);
        assert_eq!(allocations[&CriticRole::Correctness], 3);
        assert_eq!(allocations[&CriticRole::Security], 3);
        assert_eq!(allocations.values().sum::<u64>(), 10);

        let config = local_openai_config("http://127.0.0.1:1234/v1");
        let routes = roles
            .iter()
            .map(|role| {
                (
                    *role,
                    build_critic_route_profile(local_profile("local-only", "local"), &config)
                        .unwrap(),
                )
            })
            .collect();
        let mut routes = routes;
        apply_critic_round_cost_budget(Some(10), &roles, &mut routes).unwrap();
        assert_eq!(
            routes[&CriticRole::Architecture].effective_cost_limit_microusd,
            Some(4)
        );
        assert_eq!(
            routes[&CriticRole::Correctness].effective_cost_limit_microusd,
            Some(3)
        );
        assert_eq!(
            routes[&CriticRole::Security].effective_cost_limit_microusd,
            Some(3)
        );
        assert_eq!(
            routes
                .values()
                .map(|route| route.effective_cost_limit_microusd.unwrap())
                .sum::<u64>(),
            10
        );
        assert!(routes.values().all(|route| {
            route.agent_env[ROUTE_COST_BUDGET_ENV]
                .parse::<u64>()
                .is_ok()
                && route.preview.candidates[0].cost_pricing_available
        }));
        let preserved_profile: buzz_agent_pkg::route_preview::RouteProfileDocument =
            serde_json::from_str(
                &routes[&CriticRole::Correctness].agent_env[ROUTE_PROFILE_JSON_ENV],
            )
            .unwrap();
        assert_eq!(preserved_profile.max_turn_cost_microusd, Some(500_000));
        assert_eq!(
            routes[&CriticRole::Correctness].agent_env[ROUTE_PROFILE_PROVENANCE_HASH_ENV],
            "c".repeat(64)
        );
    }

    #[test]
    fn aggregate_critic_budget_requires_a_configured_local_priced_candidate() {
        let config = local_openai_config("http://127.0.0.1:1234/v1");
        let mut profile = local_profile("local-only", "local");
        profile.document.candidates[0].input_cost_microusd_per_million_tokens = None;
        profile.serialized_document = serde_json::to_string(&profile.document).unwrap();
        let route = build_critic_route_profile(profile, &config).unwrap();
        let mut routes = BTreeMap::from([(CriticRole::Correctness, route)]);
        let error =
            apply_critic_round_cost_budget(Some(10), &[CriticRole::Correctness], &mut routes)
                .unwrap_err();
        assert!(error.contains("needs input and output prices"));
    }

    #[test]
    fn critic_route_requires_declared_local_policy_and_loopback_endpoint() {
        let config = local_openai_config("http://127.0.0.1:1234/v1");
        assert!(
            build_critic_route_profile(local_profile("allow-hosted", "local"), &config)
                .unwrap_err()
                .contains("Local only")
        );
        assert!(
            build_critic_route_profile(local_profile("local-only", "hosted"), &config)
                .unwrap_err()
                .contains("declared Local")
        );
        assert!(build_critic_route_profile(
            local_profile("local-only", "local"),
            &local_openai_config("https://api.example.test/v1")
        )
        .unwrap_err()
        .contains("not loopback-only"));
    }

    #[test]
    fn critic_route_preview_is_secret_free_and_carries_exact_prompt_route() {
        let route = build_critic_route_profile(
            local_profile("local-only", "local"),
            &local_openai_config("http://localhost:1234/v1"),
        )
        .unwrap();
        assert_eq!(route.preview.profile.id, "local-critic");
        assert_eq!(route.preview.profile.version, 2);
        assert_eq!(route.preview.candidates[0].provider, "openai");
        assert!(route.preview.candidates[0].configured);
        assert_eq!(
            route.preview.candidates[0]
                .prompt_profile
                .as_ref()
                .map(|profile| profile.id.as_str()),
            Some("local-coding")
        );
        assert_eq!(route.preview.estimated_cost_limit_microusd, Some(500_000));
        assert_eq!(
            route
                .agent_env
                .get("OPENAI_COMPAT_API_KEY")
                .map(String::as_str),
            Some("test-only-secret")
        );
        assert!(!route.agent_env.contains_key("MCP_HOOK_SERVERS"));
        assert_eq!(
            route
                .agent_env
                .get("BUZZ_AGENT_PROVIDER")
                .map(String::as_str),
            Some("openai")
        );
        let preview_json = serde_json::to_string(&route.preview).unwrap();
        assert!(!preview_json.contains("test-only-secret"));
    }

    #[test]
    fn critic_route_requires_configured_local_provider_credentials() {
        let mut config = local_openai_config("http://127.0.0.1:1234/v1");
        config.env_vars.remove("OPENAI_COMPAT_API_KEY");
        assert!(
            build_critic_route_profile(local_profile("local-only", "local"), &config)
                .unwrap_err()
                .contains("No configured Local route candidate")
        );
    }

    #[test]
    fn candidate_test_destination_enforces_locality_hosted_consent_and_clean_urls() {
        let local = validate_candidate_test_destination(
            RouteProfileLocation::Local,
            RouteProfileDataPolicy::LocalOnly,
            "http://127.0.0.1:1234/v1",
            false,
        )
        .unwrap();
        assert!(!local.1);
        assert_eq!(local.0.host_str(), Some("127.0.0.1"));

        assert!(validate_candidate_test_destination(
            RouteProfileLocation::Local,
            RouteProfileDataPolicy::LocalOnly,
            "https://api.example.test/v1",
            false,
        )
        .unwrap_err()
        .contains("loopback"));
        assert!(validate_candidate_test_destination(
            RouteProfileLocation::Hosted,
            RouteProfileDataPolicy::LocalOnly,
            "https://api.example.test/v1",
            false,
        )
        .unwrap_err()
        .contains("allow-hosted"));
        assert!(validate_candidate_test_destination(
            RouteProfileLocation::Hosted,
            RouteProfileDataPolicy::AllowHosted,
            "http://api.example.test/v1",
            true,
        )
        .unwrap_err()
        .contains("HTTPS"));

        let hosted = validate_candidate_test_destination(
            RouteProfileLocation::Hosted,
            RouteProfileDataPolicy::AllowHosted,
            "https://api.example.test/v1",
            false,
        )
        .unwrap();
        assert!(hosted.1, "first hosted call only requests confirmation");
        assert!(
            !validate_candidate_test_destination(
                RouteProfileLocation::Hosted,
                RouteProfileDataPolicy::AllowHosted,
                "https://api.example.test/v1",
                true,
            )
            .unwrap()
            .1
        );

        for endpoint in [
            "https://user:pass@api.example.test/v1",
            "https://api.example.test/v1?token=secret",
            "https://api.example.test/v1#fragment",
        ] {
            assert!(validate_candidate_test_destination(
                RouteProfileLocation::Hosted,
                RouteProfileDataPolicy::AllowHosted,
                endpoint,
                true,
            )
            .is_err());
        }
    }

    fn params() -> RunCriticsParams {
        RunCriticsParams {
            objective: "Review the behavior".into(),
            scope: "Check the frozen function".into(),
            snapshot: "fn value() -> bool { true }\n".into(),
            roles: vec!["correctness".into()],
            max_output_tokens: Some(64),
            time_limit_seconds: Some(15),
            thinking_effort: None,
            estimated_round_cost_budget_microusd: None,
            route_profiles: BTreeMap::from([(
                "correctness".into(),
                buzz_run_journal::CriticRouteProfileRef {
                    id: "local-critic".into(),
                    version: 2,
                    hash: "c".repeat(64),
                },
            )]),
            coordinator_guide_sha256: "b".repeat(64),
        }
    }

    fn direct_test_spawner() -> CriticProcessSpawner {
        Arc::new(|command| {
            command
                .spawn()
                .map_err(|_| "test critic coordinator could not be started".into())
        })
    }

    #[test]
    fn validates_roles_and_hard_resource_caps() {
        assert_eq!(validate(&params()).unwrap(), vec![CriticRole::Correctness]);

        let mut invalid = params();
        invalid.coordinator_guide_sha256 = "not-a-hash".into();
        assert!(validate(&invalid).is_err());
        invalid = params();
        invalid.roles = vec![];
        assert!(validate(&invalid).is_err());
        invalid.roles = vec!["security".into(), "security".into()];
        assert!(validate(&invalid).is_err());
        invalid.roles = vec!["unknown".into()];
        assert!(validate(&invalid).is_err());
        invalid.roles = vec!["security".into()];
        invalid.max_output_tokens = Some(MAX_OUTPUT_TOKENS + 1);
        assert!(validate(&invalid).is_err());
        invalid.max_output_tokens = Some(MIN_OUTPUT_TOKENS);
        invalid.time_limit_seconds = Some(MIN_TIME_LIMIT_SECONDS - 1);
        assert!(validate(&invalid).is_err());
        invalid.time_limit_seconds = Some(MIN_TIME_LIMIT_SECONDS);
        invalid.estimated_round_cost_budget_microusd = Some(MAX_ESTIMATED_ROUND_COST_MICROUSD + 1);
        assert!(validate(&invalid).is_err());
    }

    #[test]
    fn critic_round_requires_exactly_one_route_for_every_role() {
        let mut invalid = params();
        invalid.route_profiles.clear();
        assert!(validate(&invalid)
            .unwrap_err()
            .contains("every selected critic role"));

        invalid = params();
        invalid.route_profiles.insert(
            "security".into(),
            buzz_run_journal::CriticRouteProfileRef {
                id: "local-critic".into(),
                version: 2,
                hash: "c".repeat(64),
            },
        );
        assert!(validate(&invalid)
            .unwrap_err()
            .contains("every selected critic role"));

        invalid = params();
        invalid.roles.push("security".into());
        assert!(validate(&invalid)
            .unwrap_err()
            .contains("every selected critic role"));
    }

    #[test]
    fn request_ids_are_canonical_uuids() {
        let id = Uuid::new_v4().to_string();
        assert_eq!(validate_request_id(&id).unwrap(), id);
        assert!(validate_request_id("not-a-uuid").is_err());
        assert!(validate_request_id("AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA").is_err());
    }

    #[test]
    fn response_must_match_the_frozen_snapshot_roles_and_settings() {
        let params = params();
        let roles = validate(&params).unwrap();
        let digest = hex::encode(Sha256::digest(params.snapshot.as_bytes()));
        let response = CriticCoordinatorResponse {
            version: 1,
            snapshot_sha256: digest.clone(),
            limits: CriticCoordinatorLimits {
                maximum_reviewers: MAX_REVIEWERS,
                output_tokens_per_reviewer: 64,
                time_limit_seconds_per_reviewer: 15,
                thinking_effort_requested: None,
            },
            reviewers: vec![CriticCoordinatorReviewer {
                role: "correctness".into(),
                status: "completed".into(),
                output: Some("No verified defects.".into()),
                output_truncated: false,
                stop_reason: Some("end_turn".into()),
                candidate_id: Some("local".into()),
                provider_id: Some("openai".into()),
                model_id: Some("mock-local-model".into()),
                elapsed_ms: Some(100),
                error_code: None,
            }],
        };
        assert!(validate_response(&response, &roles, &digest, 64, 15, None).is_ok());
        assert!(validate_response(&response, &roles, &"0".repeat(64), 64, 15, None).is_err());
        assert!(validate_response(&response, &roles, &digest, 128, 15, None).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn coordinator_client_accepts_the_versioned_cli_contract() {
        let temp = tempfile::tempdir().unwrap();
        let fake = temp.path().join("fake-buzz-acp");
        let params = params();
        let digest = hex::encode(Sha256::digest(params.snapshot.as_bytes()));
        let fake_response = serde_json::json!({
            "version": 1,
            "snapshot_sha256": digest,
            "limits": {
                "maximum_reviewers": 3,
                "output_tokens_per_reviewer": 64,
                "time_limit_seconds_per_reviewer": 15,
                "thinking_effort_requested": null,
            },
            "reviewers": [{
                "role": "correctness",
                "status": "completed",
                "output": "No verified defects.",
                "output_truncated": false,
                "stop_reason": "end_turn",
                "candidate_id": "local",
                "provider_id": "openai",
                "model_id": "mock-local-model",
                "elapsed_ms": 100,
                "error_code": null,
            }],
        })
        .to_string();
        let script = format!(
            "#!/bin/sh\n[ \"$1\" = \"critic-round\" ] || exit 3\ncat > received-request.json\nprintf '{{\"BUZZ_AGENT_PROVIDER\":\"%s\",\"BUZZ_AGENT_MODEL\":\"%s\",\"OPENAI_COMPAT_API_KEY\":\"%s\",\"BUZZ_AGENT_ROUTE_PROFILE_JSON\":\"%s\",\"BUZZ_ACP_ROUTE_PROFILE_ID\":\"%s\",\"MCP_HOOK_SERVERS\":\"%s\"}}\\n' \"$BUZZ_AGENT_PROVIDER\" \"$BUZZ_AGENT_MODEL\" \"$OPENAI_COMPAT_API_KEY\" \"$BUZZ_AGENT_ROUTE_PROFILE_JSON\" \"$BUZZ_ACP_ROUTE_PROFILE_ID\" \"${{MCP_HOOK_SERVERS-}}\" > received-env.json\nprintf '%s\\n' '{fake_response}'\n"
        );
        std::fs::write(&fake, script).unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();

        let request = CriticCoordinatorRequest {
            version: 1,
            objective: params.objective.clone(),
            scope: params.scope.clone(),
            snapshot: params.snapshot.clone(),
            roles: validate(&params).unwrap(),
            max_output_tokens: params.max_output_tokens,
            time_limit_seconds: params.time_limit_seconds,
            thinking_effort: params.thinking_effort,
        };
        let input = encode_request(&request).unwrap();
        let mut agent_env = BTreeMap::new();
        agent_env.insert("BUZZ_AGENT_PROVIDER".into(), "openai".into());
        agent_env.insert("BUZZ_AGENT_MODEL".into(), "local-test-model".into());
        agent_env.insert("OPENAI_COMPAT_API_KEY".into(), "test-only-secret".into());
        agent_env.insert(
            ROUTE_PROFILE_JSON_ENV.into(),
            "route-profile-fixture".into(),
        );
        agent_env.insert(
            ROUTE_PROFILE_PROVENANCE_ID_ENV.into(),
            "local-critic".into(),
        );
        let response = run_coordinator(
            direct_test_spawner(),
            fake,
            temp.path().to_path_buf(),
            std::env::var("PATH").unwrap_or_default(),
            agent_env,
            input,
            15,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let received: serde_json::Value = serde_json::from_slice(
            &std::fs::read(temp.path().join("received-request.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(received["objective"], params.objective);
        assert_eq!(received["roles"][0], "correctness");
        assert_eq!(received["max_output_tokens"], 64);
        assert_eq!(received["time_limit_seconds"], 15);
        assert!(received.get("coordinator_guide_sha256").is_none());
        assert!(!received.to_string().contains("Coordinator guidance"));
        let env: serde_json::Value =
            serde_json::from_slice(&std::fs::read(temp.path().join("received-env.json")).unwrap())
                .unwrap();
        assert_eq!(env["BUZZ_AGENT_PROVIDER"], "openai");
        assert_eq!(env["BUZZ_AGENT_MODEL"], "local-test-model");
        assert_eq!(env["OPENAI_COMPAT_API_KEY"], "test-only-secret");
        assert_eq!(env[ROUTE_PROFILE_JSON_ENV], "route-profile-fixture");
        assert_eq!(env[ROUTE_PROFILE_PROVENANCE_ID_ENV], "local-critic");
        assert_eq!(env["MCP_HOOK_SERVERS"], "");
        assert!(validate_response(&response, &request.roles, &digest, 64, 15, None).is_ok());
        assert_eq!(
            response.reviewers[0].output.as_deref(),
            Some("No verified defects.")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn each_critic_role_runs_with_its_own_profile_environment() {
        let temp = tempfile::tempdir().unwrap();
        let fake = temp.path().join("fake-buzz-acp");
        std::fs::write(
            &fake,
            "#!/bin/sh\n[ \"$1\" = \"critic-round\" ] || exit 3\ncat >/dev/null\nprintf '%s:%s:%s\\n' \"$TEST_CRITIC_ROLE\" \"$BUZZ_AGENT_MODEL\" \"$BUZZ_ACP_ROUTE_PROFILE_ID\" > \"$TEST_ROUTE_RECORD_PATH\"\nprintf '%s\\n' \"$TEST_CRITIC_RESPONSE\"\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();

        let mut params = params();
        params.roles = vec!["correctness".into(), "security".into()];
        params.route_profiles.insert(
            "security".into(),
            buzz_run_journal::CriticRouteProfileRef {
                id: "route-security".into(),
                version: 1,
                hash: "e".repeat(64),
            },
        );
        let digest = hex::encode(Sha256::digest(params.snapshot.as_bytes()));
        let mut routes = BTreeMap::new();
        for (role, model, profile_id, profile_hash) in [
            (
                CriticRole::Correctness,
                "fast-local",
                "route-correctness",
                "a",
            ),
            (CriticRole::Security, "deep-local", "route-security", "e"),
        ] {
            let record_path = temp
                .path()
                .join(format!("{}.txt", role.label()))
                .to_string_lossy()
                .into_owned();
            let response = serde_json::json!({
                "version": 1,
                "snapshot_sha256": digest,
                "limits": {
                    "maximum_reviewers": 3,
                    "output_tokens_per_reviewer": 64,
                    "time_limit_seconds_per_reviewer": 15,
                    "thinking_effort_requested": null
                },
                "reviewers": [{
                    "role": role.label(),
                    "status": "completed",
                    "output": "Independent local response.",
                    "output_truncated": false,
                    "stop_reason": "end_turn",
                    "candidate_id": "local",
                    "provider_id": "openai",
                    "model_id": model,
                    "elapsed_ms": 100,
                    "error_code": null
                }]
            })
            .to_string();
            let mut agent_env = BTreeMap::new();
            agent_env.insert("TEST_CRITIC_ROLE".into(), role.label().into());
            agent_env.insert("TEST_CRITIC_RESPONSE".into(), response);
            agent_env.insert("TEST_ROUTE_RECORD_PATH".into(), record_path);
            agent_env.insert("BUZZ_AGENT_MODEL".into(), model.into());
            agent_env.insert(ROUTE_PROFILE_PROVENANCE_ID_ENV.into(), profile_id.into());
            routes.insert(
                role,
                ResolvedCriticRoute {
                    preview: CriticRouteProfilePreview {
                        profile: buzz_run_journal::CriticRouteProfileRef {
                            id: profile_id.into(),
                            version: 1,
                            hash: profile_hash.repeat(64),
                        },
                        candidates: Vec::new(),
                        estimated_cost_limit_microusd: None,
                    },
                    agent_env,
                    effective_cost_limit_microusd: None,
                },
            );
        }

        let results = run_critic_roles(
            direct_test_spawner(),
            fake,
            temp.path().to_path_buf(),
            std::env::var("PATH").unwrap_or_default(),
            &params,
            vec![CriticRole::Correctness, CriticRole::Security],
            &digest,
            &mut routes,
            15,
            CancellationToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, CriticRole::Correctness);
        assert_eq!(results[0].1.id, "route-correctness");
        assert_eq!(results[0].3.model_id.as_deref(), Some("fast-local"));
        assert_eq!(results[1].0, CriticRole::Security);
        assert_eq!(results[1].1.id, "route-security");
        assert_eq!(results[1].3.model_id.as_deref(), Some("deep-local"));
        assert_eq!(
            std::fs::read_to_string(temp.path().join("correctness.txt")).unwrap(),
            "correctness:fast-local:route-correctness\n"
        );
        assert_eq!(
            std::fs::read_to_string(temp.path().join("security.txt")).unwrap(),
            "security:deep-local:route-security\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_terminates_the_coordinator_process_group() {
        let temp = tempfile::tempdir().unwrap();
        let fake = temp.path().join("slow-buzz-acp");
        std::fs::write(&fake, "#!/bin/sh\ncat >/dev/null\nsleep 30\n").unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();

        let request = CriticCoordinatorRequest {
            version: 1,
            objective: "Review the behavior".into(),
            scope: "Check the frozen function".into(),
            snapshot: "fn value() -> bool { true }\n".into(),
            roles: vec![CriticRole::Correctness],
            max_output_tokens: Some(64),
            time_limit_seconds: Some(120),
            thinking_effort: None,
        };
        let cancellation = CancellationToken::new();
        let child_cancellation = cancellation.clone();
        let input = encode_request(&request).unwrap();
        let task = tokio::spawn(async move {
            run_coordinator(
                direct_test_spawner(),
                fake,
                temp.path().to_path_buf(),
                std::env::var("PATH").unwrap_or_default(),
                BTreeMap::new(),
                input,
                120,
                child_cancellation,
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancellation.cancel();
        let result = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("cancelled coordinator should return promptly")
            .expect("critic task should not panic");
        assert!(result.unwrap_err().contains("canceled"));
    }
}
