//! Local, source-linked journal for Buzz-managed ACP turns.
//!
//! This records transport lifecycle evidence only. A returned ACP turn does
//! not prove that the user's task is complete, so `task_state` stays unknown.

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    fs::OpenOptions,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::types::Value as SqlValue;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use url::Url;
use uuid::Uuid;

mod goal_runs;
mod project_admission;
pub use goal_runs::{
    GoalRunRecord, GoalRunSpec, GoalRunState, GoalTaskEvidence, GoalTaskRecord, GoalTaskSpec,
    GoalTaskState,
};
pub use project_admission::{
    ProjectAdmissionJournal, ProjectAdmissionStatus, ProjectLeaseRelease, ProjectReserveOutcome,
};

const MAX_SOURCE_EVENTS: usize = 512;
const MAX_LIST_LIMIT: usize = 100;
const MAX_EVENT_LIMIT: usize = 500;
const THREAD_BRIEF_MAX_TURNS: usize = 20;
const THREAD_BRIEF_MAX_EVENTS_PER_TURN: usize = 40;
const THREAD_BRIEF_MAX_RUNS: usize = 20;
const THREAD_BRIEF_MAX_RUN_EVENTS: usize = 40;
const RUN_MAX_ATTEMPTS: usize = 20;
/// Bound the number of attempt rows a project page may disclose after source checks.
pub const PROJECT_RUN_ATTEMPT_EVIDENCE_LIMIT: usize = 100;
/// A single attempt with more sources than this is omitted from project evidence.
pub const PROJECT_RUN_ATTEMPT_SOURCE_LIMIT: usize = 64;
/// Bound relay source checks across one project page.
pub const PROJECT_RUN_SOURCE_CHECK_LIMIT: usize = 2_000;
const MAX_ROUTE_SAMPLES_PER_TURN: usize = 32;
const MAX_ROUTE_SAMPLES_PER_IDENTITY: usize = 20;
const MAX_ROUTE_SAMPLES_GLOBAL: usize = 4_096;
const ROUTE_SAMPLE_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
const ROUTE_SAMPLE_FRESH_MS: i64 = 7 * 24 * 60 * 60 * 1_000;
const ROUTE_SAMPLE_MINIMUM_COUNT: usize = 5;
const MAX_ROUTE_THROUGHPUT_SUMMARIES: usize = 128;
const MAX_CRITIC_ROUND_BYTES: usize = 96 * 1024;
const MAX_CRITIC_OUTPUT_BYTES: usize = 24 * 1024;

type RouteThroughputGroupKey = (String, String, String, String, String, String, String);
type RouteThroughputGroupSamples = Vec<(u64, i64)>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StartRecord {
    pub turn_id: String,
    /// Managed Buzz ACP process incarnation that dispatched this attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_worker_generation_nonce: Option<String>,
    /// Exact ACP adapter-child spawn serving this attempt. PID is not used as
    /// identity because the operating system may reuse it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_child_generation_id: Option<String>,
    pub channel_id: Option<String>,
    pub session_scope: String,
    pub thread_root_event_id: Option<String>,
    /// Events that triggered this dispatch attempt. This does not claim to
    /// cover fetched conversation context or later in-turn steering.
    pub batch_trigger_event_ids: Vec<String>,
    /// Earlier queued events merged back into this prompt after cancellation.
    pub merged_cancelled_event_ids: Vec<String>,
    pub agent_index: u32,
    /// Runtime-enforced settings captured when this ACP turn was dispatched.
    pub configured_worker_pool_slots: u32,
    pub idle_timeout_secs: u64,
    pub max_turn_duration_secs: u64,
    pub agent_profile: AgentProfileSnapshot,
}

/// Non-secret profile provenance pinned to one dispatched managed-agent turn.
/// The prompt itself is never written to the journal; only its SHA-256 digest.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AgentProfileSnapshot {
    pub harness_id: String,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
    pub agent_prompt_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_profile_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_profile_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_profile_hash: Option<String>,
    /// Versioned provider-routing profile pinned to this launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_profile_version: Option<u32>,
    /// Fingerprint of the launch-resolved route document and target prompts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_profile_hash: Option<String>,
}

/// Non-secret route decision emitted by Buzz Agent before its first provider
/// request for one ACP prompt. `attempt_id` is the agent's per-prompt run ID;
/// `session_id` and the journal's managed `turn_id` provide independent joins.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteDecisionRecord {
    /// ACP session identifier reported on the notification.
    pub session_id: String,
    /// Buzz Agent's per-prompt active run ID.
    pub attempt_id: String,
    /// Route profile pinned at process launch.
    pub profile_id: Option<String>,
    /// Route profile version pinned at process launch.
    pub profile_version: Option<u32>,
    /// Launch-resolved route profile and prompt binding fingerprint.
    pub profile_hash: Option<String>,
    /// Result of evaluating or bypassing the configured profile.
    pub outcome: RouteDecisionOutcome,
    /// Selected profile candidate, when evaluation chose one.
    pub candidate_id: Option<String>,
    /// Canonical provider identifier for the candidate.
    pub provider_id: Option<String>,
    /// Provider model identifier for the candidate.
    pub model_id: Option<String>,
    /// Allowlisted reason code for an abstention, refusal, or override.
    pub reason_code: Option<String>,
    /// Non-secret strict-fit summary for a selected route, when enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_fit: Option<RouteContextFitRecord>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteContextFitRecord {
    pub estimate_method: String,
    pub capacity_source: RouteContextCapacitySource,
    pub input_tokens_upper_bound: u64,
    pub capacity_tokens: u64,
}

/// A successful, usage-bearing provider request. It deliberately excludes
/// prompts, completions, endpoint URLs, credentials, and provider errors.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteThroughputSample {
    pub session_id: String,
    pub attempt_id: String,
    pub profile_id: String,
    pub profile_version: u32,
    pub profile_hash: String,
    /// Device-keyed HMAC fingerprint of the provider endpoint; the URL itself
    /// is never stored or transmitted.
    pub endpoint_hash: String,
    pub candidate_id: String,
    pub provider_id: String,
    pub model_id: String,
    /// `default` means no explicit reasoning-effort value was requested.
    pub thinking_effort: String,
    /// Monotonic provider-call sequence within the active ACP prompt.
    pub request_sequence: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// End-to-end Buzz provider-call duration, including request/response wait.
    pub elapsed_ms: u64,
    /// Provider output tokens per second, in milli-tokens/sec.
    pub effective_output_tokens_per_second_milli: u64,
}

impl RouteThroughputSample {
    pub fn validate(&self) -> Result<(), String> {
        if self.session_id.is_empty()
            || self.session_id.len() > 1024
            || self.session_id.chars().any(char::is_control)
        {
            return Err("invalid route throughput session ID".into());
        }
        if self.attempt_id.len() > 256
            || !self.attempt_id.starts_with("run_")
            || !self
                .attempt_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        {
            return Err("invalid route throughput attempt ID".into());
        }
        if !valid_route_profile_id(&self.profile_id)
            || self.profile_version == 0
            || !valid_lower_sha256(&self.profile_hash)
            || !valid_lower_sha256(&self.endpoint_hash)
            || !valid_route_candidate_id(&self.candidate_id)
            || !matches!(
                self.provider_id.as_str(),
                "anthropic" | "openai" | "databricks" | "databricks-v2" | "openrouter" | "deepseek"
            )
            || self.model_id.is_empty()
            || self.model_id.len() > 256
            || self.model_id.chars().any(char::is_control)
            || !matches!(
                self.thinking_effort.as_str(),
                "default" | "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
            )
            || self.request_sequence == 0
            || self.request_sequence > i64::MAX as u64
            || self.input_tokens == 0
            || self.input_tokens > i64::MAX as u64
            || self.output_tokens == 0
            || self.output_tokens > i64::MAX as u64
            || self.elapsed_ms == 0
            || self.elapsed_ms > 24 * 60 * 60 * 1_000
            || self.effective_output_tokens_per_second_milli > i64::MAX as u64
        {
            return Err("invalid route throughput sample".into());
        }
        let expected = u128::from(self.output_tokens)
            .checked_mul(1_000_000)
            .and_then(|value| value.checked_div(u128::from(self.elapsed_ms)))
            .and_then(|value| u64::try_from(value).ok());
        if expected != Some(self.effective_output_tokens_per_second_milli)
            || self.effective_output_tokens_per_second_milli == 0
        {
            return Err("invalid effective output throughput".into());
        }
        Ok(())
    }
}

/// Local lookup key for fresh route-speed evidence. Input size maps to a
/// coarse bucket so a tiny prompt does not stand in for a very large one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteThroughputQuery {
    pub profile_hash: String,
    pub endpoint_hash: String,
    pub candidate_id: String,
    pub provider_id: String,
    pub model_id: String,
    pub thinking_effort: String,
    pub input_tokens: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteThroughputSummary {
    pub fresh_sample_count: usize,
    /// Conservative lower-quartile rate; unknown until five fresh samples.
    pub effective_output_tokens_per_second_milli: Option<u64>,
    pub freshest_sample_at_ms: Option<i64>,
}

/// Bounded settings captured for one explicit critic review round.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CriticRoundSettings {
    pub max_output_tokens: u32,
    pub time_limit_seconds: u64,
    pub thinking_effort_requested: Option<String>,
    /// Requested aggregate estimated ceiling. Old local history may omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_round_cost_budget_microusd: Option<u64>,
    /// Legacy route shared by every reviewer. Newer rows store one route per
    /// reviewer instead; older journal rows may omit this field.
    #[serde(default)]
    pub route_profile: Option<CriticRouteProfileRef>,
    /// Local coordinator guide shown to the human before this review. The
    /// guide text is not persisted or sent to the review workers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_guide: Option<CriticGuideRef>,
}

/// Fixed local guide provenance captured for one explicitly requested review.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CriticGuideRef {
    pub path: String,
    pub sha256: String,
}

/// Versioned, secret-free identity of the resolved critic route profile.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CriticRouteProfileRef {
    pub id: String,
    pub version: u32,
    pub hash: String,
}

/// One prompt-separated reviewer result. The submitted source snapshot and
/// objective are deliberately excluded; only their hashes are stored below.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CriticReviewerRecord {
    pub role: String,
    pub status: String,
    pub output: Option<String>,
    pub output_truncated: bool,
    pub stop_reason: Option<String>,
    pub candidate_id: Option<String>,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
    /// Exact local route profile applied to this reviewer. Older rounds omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_profile: Option<CriticRouteProfileRef>,
    /// Effective estimated route ceiling for this reviewer's single ACP turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cost_limit_microusd: Option<u64>,
    pub elapsed_ms: Option<u64>,
    pub error_code: Option<String>,
}

/// Local identity-scoped review history, keyed by the exact frozen snapshot.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CriticRoundRecord {
    pub round_id: String,
    pub created_at_ms: i64,
    pub snapshot_sha256: String,
    pub objective_sha256: String,
    pub scope_sha256: String,
    pub settings: CriticRoundSettings,
    pub reviewers: Vec<CriticReviewerRecord>,
}

/// Small history row that omits reviewer finding text until the user opens it.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CriticReviewerSummary {
    pub role: String,
    pub status: String,
    pub output_truncated: bool,
    pub candidate_id: Option<String>,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
    /// Exact local route profile applied to this reviewer. Older rounds omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_profile: Option<CriticRouteProfileRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cost_limit_microusd: Option<u64>,
    pub elapsed_ms: Option<u64>,
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CriticRoundSummary {
    pub round_id: String,
    pub created_at_ms: i64,
    pub snapshot_sha256: String,
    pub objective_sha256: String,
    pub scope_sha256: String,
    pub settings: CriticRoundSettings,
    pub reviewers: Vec<CriticReviewerSummary>,
}

impl From<CriticRoundRecord> for CriticRoundSummary {
    fn from(record: CriticRoundRecord) -> Self {
        Self {
            round_id: record.round_id,
            created_at_ms: record.created_at_ms,
            snapshot_sha256: record.snapshot_sha256,
            objective_sha256: record.objective_sha256,
            scope_sha256: record.scope_sha256,
            settings: record.settings,
            reviewers: record
                .reviewers
                .into_iter()
                .map(|reviewer| CriticReviewerSummary {
                    role: reviewer.role,
                    status: reviewer.status,
                    output_truncated: reviewer.output_truncated,
                    candidate_id: reviewer.candidate_id,
                    provider_id: reviewer.provider_id,
                    model_id: reviewer.model_id,
                    route_profile: reviewer.route_profile,
                    estimated_cost_limit_microusd: reviewer.estimated_cost_limit_microusd,
                    elapsed_ms: reviewer.elapsed_ms,
                    error_code: reviewer.error_code,
                })
                .collect(),
        }
    }
}

fn validate_critic_round_record(record: &CriticRoundRecord) -> Result<(), String> {
    const MAX_ESTIMATED_COST_MICROUSD: u64 = 1_000_000_000_000;
    let round_id =
        Uuid::parse_str(&record.round_id).map_err(|_| "invalid critic round ID".to_string())?;
    if round_id.to_string() != record.round_id
        || record.created_at_ms <= 0
        || !valid_lower_sha256(&record.snapshot_sha256)
        || !valid_lower_sha256(&record.objective_sha256)
        || !valid_lower_sha256(&record.scope_sha256)
        || !(64..=2_048).contains(&record.settings.max_output_tokens)
        || !(15..=120).contains(&record.settings.time_limit_seconds)
        || record
            .settings
            .estimated_round_cost_budget_microusd
            .is_some_and(|budget| budget > MAX_ESTIMATED_COST_MICROUSD)
        || record
            .settings
            .thinking_effort_requested
            .as_deref()
            .is_some_and(|effort| {
                !matches!(
                    effort,
                    "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
                )
            })
        || record
            .settings
            .route_profile
            .as_ref()
            .is_some_and(|profile| {
                !valid_critic_route_profile_id(&profile.id)
                    || profile.version == 0
                    || !valid_lower_sha256(&profile.hash)
            })
        || record
            .settings
            .coordinator_guide
            .as_ref()
            .is_some_and(|guide| {
                guide.path != "AGENT_GUIDES/CRITICS.md" || !valid_lower_sha256(&guide.sha256)
            })
        || !(1..=3).contains(&record.reviewers.len())
    {
        return Err("invalid critic round record".into());
    }

    let mut seen_roles = BTreeSet::new();
    for reviewer in &record.reviewers {
        let valid_role = matches!(
            reviewer.role.as_str(),
            "correctness"
                | "security"
                | "architecture"
                | "ui_accessibility"
                | "performance"
                | "product"
        );
        if (!valid_role && reviewer.role != "unknown")
            || (valid_role && !seen_roles.insert(reviewer.role.as_str()))
            || (reviewer.role == "unknown"
                && (reviewer.status != "failed"
                    || reviewer.error_code.as_deref() != Some("worker_join_failed")))
            || !matches!(reviewer.status.as_str(), "completed" | "failed")
            || reviewer.output.as_deref().is_some_and(|output| {
                output.len() > MAX_CRITIC_OUTPUT_BYTES || output.contains('\0')
            })
            || reviewer
                .stop_reason
                .as_deref()
                .is_some_and(|value| !valid_critic_metadata(value, 64))
            || reviewer
                .candidate_id
                .as_deref()
                .is_some_and(|value| !valid_critic_metadata(value, 256))
            || reviewer
                .provider_id
                .as_deref()
                .is_some_and(|value| !valid_critic_metadata(value, 256))
            || reviewer
                .model_id
                .as_deref()
                .is_some_and(|value| !valid_critic_metadata(value, 256))
            || reviewer.route_profile.as_ref().is_some_and(|profile| {
                !valid_critic_route_profile_id(&profile.id)
                    || profile.version == 0
                    || !valid_lower_sha256(&profile.hash)
            })
            || reviewer
                .estimated_cost_limit_microusd
                .is_some_and(|limit| limit > MAX_ESTIMATED_COST_MICROUSD)
            || reviewer
                .elapsed_ms
                .is_some_and(|elapsed| elapsed > 24 * 60 * 60 * 1_000)
            || reviewer.error_code.as_deref().is_some_and(|code| {
                code.is_empty()
                    || code.len() > 64
                    || !code.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                    })
            })
        {
            return Err("invalid critic reviewer record".into());
        }
    }
    if let Some(round_budget) = record.settings.estimated_round_cost_budget_microusd {
        let reviewer_budget_total = record.reviewers.iter().try_fold(0u64, |total, reviewer| {
            total.checked_add(reviewer.estimated_cost_limit_microusd?)
        });
        if reviewer_budget_total.is_none_or(|total| total > round_budget) {
            return Err("invalid aggregate critic cost budget allocation".into());
        }
    }
    Ok(())
}

fn valid_critic_route_profile_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !id.ends_with('-')
        && !id.contains("--")
}

fn valid_critic_metadata(value: &str, max_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
}

/// Fresh throughput observations grouped by the exact route and input bucket.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RouteThroughputGroupSummary {
    /// Launch-resolved route-profile hash; target prompt profiles can make this
    /// differ between agents using the same saved profile version.
    pub profile_hash: String,
    /// Device-keyed endpoint fingerprint; the endpoint URL is never returned.
    pub endpoint_hash: String,
    pub candidate_id: String,
    pub provider_id: String,
    pub model_id: String,
    pub thinking_effort: String,
    pub input_bucket: String,
    pub fresh_sample_count: usize,
    /// Conservative lower-quartile rate; unknown until five fresh samples.
    pub effective_output_tokens_per_second_milli: Option<u64>,
    pub freshest_sample_at_ms: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RouteContextCapacitySource {
    OperatorDeclared,
}

/// Safe summary of the per-prompt route evaluation result.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RouteDecisionOutcome {
    Selected,
    Abstained,
    Refused,
    Overridden,
}

impl RouteDecisionRecord {
    /// Reject malformed or unbounded data at both the ACP boundary and the
    /// durable journal boundary. Raw prompt/error text is never a valid field.
    pub fn validate(&self) -> Result<(), String> {
        if self.session_id.is_empty()
            || self.session_id.len() > 1024
            || self.session_id.chars().any(char::is_control)
        {
            return Err("invalid route decision session ID".into());
        }
        if self.attempt_id.len() > 256
            || !self.attempt_id.starts_with("run_")
            || !self
                .attempt_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        {
            return Err("invalid route decision attempt ID".into());
        }

        let profile_fields = [
            self.profile_id.is_some(),
            self.profile_version.is_some(),
            self.profile_hash.is_some(),
        ];
        if profile_fields.iter().any(|present| *present)
            && !profile_fields.iter().all(|present| *present)
        {
            return Err("incomplete route profile identity".into());
        }
        if let Some(id) = &self.profile_id {
            if !valid_route_profile_id(id)
                || self.profile_version.is_none_or(|version| version == 0)
                || !self.profile_hash.as_deref().is_some_and(valid_lower_sha256)
            {
                return Err("invalid route profile identity".into());
            }
        }

        let candidate_fields = [
            self.candidate_id.is_some(),
            self.provider_id.is_some(),
            self.model_id.is_some(),
        ];
        if candidate_fields.iter().any(|present| *present)
            && !candidate_fields.iter().all(|present| *present)
        {
            return Err("incomplete route candidate identity".into());
        }
        if let Some(candidate_id) = &self.candidate_id {
            if !valid_route_candidate_id(candidate_id)
                || self.model_id.as_deref().is_none_or(|model| {
                    model.is_empty() || model.len() > 256 || model.chars().any(char::is_control)
                })
                || !matches!(
                    self.provider_id.as_deref(),
                    Some(
                        "anthropic"
                            | "openai"
                            | "databricks"
                            | "databricks-v2"
                            | "openrouter"
                            | "deepseek"
                    )
                )
            {
                return Err("invalid route candidate identity".into());
            }
        }

        if let Some(context_fit) = &self.context_fit {
            if context_fit.estimate_method != "utf8_bytes_plus_framing_and_output_reserve_v1"
                || context_fit.capacity_source != RouteContextCapacitySource::OperatorDeclared
                || context_fit.input_tokens_upper_bound == 0
                || context_fit.input_tokens_upper_bound > context_fit.capacity_tokens
                || context_fit.capacity_tokens == 0
                || self.candidate_id.is_none()
            {
                return Err("invalid route context-fit summary".into());
            }
        }

        const REASONS: &[&str] = &[
            "safety_refusal",
            "invalid_preference_configuration",
            "unknown_preference",
            "ineligible_preference",
            "no_eligible_candidate",
            "multiple_eligible_without_preference",
            "route_abstained",
            "route_configuration_invalid",
            "provider_client_initialization_failed",
            "provider_connection_unconfigured",
            "explicit_session_model_override",
            "manual_override_model_not_listed",
            "manual_override_model_ambiguous",
        ];
        match self.outcome {
            RouteDecisionOutcome::Selected => {
                if !candidate_fields.iter().all(|present| *present) || self.reason_code.is_some() {
                    return Err("selected route requires candidate identity and no reason".into());
                }
            }
            RouteDecisionOutcome::Abstained
            | RouteDecisionOutcome::Refused
            | RouteDecisionOutcome::Overridden => {
                if !self
                    .reason_code
                    .as_deref()
                    .is_some_and(|reason| REASONS.contains(&reason))
                {
                    return Err("route decision requires a known safe reason code".into());
                }
            }
        }
        Ok(())
    }
}

fn valid_route_profile_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.ends_with('-')
        && !value.contains("--")
}

fn valid_route_candidate_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_lower_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn route_input_bucket(input_tokens: u64) -> &'static str {
    match input_tokens {
        0..=2_048 => "tiny",
        2_049..=8_192 => "small",
        8_193..=32_768 => "medium",
        _ => "large",
    }
}

fn lower_quartile_route_rate(rates: &[u64]) -> Option<u64> {
    if rates.len() < ROUTE_SAMPLE_MINIMUM_COUNT {
        return None;
    }
    let mut sorted = rates.to_vec();
    sorted.sort_unstable();
    sorted.get((sorted.len() - 1) / 4).copied()
}

fn validate_route_throughput_query(query: &RouteThroughputQuery) -> Result<(), String> {
    if !valid_lower_sha256(&query.profile_hash)
        || !valid_lower_sha256(&query.endpoint_hash)
        || !valid_route_candidate_id(&query.candidate_id)
        || !matches!(
            query.provider_id.as_str(),
            "anthropic" | "openai" | "databricks" | "databricks-v2" | "openrouter" | "deepseek"
        )
        || query.model_id.is_empty()
        || query.model_id.len() > 256
        || query.model_id.chars().any(char::is_control)
        || !matches!(
            query.thinking_effort.as_str(),
            "default" | "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
        )
        || query.input_tokens == 0
    {
        return Err("invalid route throughput lookup".into());
    }
    Ok(())
}

/// Fingerprint a configured persona prompt without retaining or logging it.
pub fn agent_prompt_fingerprint(prompt: Option<&str>) -> Option<String> {
    prompt
        .filter(|prompt| !prompt.trim().is_empty())
        .map(|prompt| hex::encode(Sha256::digest(prompt.as_bytes())))
}

fn agent_profile_event(profile: &AgentProfileSnapshot) -> Value {
    let mut event = serde_json::json!({
        "schema_version": 1,
        "source": "managed_agent_runtime_config",
        "harness_id": profile.harness_id,
        "provider_id": profile.provider_id,
        "model_id": profile.model_id,
        "agent_prompt_sha256": profile.agent_prompt_sha256,
        "execution_profile_id": profile.execution_profile_id,
        "execution_profile_version": profile.execution_profile_version,
        "prompt_profile_id": profile.prompt_profile_id,
        "prompt_profile_version": profile.prompt_profile_version,
        "prompt_profile_hash": profile.prompt_profile_hash,
        "prompt_content_stored": false,
    });
    if let Some(value) = &profile.route_profile_id {
        event["route_profile_id"] = serde_json::json!(value);
    }
    if let Some(value) = profile.route_profile_version {
        event["route_profile_version"] = serde_json::json!(value);
    }
    if let Some(value) = &profile.route_profile_hash {
        event["route_profile_hash"] = serde_json::json!(value);
    }
    event
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TurnSummary {
    pub turn_id: String,
    pub channel_id: Option<String>,
    pub session_scope: String,
    pub thread_root_event_id: Option<String>,
    pub batch_trigger_event_ids: Vec<String>,
    pub merged_cancelled_event_ids: Vec<String>,
    pub agent_index: u32,
    pub acp_session_id: Option<String>,
    pub status: String,
    /// No process-liveness check is performed by this local history reader.
    pub liveness: String,
    pub task_state: String,
    pub started_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TurnEvent {
    pub sequence: i64,
    pub turn_id: String,
    pub kind: String,
    pub occurred_at_ms: i64,
    pub details: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ThreadAttemptBrief {
    pub turn: TurnSummary,
    pub recent_events: Vec<TurnEvent>,
    pub event_history_may_be_truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ThreadAttemptHistory {
    pub managed_turns: Vec<ThreadAttemptBrief>,
    pub has_more_turns: bool,
    /// Persisted write failures for this relay/owner journal. These gaps may
    /// include other threads in the same local identity scope.
    pub capture_gap_count: u64,
}

/// Stable local identity for work triggered by one readable Buzz message.
/// This is distinct from its channel, thread, workflow, ACP session, and turns.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CoordinatorRunSummary {
    pub run_id: String,
    pub channel_id: String,
    pub session_scope: String,
    pub thread_root_event_id: Option<String>,
    pub original_intent_event_id: String,
    /// NIP-MP project home verified for the channel at the first project link.
    pub project_coordinate: Option<String>,
    /// A later attempt resolved a different project for this same run.
    pub project_link_conflict: bool,
    pub attempt_turns: Vec<TurnSummary>,
    pub attempt_history_may_be_truncated: bool,
    pub recent_events: Vec<CoordinatorRunEvent>,
    pub event_history_may_be_truncated: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    /// ACP transport state does not establish whether the user's task is done.
    pub task_state: String,
}

/// Append-only coordinator-run lifecycle evidence.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CoordinatorRunEvent {
    pub sequence: i64,
    pub run_id: String,
    pub event_key: String,
    pub kind: String,
    pub occurred_at_ms: i64,
    pub details: Value,
}

/// Bounded coordinator-run history for one relay-authorized thread.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CoordinatorRunHistory {
    pub runs: Vec<CoordinatorRunSummary>,
    pub has_more_runs: bool,
    /// Persisted write failures for this relay/owner journal. These gaps may
    /// include other threads in the same local identity scope.
    pub capture_gap_count: u64,
}

/// Stable keyset position for a project-scoped candidate run query.
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct CoordinatorRunCursor {
    pub updated_at_ms: i64,
    pub run_id: String,
}

/// Candidate run page before the caller applies relay source-read checks.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectCoordinatorRunHistory {
    pub runs: Vec<CoordinatorRunSummary>,
    pub has_more_candidates: bool,
    pub next_cursor: Option<CoordinatorRunCursor>,
}

/// Attempt facts safe to return after each captured source is re-read on the
/// channel recorded by its turn. No process-control handle or inferred
/// generation is included.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectAttemptEvidence {
    pub turn_id: String,
    pub channel_id: String,
    pub session_scope: String,
    pub thread_root_event_id: Option<String>,
    pub batch_trigger_event_ids: Vec<String>,
    pub merged_cancelled_event_ids: Vec<String>,
    pub agent_index: u32,
    #[serde(skip)]
    pub acp_session_id: Option<String>,
    pub status: String,
    pub liveness: String,
    pub task_state: String,
    pub runtime_session_match: &'static str,
    pub control_target_available: bool,
    pub started_at_ms: i64,
    pub updated_at_ms: i64,
}

/// Project-safe run projection. Journal event details are excluded because
/// they can contain source IDs not covered by per-attempt authorization.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectCoordinatorRunEvidence {
    pub run_id: String,
    pub channel_id: String,
    pub session_scope: String,
    pub thread_root_event_id: Option<String>,
    pub original_intent_event_id: String,
    pub project_coordinate: Option<String>,
    pub project_link_conflict: bool,
    pub attempt_turns: Vec<ProjectAttemptEvidence>,
    pub attempt_history_may_be_truncated: bool,
    pub event_history_may_be_truncated: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub task_state: &'static str,
    pub history_reliability: &'static str,
    pub history_completeness: &'static str,
    /// Internal signal aggregated into the enclosing project page response.
    #[serde(skip)]
    pub attempt_evidence_may_be_truncated: bool,
}

/// Bounded set of attempt-source IDs and whether a page limit omitted any.
#[derive(Clone, Debug, Default)]
pub struct ProjectRunAttemptSourceTargets {
    pub targets: BTreeSet<(String, String)>,
    pub may_be_truncated: bool,
}

/// Return `(recorded_channel_id, source_event_id)` pairs for bounded source
/// authorization checks. Invalid attempts contribute no targets; oversized
/// attempts set `may_be_truncated` and contribute no targets.
pub fn project_run_attempt_source_targets(
    runs: &[CoordinatorRunSummary],
) -> ProjectRunAttemptSourceTargets {
    let mut result = ProjectRunAttemptSourceTargets::default();
    for run in runs {
        for turn in &run.attempt_turns {
            let Some(channel_id) = turn
                .channel_id
                .as_deref()
                .and_then(|value| Uuid::parse_str(value).ok())
                .map(|value| value.to_string())
            else {
                continue;
            };
            let source_ids = turn
                .batch_trigger_event_ids
                .iter()
                .chain(&turn.merged_cancelled_event_ids)
                .chain(turn.thread_root_event_id.iter())
                .map(|id| id.to_ascii_lowercase())
                .collect::<BTreeSet<_>>();
            if source_ids.is_empty() || source_ids.iter().any(|id| validate_event_id(id).is_err()) {
                continue;
            }
            if source_ids.len() > PROJECT_RUN_ATTEMPT_SOURCE_LIMIT {
                result.may_be_truncated = true;
                continue;
            }
            for source_id in source_ids {
                let target = (channel_id.clone(), source_id);
                if result.targets.contains(&target) {
                    continue;
                }
                if result.targets.len() < PROJECT_RUN_SOURCE_CHECK_LIMIT {
                    result.targets.insert(target);
                } else {
                    result.may_be_truncated = true;
                }
            }
        }
    }
    result
}

/// Project only attempts whose captured source IDs were re-read and authorized
/// on each attempt's recorded channel.
pub fn project_run_evidence(
    run: &CoordinatorRunSummary,
    readable_sources: &BTreeSet<(String, String)>,
    remaining_attempt_limit: usize,
) -> ProjectCoordinatorRunEvidence {
    let mut attempt_turns = Vec::new();
    let mut attempt_evidence_may_be_truncated = false;
    for turn in &run.attempt_turns {
        let Some(channel_id) = turn
            .channel_id
            .as_deref()
            .and_then(|value| Uuid::parse_str(value).ok())
            .map(|value| value.to_string())
        else {
            continue;
        };
        let source_ids = turn
            .batch_trigger_event_ids
            .iter()
            .chain(&turn.merged_cancelled_event_ids)
            .chain(turn.thread_root_event_id.iter())
            .map(|id| id.to_ascii_lowercase())
            .collect::<BTreeSet<_>>();
        if source_ids.is_empty()
            || source_ids.len() > PROJECT_RUN_ATTEMPT_SOURCE_LIMIT
            || source_ids.iter().any(|id| validate_event_id(id).is_err())
            || source_ids
                .iter()
                .any(|id| !readable_sources.contains(&(channel_id.clone(), id.clone())))
        {
            continue;
        }
        if attempt_turns.len() >= remaining_attempt_limit {
            attempt_evidence_may_be_truncated = true;
            continue;
        }
        attempt_turns.push(ProjectAttemptEvidence {
            turn_id: turn.turn_id.clone(),
            channel_id,
            session_scope: turn.session_scope.clone(),
            thread_root_event_id: turn.thread_root_event_id.clone(),
            batch_trigger_event_ids: turn.batch_trigger_event_ids.clone(),
            merged_cancelled_event_ids: turn.merged_cancelled_event_ids.clone(),
            agent_index: turn.agent_index,
            acp_session_id: turn.acp_session_id.clone(),
            status: turn.status.clone(),
            liveness: "unknown".into(),
            task_state: "unknown".into(),
            runtime_session_match: "unknown",
            control_target_available: false,
            started_at_ms: turn.started_at_ms,
            updated_at_ms: turn.updated_at_ms,
        });
    }
    ProjectCoordinatorRunEvidence {
        run_id: run.run_id.clone(),
        channel_id: run.channel_id.clone(),
        session_scope: run.session_scope.clone(),
        thread_root_event_id: run.thread_root_event_id.clone(),
        original_intent_event_id: run.original_intent_event_id.clone(),
        project_coordinate: run.project_coordinate.clone(),
        project_link_conflict: run.project_link_conflict,
        attempt_turns,
        attempt_history_may_be_truncated: run.attempt_history_may_be_truncated,
        event_history_may_be_truncated: run.event_history_may_be_truncated,
        created_at_ms: run.created_at_ms,
        updated_at_ms: run.updated_at_ms,
        task_state: "unknown",
        history_reliability: "best_effort",
        history_completeness: "unknown",
        attempt_evidence_may_be_truncated,
    }
}

#[derive(Clone, Debug)]
pub enum JournalCommand {
    Started(StartRecord),
    ProjectLinked {
        turn_id: String,
        project_coordinate: String,
    },
    SessionResolved {
        turn_id: String,
        session_id: String,
    },
    RouteDecision {
        turn_id: String,
        decision: RouteDecisionRecord,
    },
    RouteThroughputSample {
        turn_id: String,
        sample: RouteThroughputSample,
    },
    PromptCallStarted {
        turn_id: String,
    },
    SteerSubmitted {
        turn_id: String,
        source_event_id: String,
    },
    SteerOutcome {
        turn_id: String,
        source_event_id: String,
        outcome: SteerOutcome,
    },
    Returned {
        turn_id: String,
        outcome: String,
    },
    WorkerCrashed {
        turn_id: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SteerOutcome {
    /// The ACP adapter reported that it accepted the steer. This does not
    /// prove the model observed it or completed the user's task.
    AdapterAcknowledged,
    /// The adapter responded, but rejected the steer request.
    AdapterRejected,
    /// The steer could not be sent or was explicitly not accepted.
    AttemptFailed,
    /// The prompt ended or transport failed before delivery could be known.
    DeliveryUnknown,
}

impl SteerOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::AdapterAcknowledged => "adapter_acknowledged",
            Self::AdapterRejected => "adapter_rejected",
            Self::AttemptFailed => "attempt_failed",
            Self::DeliveryUnknown => "delivery_unknown",
        }
    }
}

pub struct RunJournal {
    db_path: PathBuf,
}

/// The managed desktop sets `BUZZ_NEST_DIR` so production and development
/// builds keep separate local journals. Standalone CLI calls default to `~/.buzz`.
pub fn nest_dir_from_env_or_default() -> Result<PathBuf, String> {
    if let Some(path) = env::var_os("BUZZ_NEST_DIR") {
        let path = PathBuf::from(path);
        if path.as_os_str().is_empty() {
            return Err("BUZZ_NEST_DIR is empty".into());
        }
        return Ok(path);
    }
    dirs::home_dir()
        .map(|home| home.join(".buzz"))
        .ok_or_else(|| "cannot resolve home directory for Buzz run journal".into())
}

impl RunJournal {
    /// Open the journal scoped to one Buzz relay and owner identity.
    pub fn open_scoped(
        nest_dir: impl AsRef<Path>,
        relay_url: &str,
        owner_pubkey: &str,
    ) -> Result<Self, String> {
        let db_file = scoped_db_file(relay_url, owner_pubkey)?;
        if nest_dir
            .as_ref()
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err("Buzz nest is a symlink; refusing run journal path".into());
        }
        fs::create_dir_all(nest_dir.as_ref())
            .map_err(|error| format!("create Buzz nest: {error}"))?;
        let journal_dir = nest_dir.as_ref().join("agent-run-journals");
        if journal_dir
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err("Buzz run journal directory is a symlink".into());
        }
        fs::create_dir_all(&journal_dir)
            .map_err(|error| format!("create Buzz run journal directory: {error}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Keep the private metadata directory restrictive without changing
            // permissions on the user's entire existing Buzz nest.
            fs::set_permissions(&journal_dir, fs::Permissions::from_mode(0o700))
                .map_err(|error| format!("secure Buzz run journal directory: {error}"))?;
        }
        let journal = Self {
            db_path: journal_dir.join(db_file),
        };
        let mut conn = journal.connect()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS managed_turns (
                turn_id TEXT PRIMARY KEY,
                channel_id TEXT,
                session_scope TEXT NOT NULL,
                thread_root_event_id TEXT,
                batch_trigger_event_ids TEXT NOT NULL,
                merged_cancelled_event_ids TEXT NOT NULL,
                agent_index INTEGER NOT NULL,
                acp_session_id TEXT,
                status TEXT NOT NULL,
                task_state TEXT NOT NULL DEFAULT 'unknown',
                started_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS managed_turn_events (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                turn_id TEXT NOT NULL REFERENCES managed_turns(turn_id),
                kind TEXT NOT NULL,
                occurred_at_ms INTEGER NOT NULL,
                details_json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS journal_capture_gaps (
                gap_id INTEGER PRIMARY KEY AUTOINCREMENT,
                occurred_at_ms INTEGER NOT NULL,
                failed_event_count INTEGER NOT NULL CHECK(failed_event_count > 0)
            );
            CREATE TABLE IF NOT EXISTS managed_turn_sources (
                turn_id TEXT NOT NULL REFERENCES managed_turns(turn_id),
                source_event_id TEXT NOT NULL,
                source_kind TEXT NOT NULL,
                PRIMARY KEY(turn_id, source_event_id, source_kind)
            );
            CREATE TABLE IF NOT EXISTS coordinator_runs (
                run_id TEXT PRIMARY KEY,
                channel_id TEXT NOT NULL,
                session_scope TEXT NOT NULL,
                thread_root_key TEXT NOT NULL,
                original_intent_event_id TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                UNIQUE(channel_id, session_scope, thread_root_key, original_intent_event_id)
            );
            CREATE TABLE IF NOT EXISTS coordinator_run_sources (
                run_id TEXT NOT NULL REFERENCES coordinator_runs(run_id),
                source_event_id TEXT NOT NULL,
                source_kind TEXT NOT NULL,
                PRIMARY KEY(run_id, source_event_id, source_kind)
            );
            CREATE TABLE IF NOT EXISTS coordinator_run_turns (
                run_id TEXT NOT NULL REFERENCES coordinator_runs(run_id),
                turn_id TEXT NOT NULL REFERENCES managed_turns(turn_id),
                linked_at_ms INTEGER NOT NULL,
                PRIMARY KEY(run_id, turn_id)
            );
            CREATE TABLE IF NOT EXISTS coordinator_run_projects (
                run_id TEXT PRIMARY KEY REFERENCES coordinator_runs(run_id),
                project_coordinate TEXT NOT NULL,
                linked_at_ms INTEGER NOT NULL,
                linked_by_turn_id TEXT NOT NULL REFERENCES managed_turns(turn_id),
                conflict_detected INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS coordinator_run_projects_by_coordinate
                ON coordinator_run_projects(project_coordinate, run_id);
            CREATE TABLE IF NOT EXISTS coordinator_run_events (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL REFERENCES coordinator_runs(run_id),
                event_key TEXT NOT NULL,
                kind TEXT NOT NULL,
                occurred_at_ms INTEGER NOT NULL,
                details_json TEXT NOT NULL,
                UNIQUE(run_id, event_key)
            );
            CREATE TABLE IF NOT EXISTS critic_rounds (
                round_id TEXT PRIMARY KEY,
                created_at_ms INTEGER NOT NULL,
                snapshot_sha256 TEXT NOT NULL,
                objective_sha256 TEXT NOT NULL,
                scope_sha256 TEXT NOT NULL,
                settings_json TEXT NOT NULL,
                reviewers_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS critic_rounds_by_created
                ON critic_rounds(created_at_ms DESC, round_id);
            CREATE TABLE IF NOT EXISTS route_throughput_samples (
                turn_id TEXT NOT NULL REFERENCES managed_turns(turn_id),
                attempt_id TEXT NOT NULL,
                request_sequence INTEGER NOT NULL CHECK(request_sequence > 0),
                profile_hash TEXT NOT NULL,
                endpoint_hash TEXT NOT NULL,
                candidate_id TEXT NOT NULL,
                provider_id TEXT NOT NULL,
                model_id TEXT NOT NULL,
                thinking_effort TEXT NOT NULL,
                input_bucket TEXT NOT NULL,
                input_tokens INTEGER NOT NULL CHECK(input_tokens > 0),
                output_tokens INTEGER NOT NULL CHECK(output_tokens > 0),
                elapsed_ms INTEGER NOT NULL CHECK(elapsed_ms > 0),
                effective_output_tokens_per_second_milli INTEGER NOT NULL
                    CHECK(effective_output_tokens_per_second_milli > 0),
                occurred_at_ms INTEGER NOT NULL,
                details_json TEXT NOT NULL,
                PRIMARY KEY(turn_id, attempt_id, request_sequence)
            );
            CREATE INDEX IF NOT EXISTS managed_turn_events_by_turn
                ON managed_turn_events(turn_id, sequence);
            CREATE INDEX IF NOT EXISTS managed_turn_sources_by_event
                ON managed_turn_sources(source_event_id, turn_id);
            CREATE INDEX IF NOT EXISTS coordinator_runs_by_thread
                ON coordinator_runs(channel_id, thread_root_key, updated_at_ms DESC);
            CREATE INDEX IF NOT EXISTS coordinator_run_sources_by_event
                ON coordinator_run_sources(source_event_id, run_id);
            CREATE INDEX IF NOT EXISTS coordinator_run_turns_by_turn
                ON coordinator_run_turns(turn_id, run_id);
            CREATE INDEX IF NOT EXISTS coordinator_run_events_by_run
                ON coordinator_run_events(run_id, sequence);
            CREATE INDEX IF NOT EXISTS route_throughput_by_identity
                ON route_throughput_samples(
                    profile_hash, endpoint_hash, candidate_id, provider_id, model_id,
                    thinking_effort, input_bucket, occurred_at_ms DESC
                );",
        )
        .map_err(|error| format!("initialize Buzz run journal: {error}"))?;
        ensure_source_index(&mut conn)?;
        ensure_coordinator_runs(&mut conn)?;
        goal_runs::ensure_goal_runs(&mut conn)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&journal.db_path, fs::Permissions::from_mode(0o600))
                .map_err(|error| format!("secure Buzz run journal file: {error}"))?;
        }
        Ok(journal)
    }

    /// Resolve the nest, relay, and owner from the managed agent's environment.
    pub fn open_default_scoped() -> Result<Self, String> {
        let relay_url = env::var("BUZZ_RELAY_URL")
            .map_err(|_| "BUZZ_RELAY_URL is required for scoped run history".to_string())?;
        let private_key = env::var("BUZZ_PRIVATE_KEY")
            .map_err(|_| "BUZZ_PRIVATE_KEY is required for scoped run history".to_string())?;
        let keys = nostr::Keys::parse(&private_key)
            .map_err(|error| format!("invalid BUZZ_PRIVATE_KEY for run history: {error}"))?;
        Self::open_scoped(
            nest_dir_from_env_or_default()?,
            &relay_url,
            &keys.public_key().to_hex(),
        )
    }

    pub fn apply(&self, command: JournalCommand) -> Result<(), String> {
        match command {
            JournalCommand::Started(record) => self.record_started(&record),
            JournalCommand::ProjectLinked {
                turn_id,
                project_coordinate,
            } => self.record_project_linked(&turn_id, &project_coordinate),
            JournalCommand::SessionResolved {
                turn_id,
                session_id,
            } => self.record_session_resolved(&turn_id, &session_id),
            JournalCommand::RouteDecision { turn_id, decision } => {
                self.record_route_decision(&turn_id, &decision)
            }
            JournalCommand::RouteThroughputSample { turn_id, sample } => {
                self.record_route_throughput_sample(&turn_id, &sample)
            }
            JournalCommand::PromptCallStarted { turn_id } => {
                self.record_prompt_call_started(&turn_id)
            }
            JournalCommand::SteerSubmitted {
                turn_id,
                source_event_id,
            } => self.record_steer_submitted(&turn_id, &source_event_id),
            JournalCommand::SteerOutcome {
                turn_id,
                source_event_id,
                outcome,
            } => self.record_steer_outcome(&turn_id, &source_event_id, outcome),
            JournalCommand::Returned { turn_id, outcome } => {
                self.record_returned(&turn_id, &outcome)
            }
            JournalCommand::WorkerCrashed { turn_id } => self.record_worker_crashed(&turn_id),
        }
    }

    /// Persist a count of earlier ACP writes that failed in process memory and
    /// were recovered by a later successful journal open/write.
    pub fn record_capture_gap(&self, failed_event_count: u64) -> Result<(), String> {
        if failed_event_count == 0 {
            return Err("capture gap count must be positive".into());
        }
        let failed_event_count = i64::try_from(failed_event_count)
            .map_err(|_| "capture gap count exceeds the storage limit".to_string())?;
        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO journal_capture_gaps(occurred_at_ms, failed_event_count)
             VALUES (?1, ?2)",
            params![now_ms(), failed_event_count],
        )
        .map_err(|error| format!("record local journal capture gap: {error}"))?;
        Ok(())
    }

    /// Return persisted missing-event count for this relay/owner journal.
    pub fn capture_gap_count(&self) -> Result<u64, String> {
        let conn = self.connect()?;
        let count: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(failed_event_count), 0) FROM journal_capture_gaps",
                [],
                |row| row.get(0),
            )
            .map_err(|error| format!("query local journal capture gaps: {error}"))?;
        Ok(count.max(0) as u64)
    }

    /// Persist one bounded critic round without retaining the submitted text.
    pub fn record_critic_round(
        &self,
        round_id: &str,
        snapshot_sha256: &str,
        objective_sha256: &str,
        scope_sha256: &str,
        settings: CriticRoundSettings,
        reviewers: Vec<CriticReviewerRecord>,
    ) -> Result<CriticRoundRecord, String> {
        let record = CriticRoundRecord {
            round_id: Uuid::parse_str(round_id)
                .map_err(|_| "invalid critic round ID".to_string())?
                .to_string(),
            created_at_ms: now_ms(),
            snapshot_sha256: snapshot_sha256.to_owned(),
            objective_sha256: objective_sha256.to_owned(),
            scope_sha256: scope_sha256.to_owned(),
            settings,
            reviewers,
        };
        validate_critic_round_record(&record)?;
        let settings_json =
            serde_json::to_string(&record.settings).map_err(|error| error.to_string())?;
        let reviewers_json =
            serde_json::to_string(&record.reviewers).map_err(|error| error.to_string())?;
        if settings_json.len() + reviewers_json.len() > MAX_CRITIC_ROUND_BYTES {
            return Err("critic round result exceeds the journal limit".into());
        }

        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO critic_rounds(
                round_id, created_at_ms, snapshot_sha256, objective_sha256,
                scope_sha256, settings_json, reviewers_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                record.round_id,
                record.created_at_ms,
                record.snapshot_sha256,
                record.objective_sha256,
                record.scope_sha256,
                settings_json,
                reviewers_json,
            ],
        )
        .map_err(|error| format!("persist local critic round: {error}"))?;
        Ok(record)
    }

    /// Read one critic round from the current relay/owner-scoped local journal.
    pub fn critic_round(&self, round_id: &str) -> Result<Option<CriticRoundRecord>, String> {
        let round_id = Uuid::parse_str(round_id)
            .map_err(|_| "invalid critic round ID".to_string())?
            .to_string();
        let conn = self.connect()?;
        let row = conn
            .query_row(
                "SELECT round_id, created_at_ms, snapshot_sha256, objective_sha256,
                        scope_sha256, settings_json, reviewers_json
                 FROM critic_rounds WHERE round_id=?1",
                [&round_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("read local critic round: {error}"))?;
        let Some((
            round_id,
            created_at_ms,
            snapshot_sha256,
            objective_sha256,
            scope_sha256,
            settings_json,
            reviewers_json,
        )) = row
        else {
            return Ok(None);
        };
        if settings_json.len() + reviewers_json.len() > MAX_CRITIC_ROUND_BYTES {
            return Err("stored critic round exceeds the journal limit".into());
        }
        let record = CriticRoundRecord {
            round_id,
            created_at_ms,
            snapshot_sha256,
            objective_sha256,
            scope_sha256,
            settings: serde_json::from_str(&settings_json)
                .map_err(|error| format!("decode local critic settings: {error}"))?,
            reviewers: serde_json::from_str(&reviewers_json)
                .map_err(|error| format!("decode local critic reviewers: {error}"))?,
        };
        validate_critic_round_record(&record)?;
        Ok(Some(record))
    }

    /// Read recent identity-scoped history without returning reviewer findings.
    pub fn recent_critic_rounds(&self, limit: usize) -> Result<Vec<CriticRoundSummary>, String> {
        if !(1..=50).contains(&limit) {
            return Err("critic history limit must be between 1 and 50".into());
        }
        let limit = i64::try_from(limit).map_err(|_| "critic history limit is too large")?;
        let conn = self.connect()?;
        let mut statement = conn
            .prepare(
                "SELECT round_id FROM critic_rounds
                 ORDER BY created_at_ms DESC, round_id DESC LIMIT ?1",
            )
            .map_err(|error| format!("prepare local critic history: {error}"))?;
        let round_ids = statement
            .query_map([limit], |row| row.get::<_, String>(0))
            .map_err(|error| format!("query local critic history: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("read local critic history IDs: {error}"))?;
        drop(statement);
        drop(conn);

        round_ids
            .into_iter()
            .map(|round_id| {
                self.critic_round(&round_id)?
                    .map(CriticRoundSummary::from)
                    .ok_or_else(|| "critic round disappeared during history read".into())
            })
            .collect()
    }

    /// Link the authoritative project home resolved for an ACP attempt to its
    /// source-triggered coordinator runs. The first project link is immutable;
    /// later disagreement is retained as conflict evidence.
    pub fn record_project_linked(
        &self,
        turn_id: &str,
        project_coordinate: &str,
    ) -> Result<(), String> {
        validate_turn_id(turn_id)?;
        validate_project_coordinate(project_coordinate)?;
        let mut conn = self.connect()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("begin coordinator project link: {error}"))?;
        ensure_turn(&tx, turn_id)?;
        let run_ids = {
            let mut stmt = tx
                .prepare("SELECT run_id FROM coordinator_run_turns WHERE turn_id=?1")
                .map_err(|error| format!("prepare coordinator project links: {error}"))?;
            let rows = stmt
                .query_map([turn_id], |row| row.get::<_, String>(0))
                .map_err(|error| format!("read coordinator project links: {error}"))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("decode coordinator project links: {error}"))?
        };
        let now = now_ms();
        for run_id in run_ids {
            let existing = tx
                .query_row(
                    "SELECT project_coordinate FROM coordinator_run_projects WHERE run_id=?1",
                    [&run_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|error| format!("read coordinator project identity: {error}"))?;
            match existing {
                None => {
                    tx.execute(
                        "INSERT INTO coordinator_run_projects
                         (run_id, project_coordinate, linked_at_ms, linked_by_turn_id)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![run_id, project_coordinate, now, turn_id],
                    )
                    .map_err(|error| format!("store coordinator project identity: {error}"))?;
                    append_coordinator_run_event(
                        &tx,
                        &run_id,
                        "project_linked",
                        "project_linked",
                        now,
                        serde_json::json!({
                            "project_coordinate": project_coordinate,
                            "turn_id": turn_id,
                            "authority": "verified_nip_mp_project_home",
                        }),
                    )?;
                    touch_coordinator_runs_for_turn(&tx, turn_id, now)?;
                }
                Some(existing) if existing == project_coordinate => {}
                Some(existing) => {
                    tx.execute(
                        "UPDATE coordinator_run_projects SET conflict_detected=1 WHERE run_id=?1",
                        [&run_id],
                    )
                    .map_err(|error| format!("flag coordinator project conflict: {error}"))?;
                    append_coordinator_run_event(
                        &tx,
                        &run_id,
                        &format!("project_link_conflict:{turn_id}:{project_coordinate}"),
                        "project_link_conflict",
                        now,
                        serde_json::json!({
                            "original_project_coordinate": existing,
                            "observed_project_coordinate": project_coordinate,
                            "turn_id": turn_id,
                        }),
                    )?;
                    touch_coordinator_runs_for_turn(&tx, turn_id, now)?;
                }
            }
        }
        tx.commit()
            .map_err(|error| format!("commit coordinator project link: {error}"))
    }

    pub fn record_started(&self, record: &StartRecord) -> Result<(), String> {
        validate_start_record(record)?;
        let trigger_ids = normalized_source_ids(&record.batch_trigger_event_ids)?;
        let cancelled_ids = normalized_source_ids(&record.merged_cancelled_event_ids)?;
        let now = now_ms();
        let mut conn = self.connect()?;
        let tx = conn
            .transaction()
            .map_err(|error| format!("begin run-journal transaction: {error}"))?;
        let inserted = tx
            .execute(
                "INSERT OR IGNORE INTO managed_turns
                 (turn_id, channel_id, session_scope, thread_root_event_id,
                  batch_trigger_event_ids, merged_cancelled_event_ids,
                  agent_index, status, task_state,
                  started_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'nonterminal', 'unknown', ?8, ?8)",
                params![
                    record.turn_id,
                    record.channel_id,
                    record.session_scope,
                    record
                        .thread_root_event_id
                        .as_deref()
                        .map(str::to_ascii_lowercase),
                    serde_json::to_string(&trigger_ids).map_err(|e| e.to_string())?,
                    serde_json::to_string(&cancelled_ids).map_err(|e| e.to_string())?,
                    i64::from(record.agent_index),
                    now,
                ],
            )
            .map_err(|error| format!("record managed turn: {error}"))?;
        if inserted == 1 {
            for (source_kind, source_ids) in [
                ("trigger", &trigger_ids),
                ("merged_cancelled", &cancelled_ids),
            ] {
                for source_event_id in source_ids {
                    tx.execute(
                        "INSERT OR IGNORE INTO managed_turn_sources
                         (turn_id, source_event_id, source_kind) VALUES (?1, ?2, ?3)",
                        params![record.turn_id, source_event_id, source_kind],
                    )
                    .map_err(|error| format!("index managed turn source: {error}"))?;
                }
            }
            insert_event(
                &tx,
                &record.turn_id,
                "turn_started",
                now,
                serde_json::json!({
                    "batch_trigger_event_count": trigger_ids.len(),
                    "merged_cancelled_event_count": cancelled_ids.len(),
                    "managed_worker_generation_nonce": record.managed_worker_generation_nonce,
                    "adapter_child_generation_id": record.adapter_child_generation_id,
                    "effective_controls": {
                        "scope": "ACP process and per-turn settings",
                        "configured_worker_pool_slots": record.configured_worker_pool_slots,
                        "idle_timeout_secs": record.idle_timeout_secs,
                        "max_turn_duration_secs": record.max_turn_duration_secs,
                    },
                    "agent_profile_v1": agent_profile_event(&record.agent_profile),
                    "resource_policy_v1": resource_policy_v1(record),
                }),
            )?;
        }
        if let Some(channel_id) = record.channel_id.as_deref() {
            ensure_coordinator_runs_for_turn(
                &tx,
                channel_id,
                &record.session_scope,
                record.thread_root_event_id.as_deref(),
                &record.turn_id,
                &trigger_ids,
                &cancelled_ids,
                now,
            )?;
            goal_runs::link_goal_task_attempts_for_turn(&tx, &record.turn_id, &trigger_ids)?;
        }
        tx.commit()
            .map_err(|error| format!("commit managed turn: {error}"))
    }

    pub fn record_session_resolved(&self, turn_id: &str, session_id: &str) -> Result<(), String> {
        validate_turn_id(turn_id)?;
        if session_id.is_empty()
            || session_id.len() > 1024
            || session_id.chars().any(char::is_control)
        {
            return Err("invalid ACP session identifier".into());
        }
        let mut conn = self.connect()?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let now = now_ms();
        ensure_turn(&tx, turn_id)?;
        tx.execute(
            "UPDATE managed_turns SET acp_session_id=?2, updated_at_ms=?3 WHERE turn_id=?1",
            params![turn_id, session_id, now],
        )
        .map_err(|error| format!("record ACP session: {error}"))?;
        insert_event(
            &tx,
            turn_id,
            "session_resolved",
            now,
            serde_json::json!({"acp_session_id": session_id}),
        )?;
        touch_coordinator_runs_for_turn(&tx, turn_id, now)?;
        tx.commit().map_err(|e| e.to_string())
    }

    /// Record that the ACP prompt call is about to begin. This deliberately
    /// does not claim that bytes reached the adapter or provider.
    pub fn record_prompt_call_started(&self, turn_id: &str) -> Result<(), String> {
        self.record_event_only(turn_id, "prompt_call_started", Value::Null)
    }

    /// Append one sanitized decision for a managed ACP attempt. The session,
    /// attempt, and launch-pinned profile identity must agree with existing
    /// turn evidence; conflicting second decisions are rejected.
    pub fn record_route_decision(
        &self,
        turn_id: &str,
        decision: &RouteDecisionRecord,
    ) -> Result<(), String> {
        validate_turn_id(turn_id)?;
        decision.validate()?;
        let details = serde_json::to_value(decision).map_err(|error| error.to_string())?;
        let details_json = serde_json::to_string(&details).map_err(|error| error.to_string())?;
        let mut conn = self.connect()?;
        let tx = conn.transaction().map_err(|error| error.to_string())?;
        let (session_id, started_details): (Option<String>, Option<String>) = tx
            .query_row(
                "SELECT managed_turns.acp_session_id,
                        (SELECT details_json FROM managed_turn_events
                         WHERE turn_id=managed_turns.turn_id AND kind='turn_started'
                         ORDER BY sequence LIMIT 1)
                 FROM managed_turns WHERE turn_id=?1",
                [turn_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "managed turn does not exist".to_string())?;
        if session_id.as_deref() != Some(decision.session_id.as_str()) {
            return Err("route decision ACP session does not match managed turn".into());
        }
        let started: Value = serde_json::from_str(
            started_details
                .as_deref()
                .ok_or_else(|| "managed turn start evidence is missing".to_string())?,
        )
        .map_err(|_| "managed turn start evidence is invalid".to_string())?;
        let profile = &started["agent_profile_v1"];
        if profile["route_profile_id"].as_str() != decision.profile_id.as_deref()
            || profile["route_profile_version"].as_u64() != decision.profile_version.map(u64::from)
            || profile["route_profile_hash"].as_str() != decision.profile_hash.as_deref()
        {
            return Err("route decision profile does not match launch snapshot".into());
        }

        let existing: Option<String> = tx
            .query_row(
                "SELECT details_json FROM managed_turn_events
                 WHERE turn_id=?1 AND kind='route_decision_v1' ORDER BY sequence LIMIT 1",
                [turn_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        if let Some(existing) = existing {
            if existing == details_json {
                return tx.commit().map_err(|error| error.to_string());
            }
            return Err("conflicting route decision already recorded for managed turn".into());
        }
        let now = now_ms();
        tx.execute(
            "UPDATE managed_turns SET updated_at_ms=?2 WHERE turn_id=?1",
            params![turn_id, now],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "INSERT INTO managed_turn_events(turn_id, kind, occurred_at_ms, details_json)
             VALUES (?1, 'route_decision_v1', ?2, ?3)",
            params![turn_id, now, details_json],
        )
        .map_err(|error| format!("append route decision event: {error}"))?;
        touch_coordinator_runs_for_turn(&tx, turn_id, now)?;
        tx.commit().map_err(|error| error.to_string())
    }

    /// Persist one local measurement only when it joins to this managed turn's
    /// selected route. Samples are bounded per turn, per route identity, and
    /// across the device-local journal.
    pub fn record_route_throughput_sample(
        &self,
        turn_id: &str,
        sample: &RouteThroughputSample,
    ) -> Result<(), String> {
        validate_turn_id(turn_id)?;
        sample.validate()?;
        let details_json = serde_json::to_string(sample).map_err(|error| error.to_string())?;
        let mut conn = self.connect()?;
        let tx = conn.transaction().map_err(|error| error.to_string())?;
        let (session_id, started_details): (Option<String>, Option<String>) = tx
            .query_row(
                "SELECT managed_turns.acp_session_id,
                        (SELECT details_json FROM managed_turn_events
                         WHERE turn_id=managed_turns.turn_id AND kind='turn_started'
                         ORDER BY sequence LIMIT 1)
                 FROM managed_turns WHERE turn_id=?1",
                [turn_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "managed turn does not exist".to_string())?;
        if session_id.as_deref() != Some(sample.session_id.as_str()) {
            return Err("route throughput session does not match managed turn".into());
        }
        let started: Value = serde_json::from_str(
            started_details
                .as_deref()
                .ok_or_else(|| "managed turn start evidence is missing".to_string())?,
        )
        .map_err(|_| "managed turn start evidence is invalid".to_string())?;
        let profile = &started["agent_profile_v1"];
        if profile["route_profile_id"].as_str() != Some(sample.profile_id.as_str())
            || profile["route_profile_version"].as_u64() != Some(u64::from(sample.profile_version))
            || profile["route_profile_hash"].as_str() != Some(sample.profile_hash.as_str())
        {
            return Err("route throughput profile does not match launch snapshot".into());
        }
        let decision_json: Option<String> = tx
            .query_row(
                "SELECT details_json FROM managed_turn_events
                 WHERE turn_id=?1 AND kind='route_decision_v1' ORDER BY sequence DESC LIMIT 1",
                [turn_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        let decision: RouteDecisionRecord = serde_json::from_str(
            decision_json
                .as_deref()
                .ok_or_else(|| "route decision evidence is missing".to_string())?,
        )
        .map_err(|_| "route decision evidence is invalid".to_string())?;
        if decision.validate().is_err()
            || decision.outcome != RouteDecisionOutcome::Selected
            || decision.session_id != sample.session_id
            || decision.attempt_id != sample.attempt_id
            || decision.profile_id.as_deref() != Some(sample.profile_id.as_str())
            || decision.profile_version != Some(sample.profile_version)
            || decision.profile_hash.as_deref() != Some(sample.profile_hash.as_str())
            || decision.candidate_id.as_deref() != Some(sample.candidate_id.as_str())
            || decision.provider_id.as_deref() != Some(sample.provider_id.as_str())
            || decision.model_id.as_deref() != Some(sample.model_id.as_str())
        {
            return Err("route throughput sample does not match selected route decision".into());
        }

        let existing: Option<String> = tx
            .query_row(
                "SELECT details_json FROM route_throughput_samples
                 WHERE turn_id=?1 AND attempt_id=?2 AND request_sequence=?3",
                params![turn_id, sample.attempt_id, sample.request_sequence as i64],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        if let Some(existing) = existing {
            if existing == details_json {
                return tx.commit().map_err(|error| error.to_string());
            }
            return Err("conflicting route throughput sample already recorded".into());
        }
        let turn_samples: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM route_throughput_samples WHERE turn_id=?1",
                [turn_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if turn_samples >= MAX_ROUTE_SAMPLES_PER_TURN as i64 {
            return Err("managed turn route throughput sample limit reached".into());
        }

        let now = now_ms();
        let input_bucket = route_input_bucket(sample.input_tokens);
        tx.execute(
            "INSERT INTO route_throughput_samples(
                 turn_id, attempt_id, request_sequence, profile_hash, candidate_id,
                 endpoint_hash, provider_id, model_id, thinking_effort, input_bucket, input_tokens,
                 output_tokens, elapsed_ms, effective_output_tokens_per_second_milli,
                 occurred_at_ms, details_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                turn_id,
                sample.attempt_id,
                sample.request_sequence as i64,
                sample.profile_hash,
                sample.candidate_id,
                sample.endpoint_hash,
                sample.provider_id,
                sample.model_id,
                sample.thinking_effort,
                input_bucket,
                sample.input_tokens as i64,
                sample.output_tokens as i64,
                sample.elapsed_ms as i64,
                sample.effective_output_tokens_per_second_milli as i64,
                now,
                details_json,
            ],
        )
        .map_err(|error| format!("record route throughput sample: {error}"))?;

        tx.execute(
            "DELETE FROM route_throughput_samples
             WHERE occurred_at_ms < ?1",
            [now.saturating_sub(ROUTE_SAMPLE_RETENTION_MS)],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "DELETE FROM route_throughput_samples WHERE rowid IN (
                 SELECT rowid FROM (
                     SELECT rowid, ROW_NUMBER() OVER (
                         PARTITION BY profile_hash, endpoint_hash, candidate_id, provider_id, model_id,
                                      thinking_effort, input_bucket
                         ORDER BY occurred_at_ms DESC, rowid DESC
                     ) AS sample_rank
                     FROM route_throughput_samples
                 ) WHERE sample_rank > ?1
             )",
            [MAX_ROUTE_SAMPLES_PER_IDENTITY as i64],
        )
        .map_err(|error| format!("bound route throughput samples per identity: {error}"))?;
        tx.execute(
            "DELETE FROM route_throughput_samples WHERE rowid IN (
                 SELECT rowid FROM route_throughput_samples
                 ORDER BY occurred_at_ms DESC, rowid DESC LIMIT -1 OFFSET ?1
             )",
            [MAX_ROUTE_SAMPLES_GLOBAL as i64],
        )
        .map_err(|error| format!("bound local route throughput sample history: {error}"))?;
        tx.execute(
            "UPDATE managed_turns SET updated_at_ms=?2 WHERE turn_id=?1",
            params![turn_id, now],
        )
        .map_err(|error| error.to_string())?;
        touch_coordinator_runs_for_turn(&tx, turn_id, now)?;
        tx.commit().map_err(|error| error.to_string())
    }

    /// Return only fresh samples for the exact profile/candidate/model/effort
    /// and input-size bucket. A conservative lower quartile remains unknown
    /// until five observations are available.
    pub fn route_throughput_summary(
        &self,
        query: &RouteThroughputQuery,
    ) -> Result<RouteThroughputSummary, String> {
        validate_route_throughput_query(query)?;
        let conn = self.connect()?;
        let fresh_after = now_ms().saturating_sub(ROUTE_SAMPLE_FRESH_MS);
        let mut stmt = conn
            .prepare(
                "SELECT effective_output_tokens_per_second_milli, occurred_at_ms
                 FROM route_throughput_samples
                 WHERE profile_hash=?1 AND endpoint_hash=?2 AND candidate_id=?3
                   AND provider_id=?4 AND model_id=?5 AND thinking_effort=?6
                   AND input_bucket=?7 AND occurred_at_ms>=?8
                 ORDER BY occurred_at_ms DESC, rowid DESC",
            )
            .map_err(|error| error.to_string())?;
        let rows = stmt
            .query_map(
                params![
                    query.profile_hash,
                    query.endpoint_hash,
                    query.candidate_id,
                    query.provider_id,
                    query.model_id,
                    query.thinking_effort,
                    route_input_bucket(query.input_tokens),
                    fresh_after,
                ],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .map_err(|error| error.to_string())?;
        let samples = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        let freshest_sample_at_ms = samples.first().map(|sample| sample.1);
        let rates = samples
            .iter()
            .map(|sample| u64::try_from(sample.0).map_err(|error| error.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(RouteThroughputSummary {
            fresh_sample_count: samples.len(),
            effective_output_tokens_per_second_milli: lower_quartile_route_rate(&rates),
            freshest_sample_at_ms,
        })
    }

    /// List fresh measured-rate groups for one saved route-profile version.
    /// Groups remain separate by resolved profile, provider endpoint, effort,
    /// candidate, and input-size bucket. Results are bounded to the newest 128
    /// groups so a profile with many agent-specific connections stays cheap.
    pub fn route_throughput_summaries(
        &self,
        profile_id: &str,
        profile_version: u32,
    ) -> Result<Vec<RouteThroughputGroupSummary>, String> {
        if !valid_route_profile_id(profile_id) || profile_version == 0 {
            return Err("invalid route throughput profile lookup".into());
        }
        let conn = self.connect()?;
        let fresh_after = now_ms().saturating_sub(ROUTE_SAMPLE_FRESH_MS);
        let mut stmt = conn
            .prepare(
                "SELECT effective_output_tokens_per_second_milli, occurred_at_ms, details_json
                 FROM route_throughput_samples
                 WHERE occurred_at_ms>=?1
                 ORDER BY occurred_at_ms DESC, rowid DESC
                 LIMIT ?2",
            )
            .map_err(|error| error.to_string())?;
        let rows = stmt
            .query_map(
                params![fresh_after, MAX_ROUTE_SAMPLES_GLOBAL as i64],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(|error| error.to_string())?;
        let rows = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        let mut groups: BTreeMap<RouteThroughputGroupKey, RouteThroughputGroupSamples> =
            BTreeMap::new();
        for (stored_rate, occurred_at_ms, details_json) in rows {
            let sample: RouteThroughputSample = serde_json::from_str(&details_json)
                .map_err(|error| format!("invalid saved route throughput sample: {error}"))?;
            sample.validate()?;
            if sample.profile_id != profile_id || sample.profile_version != profile_version {
                continue;
            }
            let stored_rate = u64::try_from(stored_rate)
                .map_err(|error| format!("invalid saved route throughput rate: {error}"))?;
            if stored_rate != sample.effective_output_tokens_per_second_milli {
                return Err("saved route throughput rate does not match its sample".into());
            }
            groups
                .entry((
                    sample.profile_hash,
                    sample.endpoint_hash,
                    sample.candidate_id,
                    sample.provider_id,
                    sample.model_id,
                    sample.thinking_effort,
                    route_input_bucket(sample.input_tokens).to_string(),
                ))
                .or_default()
                .push((stored_rate, occurred_at_ms));
        }
        let mut summaries = Vec::with_capacity(groups.len());
        for (
            (
                profile_hash,
                endpoint_hash,
                candidate_id,
                provider_id,
                model_id,
                thinking_effort,
                input_bucket,
            ),
            samples,
        ) in groups
        {
            let rates = samples.iter().map(|sample| sample.0).collect::<Vec<_>>();
            let freshest_sample_at_ms = samples
                .iter()
                .map(|sample| sample.1)
                .max()
                .ok_or_else(|| "empty route throughput group".to_string())?;
            summaries.push(RouteThroughputGroupSummary {
                profile_hash,
                endpoint_hash,
                candidate_id,
                provider_id,
                model_id,
                thinking_effort,
                input_bucket,
                fresh_sample_count: samples.len(),
                effective_output_tokens_per_second_milli: lower_quartile_route_rate(&rates),
                freshest_sample_at_ms,
            });
        }
        summaries.sort_by(|left, right| {
            right
                .freshest_sample_at_ms
                .cmp(&left.freshest_sample_at_ms)
                .then_with(|| left.candidate_id.cmp(&right.candidate_id))
        });
        summaries.truncate(MAX_ROUTE_THROUGHPUT_SUMMARIES);
        Ok(summaries)
    }

    pub fn record_steer_submitted(
        &self,
        turn_id: &str,
        source_event_id: &str,
    ) -> Result<(), String> {
        validate_event_id(source_event_id)?;
        self.record_linked_event_once(
            turn_id,
            "steer_submitted",
            serde_json::json!({"source_event_id": source_event_id.to_ascii_lowercase()}),
        )
    }

    pub fn record_steer_outcome(
        &self,
        turn_id: &str,
        source_event_id: &str,
        outcome: SteerOutcome,
    ) -> Result<(), String> {
        validate_event_id(source_event_id)?;
        self.record_linked_event_once(
            turn_id,
            "steer_outcome",
            serde_json::json!({
                "source_event_id": source_event_id.to_ascii_lowercase(),
                "outcome": outcome.as_str(),
            }),
        )
    }

    pub fn record_returned(&self, turn_id: &str, outcome: &str) -> Result<(), String> {
        const OUTCOMES: &[&str] = &[
            "acp_turn_returned",
            "error",
            "project_context_indeterminate",
            "agent_exited",
            "timeout",
            "cancelled",
            "cancel_drain_timeout",
        ];
        if !OUTCOMES.contains(&outcome) {
            return Err("invalid managed turn outcome".into());
        }
        let status = if outcome == "acp_turn_returned" {
            "returned"
        } else {
            outcome
        };
        self.record_status_event(
            turn_id,
            status,
            "turn_returned",
            serde_json::json!({"outcome": outcome}),
        )
    }

    pub fn record_worker_crashed(&self, turn_id: &str) -> Result<(), String> {
        self.record_status_event(turn_id, "worker_crashed", "worker_crashed", Value::Null)
    }

    pub fn list_recent(&self, limit: usize) -> Result<Vec<TurnSummary>, String> {
        let conn = self.connect()?;
        let mut stmt = conn
            .prepare(
                "SELECT turn_id, channel_id, session_scope, thread_root_event_id,
                        batch_trigger_event_ids, merged_cancelled_event_ids,
                        agent_index, acp_session_id, status,
                        task_state, started_at_ms, updated_at_ms
                 FROM managed_turns ORDER BY started_at_ms DESC, turn_id DESC LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([limit.clamp(1, MAX_LIST_LIMIT) as i64], summary_from_row)
            .map_err(|e| e.to_string())?;
        rows.map(|row| row.map_err(|e| e.to_string())).collect()
    }

    /// List attempts linked to one authorized message thread. Source-event
    /// filtering happens in SQLite before LIMIT so unrelated recent turns
    /// cannot crowd older matching attempts out of the result.
    pub fn list_recent_for_thread(
        &self,
        channel_id: &str,
        thread_root_event_id: &str,
        readable_source_event_ids: &[String],
        limit: usize,
    ) -> Result<Vec<TurnSummary>, String> {
        let channel_id = Uuid::parse_str(channel_id)
            .map_err(|_| "invalid channel ID for managed run history".to_string())?
            .to_string();
        validate_event_id(thread_root_event_id)?;
        let source_ids = normalized_source_ids(readable_source_event_ids)?;

        let source_filter = if source_ids.is_empty() {
            String::new()
        } else {
            let placeholders = (3..3 + source_ids.len())
                .map(|index| format!("?{index}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                " OR EXISTS (
                    SELECT 1 FROM managed_turn_sources AS source
                    WHERE source.turn_id = turn.turn_id
                      AND source.source_event_id IN ({placeholders})
                )"
            )
        };
        let limit_index = 3 + source_ids.len();
        let sql = format!(
            "SELECT turn.turn_id, turn.channel_id, turn.session_scope,
                    turn.thread_root_event_id, turn.batch_trigger_event_ids,
                    turn.merged_cancelled_event_ids, turn.agent_index,
                    turn.acp_session_id, turn.status, turn.task_state,
                    turn.started_at_ms, turn.updated_at_ms
             FROM managed_turns AS turn
             WHERE turn.channel_id=?1
               AND (turn.thread_root_event_id=?2{source_filter})
             ORDER BY turn.started_at_ms DESC, turn.turn_id DESC
             LIMIT ?{limit_index}"
        );
        let mut values = vec![
            SqlValue::Text(channel_id),
            SqlValue::Text(thread_root_event_id.to_ascii_lowercase()),
        ];
        values.extend(source_ids.into_iter().map(SqlValue::Text));
        values.push(SqlValue::Integer(limit.clamp(1, MAX_LIST_LIMIT) as i64));

        let conn = self.connect()?;
        let mut stmt = conn.prepare(&sql).map_err(|error| error.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(values), summary_from_row)
            .map_err(|error| error.to_string())?;
        rows.map(|row| row.map_err(|error| error.to_string()))
            .collect()
    }

    /// Build the bounded local ACP-attempt projection shared by the CLI and
    /// Desktop thread briefs. Callers supply only IDs from an authorized read.
    pub fn thread_attempt_history(
        &self,
        channel_id: &str,
        thread_root_event_id: &str,
        readable_source_event_ids: &[String],
    ) -> Result<ThreadAttemptHistory, String> {
        let mut turns = self.list_recent_for_thread(
            channel_id,
            thread_root_event_id,
            readable_source_event_ids,
            THREAD_BRIEF_MAX_TURNS + 1,
        )?;
        let has_more_turns = turns.len() > THREAD_BRIEF_MAX_TURNS;
        turns.truncate(THREAD_BRIEF_MAX_TURNS);

        let managed_turns = turns
            .into_iter()
            .map(|turn| {
                let (recent_events, event_history_may_be_truncated) =
                    self.recent_events(&turn.turn_id, THREAD_BRIEF_MAX_EVENTS_PER_TURN)?;
                Ok(ThreadAttemptBrief {
                    turn,
                    recent_events,
                    event_history_may_be_truncated,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(ThreadAttemptHistory {
            managed_turns,
            has_more_turns,
            capture_gap_count: self.capture_gap_count()?,
        })
    }

    /// Return stable coordinator-run identities for one relay-authorized thread.
    /// The caller must supply only source IDs from its successful relay read.
    pub fn thread_coordinator_runs(
        &self,
        channel_id: &str,
        thread_root_event_id: &str,
        readable_source_event_ids: &[String],
    ) -> Result<CoordinatorRunHistory, String> {
        let channel_id = Uuid::parse_str(channel_id)
            .map_err(|_| "invalid channel ID for coordinator run history".to_string())?
            .to_string();
        validate_event_id(thread_root_event_id)?;
        let source_ids = normalized_source_ids(readable_source_event_ids)?;
        let source_filter = if source_ids.is_empty() {
            String::new()
        } else {
            let placeholders = (3..3 + source_ids.len())
                .map(|index| format!("?{index}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                " OR (run.session_scope='conversation' AND EXISTS (
                    SELECT 1 FROM coordinator_run_sources AS source
                    WHERE source.run_id = run.run_id
                      AND source.source_event_id IN ({placeholders})
                ))"
            )
        };
        let limit_index = 3 + source_ids.len();
        let sql = format!(
            "SELECT run.run_id, run.channel_id, run.session_scope,
                    run.thread_root_key, run.original_intent_event_id,
                    run.created_at_ms, run.updated_at_ms
             FROM coordinator_runs AS run
             WHERE run.channel_id=?1
               AND (run.thread_root_key=?2{source_filter})
             ORDER BY run.updated_at_ms DESC, run.run_id DESC
             LIMIT ?{limit_index}"
        );
        let mut values = vec![
            SqlValue::Text(channel_id),
            SqlValue::Text(thread_root_event_id.to_ascii_lowercase()),
        ];
        values.extend(source_ids.into_iter().map(SqlValue::Text));
        values.push(SqlValue::Integer((THREAD_BRIEF_MAX_RUNS + 1) as i64));

        let conn = self.connect()?;
        let mut stmt = conn.prepare(&sql).map_err(|error| error.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(values), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })
            .map_err(|error| error.to_string())?;
        let mut rows = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        let has_more_runs = rows.len() > THREAD_BRIEF_MAX_RUNS;
        rows.truncate(THREAD_BRIEF_MAX_RUNS);
        drop(stmt);

        let runs = rows
            .into_iter()
            .map(|row| coordinator_run_summary(&conn, row))
            .collect::<Result<Vec<_>, String>>()?;
        Ok(CoordinatorRunHistory {
            runs,
            has_more_runs,
            capture_gap_count: self.capture_gap_count()?,
        })
    }

    /// Fetch one stable run only after the caller has read its exact thread or
    /// one of its source events from the relay.
    pub fn coordinator_run_in_thread(
        &self,
        run_id: &str,
        channel_id: &str,
        thread_root_event_id: &str,
        readable_source_event_ids: &[String],
    ) -> Result<Option<CoordinatorRunSummary>, String> {
        let run_id = Uuid::parse_str(run_id)
            .map_err(|_| "invalid coordinator run ID".to_string())?
            .to_string();
        let channel_id = Uuid::parse_str(channel_id)
            .map_err(|_| "invalid channel ID for coordinator run lookup".to_string())?
            .to_string();
        validate_event_id(thread_root_event_id)?;
        let readable_source_ids = normalized_source_ids(readable_source_event_ids)?;
        let conn = self.connect()?;
        let row = conn
            .query_row(
                "SELECT run_id, channel_id, session_scope, thread_root_key,
                        original_intent_event_id, created_at_ms, updated_at_ms
                 FROM coordinator_runs WHERE run_id=?1",
                [&run_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| error.to_string())?;
        let Some(row) = row else {
            return Ok(None);
        };
        if row.1 != channel_id {
            return Ok(None);
        }
        let thread_authorized =
            row.2 == "thread" && row.3 == thread_root_event_id.to_ascii_lowercase();
        let source_authorized = if row.2 == "conversation" && !readable_source_ids.is_empty() {
            let placeholders = (2..2 + readable_source_ids.len())
                .map(|index| format!("?{index}"))
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "SELECT EXISTS(
                    SELECT 1 FROM coordinator_run_sources
                    WHERE run_id=?1 AND source_event_id IN ({placeholders})
                )"
            );
            let mut values = vec![SqlValue::Text(run_id.clone())];
            values.extend(readable_source_ids.into_iter().map(SqlValue::Text));
            conn.query_row(&sql, rusqlite::params_from_iter(values), |row| row.get(0))
                .map_err(|error| error.to_string())?
        } else {
            false
        };
        if !thread_authorized && !source_authorized {
            return Ok(None);
        }
        coordinator_run_summary(&conn, row).map(Some)
    }

    /// Return bounded local candidates for a currently validated project home.
    ///
    /// The caller must revalidate the project coordinate against relay state
    /// and prove readability of each returned source before exposing run data.
    pub fn project_coordinator_runs(
        &self,
        project_coordinate: &str,
        home_channel_id: &str,
        limit: usize,
        cursor: Option<&CoordinatorRunCursor>,
    ) -> Result<ProjectCoordinatorRunHistory, String> {
        validate_project_coordinate(project_coordinate)?;
        let home_channel_id = Uuid::parse_str(home_channel_id)
            .map_err(|_| "invalid project home channel ID".to_string())?
            .to_string();
        if let Some(cursor) = cursor {
            if cursor.updated_at_ms < 0 {
                return Err("invalid project-run cursor timestamp".into());
            }
            Uuid::parse_str(&cursor.run_id)
                .map_err(|_| "invalid project-run cursor ID".to_string())?;
        }
        let limit = limit.clamp(1, MAX_LIST_LIMIT);
        let (sql, values) = if let Some(cursor) = cursor {
            (
                "SELECT run.run_id, run.channel_id, run.session_scope,
                        run.thread_root_key, run.original_intent_event_id,
                        run.created_at_ms, run.updated_at_ms
                 FROM coordinator_runs AS run
                 JOIN coordinator_run_projects AS project ON project.run_id=run.run_id
                 WHERE project.project_coordinate=?1 AND run.channel_id=?2
                   AND (run.updated_at_ms < ?3
                     OR (run.updated_at_ms = ?3 AND run.run_id < ?4))
                 ORDER BY run.updated_at_ms DESC, run.run_id DESC
                 LIMIT ?5",
                vec![
                    SqlValue::Text(project_coordinate.to_string()),
                    SqlValue::Text(home_channel_id),
                    SqlValue::Integer(cursor.updated_at_ms),
                    SqlValue::Text(cursor.run_id.clone()),
                    SqlValue::Integer((limit + 1) as i64),
                ],
            )
        } else {
            (
                "SELECT run.run_id, run.channel_id, run.session_scope,
                        run.thread_root_key, run.original_intent_event_id,
                        run.created_at_ms, run.updated_at_ms
                 FROM coordinator_runs AS run
                 JOIN coordinator_run_projects AS project ON project.run_id=run.run_id
                 WHERE project.project_coordinate=?1 AND run.channel_id=?2
                 ORDER BY run.updated_at_ms DESC, run.run_id DESC
                 LIMIT ?3",
                vec![
                    SqlValue::Text(project_coordinate.to_string()),
                    SqlValue::Text(home_channel_id),
                    SqlValue::Integer((limit + 1) as i64),
                ],
            )
        };
        let conn = self.connect()?;
        let mut stmt = conn
            .prepare(sql)
            .map_err(|error| format!("prepare project coordinator runs: {error}"))?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(values), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })
            .map_err(|error| format!("read project coordinator runs: {error}"))?;
        let mut rows = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("decode project coordinator runs: {error}"))?;
        let has_more_candidates = rows.len() > limit;
        rows.truncate(limit);
        drop(stmt);
        let next_cursor = if has_more_candidates {
            rows.last().map(|row| CoordinatorRunCursor {
                updated_at_ms: row.6,
                run_id: row.0.clone(),
            })
        } else {
            None
        };
        let runs = rows
            .into_iter()
            .map(|row| coordinator_run_summary(&conn, row))
            .collect::<Result<Vec<_>, String>>()?;
        Ok(ProjectCoordinatorRunHistory {
            runs,
            has_more_candidates,
            next_cursor,
        })
    }

    pub fn get(&self, turn_id: &str) -> Result<Option<TurnSummary>, String> {
        validate_turn_id(turn_id)?;
        let conn = self.connect()?;
        conn.query_row(
            "SELECT turn_id, channel_id, session_scope, thread_root_event_id,
                    batch_trigger_event_ids, merged_cancelled_event_ids,
                    agent_index, acp_session_id, status,
                    task_state, started_at_ms, updated_at_ms
             FROM managed_turns WHERE turn_id=?1",
            [turn_id],
            summary_from_row,
        )
        .optional()
        .map_err(|e| e.to_string())
    }

    pub fn events(&self, turn_id: &str, limit: usize) -> Result<Vec<TurnEvent>, String> {
        validate_turn_id(turn_id)?;
        let conn = self.connect()?;
        let mut stmt = conn
            .prepare(
                "SELECT sequence, turn_id, kind, occurred_at_ms, details_json
                 FROM managed_turn_events WHERE turn_id=?1 ORDER BY sequence LIMIT ?2",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(
                params![turn_id, limit.clamp(1, MAX_EVENT_LIMIT) as i64],
                |row| {
                    let details: String = row.get(4)?;
                    Ok(TurnEvent {
                        sequence: row.get(0)?,
                        turn_id: row.get(1)?,
                        kind: row.get(2)?,
                        occurred_at_ms: row.get(3)?,
                        details: serde_json::from_str(&details).unwrap_or(Value::Null),
                    })
                },
            )
            .map_err(|e| e.to_string())?;
        rows.map(|row| row.map_err(|e| e.to_string())).collect()
    }

    /// Return the newest bounded event window in chronological order, and
    /// whether older events were omitted from this turn's history.
    pub fn recent_events(
        &self,
        turn_id: &str,
        limit: usize,
    ) -> Result<(Vec<TurnEvent>, bool), String> {
        validate_turn_id(turn_id)?;
        let cap = limit.clamp(1, MAX_EVENT_LIMIT);
        let conn = self.connect()?;
        let total: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM managed_turn_events WHERE turn_id=?1",
                [turn_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT sequence, turn_id, kind, occurred_at_ms, details_json
                 FROM managed_turn_events WHERE turn_id=?1
                 ORDER BY sequence DESC LIMIT ?2",
            )
            .map_err(|error| error.to_string())?;
        let rows = stmt
            .query_map(params![turn_id, cap as i64], |row| {
                let details: String = row.get(4)?;
                Ok(TurnEvent {
                    sequence: row.get(0)?,
                    turn_id: row.get(1)?,
                    kind: row.get(2)?,
                    occurred_at_ms: row.get(3)?,
                    details: serde_json::from_str(&details).unwrap_or(Value::Null),
                })
            })
            .map_err(|error| error.to_string())?;
        let mut events = rows
            .map(|row| row.map_err(|error| error.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        events.reverse();
        let has_more = total > events.len() as i64;
        Ok((events, has_more))
    }

    fn record_status_event(
        &self,
        turn_id: &str,
        status: &str,
        kind: &str,
        details: Value,
    ) -> Result<(), String> {
        validate_turn_id(turn_id)?;
        let conn = self.connect()?;
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        let now = now_ms();
        ensure_turn(&tx, turn_id)?;
        tx.execute(
            "UPDATE managed_turns SET status=?2, updated_at_ms=?3 WHERE turn_id=?1",
            params![turn_id, status, now],
        )
        .map_err(|error| format!("update managed turn: {error}"))?;
        insert_event(&tx, turn_id, kind, now, details)?;
        touch_coordinator_runs_for_turn(&tx, turn_id, now)?;
        tx.commit().map_err(|e| e.to_string())
    }

    fn record_event_only(&self, turn_id: &str, kind: &str, details: Value) -> Result<(), String> {
        validate_turn_id(turn_id)?;
        let conn = self.connect()?;
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        let now = now_ms();
        ensure_turn(&tx, turn_id)?;
        tx.execute(
            "UPDATE managed_turns SET updated_at_ms=?2 WHERE turn_id=?1",
            params![turn_id, now],
        )
        .map_err(|e| e.to_string())?;
        insert_event(&tx, turn_id, kind, now, details)?;
        touch_coordinator_runs_for_turn(&tx, turn_id, now)?;
        tx.commit().map_err(|e| e.to_string())
    }

    fn record_linked_event_once(
        &self,
        turn_id: &str,
        kind: &str,
        details: Value,
    ) -> Result<(), String> {
        validate_turn_id(turn_id)?;
        let details_json = serde_json::to_string(&details).map_err(|error| error.to_string())?;
        let mut conn = self.connect()?;
        let tx = conn.transaction().map_err(|error| error.to_string())?;
        ensure_turn(&tx, turn_id)?;
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM managed_turn_events
                    WHERE turn_id=?1 AND kind=?2 AND details_json=?3
                )",
                params![turn_id, kind, details_json],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if !exists {
            let now = now_ms();
            tx.execute(
                "UPDATE managed_turns SET updated_at_ms=?2 WHERE turn_id=?1",
                params![turn_id, now],
            )
            .map_err(|error| error.to_string())?;
            tx.execute(
                "INSERT INTO managed_turn_events(turn_id, kind, occurred_at_ms, details_json)
                 VALUES (?1, ?2, ?3, ?4)",
                params![turn_id, kind, now, details_json],
            )
            .map_err(|error| format!("append managed turn event: {error}"))?;
            touch_coordinator_runs_for_turn(&tx, turn_id, now)?;
            if kind == "steer_submitted" {
                if let Some(source_event_id) =
                    details.get("source_event_id").and_then(Value::as_str)
                {
                    tx.execute(
                        "INSERT OR IGNORE INTO managed_turn_sources
                         (turn_id, source_event_id, source_kind) VALUES (?1, ?2, 'steer')",
                        params![turn_id, source_event_id],
                    )
                    .map_err(|error| format!("index managed steer source: {error}"))?;
                    link_steer_source_to_runs(&tx, turn_id, source_event_id, now)?;
                }
            }
        }
        tx.commit().map_err(|error| error.to_string())
    }

    fn connect(&self) -> Result<Connection, String> {
        if self
            .db_path
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err("run journal database is a symlink".into());
        }
        let mut create = OpenOptions::new();
        create.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            create.mode(0o600);
        }
        match create.open(&self.db_path) {
            Ok(file) => drop(file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(format!("create Buzz run journal file: {error}")),
        }
        let metadata = self
            .db_path
            .symlink_metadata()
            .map_err(|error| format!("inspect Buzz run journal file: {error}"))?;
        if !metadata.file_type().is_file() {
            return Err("run journal path is not a regular file".into());
        }
        let conn = Connection::open(&self.db_path)
            .map_err(|error| format!("open Buzz run journal: {error}"))?;
        conn.pragma_update(None, "busy_timeout", 5000)
            .map_err(|error| format!("configure Buzz run journal: {error}"))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|error| format!("configure Buzz run journal: {error}"))?;
        Ok(conn)
    }
}

fn scoped_db_file(relay_url: &str, owner_pubkey: &str) -> Result<String, String> {
    scoped_db_file_for("agent-run-journal", relay_url, owner_pubkey)
}

fn scoped_db_file_for(prefix: &str, relay_url: &str, owner_pubkey: &str) -> Result<String, String> {
    let owner_pubkey = owner_pubkey.trim().to_ascii_lowercase();
    if owner_pubkey.len() != 64 || !owner_pubkey.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("invalid owner public key for run history".into());
    }
    let mut url =
        Url::parse(relay_url.trim()).map_err(|error| format!("invalid relay URL: {error}"))?;
    let scheme = match url.scheme() {
        "http" | "ws" => "ws",
        "https" | "wss" => "wss",
        _ => return Err("run history relay URL must use HTTP(S) or WS(S)".into()),
    };
    url.set_scheme(scheme)
        .map_err(|_| "invalid relay URL scheme".to_string())?;
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(
            "relay URL credentials and fragments are not allowed in run history scope".into(),
        );
    }
    let path = url.path().trim_end_matches('/').to_string();
    url.set_path(if path.is_empty() { "/" } else { &path });
    let mut hasher = Sha256::new();
    hasher.update(owner_pubkey.as_bytes());
    hasher.update(b"\0");
    hasher.update(url.as_str().as_bytes());
    Ok(format!(
        "{prefix}-{}.sqlite3",
        hex::encode(hasher.finalize())
    ))
}

fn ensure_source_index(conn: &mut Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS managed_run_journal_migrations (
            version INTEGER PRIMARY KEY
        );",
    )
    .map_err(|error| format!("initialize run-journal migrations: {error}"))?;
    let applied: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM managed_run_journal_migrations WHERE version=1)",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("check run-journal migration: {error}"))?;
    if applied {
        return Ok(());
    }

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| format!("begin run-journal migration: {error}"))?;
    let legacy_sources = {
        let mut stmt = tx
            .prepare(
                "SELECT turn_id, batch_trigger_event_ids, merged_cancelled_event_ids
                 FROM managed_turns",
            )
            .map_err(|error| format!("prepare run-journal migration: {error}"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|error| format!("read run-journal migration rows: {error}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("decode run-journal migration rows: {error}"))?
    };
    for (turn_id, trigger_json, merged_json) in legacy_sources {
        for (source_kind, source_json) in
            [("trigger", trigger_json), ("merged_cancelled", merged_json)]
        {
            let decoded: Vec<String> = serde_json::from_str(&source_json)
                .map_err(|error| format!("decode run-journal source history: {error}"))?;
            for source_event_id in normalized_source_ids(&decoded)? {
                tx.execute(
                    "INSERT OR IGNORE INTO managed_turn_sources
                     (turn_id, source_event_id, source_kind) VALUES (?1, ?2, ?3)",
                    params![turn_id, source_event_id, source_kind],
                )
                .map_err(|error| format!("backfill managed turn source: {error}"))?;
            }
        }
    }
    tx.execute(
        "INSERT OR IGNORE INTO managed_run_journal_migrations(version) VALUES (1)",
        [],
    )
    .map_err(|error| format!("record run-journal migration: {error}"))?;
    tx.commit()
        .map_err(|error| format!("commit run-journal migration: {error}"))
}

/// Backfill stable coordinator-run identities from existing trigger-linked
/// attempts. Run IDs are local to this relay/owner journal and never replace
/// channel, thread, workflow, session, or ACP turn identities.
fn ensure_coordinator_runs(conn: &mut Connection) -> Result<(), String> {
    let applied: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM managed_run_journal_migrations WHERE version=2)",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("check coordinator-run migration: {error}"))?;
    if applied {
        return Ok(());
    }

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| format!("begin coordinator-run migration: {error}"))?;
    let legacy_turns = {
        let mut stmt = tx
            .prepare(
                "SELECT turn_id, channel_id, session_scope, thread_root_event_id,
                        batch_trigger_event_ids, merged_cancelled_event_ids, started_at_ms
                 FROM managed_turns
                 WHERE channel_id IS NOT NULL
                 ORDER BY started_at_ms, turn_id",
            )
            .map_err(|error| format!("prepare coordinator-run backfill: {error}"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })
            .map_err(|error| format!("read coordinator-run backfill: {error}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("decode coordinator-run backfill: {error}"))?
    };
    for (turn_id, channel_id, scope, root, trigger_json, cancelled_json, started_at_ms) in
        legacy_turns
    {
        let triggers: Vec<String> = serde_json::from_str(&trigger_json)
            .map_err(|error| format!("decode coordinator-run trigger sources: {error}"))?;
        let cancelled: Vec<String> = serde_json::from_str(&cancelled_json)
            .map_err(|error| format!("decode coordinator-run merged sources: {error}"))?;
        ensure_coordinator_runs_for_turn(
            &tx,
            &channel_id,
            &scope,
            root.as_deref(),
            &turn_id,
            &normalized_source_ids(&triggers)?,
            &normalized_source_ids(&cancelled)?,
            started_at_ms,
        )?;
    }
    tx.execute(
        "INSERT OR IGNORE INTO managed_run_journal_migrations(version) VALUES (2)",
        [],
    )
    .map_err(|error| format!("record coordinator-run migration: {error}"))?;
    tx.commit()
        .map_err(|error| format!("commit coordinator-run migration: {error}"))
}

#[allow(clippy::too_many_arguments)] // fields come from the persisted managed-turn row
fn ensure_coordinator_runs_for_turn(
    tx: &Transaction<'_>,
    channel_id: &str,
    session_scope: &str,
    thread_root_event_id: Option<&str>,
    turn_id: &str,
    trigger_ids: &[String],
    cancelled_ids: &[String],
    at: i64,
) -> Result<(), String> {
    if session_scope == "heartbeat" {
        return Ok(());
    }
    for (source_kind, ids) in [
        ("trigger", trigger_ids),
        ("merged_cancelled", cancelled_ids),
    ] {
        for source_event_id in ids {
            // Conversation-scoped ACP prompts have no thread root. Their
            // source event is the precise local conversation anchor; use it
            // as the run's root key without changing the ACP session scope.
            let root_key = thread_root_event_id
                .unwrap_or(source_event_id)
                .to_ascii_lowercase();
            ensure_coordinator_run_for_source(
                tx,
                channel_id,
                session_scope,
                &root_key,
                source_event_id,
                source_kind,
                turn_id,
                at,
            )?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)] // fields map directly to one coordinator-run source row
fn ensure_coordinator_run_for_source(
    tx: &Transaction<'_>,
    channel_id: &str,
    session_scope: &str,
    root_key: &str,
    source_event_id: &str,
    source_kind: &str,
    turn_id: &str,
    at: i64,
) -> Result<(), String> {
    let proposed_id = Uuid::new_v4().to_string();
    let inserted = tx
        .execute(
            "INSERT OR IGNORE INTO coordinator_runs
             (run_id, channel_id, session_scope, thread_root_key,
              original_intent_event_id, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![
                proposed_id,
                channel_id,
                session_scope,
                root_key,
                source_event_id,
                at
            ],
        )
        .map_err(|error| format!("create coordinator run: {error}"))?;
    let run_id: String = tx
        .query_row(
            "SELECT run_id FROM coordinator_runs
             WHERE channel_id=?1 AND session_scope=?2 AND thread_root_key=?3
               AND original_intent_event_id=?4",
            params![channel_id, session_scope, root_key, source_event_id],
            |row| row.get(0),
        )
        .map_err(|error| format!("read coordinator run identity: {error}"))?;
    if inserted == 1 {
        append_coordinator_run_event(
            tx,
            &run_id,
            "created",
            "run_created",
            at,
            serde_json::json!({
                "original_intent_event_id": source_event_id,
                "source_kind": source_kind,
            }),
        )?;
    }
    tx.execute(
        "INSERT OR IGNORE INTO coordinator_run_sources
         (run_id, source_event_id, source_kind) VALUES (?1, ?2, ?3)",
        params![run_id, source_event_id, source_kind],
    )
    .map_err(|error| format!("index coordinator-run source: {error}"))?;
    let linked = tx
        .execute(
            "INSERT OR IGNORE INTO coordinator_run_turns(run_id, turn_id, linked_at_ms)
             VALUES (?1, ?2, ?3)",
            params![run_id, turn_id, at],
        )
        .map_err(|error| format!("link coordinator run to attempt: {error}"))?;
    if linked == 1 {
        append_coordinator_run_event(
            tx,
            &run_id,
            &format!("attempt:{turn_id}"),
            "attempt_linked",
            at,
            serde_json::json!({"turn_id": turn_id}),
        )?;
    }
    tx.execute(
        "UPDATE coordinator_runs SET updated_at_ms=MAX(updated_at_ms, ?2) WHERE run_id=?1",
        params![run_id, at],
    )
    .map_err(|error| format!("update coordinator-run timestamp: {error}"))?;
    Ok(())
}

fn link_steer_source_to_runs(
    tx: &Transaction<'_>,
    turn_id: &str,
    source_event_id: &str,
    at: i64,
) -> Result<(), String> {
    let run_ids = {
        let mut stmt = tx
            .prepare("SELECT run_id FROM coordinator_run_turns WHERE turn_id=?1")
            .map_err(|error| format!("prepare coordinator steer link: {error}"))?;
        let rows = stmt
            .query_map([turn_id], |row| row.get::<_, String>(0))
            .map_err(|error| format!("read coordinator steer links: {error}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("decode coordinator steer links: {error}"))?
    };
    for run_id in run_ids {
        tx.execute(
            "INSERT OR IGNORE INTO coordinator_run_sources
             (run_id, source_event_id, source_kind) VALUES (?1, ?2, 'steer')",
            params![run_id, source_event_id],
        )
        .map_err(|error| format!("index coordinator steer source: {error}"))?;
        append_coordinator_run_event(
            tx,
            &run_id,
            &format!("steer:{turn_id}:{source_event_id}"),
            "steer_source_linked",
            at,
            serde_json::json!({"turn_id": turn_id, "source_event_id": source_event_id}),
        )?;
        tx.execute(
            "UPDATE coordinator_runs SET updated_at_ms=MAX(updated_at_ms, ?2) WHERE run_id=?1",
            params![run_id, at],
        )
        .map_err(|error| format!("update coordinator steer timestamp: {error}"))?;
    }
    Ok(())
}

fn touch_coordinator_runs_for_turn(
    tx: &Transaction<'_>,
    turn_id: &str,
    at: i64,
) -> Result<(), String> {
    tx.execute(
        "UPDATE coordinator_runs SET updated_at_ms=MAX(updated_at_ms, ?2)
         WHERE run_id IN (
             SELECT run_id FROM coordinator_run_turns WHERE turn_id=?1
         )",
        params![turn_id, at],
    )
    .map(|_| ())
    .map_err(|error| format!("update coordinator-run activity: {error}"))
}

fn append_coordinator_run_event(
    tx: &Transaction<'_>,
    run_id: &str,
    event_key: &str,
    kind: &str,
    at: i64,
    details: Value,
) -> Result<(), String> {
    let details = serde_json::to_string(&details).map_err(|error| error.to_string())?;
    tx.execute(
        "INSERT OR IGNORE INTO coordinator_run_events
         (run_id, event_key, kind, occurred_at_ms, details_json)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![run_id, event_key, kind, at, details],
    )
    .map(|_| ())
    .map_err(|error| format!("append coordinator-run event: {error}"))
}

fn coordinator_run_attempts(
    conn: &Connection,
    run_id: &str,
) -> Result<(Vec<TurnSummary>, bool), String> {
    let mut stmt = conn
        .prepare(
            "SELECT turn.turn_id, turn.channel_id, turn.session_scope,
                    turn.thread_root_event_id, turn.batch_trigger_event_ids,
                    turn.merged_cancelled_event_ids, turn.agent_index,
                    turn.acp_session_id, turn.status, turn.task_state,
                    turn.started_at_ms, turn.updated_at_ms
             FROM managed_turns AS turn
             JOIN coordinator_run_turns AS link ON link.turn_id=turn.turn_id
             WHERE link.run_id=?1
             ORDER BY turn.started_at_ms DESC, turn.turn_id DESC
             LIMIT ?2",
        )
        .map_err(|error| error.to_string())?;
    let rows = stmt
        .query_map(
            params![run_id, (RUN_MAX_ATTEMPTS + 1) as i64],
            summary_from_row,
        )
        .map_err(|error| error.to_string())?;
    let mut turns = rows
        .map(|row| row.map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let has_more = turns.len() > RUN_MAX_ATTEMPTS;
    turns.truncate(RUN_MAX_ATTEMPTS);
    Ok((turns, has_more))
}

fn coordinator_run_events(
    conn: &Connection,
    run_id: &str,
    limit: usize,
) -> Result<(Vec<CoordinatorRunEvent>, bool), String> {
    let total: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM coordinator_run_events WHERE run_id=?1",
            [run_id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    let cap = limit.clamp(1, THREAD_BRIEF_MAX_RUN_EVENTS);
    let mut stmt = conn
        .prepare(
            "SELECT sequence, run_id, event_key, kind, occurred_at_ms, details_json
             FROM coordinator_run_events WHERE run_id=?1 ORDER BY sequence DESC LIMIT ?2",
        )
        .map_err(|error| error.to_string())?;
    let rows = stmt
        .query_map(params![run_id, cap as i64], |row| {
            let details: String = row.get(5)?;
            Ok(CoordinatorRunEvent {
                sequence: row.get(0)?,
                run_id: row.get(1)?,
                event_key: row.get(2)?,
                kind: row.get(3)?,
                occurred_at_ms: row.get(4)?,
                details: serde_json::from_str(&details).unwrap_or(Value::Null),
            })
        })
        .map_err(|error| error.to_string())?;
    let mut events = rows
        .map(|row| row.map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    events.reverse();
    let has_more = total > events.len() as i64;
    Ok((events, has_more))
}

fn coordinator_run_summary(
    conn: &Connection,
    row: (String, String, String, String, String, i64, i64),
) -> Result<CoordinatorRunSummary, String> {
    let (run_id, channel_id, session_scope, root_key, source_id, created, updated) = row;
    let (attempt_turns, attempt_history_may_be_truncated) =
        coordinator_run_attempts(conn, &run_id)?;
    let (recent_events, event_history_may_be_truncated) =
        coordinator_run_events(conn, &run_id, THREAD_BRIEF_MAX_RUN_EVENTS)?;
    let project_link = conn
        .query_row(
            "SELECT project_coordinate, conflict_detected
             FROM coordinator_run_projects WHERE run_id=?1",
            [&run_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)),
        )
        .optional()
        .map_err(|error| format!("read coordinator-run project link: {error}"))?;
    let is_thread_scope = session_scope == "thread";
    Ok(CoordinatorRunSummary {
        run_id,
        channel_id,
        session_scope,
        thread_root_event_id: is_thread_scope.then_some(root_key),
        original_intent_event_id: source_id,
        project_coordinate: project_link
            .as_ref()
            .map(|(coordinate, _)| coordinate.clone()),
        project_link_conflict: project_link.is_some_and(|(_, conflict)| conflict),
        attempt_turns,
        attempt_history_may_be_truncated,
        recent_events,
        event_history_may_be_truncated,
        created_at_ms: created,
        updated_at_ms: updated,
        task_state: "unknown".into(),
    })
}

fn validate_project_coordinate(coordinate: &str) -> Result<(), String> {
    let mut parts = coordinate.splitn(3, ':');
    let kind = parts.next();
    let owner = parts.next().unwrap_or_default();
    let slug = parts.next().unwrap_or_default();
    if kind != Some("30621")
        || owner.len() != 64
        || !owner.bytes().all(|byte| byte.is_ascii_hexdigit())
        || slug.is_empty()
        || slug.len() > 1024
        || slug.chars().any(char::is_control)
        || coordinate.len() > 1100
    {
        return Err("invalid NIP-MP project coordinate".into());
    }
    Ok(())
}

fn validate_start_record(record: &StartRecord) -> Result<(), String> {
    validate_turn_id(&record.turn_id)?;
    if let Some(nonce) = &record.managed_worker_generation_nonce {
        let parsed = Uuid::parse_str(nonce)
            .map_err(|_| "invalid managed worker generation nonce".to_string())?;
        if parsed.simple().to_string() != *nonce {
            return Err("managed worker generation nonce is not canonical".into());
        }
    }
    if let Some(generation_id) = &record.adapter_child_generation_id {
        let parsed = Uuid::parse_str(generation_id)
            .map_err(|_| "invalid ACP child generation ID".to_string())?;
        if parsed.to_string() != *generation_id {
            return Err("ACP child generation ID is not canonical".into());
        }
    }
    if record.configured_worker_pool_slots == 0
        || record.idle_timeout_secs == 0
        || record.max_turn_duration_secs == 0
        || record.idle_timeout_secs >= record.max_turn_duration_secs
    {
        return Err("invalid enforced ACP resource controls".into());
    }
    if !matches!(
        record.session_scope.as_str(),
        "conversation" | "thread" | "heartbeat"
    ) {
        return Err("invalid managed turn session scope".into());
    }
    if record.session_scope == "heartbeat" {
        if record.channel_id.is_some() || record.thread_root_event_id.is_some() {
            return Err("heartbeat turn cannot have channel/thread identity".into());
        }
    } else if record
        .channel_id
        .as_deref()
        .and_then(|id| Uuid::parse_str(id).ok())
        .is_none()
    {
        return Err("invalid managed turn channel ID".into());
    }
    if (record.session_scope == "thread") != record.thread_root_event_id.is_some() {
        return Err("thread scope and root event ID must be provided together".into());
    }
    if let Some(root) = &record.thread_root_event_id {
        validate_event_id(root)?;
    }
    normalized_source_ids(&record.batch_trigger_event_ids)?;
    normalized_source_ids(&record.merged_cancelled_event_ids)?;
    validate_profile_identifier(&record.agent_profile.harness_id, "harness")?;
    if let Some(provider) = &record.agent_profile.provider_id {
        validate_profile_identifier(provider, "provider")?;
    }
    if let Some(model) = &record.agent_profile.model_id {
        validate_profile_identifier(model, "model")?;
    }
    match (
        record.agent_profile.execution_profile_id.as_deref(),
        record.agent_profile.execution_profile_version,
    ) {
        (Some(id), Some(version)) if version > 0 => {
            validate_profile_identifier(id, "execution profile")?;
        }
        (Some(_), _) => return Err("execution profile version is missing or invalid".into()),
        (None, Some(_)) => return Err("execution profile ID is missing".into()),
        (None, None) => {}
    }
    match (
        record.agent_profile.prompt_profile_id.as_deref(),
        record.agent_profile.prompt_profile_version,
        record.agent_profile.prompt_profile_hash.as_deref(),
    ) {
        (Some(id), Some(version), Some(hash)) if version > 0 => {
            validate_profile_identifier(id, "prompt profile")?;
            if hash.len() != 64
                || !hash
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            {
                return Err("invalid prompt profile fingerprint".into());
            }
        }
        (None, None, None) => {}
        _ => return Err("prompt profile identity is incomplete or invalid".into()),
    }
    match (
        record.agent_profile.route_profile_id.as_deref(),
        record.agent_profile.route_profile_version,
        record.agent_profile.route_profile_hash.as_deref(),
    ) {
        (Some(id), Some(version), Some(hash)) if version > 0 => {
            validate_profile_identifier(id, "route profile")?;
            if hash.len() != 64
                || !hash
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            {
                return Err("invalid route profile fingerprint".into());
            }
        }
        (None, None, None) => {}
        _ => return Err("route profile identity is incomplete or invalid".into()),
    }
    if let Some(fingerprint) = &record.agent_profile.agent_prompt_sha256 {
        if fingerprint.len() != 64
            || !fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err("invalid agent prompt fingerprint".into());
        }
    }
    Ok(())
}

fn validate_profile_identifier(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(format!("invalid agent profile {label} identifier"));
    }
    Ok(())
}

fn normalized_source_ids(ids: &[String]) -> Result<Vec<String>, String> {
    if ids.len() > MAX_SOURCE_EVENTS {
        return Err("too many managed turn source events".into());
    }
    let mut unique = BTreeSet::new();
    for id in ids {
        validate_event_id(id)?;
        unique.insert(id.to_ascii_lowercase());
    }
    Ok(unique.into_iter().collect())
}

fn validate_turn_id(turn_id: &str) -> Result<(), String> {
    Uuid::parse_str(turn_id)
        .map(|_| ())
        .map_err(|_| "invalid managed turn ID".into())
}

fn validate_event_id(event_id: &str) -> Result<(), String> {
    if event_id.len() != 64 || !event_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("invalid managed turn source event ID".into());
    }
    Ok(())
}

fn ensure_turn(tx: &Transaction<'_>, turn_id: &str) -> Result<(), String> {
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM managed_turns WHERE turn_id=?1)",
            [turn_id],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    if exists {
        Ok(())
    } else {
        Err("managed turn does not exist".into())
    }
}

fn insert_event(
    tx: &Transaction<'_>,
    turn_id: &str,
    kind: &str,
    at: i64,
    details: Value,
) -> Result<(), String> {
    let details = serde_json::to_string(&details).map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO managed_turn_events(turn_id, kind, occurred_at_ms, details_json)
         VALUES (?1, ?2, ?3, ?4)",
        params![turn_id, kind, at, details],
    )
    .map(|_| ())
    .map_err(|error| format!("append managed turn event: {error}"))
}

fn summary_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TurnSummary> {
    let batch_trigger_event_ids: String = row.get(4)?;
    let merged_cancelled_event_ids: String = row.get(5)?;
    let agent_index: i64 = row.get(6)?;
    Ok(TurnSummary {
        turn_id: row.get(0)?,
        channel_id: row.get(1)?,
        session_scope: row.get(2)?,
        thread_root_event_id: row.get(3)?,
        batch_trigger_event_ids: serde_json::from_str(&batch_trigger_event_ids).unwrap_or_default(),
        merged_cancelled_event_ids: serde_json::from_str(&merged_cancelled_event_ids)
            .unwrap_or_default(),
        agent_index: agent_index.max(0) as u32,
        acp_session_id: row.get(7)?,
        status: row.get(8)?,
        liveness: "unknown".into(),
        task_state: row.get(9)?,
        started_at_ms: row.get(10)?,
        updated_at_ms: row.get(11)?,
    })
}

fn resource_policy_v1(record: &StartRecord) -> Value {
    serde_json::json!({
        "schema_version": 1,
        "scope": "run",
        "applies_to": "current_attempt",
        "metrics": [
            {
                "id": "agent.parallelism",
                "label": "ACP worker pool slots",
                "value": record.configured_worker_pool_slots,
                "unit": "slots",
                "scope": "agent",
                "state": "configured",
                "source": "buzz_acp_runtime_config",
                "enforcement": "hard_runtime",
            },
            {
                "id": "agent.idle_timeout",
                "label": "Idle timeout",
                "value": record.idle_timeout_secs,
                "unit": "seconds",
                "scope": "agent",
                "state": "configured",
                "source": "buzz_acp_runtime_config",
                "enforcement": "turn_boundary",
            },
            {
                "id": "agent.max_turn_duration",
                "label": "Maximum turn duration",
                "value": record.max_turn_duration_secs,
                "unit": "seconds",
                "scope": "agent",
                "state": "configured",
                "source": "buzz_acp_runtime_config",
                "enforcement": "turn_boundary",
            },
            unavailable_metric(
                "project.aggregate_concurrency",
                "Aggregate project concurrency",
                "agents",
                "project",
            ),
            unavailable_metric(
                "run.aggregate_concurrency",
                "Aggregate run concurrency",
                "agents",
                "run",
            ),
            unavailable_metric(
                "run.token_usage",
                "Token usage",
                "provider-specific",
                "run",
            ),
            unavailable_metric("run.spend", "Spend", "provider-specific", "run"),
            unavailable_metric("device.memory", "RAM and VRAM usage", "bytes", "device"),
            unavailable_metric(
                "model.throughput",
                "Model throughput",
                "tokens per second",
                "model",
            ),
        ],
    })
}

fn unavailable_metric(id: &str, label: &str, unit: &str, scope: &str) -> Value {
    serde_json::json!({
        "id": id,
        "label": label,
        "value": null,
        "unit": unit,
        "scope": scope,
        "state": "unknown",
        "source": "unavailable",
        "enforcement": "unknown",
    })
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start_record() -> StartRecord {
        StartRecord {
            turn_id: "123e4567-e89b-12d3-a456-426614174000".into(),
            managed_worker_generation_nonce: Some("123e4567e89b12d3a456426614174000".into()),
            adapter_child_generation_id: Some("123e4567-e89b-12d3-a456-426614174002".into()),
            channel_id: Some("123e4567-e89b-12d3-a456-426614174001".into()),
            session_scope: "thread".into(),
            thread_root_event_id: Some("a".repeat(64)),
            batch_trigger_event_ids: vec!["b".repeat(64), "b".repeat(64)],
            merged_cancelled_event_ids: vec!["c".repeat(64)],
            agent_index: 2,
            configured_worker_pool_slots: 3,
            idle_timeout_secs: 1_500,
            max_turn_duration_secs: 7_200,
            agent_profile: AgentProfileSnapshot {
                harness_id: "goose".into(),
                provider_id: Some("anthropic".into()),
                model_id: Some("claude-opus".into()),
                agent_prompt_sha256: agent_prompt_fingerprint(Some("private prompt")),
                execution_profile_id: Some("critic_security".into()),
                execution_profile_version: Some(1),
                prompt_profile_id: Some("openai-coding".into()),
                prompt_profile_version: Some(2),
                prompt_profile_hash: Some("d".repeat(64)),
                route_profile_id: None,
                route_profile_version: None,
                route_profile_hash: None,
            },
        }
    }

    #[test]
    fn agent_prompt_fingerprint_hashes_exact_nonempty_configured_text() {
        assert_eq!(
            agent_prompt_fingerprint(Some("private prompt")).as_deref(),
            Some("6fe06b970bb77bb96bee521acbebf7e932c2bbc684494ad299a7e1851347fc8e")
        );
        assert_eq!(agent_prompt_fingerprint(Some(" \n\t ")), None);
        assert_eq!(agent_prompt_fingerprint(None), None);
    }

    #[test]
    fn old_agent_profile_snapshots_default_to_no_execution_profile() {
        let profile: AgentProfileSnapshot = serde_json::from_value(serde_json::json!({
            "harness_id": "goose",
            "provider_id": null,
            "model_id": null,
            "agent_prompt_sha256": null
        }))
        .unwrap();

        assert!(profile.execution_profile_id.is_none());
        assert!(profile.execution_profile_version.is_none());
        assert!(profile.prompt_profile_id.is_none());
        assert!(profile.prompt_profile_version.is_none());
        assert!(profile.prompt_profile_hash.is_none());
        assert!(profile.route_profile_id.is_none());
        assert!(profile.route_profile_version.is_none());
        assert!(profile.route_profile_hash.is_none());
    }

    #[test]
    fn old_start_records_default_to_no_process_generation_mapping() {
        let mut legacy = serde_json::to_value(start_record()).unwrap();
        legacy
            .as_object_mut()
            .unwrap()
            .remove("managed_worker_generation_nonce");
        legacy
            .as_object_mut()
            .unwrap()
            .remove("adapter_child_generation_id");

        let decoded: StartRecord = serde_json::from_value(legacy).unwrap();
        assert!(decoded.managed_worker_generation_nonce.is_none());
        assert!(decoded.adapter_child_generation_id.is_none());
    }

    #[test]
    fn start_records_require_canonical_generation_identities() {
        let mut record = start_record();
        record.managed_worker_generation_nonce = Some("not-a-uuid".into());
        assert!(validate_start_record(&record)
            .unwrap_err()
            .contains("managed worker generation nonce"));

        let mut record = start_record();
        record.adapter_child_generation_id = Some("123e4567e89b12d3a456426614174002".into());
        assert!(validate_start_record(&record)
            .unwrap_err()
            .contains("ACP child generation ID is not canonical"));
    }

    #[test]
    fn execution_profile_provenance_requires_id_and_positive_version() {
        let mut record = start_record();
        record.agent_profile.execution_profile_id = Some("critic".into());
        record.agent_profile.execution_profile_version = None;
        assert!(validate_start_record(&record)
            .unwrap_err()
            .contains("execution profile version"));

        record.agent_profile.execution_profile_id = None;
        record.agent_profile.execution_profile_version = Some(1);
        assert!(validate_start_record(&record)
            .unwrap_err()
            .contains("execution profile ID"));

        record.agent_profile.execution_profile_id = Some("critic".into());
        record.agent_profile.execution_profile_version = Some(0);
        assert!(validate_start_record(&record)
            .unwrap_err()
            .contains("execution profile version"));
    }

    #[test]
    fn route_profile_provenance_requires_complete_identity_and_hash() {
        let mut record = start_record();
        record.agent_profile.route_profile_id = Some("local-first".into());
        record.agent_profile.route_profile_hash = Some("f".repeat(64));
        assert!(validate_start_record(&record)
            .unwrap_err()
            .contains("route profile identity is incomplete"));

        record.agent_profile.route_profile_version = Some(1);
        record.agent_profile.route_profile_hash = Some("F".repeat(64));
        assert!(validate_start_record(&record)
            .unwrap_err()
            .contains("invalid route profile fingerprint"));

        record.agent_profile.route_profile_hash = Some("f".repeat(64));
        assert!(validate_start_record(&record).is_ok());
    }

    #[test]
    fn route_decision_accepts_registered_manual_override_rejection_reasons() {
        let mut decision = RouteDecisionRecord {
            session_id: "acp-session-1".into(),
            attempt_id: "run_attempt-1".into(),
            profile_id: None,
            profile_version: None,
            profile_hash: None,
            outcome: RouteDecisionOutcome::Refused,
            candidate_id: None,
            provider_id: None,
            model_id: None,
            reason_code: None,
            context_fit: None,
        };
        for reason in [
            "manual_override_model_not_listed",
            "manual_override_model_ambiguous",
        ] {
            decision.reason_code = Some(reason.into());
            decision
                .validate()
                .unwrap_or_else(|error| panic!("{reason} should be accepted: {error}"));
        }
    }

    #[test]
    fn route_decision_is_joined_redacted_and_single_per_managed_turn() {
        let nest = tempfile::tempdir().unwrap();
        let mut start = start_record();
        start.agent_profile.route_profile_id = Some("local-first".into());
        start.agent_profile.route_profile_version = Some(3);
        start.agent_profile.route_profile_hash = Some("a".repeat(64));
        let journal = open_journal(nest.path());
        journal.record_started(&start).unwrap();
        journal
            .record_session_resolved(&start.turn_id, "acp-session-1")
            .unwrap();
        let decision = RouteDecisionRecord {
            session_id: "acp-session-1".into(),
            attempt_id: "run_attempt-1".into(),
            profile_id: Some("local-first".into()),
            profile_version: Some(3),
            profile_hash: Some("a".repeat(64)),
            outcome: RouteDecisionOutcome::Selected,
            candidate_id: Some("local-fast".into()),
            provider_id: Some("openai".into()),
            model_id: Some("gpt-test".into()),
            reason_code: None,
            context_fit: Some(RouteContextFitRecord {
                estimate_method: "utf8_bytes_plus_framing_and_output_reserve_v1".into(),
                capacity_source: RouteContextCapacitySource::OperatorDeclared,
                input_tokens_upper_bound: 512,
                capacity_tokens: 2048,
            }),
        };
        journal
            .apply(JournalCommand::RouteDecision {
                turn_id: start.turn_id.clone(),
                decision: decision.clone(),
            })
            .unwrap();
        journal
            .record_route_decision(&start.turn_id, &decision)
            .expect("replayed identical notification should be idempotent");
        let events = journal.events(&start.turn_id, 100).unwrap();
        let decisions: Vec<_> = events
            .iter()
            .filter(|event| event.kind == "route_decision_v1")
            .collect();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].details["attemptId"], "run_attempt-1");
        assert_eq!(decisions[0].details["candidateId"], "local-fast");
        assert_eq!(decisions[0].details["profileHash"], "a".repeat(64));
        assert_eq!(
            decisions[0].details["contextFit"]["inputTokensUpperBound"],
            512
        );
        assert_eq!(
            decisions[0].details["contextFit"]["capacitySource"],
            "operator_declared"
        );
        let serialized = decisions[0].details.to_string();
        assert!(!serialized.contains("private prompt"));
        assert!(!serialized.contains("api_key"));

        let mut samples = Vec::new();
        for (sequence, rate) in (10_u64..=50).step_by(10).enumerate() {
            let sample = RouteThroughputSample {
                session_id: "acp-session-1".into(),
                attempt_id: "run_attempt-1".into(),
                profile_id: "local-first".into(),
                profile_version: 3,
                profile_hash: "a".repeat(64),
                endpoint_hash: "b".repeat(64),
                candidate_id: "local-fast".into(),
                provider_id: "openai".into(),
                model_id: "gpt-test".into(),
                thinking_effort: "default".into(),
                request_sequence: sequence as u64 + 1,
                input_tokens: 1_500,
                output_tokens: rate,
                elapsed_ms: 1_000,
                effective_output_tokens_per_second_milli: rate * 1_000,
            };
            journal
                .record_route_throughput_sample(&start.turn_id, &sample)
                .unwrap();
            samples.push(sample);
            let summary = journal
                .route_throughput_summary(&RouteThroughputQuery {
                    profile_hash: "a".repeat(64),
                    endpoint_hash: "b".repeat(64),
                    candidate_id: "local-fast".into(),
                    provider_id: "openai".into(),
                    model_id: "gpt-test".into(),
                    thinking_effort: "default".into(),
                    input_tokens: 1_500,
                })
                .unwrap();
            assert_eq!(summary.fresh_sample_count, sequence + 1);
            assert_eq!(
                summary.effective_output_tokens_per_second_milli,
                if sequence < 4 { None } else { Some(20_000) },
                "require five fresh samples and use the lower quartile"
            );
        }

        let mut separate_group = samples[0].clone();
        separate_group.endpoint_hash = "c".repeat(64);
        separate_group.request_sequence = 6;
        separate_group.input_tokens = 5_000;
        separate_group.output_tokens = 8;
        separate_group.effective_output_tokens_per_second_milli = 8_000;
        journal
            .record_route_throughput_sample(&start.turn_id, &separate_group)
            .unwrap();
        samples.push(separate_group);
        let grouped = journal
            .route_throughput_summaries("local-first", 3)
            .unwrap();
        assert_eq!(grouped.len(), 2);
        let tiny_group = grouped
            .iter()
            .find(|group| group.endpoint_hash == "b".repeat(64))
            .unwrap();
        assert_eq!(tiny_group.input_bucket, "tiny");
        assert_eq!(tiny_group.fresh_sample_count, 5);
        assert_eq!(
            tiny_group.effective_output_tokens_per_second_milli,
            Some(20_000)
        );
        let small_group = grouped
            .iter()
            .find(|group| group.endpoint_hash == "c".repeat(64))
            .unwrap();
        assert_eq!(small_group.input_bucket, "small");
        assert_eq!(small_group.fresh_sample_count, 1);
        assert_eq!(small_group.effective_output_tokens_per_second_milli, None);
        assert!(journal
            .route_throughput_summaries("local-first", 2)
            .unwrap()
            .is_empty());
        assert!(journal
            .route_throughput_summaries("other-profile", 3)
            .unwrap()
            .is_empty());
        journal
            .connect()
            .unwrap()
            .execute(
                "UPDATE route_throughput_samples SET occurred_at_ms=0 WHERE endpoint_hash=?1",
                ["c".repeat(64)],
            )
            .unwrap();
        assert_eq!(
            journal
                .route_throughput_summaries("local-first", 3)
                .unwrap()
                .len(),
            1,
            "older-than-seven-days groups stay out of the UI"
        );

        let replay = samples.last().unwrap();
        journal
            .record_route_throughput_sample(&start.turn_id, replay)
            .expect("identical sample replay is idempotent");
        let mut conflicting_sample = replay.clone();
        conflicting_sample.output_tokens = 60;
        conflicting_sample.effective_output_tokens_per_second_milli = 60_000;
        assert!(journal
            .record_route_throughput_sample(&start.turn_id, &conflicting_sample)
            .unwrap_err()
            .contains("conflicting route throughput sample"));

        let serialized_samples = serde_json::to_string(&samples).unwrap();
        assert!(!serialized_samples.contains("private prompt"));
        assert!(!serialized_samples.contains("completion"));
        assert!(!serialized_samples.contains("api_key"));

        let mut conflicting = decision.clone();
        conflicting.attempt_id = "run_attempt-2".into();
        assert!(journal
            .record_route_decision(&start.turn_id, &conflicting)
            .unwrap_err()
            .contains("conflicting route decision"));
        let mut wrong_profile = decision.clone();
        wrong_profile.profile_hash = Some("b".repeat(64));
        assert!(journal
            .record_route_decision(&start.turn_id, &wrong_profile)
            .unwrap_err()
            .contains("does not match launch snapshot"));
        let mut unsafe_reason = decision;
        unsafe_reason.outcome = RouteDecisionOutcome::Refused;
        unsafe_reason.reason_code = Some("raw provider error and credentials".into());
        assert!(unsafe_reason.validate().is_err());
    }

    fn open_journal(nest: &Path) -> RunJournal {
        RunJournal::open_scoped(nest, "ws://localhost:3000", &"d".repeat(64)).unwrap()
    }

    fn critic_reviewer(role: &str) -> CriticReviewerRecord {
        CriticReviewerRecord {
            role: role.into(),
            status: "completed".into(),
            output: Some("Review found one concrete issue.".into()),
            output_truncated: false,
            stop_reason: Some("end_turn".into()),
            candidate_id: Some("local-review".into()),
            provider_id: Some("openai".into()),
            model_id: Some("local-model".into()),
            route_profile: None,
            estimated_cost_limit_microusd: None,
            elapsed_ms: Some(900),
            error_code: None,
        }
    }

    fn critic_settings() -> CriticRoundSettings {
        CriticRoundSettings {
            max_output_tokens: 1_024,
            time_limit_seconds: 60,
            thinking_effort_requested: Some("medium".into()),
            estimated_round_cost_budget_microusd: None,
            route_profile: None,
            coordinator_guide: None,
        }
    }

    #[test]
    fn critic_route_profile_identity_is_optional_and_persisted_when_present() {
        let legacy: CriticRoundSettings = serde_json::from_value(serde_json::json!({
            "maxOutputTokens": 1024,
            "timeLimitSeconds": 60,
            "thinkingEffortRequested": "medium"
        }))
        .unwrap();
        assert_eq!(legacy.route_profile, None);
        assert_eq!(legacy.estimated_round_cost_budget_microusd, None);
        assert_eq!(legacy.coordinator_guide, None);

        let nest = tempfile::tempdir().unwrap();
        let journal = open_journal(nest.path());
        let mut settings = critic_settings();
        settings.route_profile = Some(CriticRouteProfileRef {
            id: "local-critic".into(),
            version: 2,
            hash: "d".repeat(64),
        });
        settings.coordinator_guide = Some(CriticGuideRef {
            path: "AGENT_GUIDES/CRITICS.md".into(),
            sha256: "e".repeat(64),
        });
        let record = journal
            .record_critic_round(
                "123e4567-e89b-12d3-a456-426614174011",
                &"a".repeat(64),
                &"b".repeat(64),
                &"c".repeat(64),
                settings.clone(),
                vec![critic_reviewer("security")],
            )
            .unwrap();
        assert_eq!(record.settings, settings);

        for profile in [
            CriticRouteProfileRef {
                id: "Bad ID".into(),
                version: 1,
                hash: "d".repeat(64),
            },
            CriticRouteProfileRef {
                id: "local-critic".into(),
                version: 0,
                hash: "d".repeat(64),
            },
            CriticRouteProfileRef {
                id: "local-critic".into(),
                version: 1,
                hash: "D".repeat(64),
            },
        ] {
            let mut invalid = critic_settings();
            invalid.route_profile = Some(profile);
            assert!(journal
                .record_critic_round(
                    "123e4567-e89b-12d3-a456-426614174012",
                    &"a".repeat(64),
                    &"b".repeat(64),
                    &"c".repeat(64),
                    invalid,
                    vec![critic_reviewer("security")],
                )
                .is_err());
        }

        for guide in [
            CriticGuideRef {
                path: "../CRITICS.md".into(),
                sha256: "e".repeat(64),
            },
            CriticGuideRef {
                path: "AGENT_GUIDES/CRITICS.md".into(),
                sha256: "E".repeat(64),
            },
        ] {
            let mut invalid = critic_settings();
            invalid.coordinator_guide = Some(guide);
            assert!(journal
                .record_critic_round(
                    "123e4567-e89b-12d3-a456-426614174013",
                    &"a".repeat(64),
                    &"b".repeat(64),
                    &"c".repeat(64),
                    invalid,
                    vec![critic_reviewer("security")],
                )
                .is_err());
        }
    }

    #[test]
    fn critic_reviewer_route_provenance_is_optional_compatible_and_validated() {
        let legacy: CriticReviewerRecord = serde_json::from_value(serde_json::json!({
            "role": "security",
            "status": "completed",
            "output": "Review complete.",
            "outputTruncated": false,
            "stopReason": "end_turn",
            "candidateId": "local-review",
            "providerId": "openai",
            "modelId": "local-model",
            "elapsedMs": 900,
            "errorCode": null
        }))
        .unwrap();
        assert_eq!(legacy.route_profile, None);

        let nest = tempfile::tempdir().unwrap();
        let journal = open_journal(nest.path());
        let profile = CriticRouteProfileRef {
            id: "local-security".into(),
            version: 4,
            hash: "e".repeat(64),
        };
        let mut reviewer = critic_reviewer("security");
        reviewer.route_profile = Some(profile.clone());
        let saved = journal
            .record_critic_round(
                "123e4567-e89b-12d3-a456-426614174013",
                &"a".repeat(64),
                &"b".repeat(64),
                &"c".repeat(64),
                critic_settings(),
                vec![reviewer],
            )
            .unwrap();
        assert_eq!(saved.reviewers[0].route_profile, Some(profile.clone()));
        assert_eq!(
            CriticRoundSummary::from(saved).reviewers[0].route_profile,
            Some(profile)
        );

        let mut invalid_reviewer = critic_reviewer("security");
        invalid_reviewer.route_profile = Some(CriticRouteProfileRef {
            id: "Bad ID".into(),
            version: 1,
            hash: "f".repeat(64),
        });
        assert!(journal
            .record_critic_round(
                "123e4567-e89b-12d3-a456-426614174014",
                &"a".repeat(64),
                &"b".repeat(64),
                &"c".repeat(64),
                critic_settings(),
                vec![invalid_reviewer],
            )
            .is_err());
    }

    #[test]
    fn critic_cost_budgets_persist_and_reject_overallocated_reviewer_caps() {
        let nest = tempfile::tempdir().unwrap();
        let journal = open_journal(nest.path());
        let mut settings = critic_settings();
        settings.estimated_round_cost_budget_microusd = Some(100);
        let mut reviewer = critic_reviewer("security");
        reviewer.estimated_cost_limit_microusd = Some(60);
        let saved = journal
            .record_critic_round(
                "123e4567-e89b-12d3-a456-426614174015",
                &"a".repeat(64),
                &"b".repeat(64),
                &"c".repeat(64),
                settings.clone(),
                vec![reviewer.clone()],
            )
            .unwrap();
        assert_eq!(saved.settings, settings);
        assert_eq!(saved.reviewers[0].estimated_cost_limit_microusd, Some(60));
        assert_eq!(
            CriticRoundSummary::from(saved).reviewers[0].estimated_cost_limit_microusd,
            Some(60)
        );

        reviewer.estimated_cost_limit_microusd = Some(101);
        assert!(journal
            .record_critic_round(
                "123e4567-e89b-12d3-a456-426614174016",
                &"a".repeat(64),
                &"b".repeat(64),
                &"c".repeat(64),
                settings,
                vec![reviewer],
            )
            .is_err());
    }

    #[test]
    fn critic_round_reopens_by_id_and_persists_no_submitted_text() {
        let nest = tempfile::tempdir().unwrap();
        let round_id = "123e4567-e89b-12d3-a456-426614174002";
        let first = open_journal(nest.path());
        let saved = first
            .record_critic_round(
                round_id,
                &"a".repeat(64),
                &"b".repeat(64),
                &"c".repeat(64),
                critic_settings(),
                vec![critic_reviewer("security")],
            )
            .unwrap();
        drop(first);

        let reopened = open_journal(nest.path());
        assert_eq!(
            reopened.critic_round(round_id).unwrap(),
            Some(saved.clone())
        );
        assert!(reopened
            .critic_round("123e4567-e89b-12d3-a456-426614174003")
            .unwrap()
            .is_none());

        let conn = Connection::open(&reopened.db_path).unwrap();
        let stored: String = conn
            .query_row(
                "SELECT snapshot_sha256 || objective_sha256 || scope_sha256 || settings_json || reviewers_json
                 FROM critic_rounds WHERE round_id=?1",
                [round_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(stored.contains(&"a".repeat(64)));
        assert!(stored.contains("Review found one concrete issue."));
        assert!(!stored.contains("submitted snapshot text"));
        assert!(!stored.contains("original objective text"));
        assert!(!stored.contains("requested scope text"));
    }

    #[test]
    fn critic_round_rejects_invalid_hash_effort_roles_and_oversized_output() {
        let nest = tempfile::tempdir().unwrap();
        let journal = open_journal(nest.path());
        let round_id = "123e4567-e89b-12d3-a456-426614174004";

        assert!(journal
            .record_critic_round(
                round_id,
                &"A".repeat(64),
                &"b".repeat(64),
                &"c".repeat(64),
                critic_settings(),
                vec![critic_reviewer("security")],
            )
            .is_err());

        let mut invalid_effort = critic_settings();
        invalid_effort.thinking_effort_requested = Some("unbounded".into());
        assert!(journal
            .record_critic_round(
                round_id,
                &"a".repeat(64),
                &"b".repeat(64),
                &"c".repeat(64),
                invalid_effort,
                vec![critic_reviewer("security")],
            )
            .is_err());

        assert!(journal
            .record_critic_round(
                round_id,
                &"a".repeat(64),
                &"b".repeat(64),
                &"c".repeat(64),
                critic_settings(),
                vec![critic_reviewer("security"), critic_reviewer("security")],
            )
            .is_err());

        let mut oversized = critic_reviewer("security");
        oversized.output = Some("x".repeat(MAX_CRITIC_OUTPUT_BYTES + 1));
        assert!(journal
            .record_critic_round(
                round_id,
                &"a".repeat(64),
                &"b".repeat(64),
                &"c".repeat(64),
                critic_settings(),
                vec![oversized],
            )
            .is_err());
    }

    #[test]
    fn recent_critic_history_is_bounded_and_omits_findings_until_detail_read() {
        let nest = tempfile::tempdir().unwrap();
        let journal = open_journal(nest.path());
        for (round_id, role) in [
            ("123e4567-e89b-12d3-a456-426614174006", "security"),
            ("123e4567-e89b-12d3-a456-426614174007", "product"),
        ] {
            journal
                .record_critic_round(
                    round_id,
                    &"a".repeat(64),
                    &"b".repeat(64),
                    &"c".repeat(64),
                    critic_settings(),
                    vec![critic_reviewer(role)],
                )
                .unwrap();
        }

        let history = journal.recent_critic_rounds(2).unwrap();
        assert_eq!(history.len(), 2);
        assert!(history.iter().all(|round| round.reviewers.len() == 1));
        let summary_json = serde_json::to_string(&history).unwrap();
        assert!(!summary_json.contains("Review found one concrete issue."));
        assert!(journal.recent_critic_rounds(0).is_err());
        assert!(journal.recent_critic_rounds(51).is_err());
    }

    #[test]
    fn journal_reopens_with_typed_source_and_lifecycle_evidence() {
        let nest = tempfile::tempdir().unwrap();
        let start = start_record();
        {
            let journal = open_journal(nest.path());
            journal.record_started(&start).unwrap();
            journal
                .record_session_resolved(&start.turn_id, "acp-session-7")
                .unwrap();
            journal.record_prompt_call_started(&start.turn_id).unwrap();
            let steer_event_id = "e".repeat(64);
            journal
                .record_steer_submitted(&start.turn_id, &steer_event_id)
                .unwrap();
            journal
                .record_steer_outcome(
                    &start.turn_id,
                    &steer_event_id,
                    SteerOutcome::AdapterAcknowledged,
                )
                .unwrap();
            journal
                .record_returned(&start.turn_id, "acp_turn_returned")
                .unwrap();
        }

        let journal = open_journal(nest.path());
        let turn = journal.get(&start.turn_id).unwrap().unwrap();
        assert_eq!(turn.session_scope, "thread");
        let expected_root = "a".repeat(64);
        assert_eq!(
            turn.thread_root_event_id.as_deref(),
            Some(expected_root.as_str())
        );
        assert_eq!(turn.batch_trigger_event_ids, vec!["b".repeat(64)]);
        assert_eq!(turn.merged_cancelled_event_ids, vec!["c".repeat(64)]);
        assert_eq!(turn.acp_session_id.as_deref(), Some("acp-session-7"));
        assert_eq!(turn.status, "returned");
        assert_eq!(turn.liveness, "unknown");
        assert_eq!(turn.task_state, "unknown");
        let events = journal.events(&start.turn_id, 20).unwrap();
        assert_eq!(events.len(), 6);
        assert_eq!(
            events[0].details["managed_worker_generation_nonce"],
            "123e4567e89b12d3a456426614174000"
        );
        assert_eq!(
            events[0].details["adapter_child_generation_id"],
            "123e4567-e89b-12d3-a456-426614174002"
        );
        assert_eq!(
            events[0].details["agent_profile_v1"],
            serde_json::json!({
                "schema_version": 1,
                "source": "managed_agent_runtime_config",
                "harness_id": "goose",
                "provider_id": "anthropic",
                "model_id": "claude-opus",
                "agent_prompt_sha256": agent_prompt_fingerprint(Some("private prompt")),
                "execution_profile_id": "critic_security",
                "execution_profile_version": 1,
                "prompt_profile_id": "openai-coding",
                "prompt_profile_version": 2,
                "prompt_profile_hash": "d".repeat(64),
                "prompt_content_stored": false,
            })
        );
        assert!(!events[0].details.to_string().contains("private prompt"));
        assert_eq!(
            events[0].details["effective_controls"],
            serde_json::json!({
                "scope": "ACP process and per-turn settings",
                "configured_worker_pool_slots": 3,
                "idle_timeout_secs": 1_500,
                "max_turn_duration_secs": 7_200,
            })
        );
        let policy = &events[0].details["resource_policy_v1"];
        let metrics = policy["metrics"].as_array().unwrap();
        assert_eq!(policy["schema_version"], 1);
        assert_eq!(policy["scope"], "run");
        assert_eq!(policy["applies_to"], "current_attempt");
        assert_eq!(metrics.len(), 9);
        assert_eq!(
            metrics[0],
            serde_json::json!({
                "id": "agent.parallelism",
                "label": "ACP worker pool slots",
                "value": 3,
                "unit": "slots",
                "scope": "agent",
                "state": "configured",
                "source": "buzz_acp_runtime_config",
                "enforcement": "hard_runtime",
            })
        );
        assert_eq!(metrics[1]["enforcement"], "turn_boundary");
        assert_eq!(metrics[2]["value"], 7_200);
        assert_eq!(metrics[5]["state"], "unknown");
        assert_eq!(metrics[5]["value"], Value::Null);
        assert_eq!(metrics[7]["scope"], "device");
    }

    #[test]
    fn capture_gaps_survive_reopen_and_remain_identity_scoped() {
        let nest = tempfile::tempdir().unwrap();
        let owner = "d".repeat(64);
        {
            let journal =
                RunJournal::open_scoped(nest.path(), "ws://localhost:3000", &owner).unwrap();
            journal.record_capture_gap(3).unwrap();
            journal.record_capture_gap(2).unwrap();
            assert!(journal.record_capture_gap(0).is_err());
        }

        let reopened = RunJournal::open_scoped(nest.path(), "ws://localhost:3000", &owner).unwrap();
        assert_eq!(reopened.capture_gap_count().unwrap(), 5);

        let other_owner =
            RunJournal::open_scoped(nest.path(), "ws://localhost:3000", &"e".repeat(64)).unwrap();
        assert_eq!(other_owner.capture_gap_count().unwrap(), 0);
    }

    #[test]
    fn steer_events_are_source_linked_idempotent_and_reject_invalid_ids() {
        let nest = tempfile::tempdir().unwrap();
        let journal = open_journal(nest.path());
        let start = start_record();
        journal.record_started(&start).unwrap();
        let event_id = "e".repeat(64);

        journal
            .record_steer_submitted(&start.turn_id, &event_id)
            .unwrap();
        journal
            .record_steer_submitted(&start.turn_id, &event_id)
            .unwrap();
        journal
            .record_steer_outcome(&start.turn_id, &event_id, SteerOutcome::DeliveryUnknown)
            .unwrap();
        journal
            .record_steer_outcome(&start.turn_id, &event_id, SteerOutcome::DeliveryUnknown)
            .unwrap();
        assert!(journal
            .record_steer_submitted(&start.turn_id, "not-an-event-id")
            .is_err());

        let events = journal.events(&start.turn_id, 20).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[1].kind, "steer_submitted");
        assert_eq!(events[1].details["source_event_id"], event_id);
        assert_eq!(events[2].kind, "steer_outcome");
        assert_eq!(events[2].details["outcome"], "delivery_unknown");
    }

    #[test]
    fn recent_events_returns_latest_events_in_chronological_order_with_truncation() {
        let nest = tempfile::tempdir().unwrap();
        let journal = open_journal(nest.path());
        let start = start_record();
        journal.record_started(&start).unwrap();
        journal
            .record_session_resolved(&start.turn_id, "acp-session-7")
            .unwrap();
        journal.record_prompt_call_started(&start.turn_id).unwrap();
        journal
            .record_returned(&start.turn_id, "acp_turn_returned")
            .unwrap();

        let (events, has_more) = journal.recent_events(&start.turn_id, 2).unwrap();
        assert!(has_more);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "prompt_call_started");
        assert_eq!(events[1].kind, "turn_returned");
        assert!(events[0].sequence < events[1].sequence);

        let (all_events, has_more) = journal.recent_events(&start.turn_id, 20).unwrap();
        assert!(!has_more);
        assert_eq!(all_events.len(), 4);
    }

    #[test]
    fn thread_attempt_history_is_a_bounded_shared_projection() {
        let nest = tempfile::tempdir().unwrap();
        let journal = open_journal(nest.path());
        let start = start_record();
        journal.record_started(&start).unwrap();
        journal.record_prompt_call_started(&start.turn_id).unwrap();
        journal
            .record_returned(&start.turn_id, "acp_turn_returned")
            .unwrap();

        let history = journal
            .thread_attempt_history(
                start.channel_id.as_deref().unwrap(),
                start.thread_root_event_id.as_deref().unwrap(),
                &start.batch_trigger_event_ids,
            )
            .unwrap();
        assert!(!history.has_more_turns);
        assert_eq!(history.managed_turns.len(), 1);
        assert_eq!(history.managed_turns[0].turn.turn_id, start.turn_id);
        assert_eq!(
            history.managed_turns[0].recent_events[1].kind,
            "prompt_call_started"
        );
        assert!(!history.managed_turns[0].event_history_may_be_truncated);
    }

    #[test]
    fn coordinator_runs_keep_stable_identity_across_attempts_and_link_steers() {
        let nest = tempfile::tempdir().unwrap();
        let journal = open_journal(nest.path());
        let source_id = "b".repeat(64);
        let root_id = "a".repeat(64);
        let channel_id = "123e4567-e89b-12d3-a456-426614174001";
        let mut first = start_record();
        first.batch_trigger_event_ids = vec![source_id.clone()];
        first.merged_cancelled_event_ids.clear();
        journal.record_started(&first).unwrap();
        journal
            .record_returned(&first.turn_id, "acp_turn_returned")
            .unwrap();

        let mut retry = first.clone();
        retry.turn_id = "223e4567-e89b-12d3-a456-426614174000".into();
        journal.record_started(&retry).unwrap();
        journal.record_started(&retry).unwrap();
        let steer_id = "e".repeat(64);
        journal
            .record_steer_submitted(&retry.turn_id, &steer_id)
            .unwrap();

        let history = journal
            .thread_coordinator_runs(
                channel_id,
                &root_id,
                &[root_id.clone(), source_id.clone(), steer_id.clone()],
            )
            .unwrap();
        assert!(!history.has_more_runs);
        assert_eq!(history.runs.len(), 1);
        let run = &history.runs[0];
        assert_eq!(run.original_intent_event_id, source_id);
        assert_eq!(run.thread_root_event_id.as_deref(), Some(root_id.as_str()));
        assert_eq!(run.attempt_turns.len(), 2);
        assert_eq!(run.task_state, "unknown");
        assert_eq!(
            run.recent_events
                .iter()
                .filter(|event| event.kind == "attempt_linked")
                .count(),
            2
        );
        assert_eq!(
            run.recent_events.last().map(|event| event.kind.as_str()),
            Some("steer_source_linked")
        );
        let run_id = run.run_id.clone();
        assert!(journal
            .coordinator_run_in_thread(
                &run_id,
                channel_id,
                &root_id,
                &[source_id.clone(), steer_id.clone()],
            )
            .unwrap()
            .is_some());
        assert!(journal
            .coordinator_run_in_thread(
                &run_id,
                channel_id,
                &"f".repeat(64),
                &[source_id.clone(), steer_id.clone()],
            )
            .unwrap()
            .is_none());

        let other_thread = journal
            .thread_coordinator_runs(channel_id, &"f".repeat(64), &[root_id, source_id, steer_id])
            .unwrap();
        assert!(other_thread.runs.is_empty());
    }

    #[test]
    fn coordinator_run_pins_verified_project_and_surfaces_conflicting_relink() {
        let nest = tempfile::tempdir().unwrap();
        let journal = open_journal(nest.path());
        let source_id = "b".repeat(64);
        let root_id = "a".repeat(64);
        let channel_id = "123e4567-e89b-12d3-a456-426614174001";
        let mut first = start_record();
        first.batch_trigger_event_ids = vec![source_id.clone()];
        first.merged_cancelled_event_ids.clear();
        journal.record_started(&first).unwrap();

        let original_project = format!("30621:{}:buzz", "c".repeat(64));
        journal
            .record_project_linked(&first.turn_id, &original_project)
            .unwrap();
        assert!(journal
            .record_project_linked(&first.turn_id, "30617:not-a-project")
            .is_err());

        let mut retry = first.clone();
        retry.turn_id = "223e4567-e89b-12d3-a456-426614174000".into();
        journal.record_started(&retry).unwrap();
        journal
            .record_project_linked(&retry.turn_id, &original_project)
            .unwrap();

        let mut conflicting_retry = retry.clone();
        conflicting_retry.turn_id = "323e4567-e89b-12d3-a456-426614174000".into();
        journal.record_started(&conflicting_retry).unwrap();
        let changed_project = format!("30621:{}:other", "d".repeat(64));
        journal
            .record_project_linked(&conflicting_retry.turn_id, &changed_project)
            .unwrap();

        let history = journal
            .thread_coordinator_runs(channel_id, &root_id, &[root_id.clone(), source_id])
            .unwrap();
        assert_eq!(history.runs.len(), 1);
        let run = &history.runs[0];
        assert_eq!(
            run.project_coordinate.as_deref(),
            Some(original_project.as_str())
        );
        assert!(run.project_link_conflict);
        assert!(run.recent_events.iter().any(|event| {
            event.kind == "project_link_conflict"
                && event.details["observed_project_coordinate"] == changed_project
        }));
    }

    #[test]
    fn project_run_candidates_filter_by_coordinate_and_home_channel_then_keyset_page() {
        let nest = tempfile::tempdir().unwrap();
        let journal = open_journal(nest.path());
        let coordinate = format!("30621:{}:buzz", "c".repeat(64));
        let other_coordinate = format!("30621:{}:other", "e".repeat(64));
        let home_channel = "123e4567-e89b-12d3-a456-426614174001";
        let other_channel = "123e4567-e89b-12d3-a456-426614174099";

        for (turn_id, source_id, project, channel_id) in [
            (
                "123e4567-e89b-12d3-a456-426614174000",
                "b".repeat(64),
                coordinate.as_str(),
                home_channel,
            ),
            (
                "223e4567-e89b-12d3-a456-426614174000",
                "c".repeat(64),
                coordinate.as_str(),
                home_channel,
            ),
            (
                "323e4567-e89b-12d3-a456-426614174000",
                "d".repeat(64),
                coordinate.as_str(),
                home_channel,
            ),
            (
                "423e4567-e89b-12d3-a456-426614174000",
                "e".repeat(64),
                other_coordinate.as_str(),
                home_channel,
            ),
            (
                "523e4567-e89b-12d3-a456-426614174000",
                "f".repeat(64),
                coordinate.as_str(),
                other_channel,
            ),
        ] {
            let mut start = start_record();
            start.turn_id = turn_id.into();
            start.channel_id = Some(channel_id.into());
            start.batch_trigger_event_ids = vec![source_id];
            start.merged_cancelled_event_ids.clear();
            journal.record_started(&start).unwrap();
            journal.record_project_linked(turn_id, project).unwrap();
        }

        // Equal timestamps exercise the run-ID tiebreaker in the stable cursor.
        journal
            .connect()
            .unwrap()
            .execute("UPDATE coordinator_runs SET updated_at_ms=1000", [])
            .unwrap();
        let first_page = journal
            .project_coordinator_runs(&coordinate, home_channel, 2, None)
            .unwrap();
        assert_eq!(first_page.runs.len(), 2);
        assert!(first_page.has_more_candidates);
        let cursor = first_page.next_cursor.clone().unwrap();

        let second_page = journal
            .project_coordinator_runs(&coordinate, home_channel, 2, Some(&cursor))
            .unwrap();
        assert_eq!(second_page.runs.len(), 1);
        assert!(!second_page.has_more_candidates);
        assert!(second_page.next_cursor.is_none());

        let mut all_ids = first_page
            .runs
            .iter()
            .chain(second_page.runs.iter())
            .map(|run| run.run_id.clone())
            .collect::<Vec<_>>();
        let mut sorted_ids = all_ids.clone();
        sorted_ids.sort_by(|left, right| right.cmp(left));
        assert_eq!(all_ids, sorted_ids);
        all_ids.sort();
        all_ids.dedup();
        assert_eq!(all_ids.len(), 3);
    }

    #[test]
    fn project_run_candidate_query_rejects_invalid_scope_and_cursor() {
        let nest = tempfile::tempdir().unwrap();
        let journal = open_journal(nest.path());
        let coordinate = format!("30621:{}:buzz", "c".repeat(64));
        let channel = "123e4567-e89b-12d3-a456-426614174001";
        assert!(journal
            .project_coordinator_runs("30617:not-a-project", channel, 10, None)
            .is_err());
        assert!(journal
            .project_coordinator_runs(&coordinate, "not-a-channel", 10, None)
            .is_err());
        assert!(journal
            .project_coordinator_runs(
                &coordinate,
                channel,
                10,
                Some(&CoordinatorRunCursor {
                    updated_at_ms: -1,
                    run_id: Uuid::new_v4().to_string(),
                }),
            )
            .is_err());
        assert!(journal
            .project_coordinator_runs(
                &coordinate,
                channel,
                10,
                Some(&CoordinatorRunCursor {
                    updated_at_ms: 100,
                    run_id: "not-a-run-id".into(),
                }),
            )
            .is_err());
    }

    #[test]
    fn project_attempt_projection_requires_all_sources_on_recorded_channel() {
        let channel = "123e4567-e89b-12d3-a456-426614174001";
        let root = "a".repeat(64);
        let trigger = "b".repeat(64);
        let merged = "c".repeat(64);
        let run = CoordinatorRunSummary {
            run_id: "123e4567-e89b-12d3-a456-426614174000".into(),
            channel_id: channel.into(),
            session_scope: "thread".into(),
            thread_root_event_id: Some(root.clone()),
            original_intent_event_id: trigger.clone(),
            project_coordinate: None,
            project_link_conflict: false,
            attempt_turns: vec![TurnSummary {
                turn_id: "223e4567-e89b-12d3-a456-426614174000".into(),
                channel_id: Some(channel.into()),
                session_scope: "thread".into(),
                thread_root_event_id: Some(root.clone()),
                batch_trigger_event_ids: vec![trigger.clone()],
                merged_cancelled_event_ids: vec![merged.clone()],
                agent_index: 2,
                acp_session_id: Some("captured-session".into()),
                status: "returned".into(),
                liveness: "unknown".into(),
                task_state: "unknown".into(),
                started_at_ms: 10,
                updated_at_ms: 20,
            }],
            attempt_history_may_be_truncated: false,
            recent_events: vec![CoordinatorRunEvent {
                sequence: 1,
                run_id: "123e4567-e89b-12d3-a456-426614174000".into(),
                event_key: "steer:event".into(),
                kind: "steer_source_linked".into(),
                occurred_at_ms: 20,
                details: serde_json::json!({"source_event_id": "private-id"}),
            }],
            event_history_may_be_truncated: false,
            created_at_ms: 10,
            updated_at_ms: 20,
            task_state: "unknown".into(),
        };

        let targets = project_run_attempt_source_targets(std::slice::from_ref(&run));
        assert_eq!(targets.targets.len(), 3);
        let readable = BTreeSet::from([
            (channel.into(), root.clone()),
            (channel.into(), trigger.clone()),
            (channel.into(), merged.clone()),
        ]);
        let evidence = project_run_evidence(&run, &readable, 10);
        assert_eq!(evidence.attempt_turns.len(), 1);
        assert_eq!(
            evidence.attempt_turns[0].acp_session_id.as_deref(),
            Some("captured-session")
        );
        assert_eq!(evidence.attempt_turns[0].runtime_session_match, "unknown");
        assert!(!evidence.attempt_turns[0].control_target_available);
        assert_eq!(evidence.task_state, "unknown");
        assert_eq!(evidence.history_reliability, "best_effort");
        assert_eq!(evidence.history_completeness, "unknown");
        let serialized = serde_json::to_value(evidence).unwrap();
        assert!(serialized.get("recent_events").is_none());
        assert!(serialized["attempt_turns"][0]
            .get("acp_session_id")
            .is_none());
        assert!(serialized.to_string().find("private-id").is_none());

        let wrong_channel = BTreeSet::from([
            ("123e4567-e89b-12d3-a456-426614174099".into(), root),
            (channel.into(), trigger),
            (channel.into(), merged),
        ]);
        assert!(project_run_evidence(&run, &wrong_channel, 10)
            .attempt_turns
            .is_empty());
    }

    #[test]
    fn project_attempt_page_limits_report_omitted_rows_and_sources() {
        let channel = "123e4567-e89b-12d3-a456-426614174001";
        let mut run = CoordinatorRunSummary {
            run_id: "123e4567-e89b-12d3-a456-426614174000".into(),
            channel_id: channel.into(),
            session_scope: "thread".into(),
            thread_root_event_id: None,
            original_intent_event_id: "a".repeat(64),
            project_coordinate: None,
            project_link_conflict: false,
            attempt_turns: vec![TurnSummary {
                turn_id: "223e4567-e89b-12d3-a456-426614174000".into(),
                channel_id: Some(channel.into()),
                session_scope: "thread".into(),
                thread_root_event_id: None,
                batch_trigger_event_ids: vec!["b".repeat(64)],
                merged_cancelled_event_ids: Vec::new(),
                agent_index: 0,
                acp_session_id: Some("captured-session".into()),
                status: "returned".into(),
                liveness: "unknown".into(),
                task_state: "unknown".into(),
                started_at_ms: 10,
                updated_at_ms: 20,
            }],
            attempt_history_may_be_truncated: false,
            recent_events: Vec::new(),
            event_history_may_be_truncated: false,
            created_at_ms: 10,
            updated_at_ms: 20,
            task_state: "unknown".into(),
        };
        let readable = BTreeSet::from([(channel.into(), "b".repeat(64))]);

        let mut second_turn = run.attempt_turns[0].clone();
        second_turn.turn_id = "323e4567-e89b-12d3-a456-426614174000".into();
        run.attempt_turns.push(second_turn);
        let page = project_run_evidence(&run, &readable, 1);
        assert_eq!(page.attempt_turns.len(), 1);
        assert!(page.attempt_evidence_may_be_truncated);
        assert!(!project_run_evidence(&run, &readable, 2).attempt_evidence_may_be_truncated);

        run.attempt_turns.truncate(1);
        run.attempt_turns[0].batch_trigger_event_ids = (0..=PROJECT_RUN_ATTEMPT_SOURCE_LIMIT)
            .map(|index| format!("{index:064x}"))
            .collect();
        let oversized_attempt = project_run_attempt_source_targets(std::slice::from_ref(&run));
        assert!(oversized_attempt.targets.is_empty());
        assert!(oversized_attempt.may_be_truncated);

        let many_runs = (0..32)
            .map(|run_index| {
                let mut bounded_run = run.clone();
                let mut turn = run.attempt_turns[0].clone();
                turn.batch_trigger_event_ids = (0..PROJECT_RUN_ATTEMPT_SOURCE_LIMIT)
                    .map(|source_index| {
                        format!(
                            "{:064x}",
                            run_index * PROJECT_RUN_ATTEMPT_SOURCE_LIMIT + source_index
                        )
                    })
                    .collect();
                bounded_run.attempt_turns = vec![turn];
                bounded_run
            })
            .collect::<Vec<_>>();
        let bounded_targets = project_run_attempt_source_targets(&many_runs);
        assert_eq!(
            bounded_targets.targets.len(),
            PROJECT_RUN_SOURCE_CHECK_LIMIT
        );
        assert!(bounded_targets.may_be_truncated);
    }

    #[test]
    fn coordinator_run_migration_backfills_existing_attempts_once() {
        let nest = tempfile::tempdir().unwrap();
        let start = start_record();
        let journal = open_journal(nest.path());
        journal.record_started(&start).unwrap();
        let db_path = journal.db_path.clone();
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute("DELETE FROM coordinator_run_events", [])
                .unwrap();
            conn.execute("DELETE FROM coordinator_run_turns", [])
                .unwrap();
            conn.execute("DELETE FROM coordinator_run_sources", [])
                .unwrap();
            conn.execute("DELETE FROM coordinator_runs", []).unwrap();
            conn.execute(
                "DELETE FROM managed_run_journal_migrations WHERE version=2",
                [],
            )
            .unwrap();
        }

        let reopened = open_journal(nest.path());
        let root = start.thread_root_event_id.unwrap();
        let history = reopened
            .thread_coordinator_runs(
                start.channel_id.as_deref().unwrap(),
                &root,
                &start.batch_trigger_event_ids,
            )
            .unwrap();
        assert_eq!(history.runs.len(), 2);
        assert!(history
            .runs
            .iter()
            .all(|run| run.attempt_turns.len() == 1 && !run.recent_events.is_empty()));

        let again = open_journal(nest.path())
            .thread_coordinator_runs(
                start.channel_id.as_deref().unwrap(),
                &root,
                &start.batch_trigger_event_ids,
            )
            .unwrap();
        assert_eq!(
            history
                .runs
                .iter()
                .map(|run| &run.run_id)
                .collect::<Vec<_>>(),
            again.runs.iter().map(|run| &run.run_id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn start_rejects_mismatched_thread_identity_and_caps_source_ids() {
        let nest = tempfile::tempdir().unwrap();
        let journal = open_journal(nest.path());
        let mut invalid = start_record();
        invalid.thread_root_event_id = None;
        assert!(journal.record_started(&invalid).is_err());

        let mut oversized = start_record();
        oversized.batch_trigger_event_ids = vec!["c".repeat(64); MAX_SOURCE_EVENTS + 1];
        assert!(journal.record_started(&oversized).is_err());
    }

    #[test]
    fn scoped_journals_separate_relay_and_owner_and_normalize_transport_scheme() {
        let nest = tempfile::tempdir().unwrap();
        let start = start_record();
        let same_scope =
            RunJournal::open_scoped(nest.path(), "http://localhost:3000/", &"d".repeat(64))
                .unwrap();
        same_scope.record_started(&start).unwrap();

        let equivalent_scope =
            RunJournal::open_scoped(nest.path(), "ws://localhost:3000", &"D".repeat(64)).unwrap();
        assert!(equivalent_scope.get(&start.turn_id).unwrap().is_some());

        let other_relay =
            RunJournal::open_scoped(nest.path(), "wss://other.example", &"d".repeat(64)).unwrap();
        assert!(other_relay.get(&start.turn_id).unwrap().is_none());

        let other_owner =
            RunJournal::open_scoped(nest.path(), "ws://localhost:3000", &"e".repeat(64)).unwrap();
        assert!(other_owner.get(&start.turn_id).unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn journal_permissions_do_not_change_the_existing_nest_directory() {
        use std::os::unix::fs::PermissionsExt;

        let nest = tempfile::tempdir().unwrap();
        fs::set_permissions(nest.path(), fs::Permissions::from_mode(0o750)).unwrap();
        let journal = open_journal(nest.path());

        assert_eq!(
            fs::metadata(nest.path()).unwrap().permissions().mode() & 0o777,
            0o750
        );
        let journal_dir = journal.db_path.parent().unwrap();
        assert_eq!(
            fs::metadata(journal_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&journal.db_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn thread_attempt_filter_runs_before_limit_and_matches_readable_sources() {
        let nest = tempfile::tempdir().unwrap();
        let journal = open_journal(nest.path());
        let channel = "123e4567-e89b-12d3-a456-426614174001";
        let root = "a".repeat(64);
        let trigger = "b".repeat(64);

        let mut matching = start_record();
        matching.channel_id = Some(channel.into());
        matching.thread_root_event_id = Some("c".repeat(64));
        matching.batch_trigger_event_ids = vec![trigger.clone()];
        journal.record_started(&matching).unwrap();

        for index in 0..110_u64 {
            let mut unrelated = start_record();
            unrelated.turn_id = format!("123e4567-e89b-12d3-a456-{index:012x}");
            unrelated.channel_id = Some(channel.into());
            unrelated.thread_root_event_id = Some(format!("{:064x}", index + 1000));
            unrelated.batch_trigger_event_ids = vec![format!("{:064x}", index + 2000)];
            journal.record_started(&unrelated).unwrap();
        }

        let turns = journal
            .list_recent_for_thread(channel, &root, &[trigger], 10)
            .unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].turn_id, matching.turn_id);
    }

    #[test]
    fn thread_attempt_source_index_backfills_existing_journal_rows() {
        let nest = tempfile::tempdir().unwrap();
        let journal = open_journal(nest.path());
        let start = start_record();
        journal.record_started(&start).unwrap();
        let db_path = journal.db_path.clone();
        drop(journal);

        let legacy = Connection::open(&db_path).unwrap();
        legacy
            .execute_batch(
                "DROP TABLE managed_run_journal_migrations;
                 DROP TABLE managed_turn_sources;",
            )
            .unwrap();
        drop(legacy);

        let reopened = open_journal(nest.path());
        let turns = reopened
            .list_recent_for_thread(
                start.channel_id.as_deref().unwrap(),
                start.thread_root_event_id.as_deref().unwrap(),
                &[],
                10,
            )
            .unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].turn_id, start.turn_id);
    }
}
