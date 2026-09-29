//! Deterministic eligibility preview and single-candidate model dispatch.
//!
//! Preview accepts caller-supplied evidence only. It does not discover models
//! or verify that evidence. `complete_routed` dispatches only after the preview
//! chooses one bound candidate; it does not retry or fall back. Unknown evidence
//! fails any hard requirement that depends on it.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::config::MAX_SYSTEM_PROMPT_BYTES;
use crate::config::{Config, Provider};
use crate::llm::Llm;
use crate::task_fit_evidence::{
    TaskFitEligibility, TaskFitEligibilityPolicy, TaskFitRejectedReason, TaskFitUnknownReason,
};
use crate::types::{
    AgentError, ContentBlock, HistoryItem, LlmResponse, ToolDef, ToolResultContent,
};

/// Provenance label attached to a supplied fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceSource {
    Adapter,
    ProviderApi,
    LocalManifest,
    RuntimeObserved,
    Measured,
    OperatorConfig,
    ConservativeEstimate,
}

/// A fact with explicit provenance, or a fact that remains unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Evidence<T> {
    Unknown,
    Known { value: T, source: EvidenceSource },
}

impl<T> Evidence<T> {
    fn value(&self) -> Option<&T> {
        match self {
            Self::Unknown => None,
            Self::Known { value, .. } => Some(value),
        }
    }
}

impl RouteCandidate {
    pub fn context_capacity_tokens(&self) -> Option<u64> {
        self.context_tokens.value().copied()
    }

    pub fn context_fit_summary(&self) -> Option<RouteContextFitSummary> {
        let (input_tokens_upper_bound, capacity_tokens) = (
            self.input_context_upper_bound_tokens.value().copied()?,
            self.context_tokens.value().copied()?,
        );
        Some(RouteContextFitSummary {
            estimate_method: ContextEstimateMethod::Utf8BytesPlusFramingAndOutputReserveV1,
            capacity_source: ContextCapacitySource::OperatorDeclared,
            input_tokens_upper_bound,
            capacity_tokens,
        })
    }
}

/// Where candidate execution sends task data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DataLocation {
    Local,
    Hosted,
}

/// A model/runtime candidate plus only the evidence used by the hard gates.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteCandidate {
    pub id: String,
    pub provider: Provider,
    pub model: String,
    pub available: Evidence<bool>,
    pub data_location: Evidence<DataLocation>,
    /// Estimated upper-bound cost for this task, in micro-USD.
    pub max_cost_microusd: Evidence<u64>,
    /// Estimated upper-bound wall time for this task, in seconds.
    pub max_seconds: Evidence<u64>,
    /// Operator-declared context window capacity, in tokens.
    pub context_tokens: Evidence<u64>,
    /// Conservative UTF-8 upper-bound estimate for this request's input plus
    /// the configured output reserve. It is not exact provider tokenization or
    /// guaranteed provider context-window accounting.
    pub input_context_upper_bound_tokens: Evidence<u64>,
    /// Measured throughput, in milli-tokens per second.
    pub tokens_per_second_milli: Evidence<u64>,
    /// Known tools that this candidate can use. Unknown means no tool claim.
    pub tools: Evidence<BTreeSet<String>>,
}

/// Data-location policy; local-only is the default.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DataPolicy {
    #[default]
    LocalOnly,
    Allow(BTreeSet<DataLocation>),
}

/// Hard constraints and an optional explicit user preference.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RouteRequirements {
    pub data_policy: DataPolicy,
    pub max_cost_microusd: Option<u64>,
    pub max_seconds: Option<u64>,
    pub min_context_tokens: Option<u64>,
    /// Require a candidate capacity to cover the request-specific conservative
    /// UTF-8 upper-bound estimate.
    pub require_context_fit: bool,
    /// Minimum tokens/second, expressed in milli-tokens/second.
    pub min_tokens_per_second_milli: Option<u64>,
    /// During an explicitly enabled warm-up, unknown measured throughput may
    /// remain eligible in the configured preference order. Measured values
    /// below the floor are still excluded.
    pub allow_unknown_throughput_warmup: bool,
    /// Require a qualified task-fit decision for each candidate.
    pub require_task_fit: bool,
    /// Task-fit decisions resolved for this exact candidate set.
    pub task_fit_by_candidate: std::collections::BTreeMap<String, TaskFitEligibility>,
    pub required_tools: BTreeSet<String>,
    /// Select among eligible candidates; it cannot override a hard exclusion.
    pub preferred_candidate_id: Option<String>,
    /// Ordered explicit preferences. The first eligible ID wins; missing or
    /// duplicate IDs make the route configuration invalid instead of silently
    /// changing its meaning.
    pub preference_order: Vec<String>,
    /// A safety refusal is terminal and skips all candidate evaluation.
    pub safety_refusal: bool,
}

const ROUTE_PROFILE_MAX_BYTES: usize = 64 * 1024;
const ROUTE_PROFILE_MAX_CANDIDATES: usize = 16;
const ROUTE_PROMPT_ADDENDUM_MAX_BYTES: usize = 16 * 1024;
const MAX_ROUTE_COST_MICROUSD: u64 = 1_000_000_000_000;
const MICROUSD_PER_MILLION_TOKENS: u128 = 1_000_000;
const CONTEXT_FRAMING_BASE_TOKENS: u64 = 256;
const CONTEXT_FRAMING_PER_ITEM_TOKENS: u64 = 128;

fn is_false(value: &bool) -> bool {
    !*value
}

fn context_capacity_evidence(capacity: Option<u64>) -> Evidence<u64> {
    match capacity {
        Some(value) => Evidence::Known {
            value,
            source: EvidenceSource::OperatorConfig,
        },
        None => Evidence::Unknown,
    }
}

/// A versioned, conservative UTF-8 upper-bound estimate for request sizing.
/// It counts UTF-8 bytes, adds fixed message/tool framing allowances, and
/// reserves the configured output ceiling. It is not exact provider
/// tokenization or guaranteed provider context-window accounting.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextEstimateMethod {
    Utf8BytesPlusFramingAndOutputReserveV1,
}

/// Provenance for the candidate capacity used by the strict fit check.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextCapacitySource {
    OperatorDeclared,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteContextFitSummary {
    pub estimate_method: ContextEstimateMethod,
    pub capacity_source: ContextCapacitySource,
    pub input_tokens_upper_bound: u64,
    pub capacity_tokens: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextEstimateError {
    UnsupportedPromptContent,
    MultimodalHistory,
    OpaqueReasoningReplay,
    OpaqueToolCallReplay,
    Overflow,
}

impl std::fmt::Display for ContextEstimateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::UnsupportedPromptContent => "incoming prompt contains unsupported content",
            Self::MultimodalHistory => "session history contains multimodal content",
            Self::OpaqueReasoningReplay => {
                "session history contains provider-owned reasoning replay data"
            }
            Self::OpaqueToolCallReplay => {
                "session history contains provider-owned tool-call metadata"
            }
            Self::Overflow => "request context estimate overflowed its supported range",
        };
        f.write_str(message)
    }
}

impl std::error::Error for ContextEstimateError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextFitCheckError {
    EstimateUnavailable(ContextEstimateError),
    CapacityExceeded {
        input_tokens_upper_bound: u64,
        capacity_tokens: u64,
    },
}

impl std::fmt::Display for ContextFitCheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EstimateUnavailable(error) => write!(f, "{error}"),
            Self::CapacityExceeded {
                input_tokens_upper_bound,
                capacity_tokens,
            } => write!(
                f,
                "conservative UTF-8 upper-bound estimate is {input_tokens_upper_bound} tokens; operator-declared candidate capacity is {capacity_tokens} tokens"
            ),
        }
    }
}

/// Shared enforcement point used before every strict-fit provider request.
pub fn check_context_fit(
    capacity_tokens: u64,
    system_prompt: &str,
    history: &[HistoryItem],
    tools: &[ToolDef],
    output_reserve_tokens: u32,
) -> Result<RouteContextFitSummary, ContextFitCheckError> {
    let input_tokens_upper_bound =
        estimate_history_context_upper_bound(system_prompt, history, tools, output_reserve_tokens)
            .map_err(ContextFitCheckError::EstimateUnavailable)?;
    checked_context_fit(input_tokens_upper_bound, capacity_tokens)
}

/// Check a provider request whose only user content is one borrowed text
/// block. Used for synthetic summaries that do not send the session history.
pub fn check_user_text_context_fit(
    capacity_tokens: u64,
    system_prompt: &str,
    user_prompt: &str,
    tools: &[ToolDef],
    output_reserve_tokens: u32,
) -> Result<RouteContextFitSummary, ContextFitCheckError> {
    let (mut text_bytes, mut framed_items) = estimate_history_parts(system_prompt, &[], tools)
        .map_err(ContextFitCheckError::EstimateUnavailable)?;
    framed_items = framed_items
        .checked_add(1)
        .ok_or(ContextFitCheckError::EstimateUnavailable(
            ContextEstimateError::Overflow,
        ))?;
    add_bytes(&mut text_bytes, user_prompt.len())
        .map_err(ContextFitCheckError::EstimateUnavailable)?;
    let input_tokens_upper_bound =
        finish_context_estimate(text_bytes, framed_items, output_reserve_tokens)
            .map_err(ContextFitCheckError::EstimateUnavailable)?;
    checked_context_fit(input_tokens_upper_bound, capacity_tokens)
}

fn checked_context_fit(
    input_tokens_upper_bound: u64,
    capacity_tokens: u64,
) -> Result<RouteContextFitSummary, ContextFitCheckError> {
    if input_tokens_upper_bound > capacity_tokens {
        return Err(ContextFitCheckError::CapacityExceeded {
            input_tokens_upper_bound,
            capacity_tokens,
        });
    }
    Ok(RouteContextFitSummary {
        estimate_method: ContextEstimateMethod::Utf8BytesPlusFramingAndOutputReserveV1,
        capacity_source: ContextCapacitySource::OperatorDeclared,
        input_tokens_upper_bound,
        capacity_tokens,
    })
}

/// Estimate the first outgoing request, including prior session history and
/// the incoming ACP content before it is appended to that history.
pub fn estimate_prompt_context_upper_bound(
    system_prompt: &str,
    history: &[HistoryItem],
    incoming_prompt: &[ContentBlock],
    tools: &[ToolDef],
    output_reserve_tokens: u32,
) -> Result<u64, ContextEstimateError> {
    let (mut text_bytes, mut framed_items) = estimate_history_parts(system_prompt, history, tools)?;
    // Match prompt_to_text's newline joining without allocating cloned strings
    // or a temporary history item for the incoming request.
    framed_items = framed_items
        .checked_add(1)
        .ok_or(ContextEstimateError::Overflow)?;
    for (index, block) in incoming_prompt.iter().enumerate() {
        if index > 0 {
            add_bytes(&mut text_bytes, "\n".len())?;
        }
        match block {
            ContentBlock::Text { text } => add_bytes(&mut text_bytes, text.len())?,
            ContentBlock::ResourceLink { uri } => {
                add_bytes(&mut text_bytes, "[resource: ".len())?;
                add_bytes(&mut text_bytes, uri.len())?;
                add_bytes(&mut text_bytes, "]".len())?;
            }
            ContentBlock::Unsupported => {
                return Err(ContextEstimateError::UnsupportedPromptContent);
            }
        }
    }
    finish_context_estimate(text_bytes, framed_items, output_reserve_tokens)
}

/// Estimate the serializable text/history/tool inputs available at one
/// provider-call boundary. Strict mode rejects opaque multimodal or
/// provider-specific replay state instead of guessing at its model-dependent
/// token cost.
pub fn estimate_history_context_upper_bound(
    system_prompt: &str,
    history: &[HistoryItem],
    tools: &[ToolDef],
    output_reserve_tokens: u32,
) -> Result<u64, ContextEstimateError> {
    let mut text_bytes =
        u64::try_from(system_prompt.len()).map_err(|_| ContextEstimateError::Overflow)?;
    let mut framed_items = 1u64;

    estimate_history_and_tools(&mut text_bytes, &mut framed_items, history, tools)?;
    finish_context_estimate(text_bytes, framed_items, output_reserve_tokens)
}

fn estimate_history_parts(
    system_prompt: &str,
    history: &[HistoryItem],
    tools: &[ToolDef],
) -> Result<(u64, u64), ContextEstimateError> {
    let mut text_bytes =
        u64::try_from(system_prompt.len()).map_err(|_| ContextEstimateError::Overflow)?;
    let mut framed_items = 1u64;
    estimate_history_and_tools(&mut text_bytes, &mut framed_items, history, tools)?;
    Ok((text_bytes, framed_items))
}

fn estimate_history_and_tools(
    text_bytes: &mut u64,
    framed_items: &mut u64,
    history: &[HistoryItem],
    tools: &[ToolDef],
) -> Result<(), ContextEstimateError> {
    for item in history {
        *framed_items = framed_items
            .checked_add(1)
            .ok_or(ContextEstimateError::Overflow)?;
        match item {
            HistoryItem::User(text) => add_bytes(text_bytes, text.len())?,
            HistoryItem::Assistant {
                text,
                tool_calls,
                reasoning_details,
            } => {
                add_bytes(text_bytes, text.len())?;
                if reasoning_details.is_some() {
                    return Err(ContextEstimateError::OpaqueReasoningReplay);
                }
                for call in tool_calls {
                    if !call.provider_extra.is_empty() {
                        return Err(ContextEstimateError::OpaqueToolCallReplay);
                    }
                    add_bytes(text_bytes, call.provider_id.len())?;
                    add_bytes(text_bytes, call.name.len())?;
                    add_bytes(
                        text_bytes,
                        serde_json::to_vec(&call.arguments)
                            .map_err(|_| ContextEstimateError::Overflow)?
                            .len(),
                    )?;
                    *framed_items = framed_items
                        .checked_add(1)
                        .ok_or(ContextEstimateError::Overflow)?;
                }
            }
            HistoryItem::ToolResult(result) => {
                add_bytes(text_bytes, result.provider_id.len())?;
                for content in &result.content {
                    match content {
                        ToolResultContent::Text(text) => add_bytes(text_bytes, text.len())?,
                        ToolResultContent::Image { .. } => {
                            return Err(ContextEstimateError::MultimodalHistory);
                        }
                    }
                    *framed_items = framed_items
                        .checked_add(1)
                        .ok_or(ContextEstimateError::Overflow)?;
                }
            }
        }
    }

    for tool in tools {
        add_bytes(text_bytes, tool.name.len())?;
        add_bytes(text_bytes, tool.description.len())?;
        add_bytes(
            text_bytes,
            serde_json::to_vec(&tool.input_schema)
                .map_err(|_| ContextEstimateError::Overflow)?
                .len(),
        )?;
        *framed_items = framed_items
            .checked_add(1)
            .ok_or(ContextEstimateError::Overflow)?;
    }

    Ok(())
}

fn finish_context_estimate(
    text_bytes: u64,
    framed_items: u64,
    output_reserve_tokens: u32,
) -> Result<u64, ContextEstimateError> {
    text_bytes
        .checked_add(CONTEXT_FRAMING_BASE_TOKENS)
        .and_then(|total| {
            framed_items
                .checked_mul(CONTEXT_FRAMING_PER_ITEM_TOKENS)
                .and_then(|framing| total.checked_add(framing))
        })
        .and_then(|total| total.checked_add(u64::from(output_reserve_tokens)))
        .ok_or(ContextEstimateError::Overflow)
}

fn add_bytes(total: &mut u64, bytes: usize) -> Result<(), ContextEstimateError> {
    *total = total
        .checked_add(u64::try_from(bytes).map_err(|_| ContextEstimateError::Overflow)?)
        .ok_or(ContextEstimateError::Overflow)?;
    Ok(())
}

/// Versioned, non-secret route configuration. Provider credentials and API
/// endpoints are resolved from the normal provider environment; this document
/// may only choose an adapter, model, locality declaration, and prompt addendum.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RouteProfileDocument {
    pub version: u16,
    #[serde(default)]
    pub data_policy: RouteProfileDataPolicy,
    #[serde(default)]
    pub preference_order: Vec<String>,
    /// Opt-in strict fit gate. Absent in existing profile files, so legacy
    /// profiles retain ordered-candidate behavior.
    #[serde(default, skip_serializing_if = "is_false")]
    pub strict_context_fit: bool,
    /// Optional estimated ceiling in micro-USD for the selected candidate's
    /// provider requests during one ACP prompt turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turn_cost_microusd: Option<u64>,
    /// Rank eligible candidates by fresh local effective output tokens/sec.
    /// Missing evidence is excluded unless preference-order warm-up is enabled.
    #[serde(default, skip_serializing_if = "is_false")]
    pub prefer_fastest_measured: bool,
    /// Minimum fresh effective output tokens/sec, in milli-tokens/sec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_effective_output_tokens_per_second_milli: Option<u64>,
    /// Opt in to dispatching an unknown-speed candidate by preference order
    /// while it accumulates enough fresh local observations.
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_preference_order_warmup: bool,
    /// Optional hard task-fit gate. Missing or unverified evidence abstains.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_fit_policy: Option<TaskFitEligibilityPolicy>,
    pub candidates: Vec<RouteProfileCandidate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_hash: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RouteProfileDataPolicy {
    #[default]
    LocalOnly,
    AllowHosted,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RouteProfileLocation {
    Local,
    Hosted,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RouteProfileCandidate {
    pub id: String,
    pub provider: String,
    pub model: String,
    pub data_location: RouteProfileLocation,
    /// User/operator-declared context window capacity. Buzz does not verify it
    /// against provider metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_capacity_tokens: Option<u64>,
    /// Operator-entered USD micro-units per million input tokens. Rates are
    /// used only for a request with an enabled cost ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_cost_microusd_per_million_tokens: Option<u64>,
    /// Operator-entered USD micro-units per million output tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_cost_microusd_per_million_tokens: Option<u64>,
    #[serde(default)]
    pub prompt_addendum: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_profile: Option<RoutePromptProfileRef>,
}

/// Per-agent-turn estimate budget attached to a selected route. Before each
/// provider request Buzz reserves the request's conservative input estimate
/// plus the full configured output ceiling. This bounds the estimate, not the
/// provider's eventual invoice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteCostBudget {
    pub max_turn_cost_microusd: u64,
    pub input_cost_microusd_per_million_tokens: u64,
    pub output_cost_microusd_per_million_tokens: u64,
}

impl RouteCostBudget {
    pub fn estimate_call_microusd(
        &self,
        system_prompt: &str,
        history: &[HistoryItem],
        tools: &[ToolDef],
        max_output_tokens: u32,
    ) -> Option<u64> {
        // At a provider-call boundary the incoming ACP prompt has already
        // been appended to `history`; do not count a second empty incoming
        // item on every tool-loop round.
        let total_tokens =
            estimate_history_context_upper_bound(system_prompt, history, tools, max_output_tokens)
                .ok()?;
        let input_tokens = total_tokens.checked_sub(u64::from(max_output_tokens))?;
        estimate_request_cost_with_retry_ceiling_microusd(
            input_tokens,
            max_output_tokens,
            self.input_cost_microusd_per_million_tokens,
            self.output_cost_microusd_per_million_tokens,
        )
    }

    pub fn reserve_call(
        &self,
        already_reserved_microusd: u64,
        next_call_microusd: u64,
    ) -> Result<u64, String> {
        let reserved = already_reserved_microusd
            .checked_add(next_call_microusd)
            .ok_or_else(|| {
                "route cost estimate overflowed; no provider request was sent".to_owned()
            })?;
        if reserved > self.max_turn_cost_microusd {
            let remaining = self
                .max_turn_cost_microusd
                .saturating_sub(already_reserved_microusd);
            return Err(format!(
                "route cost ceiling stopped this request: conservative estimate is {next_call_microusd} micro-USD, but only {remaining} micro-USD remains for this agent turn"
            ));
        }
        Ok(reserved)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RoutePromptProfileRef {
    pub id: String,
    pub version: u32,
    pub prompt_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteProfileError {
    TooLarge,
    InvalidJson(String),
    UnsupportedVersion(u16),
    CandidateCount,
    InvalidCandidateId(String),
    DuplicateCandidateId(String),
    InvalidProvider {
        candidate_id: String,
        provider: String,
    },
    InvalidModel {
        candidate_id: String,
    },
    InvalidContextCapacity {
        candidate_id: String,
    },
    InvalidMinimumThroughput,
    InvalidMaximumCost,
    InvalidCandidatePricing {
        candidate_id: String,
    },
    PromptAddendumTooLarge {
        candidate_id: String,
    },
    InvalidPromptProfile {
        candidate_id: String,
    },
    InvalidProfileMetadata,
    DuplicatePreference(String),
    UnknownPreference(String),
    CandidateConfiguration {
        candidate_id: String,
    },
    LocalityMismatch {
        candidate_id: String,
    },
    CombinedPromptTooLarge {
        candidate_id: String,
    },
    InvalidTaskFitPolicy(String),
}

impl std::fmt::Display for RouteProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => write!(f, "route profile exceeds {ROUTE_PROFILE_MAX_BYTES} bytes"),
            Self::InvalidJson(error) => write!(f, "route profile JSON is invalid: {error}"),
            Self::UnsupportedVersion(version) => {
                write!(f, "route profile version {version} is unsupported")
            }
            Self::CandidateCount => write!(
                f,
                "route profile must contain 1..={ROUTE_PROFILE_MAX_CANDIDATES} candidates"
            ),
            Self::InvalidCandidateId(id) => write!(f, "route candidate ID {id:?} is invalid"),
            Self::DuplicateCandidateId(id) => {
                write!(f, "route profile repeats candidate ID {id:?}")
            }
            Self::InvalidProvider {
                candidate_id,
                provider,
            } => write!(
                f,
                "route candidate {candidate_id:?} uses unsupported provider {provider:?}"
            ),
            Self::InvalidModel { candidate_id } => {
                write!(
                    f,
                    "route candidate {candidate_id:?} has an invalid model ID"
                )
            }
            Self::InvalidContextCapacity { candidate_id } => write!(
                f,
                "route candidate {candidate_id:?} context capacity must be between 1 and 1000000000 tokens"
            ),
            Self::InvalidMinimumThroughput => write!(
                f,
                "minimum measured output throughput must be between 1 and 1000000000 milli-tokens/sec"
            ),
            Self::InvalidMaximumCost => write!(
                f,
                "per-turn cost ceiling must be between 0 and {MAX_ROUTE_COST_MICROUSD} micro-USD"
            ),
            Self::InvalidCandidatePricing { candidate_id } => write!(
                f,
                "route candidate {candidate_id:?} must provide both input and output prices, each no greater than {MAX_ROUTE_COST_MICROUSD} micro-USD per million tokens"
            ),
            Self::PromptAddendumTooLarge { candidate_id } => write!(
                f,
                "route candidate {candidate_id:?} prompt addendum exceeds {ROUTE_PROMPT_ADDENDUM_MAX_BYTES} bytes"
            ),
            Self::InvalidPromptProfile { candidate_id } => write!(
                f,
                "route candidate {candidate_id:?} has invalid prompt-profile provenance"
            ),
            Self::InvalidProfileMetadata => {
                write!(
                    f,
                    "route profile identity metadata is incomplete or invalid"
                )
            }
            Self::DuplicatePreference(id) => {
                write!(f, "route preference order repeats candidate ID {id:?}")
            }
            Self::UnknownPreference(id) => {
                write!(
                    f,
                    "route preference order names unknown candidate ID {id:?}"
                )
            }
            Self::CandidateConfiguration { candidate_id } => write!(
                f,
                "route candidate {candidate_id:?} has no valid provider configuration"
            ),
            Self::LocalityMismatch { candidate_id } => write!(
                f,
                "route candidate {candidate_id:?} declares local data handling but its configured endpoint is not loopback"
            ),
            Self::CombinedPromptTooLarge { candidate_id } => write!(
                f,
                "route candidate {candidate_id:?} combined system prompt exceeds {MAX_SYSTEM_PROMPT_BYTES} bytes"
            ),
            Self::InvalidTaskFitPolicy(reason) => {
                write!(f, "route task-fit policy is invalid: {reason}")
            }
        }
    }
}

impl std::error::Error for RouteProfileError {}

impl RouteProfileDocument {
    /// Parse the bounded route-profile format. Unknown fields are rejected so
    /// credentials cannot be smuggled into this non-secret configuration file.
    pub fn parse(bytes: &[u8]) -> Result<Self, RouteProfileError> {
        if bytes.len() > ROUTE_PROFILE_MAX_BYTES {
            return Err(RouteProfileError::TooLarge);
        }
        let profile: Self = serde_json::from_slice(bytes)
            .map_err(|error| RouteProfileError::InvalidJson(error.to_string()))?;
        profile.validate()?;
        Ok(profile)
    }

    pub fn validate(&self) -> Result<(), RouteProfileError> {
        if self.version != 1 {
            return Err(RouteProfileError::UnsupportedVersion(self.version));
        }
        if self.candidates.is_empty() || self.candidates.len() > ROUTE_PROFILE_MAX_CANDIDATES {
            return Err(RouteProfileError::CandidateCount);
        }
        if self
            .min_effective_output_tokens_per_second_milli
            .is_some_and(|minimum| !(1..=1_000_000_000).contains(&minimum))
        {
            return Err(RouteProfileError::InvalidMinimumThroughput);
        }
        if self
            .max_turn_cost_microusd
            .is_some_and(|limit| limit > MAX_ROUTE_COST_MICROUSD)
        {
            return Err(RouteProfileError::InvalidMaximumCost);
        }
        if let Some(policy) = &self.task_fit_policy {
            policy
                .validate()
                .map_err(|error| RouteProfileError::InvalidTaskFitPolicy(error.to_string()))?;
        }

        let mut ids = BTreeSet::new();
        for candidate in &self.candidates {
            let valid_id = !candidate.id.is_empty()
                && candidate.id.len() <= 64
                && candidate
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                && candidate.id.as_bytes()[0].is_ascii_lowercase();
            if !valid_id {
                return Err(RouteProfileError::InvalidCandidateId(candidate.id.clone()));
            }
            if !ids.insert(candidate.id.as_str()) {
                return Err(RouteProfileError::DuplicateCandidateId(
                    candidate.id.clone(),
                ));
            }
            if parse_provider_id(&candidate.provider).is_none() {
                return Err(RouteProfileError::InvalidProvider {
                    candidate_id: candidate.id.clone(),
                    provider: candidate.provider.clone(),
                });
            }
            if candidate.model.trim().is_empty()
                || candidate.model.trim() != candidate.model
                || candidate.model.len() > 256
                || candidate.model.chars().any(char::is_control)
            {
                return Err(RouteProfileError::InvalidModel {
                    candidate_id: candidate.id.clone(),
                });
            }
            if candidate
                .context_capacity_tokens
                .is_some_and(|capacity| !(1..=1_000_000_000).contains(&capacity))
            {
                return Err(RouteProfileError::InvalidContextCapacity {
                    candidate_id: candidate.id.clone(),
                });
            }
            let pricing_is_valid = match (
                candidate.input_cost_microusd_per_million_tokens,
                candidate.output_cost_microusd_per_million_tokens,
            ) {
                (None, None) => true,
                (Some(input), Some(output)) => {
                    input <= MAX_ROUTE_COST_MICROUSD && output <= MAX_ROUTE_COST_MICROUSD
                }
                _ => false,
            };
            if !pricing_is_valid {
                return Err(RouteProfileError::InvalidCandidatePricing {
                    candidate_id: candidate.id.clone(),
                });
            }
            if candidate.prompt_addendum.len() > ROUTE_PROMPT_ADDENDUM_MAX_BYTES {
                return Err(RouteProfileError::PromptAddendumTooLarge {
                    candidate_id: candidate.id.clone(),
                });
            }
            if let Some(profile) = &candidate.prompt_profile {
                if !valid_profile_reference(profile) {
                    return Err(RouteProfileError::InvalidPromptProfile {
                        candidate_id: candidate.id.clone(),
                    });
                }
            }
        }

        match (
            self.profile_id.as_deref(),
            self.profile_version,
            self.profile_hash.as_deref(),
        ) {
            (None, None, None) => {}
            (Some(id), Some(version), Some(hash))
                if valid_profile_id(id) && version > 0 && valid_sha256(hash) => {}
            _ => return Err(RouteProfileError::InvalidProfileMetadata),
        }

        let mut preferences = BTreeSet::new();
        for id in &self.preference_order {
            if !preferences.insert(id.as_str()) {
                return Err(RouteProfileError::DuplicatePreference(id.clone()));
            }
            if !ids.contains(id.as_str()) {
                return Err(RouteProfileError::UnknownPreference(id.clone()));
            }
        }
        Ok(())
    }

    pub fn requirements(&self) -> RouteRequirements {
        let data_policy = match self.data_policy {
            RouteProfileDataPolicy::LocalOnly => DataPolicy::LocalOnly,
            RouteProfileDataPolicy::AllowHosted => {
                DataPolicy::Allow(BTreeSet::from([DataLocation::Local, DataLocation::Hosted]))
            }
        };
        RouteRequirements {
            data_policy,
            max_cost_microusd: self.max_turn_cost_microusd,
            preference_order: self.preference_order.clone(),
            require_context_fit: self.strict_context_fit,
            min_tokens_per_second_milli: self
                .min_effective_output_tokens_per_second_milli
                .or_else(|| self.prefer_fastest_measured.then_some(1)),
            allow_unknown_throughput_warmup: self.allow_preference_order_warmup,
            require_task_fit: self.task_fit_policy.is_some(),
            ..Default::default()
        }
    }

    pub fn cost_budget_for_candidate(&self, candidate_id: &str) -> Option<RouteCostBudget> {
        let max_turn_cost_microusd = self.max_turn_cost_microusd?;
        self.cost_budget_for_candidate_with_limit(candidate_id, max_turn_cost_microusd)
    }

    /// Build a request budget using a stricter caller-provided ceiling while
    /// retaining the route candidate's operator-entered price rates.
    pub fn cost_budget_for_candidate_with_limit(
        &self,
        candidate_id: &str,
        max_turn_cost_microusd: u64,
    ) -> Option<RouteCostBudget> {
        let candidate = self
            .candidates
            .iter()
            .find(|candidate| candidate.id == candidate_id)?;
        Some(RouteCostBudget {
            max_turn_cost_microusd,
            input_cost_microusd_per_million_tokens: candidate
                .input_cost_microusd_per_million_tokens?,
            output_cost_microusd_per_million_tokens: candidate
                .output_cost_microusd_per_million_tokens?,
        })
    }

    /// Lower the profile's per-turn cost ceiling without changing any saved
    /// route identity fields. Used by a trusted critic launcher to impose a
    /// per-reviewer share of an aggregate estimated round budget.
    pub fn lower_max_turn_cost_ceiling(
        &mut self,
        ceiling_microusd: u64,
    ) -> Result<(), RouteProfileError> {
        if ceiling_microusd > MAX_ROUTE_COST_MICROUSD {
            return Err(RouteProfileError::InvalidMaximumCost);
        }
        self.max_turn_cost_microusd = Some(
            self.max_turn_cost_microusd
                .map_or(ceiling_microusd, |configured| {
                    configured.min(ceiling_microusd)
                }),
        );
        Ok(())
    }

    pub fn uses_throughput_routing(&self) -> bool {
        self.prefer_fastest_measured || self.min_effective_output_tokens_per_second_milli.is_some()
    }

    pub fn uses_task_fit_routing(&self) -> bool {
        self.task_fit_policy.is_some()
    }

    /// Bind each profile entry to its configured provider connection and to
    /// the session's system prompt. A locally declared endpoint must use a
    /// loopback host; arbitrary remote hosts cannot be mislabeled local.
    pub fn bind_candidates(
        &self,
        session_system_prompt: &str,
        mut resolve_config: impl FnMut(Provider, &str, String) -> Result<Config, String>,
    ) -> Result<Vec<BoundRouteCandidate>, RouteProfileError> {
        self.validate()?;
        self.candidates
            .iter()
            .map(|entry| {
                let provider = parse_provider_id(&entry.provider).ok_or_else(|| {
                    RouteProfileError::InvalidProvider {
                        candidate_id: entry.id.clone(),
                        provider: entry.provider.clone(),
                    }
                })?;
                let prompt = if entry.prompt_addendum.trim().is_empty() {
                    session_system_prompt.to_owned()
                } else {
                    format!(
                        "{session_system_prompt}\n\n## Target-specific route instructions\n\n{}",
                        entry.prompt_addendum
                    )
                };
                if prompt.len() > MAX_SYSTEM_PROMPT_BYTES {
                    return Err(RouteProfileError::CombinedPromptTooLarge {
                        candidate_id: entry.id.clone(),
                    });
                }
                let location = match entry.data_location {
                    RouteProfileLocation::Local => DataLocation::Local,
                    RouteProfileLocation::Hosted => DataLocation::Hosted,
                };
                let config = match resolve_config(provider, &entry.model, prompt) {
                    Ok(config) => config,
                    Err(_) => {
                        return Ok(BoundRouteCandidate::unavailable(RouteCandidate {
                            id: entry.id.clone(),
                            provider,
                            model: entry.model.clone(),
                            available: Evidence::Known {
                                value: false,
                                source: EvidenceSource::OperatorConfig,
                            },
                            data_location: Evidence::Known {
                                value: location,
                                source: EvidenceSource::OperatorConfig,
                            },
                            max_cost_microusd: Evidence::Unknown,
                            max_seconds: Evidence::Unknown,
                            context_tokens: context_capacity_evidence(
                                entry.context_capacity_tokens,
                            ),
                            input_context_upper_bound_tokens: Evidence::Unknown,
                            tokens_per_second_milli: Evidence::Unknown,
                            tools: Evidence::Unknown,
                        }));
                    }
                };
                if matches!(entry.data_location, RouteProfileLocation::Local)
                    && !is_loopback_endpoint(&config.base_url)
                {
                    return Err(RouteProfileError::LocalityMismatch {
                        candidate_id: entry.id.clone(),
                    });
                }
                let candidate = RouteCandidate {
                    id: entry.id.clone(),
                    provider,
                    model: entry.model.clone(),
                    available: Evidence::Known {
                        value: true,
                        source: EvidenceSource::OperatorConfig,
                    },
                    data_location: Evidence::Known {
                        value: location,
                        source: EvidenceSource::OperatorConfig,
                    },
                    max_cost_microusd: Evidence::Unknown,
                    max_seconds: Evidence::Unknown,
                    context_tokens: context_capacity_evidence(entry.context_capacity_tokens),
                    input_context_upper_bound_tokens: Evidence::Unknown,
                    tokens_per_second_milli: Evidence::Unknown,
                    tools: Evidence::Unknown,
                };
                BoundRouteCandidate::new(candidate, config).map_err(|_| {
                    RouteProfileError::CandidateConfiguration {
                        candidate_id: entry.id.clone(),
                    }
                })
            })
            .collect()
    }
}

fn valid_profile_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.as_bytes()[0].is_ascii_lowercase()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !id.ends_with('-')
        && !id.contains("--")
}

fn valid_sha256(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_profile_reference(profile: &RoutePromptProfileRef) -> bool {
    valid_profile_id(&profile.id) && profile.version > 0 && valid_sha256(&profile.prompt_hash)
}

fn parse_provider_id(value: &str) -> Option<Provider> {
    match value.trim().to_ascii_lowercase().as_str() {
        "anthropic" => Some(Provider::Anthropic),
        "openai" | "openai-compat" => Some(Provider::OpenAi),
        "databricks" => Some(Provider::Databricks),
        "databricks-v2" | "databricks_v2" => Some(Provider::DatabricksV2),
        "openrouter" => Some(Provider::OpenRouter),
        "deepseek" => Some(Provider::DeepSeek),
        _ => None,
    }
}

pub(crate) fn is_loopback_endpoint(base_url: &str) -> bool {
    use std::net::{Ipv4Addr, Ipv6Addr};
    use url::Host;

    let Ok(url) = url::Url::parse(base_url) else {
        return false;
    };
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateDisposition {
    Chosen,
    Eligible,
    Excluded { reasons: Vec<String> },
    NotEvaluated { reason: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct CandidatePreview {
    pub id: String,
    pub provider: Provider,
    pub model: String,
    pub disposition: CandidateDisposition,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteDecision {
    Chosen { candidate_id: String },
    Abstain { reason: String },
}

/// A preview result. It is descriptive only and contains no provider payload.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutePreview {
    pub decision: RouteDecision,
    pub candidates: Vec<CandidatePreview>,
}

/// Candidate evidence bound to the provider configuration that can execute it.
/// The configuration is intentionally not formatted or returned because it
/// contains credentials.
#[derive(Clone)]
pub struct BoundRouteCandidate {
    pub candidate: RouteCandidate,
    config: Option<Config>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteBindingError {
    ProviderMismatch,
    ModelIdBlank,
}

impl BoundRouteCandidate {
    /// Bind untrusted route metadata to provider configuration without exposing
    /// credentials. The model may differ from `config.model` so an explicitly
    /// selected catalog model can use the provider's configured connection.
    pub fn new(candidate: RouteCandidate, config: Config) -> Result<Self, RouteBindingError> {
        if candidate.provider != config.provider {
            return Err(RouteBindingError::ProviderMismatch);
        }
        if candidate.model.trim().is_empty() {
            return Err(RouteBindingError::ModelIdBlank);
        }
        Ok(Self {
            candidate,
            config: Some(config),
        })
    }

    /// Keep an unconfigured candidate visible in previews while ensuring it
    /// can never be dispatched.
    pub fn unavailable(candidate: RouteCandidate) -> Self {
        Self {
            candidate,
            config: None,
        }
    }

    pub fn config(&self) -> Option<&Config> {
        self.config.as_ref()
    }

    pub fn data_location(&self) -> Option<DataLocation> {
        self.candidate.data_location.value().copied()
    }
}

/// Result of attempting exactly one preview-selected provider route.
#[allow(clippy::large_enum_variant)] // preserves a direct, allocation-free provider result
pub enum RouteExecution {
    /// No provider call was made because the route preview abstained.
    Abstained { preview: RoutePreview },
    /// A candidate was selected and its result is reported here. No alternate
    /// provider is tried after local setup or provider errors.
    Selected {
        preview: RoutePreview,
        candidate_id: String,
        result: Result<LlmResponse, AgentError>,
    },
}

/// A preflight result ready for the ACP run loop. `Abstained` contains the
/// candidate-by-candidate explanation and guarantees no provider call occurred.
#[allow(clippy::large_enum_variant)] // selection keeps its validated candidate by value
pub enum BoundRouteSelection {
    Chosen {
        selected: BoundRouteCandidate,
        preview: RoutePreview,
    },
    Abstained {
        preview: RoutePreview,
    },
}

/// Bind a versioned profile, apply its hard gates and configured order, then
/// return exactly one executable candidate or an explicit abstention.
pub fn select_profile_route(
    profile: &RouteProfileDocument,
    session_system_prompt: &str,
    resolve_config: impl FnMut(Provider, &str, String) -> Result<Config, String>,
) -> Result<BoundRouteSelection, RouteProfileError> {
    let candidates = profile.bind_candidates(session_system_prompt, resolve_config)?;
    select_bound_profile_candidates(profile, candidates, Default::default())
}

/// Bind and select against the exact incoming text prompt, current session
/// history, system prompt, and available tools. Existing profiles stay on the
/// legacy path unless strict context fit was explicitly enabled.
pub fn select_profile_route_for_prompt(
    profile: &RouteProfileDocument,
    session_system_prompt: &str,
    incoming_prompt: &[ContentBlock],
    history: &[HistoryItem],
    tools_for_candidate: impl Fn(&Config) -> Vec<ToolDef>,
    resolve_config: impl FnMut(Provider, &str, String) -> Result<Config, String>,
) -> Result<BoundRouteSelection, RouteProfileError> {
    select_profile_route_for_prompt_with_throughput(
        profile,
        session_system_prompt,
        incoming_prompt,
        history,
        tools_for_candidate,
        |_, _, _| Evidence::Unknown,
        resolve_config,
    )
}

/// Bind and select against the incoming request, injecting only exact-identity
/// measured throughput evidence supplied by the local caller.
pub fn select_profile_route_for_prompt_with_throughput(
    profile: &RouteProfileDocument,
    session_system_prompt: &str,
    incoming_prompt: &[ContentBlock],
    history: &[HistoryItem],
    tools_for_candidate: impl Fn(&Config) -> Vec<ToolDef>,
    measured_throughput: impl FnMut(&RouteProfileCandidate, &Config, u64) -> Evidence<u64>,
    resolve_config: impl FnMut(Provider, &str, String) -> Result<Config, String>,
) -> Result<BoundRouteSelection, RouteProfileError> {
    select_profile_route_for_prompt_with_task_fit_evidence(
        profile,
        session_system_prompt,
        incoming_prompt,
        history,
        tools_for_candidate,
        measured_throughput,
        |_, _, _| TaskFitEligibility::Unknown(TaskFitUnknownReason::MissingReport),
        resolve_config,
    )
}

/// Bind and select candidates with local throughput and task-fit evidence.
///
/// A task-fit profile must receive a decision produced from a validated report,
/// verified local review, and the exact active route identity. The default
/// throughput-only entry point supplies `Unknown` and therefore abstains on a
/// profile that enables task-fit routing.
#[allow(clippy::too_many_arguments)] // public routing seam keeps independent evidence sources explicit
pub fn select_profile_route_for_prompt_with_task_fit_evidence(
    profile: &RouteProfileDocument,
    session_system_prompt: &str,
    incoming_prompt: &[ContentBlock],
    history: &[HistoryItem],
    tools_for_candidate: impl Fn(&Config) -> Vec<ToolDef>,
    mut measured_throughput: impl FnMut(&RouteProfileCandidate, &Config, u64) -> Evidence<u64>,
    mut task_fit_evidence: impl FnMut(
        &RouteProfileCandidate,
        &Config,
        &TaskFitEligibilityPolicy,
    ) -> TaskFitEligibility,
    resolve_config: impl FnMut(Provider, &str, String) -> Result<Config, String>,
) -> Result<BoundRouteSelection, RouteProfileError> {
    let needs_request_estimate = profile.strict_context_fit
        || profile.max_turn_cost_microusd.is_some()
        || profile.uses_throughput_routing()
        || profile.uses_task_fit_routing();
    if !needs_request_estimate {
        return select_profile_route(profile, session_system_prompt, resolve_config);
    }
    let mut candidates = profile.bind_candidates(session_system_prompt, resolve_config)?;
    let mut task_fit_by_candidate = std::collections::BTreeMap::new();
    for (entry, bound) in profile.candidates.iter().zip(&mut candidates) {
        let Some(config) = bound.config.as_ref() else {
            if profile.uses_task_fit_routing() {
                task_fit_by_candidate.insert(
                    entry.id.clone(),
                    TaskFitEligibility::Unknown(TaskFitUnknownReason::MissingReport),
                );
            }
            continue;
        };
        if let Some(policy) = profile.task_fit_policy.as_ref() {
            task_fit_by_candidate
                .insert(entry.id.clone(), task_fit_evidence(entry, config, policy));
        }
        let tools = tools_for_candidate(config);
        let estimated_input = estimate_prompt_context_upper_bound(
            &config.system_prompt,
            history,
            incoming_prompt,
            &tools,
            config.max_output_tokens,
        );
        if profile.strict_context_fit || profile.max_turn_cost_microusd.is_some() {
            bound.candidate.input_context_upper_bound_tokens = match &estimated_input {
                Ok(estimate) => Evidence::Known {
                    value: *estimate,
                    source: EvidenceSource::ConservativeEstimate,
                },
                Err(_) => Evidence::Unknown,
            };
        }
        if profile.max_turn_cost_microusd.is_some() {
            bound.candidate.max_cost_microusd = match (
                &estimated_input,
                entry.input_cost_microusd_per_million_tokens,
                entry.output_cost_microusd_per_million_tokens,
            ) {
                (Ok(total_tokens), Some(input_rate), Some(output_rate)) => total_tokens
                    .checked_sub(u64::from(config.max_output_tokens))
                    .and_then(|input_tokens| {
                        estimate_request_cost_with_retry_ceiling_microusd(
                            input_tokens,
                            config.max_output_tokens,
                            input_rate,
                            output_rate,
                        )
                    })
                    .map(|value| Evidence::Known {
                        value,
                        source: EvidenceSource::ConservativeEstimate,
                    })
                    .unwrap_or(Evidence::Unknown),
                _ => Evidence::Unknown,
            };
        }
        if profile.uses_throughput_routing() {
            bound.candidate.tokens_per_second_milli = match estimated_input {
                Ok(estimate) => measured_throughput(entry, config, estimate),
                Err(_) => Evidence::Unknown,
            };
        }
    }
    select_bound_profile_candidates(profile, candidates, task_fit_by_candidate)
}

fn estimate_request_cost_microusd(
    input_tokens: u64,
    output_tokens: u32,
    input_rate_microusd_per_million: u64,
    output_rate_microusd_per_million: u64,
) -> Option<u64> {
    fn rounded_cost(tokens: u64, rate: u64) -> Option<u128> {
        u128::from(tokens)
            .checked_mul(u128::from(rate))?
            .checked_add(MICROUSD_PER_MILLION_TOKENS - 1)
            .map(|cost| cost / MICROUSD_PER_MILLION_TOKENS)
    }

    let total = rounded_cost(input_tokens, input_rate_microusd_per_million)?.checked_add(
        rounded_cost(u64::from(output_tokens), output_rate_microusd_per_million)?,
    )?;
    u64::try_from(total).ok()
}

/// Reserve for every retry attempt because a timed-out or malformed response
/// may still have been processed and billed by the provider.
fn estimate_request_cost_with_retry_ceiling_microusd(
    input_tokens: u64,
    output_tokens: u32,
    input_rate_microusd_per_million_tokens: u64,
    output_rate_microusd_per_million_tokens: u64,
) -> Option<u64> {
    estimate_request_cost_microusd(
        input_tokens,
        output_tokens,
        input_rate_microusd_per_million_tokens,
        output_rate_microusd_per_million_tokens,
    )?
    .checked_mul(u64::from(crate::llm::MAX_RETRIES))
}

fn select_bound_profile_candidates(
    profile: &RouteProfileDocument,
    candidates: Vec<BoundRouteCandidate>,
    task_fit_by_candidate: std::collections::BTreeMap<String, TaskFitEligibility>,
) -> Result<BoundRouteSelection, RouteProfileError> {
    let facts: Vec<_> = candidates
        .iter()
        .map(|candidate| candidate.candidate.clone())
        .collect();
    let mut requirements = profile.requirements();
    requirements.task_fit_by_candidate = task_fit_by_candidate;
    if profile.prefer_fastest_measured {
        let preference_rank = profile
            .preference_order
            .iter()
            .enumerate()
            .map(|(index, id)| (id.as_str(), index))
            .collect::<std::collections::HashMap<_, _>>();
        let mut ordered = facts.iter().collect::<Vec<_>>();
        let warm_unknown_first = profile.allow_preference_order_warmup
            && ordered
                .iter()
                .any(|candidate| candidate.tokens_per_second_milli.value().is_none());
        ordered.sort_by(|left, right| {
            let left_rate = left.tokens_per_second_milli.value().copied();
            let right_rate = right.tokens_per_second_milli.value().copied();
            if warm_unknown_first {
                match (left_rate, right_rate) {
                    (None, Some(_)) => return std::cmp::Ordering::Less,
                    (Some(_), None) => return std::cmp::Ordering::Greater,
                    (None, None) => {
                        return preference_rank
                            .get(left.id.as_str())
                            .copied()
                            .unwrap_or(usize::MAX)
                            .cmp(
                                &preference_rank
                                    .get(right.id.as_str())
                                    .copied()
                                    .unwrap_or(usize::MAX),
                            )
                            .then_with(|| left.id.cmp(&right.id));
                    }
                    (Some(_), Some(_)) => {}
                }
            }
            right_rate
                .cmp(&left_rate)
                .then_with(|| {
                    preference_rank
                        .get(left.id.as_str())
                        .copied()
                        .unwrap_or(usize::MAX)
                        .cmp(
                            &preference_rank
                                .get(right.id.as_str())
                                .copied()
                                .unwrap_or(usize::MAX),
                        )
                })
                .then_with(|| left.id.cmp(&right.id))
        });
        requirements.preference_order = ordered
            .into_iter()
            .map(|candidate| candidate.id.clone())
            .collect();
    }
    let preview = preview_route(&requirements, &facts);
    match &preview.decision {
        RouteDecision::Chosen { candidate_id } => {
            let selected = candidates
                .into_iter()
                .find(|candidate| candidate.candidate.id == *candidate_id);
            match selected {
                Some(selected) if selected.config.is_some() => {
                    Ok(BoundRouteSelection::Chosen { selected, preview })
                }
                _ => Ok(BoundRouteSelection::Abstained {
                    preview: RoutePreview {
                        decision: RouteDecision::Abstain {
                            reason: "selected candidate has no bound provider configuration".into(),
                        },
                        candidates: preview.candidates,
                    },
                }),
            }
        }
        RouteDecision::Abstain { reason } => {
            if profile.strict_context_fit
                && reason == "no candidate satisfies the hard requirements"
            {
                let context_failures = preview.candidates.iter().all(|candidate| {
                    matches!(
                        &candidate.disposition,
                        CandidateDisposition::Excluded { reasons }
                            if reasons.iter().any(|reason| reason.contains("context fit"))
                    )
                });
                if context_failures {
                    let unknown = preview.candidates.iter().all(|candidate| {
                        matches!(
                            &candidate.disposition,
                            CandidateDisposition::Excluded { reasons }
                                if reasons.iter().any(|reason| reason.contains("context fit unavailable"))
                        )
                    });
                    return Ok(BoundRouteSelection::Abstained {
                        preview: RoutePreview {
                            decision: RouteDecision::Abstain {
                                reason: if unknown {
                                    "strict context fit is unavailable".into()
                                } else {
                                    "no candidate satisfies strict context fit".into()
                                },
                            },
                            candidates: preview.candidates,
                        },
                    });
                }
            }
            Ok(BoundRouteSelection::Abstained { preview })
        }
    }
}

/// Evaluate candidates, then issue one provider request for the selected
/// candidate. All eligibility facts are supplied by the caller and must come
/// from the relevant adapter/configuration source; this function does not
/// discover or verify them. It does not fall back or re-route on errors.
pub async fn complete_routed(
    requirements: &RouteRequirements,
    candidates: &[BoundRouteCandidate],
    history: &[HistoryItem],
    tools: &[ToolDef],
) -> RouteExecution {
    let evidence: Vec<_> = candidates
        .iter()
        .map(|candidate| candidate.candidate.clone())
        .collect();
    let preview = preview_route(requirements, &evidence);
    let candidate_id = match &preview.decision {
        RouteDecision::Chosen { candidate_id } => candidate_id.clone(),
        RouteDecision::Abstain { .. } => return RouteExecution::Abstained { preview },
    };
    let Some(selected) = candidates
        .iter()
        .find(|candidate| candidate.candidate.id == candidate_id)
    else {
        // This is unreachable for a preview produced from `evidence`; fail
        // closed if the selection contract changes in a future refactor.
        return RouteExecution::Abstained { preview };
    };

    let Some(config) = selected.config.as_ref() else {
        return RouteExecution::Abstained {
            preview: RoutePreview {
                decision: RouteDecision::Abstain {
                    reason: "selected candidate has no bound provider configuration".into(),
                },
                candidates: preview.candidates,
            },
        };
    };
    let result = match Llm::new_for_route(
        config,
        selected.data_location() == Some(DataLocation::Local),
    ) {
        Ok(llm) => {
            llm.complete(
                config,
                &config.system_prompt,
                history,
                tools,
                &selected.candidate.model,
            )
            .await
        }
        Err(error) => Err(error),
    };
    RouteExecution::Selected {
        preview,
        candidate_id,
        result,
    }
}

/// Evaluate supplied candidate evidence against hard requirements.
///
/// Selection is made only for a single eligible candidate or an explicit
/// preference. Multiple eligible choices without a preference abstain because
/// this selector admits or excludes candidates but does not rank model quality.
pub fn preview_route(
    requirements: &RouteRequirements,
    candidates: &[RouteCandidate],
) -> RoutePreview {
    if requirements.safety_refusal {
        return RoutePreview {
            decision: RouteDecision::Abstain {
                reason: "safety refusal is terminal".into(),
            },
            candidates: candidates
                .iter()
                .map(|candidate| CandidatePreview {
                    id: candidate.id.clone(),
                    provider: candidate.provider,
                    model: candidate.model.clone(),
                    disposition: CandidateDisposition::NotEvaluated {
                        reason: "safety refusal is terminal".into(),
                    },
                })
                .collect(),
        };
    }

    let mut seen_ids = BTreeSet::new();
    let duplicate_ids: BTreeSet<_> = candidates
        .iter()
        .filter_map(|candidate| {
            (!seen_ids.insert(candidate.id.as_str())).then_some(candidate.id.as_str())
        })
        .collect();
    let mut eligible_ids = Vec::new();
    let mut previews: Vec<_> = candidates
        .iter()
        .map(|candidate| {
            let reasons = exclusion_reasons(requirements, candidate);
            let mut reasons = reasons;
            if candidate.id.trim().is_empty() {
                reasons.push("candidate id is blank".into());
            }
            if candidate.model.trim().is_empty() {
                reasons.push("model id is blank".into());
            }
            if duplicate_ids.contains(candidate.id.as_str()) {
                reasons.push("candidate id is not unique".into());
            }
            let disposition = if reasons.is_empty() {
                eligible_ids.push(candidate.id.as_str());
                CandidateDisposition::Eligible
            } else {
                CandidateDisposition::Excluded { reasons }
            };
            CandidatePreview {
                id: candidate.id.clone(),
                provider: candidate.provider,
                model: candidate.model.clone(),
                disposition,
            }
        })
        .collect();

    if requirements.preferred_candidate_id.is_some() && !requirements.preference_order.is_empty() {
        return RoutePreview {
            decision: RouteDecision::Abstain {
                reason: "route configuration has both a single preference and a preference order"
                    .into(),
            },
            candidates: previews,
        };
    }

    let selected = if !requirements.preference_order.is_empty() {
        let mut seen_preferences = BTreeSet::new();
        for preferred in &requirements.preference_order {
            if !seen_preferences.insert(preferred.as_str()) {
                return RoutePreview {
                    decision: RouteDecision::Abstain {
                        reason: "route preference order contains a duplicate candidate ID".into(),
                    },
                    candidates: previews,
                };
            }
            if !candidates
                .iter()
                .any(|candidate| candidate.id == *preferred)
            {
                return RoutePreview {
                    decision: RouteDecision::Abstain {
                        reason: format!("route preference names unknown candidate {preferred:?}"),
                    },
                    candidates: previews,
                };
            }
        }
        requirements
            .preference_order
            .iter()
            .find(|preferred| eligible_ids.contains(&preferred.as_str()))
            .map(String::as_str)
    } else {
        match requirements.preferred_candidate_id.as_deref() {
            Some(preferred) if eligible_ids.contains(&preferred) => Some(preferred),
            Some(preferred) => {
                let reason = if candidates.iter().any(|candidate| candidate.id == preferred) {
                    "preferred candidate is ineligible"
                } else {
                    "preferred candidate is unknown"
                };
                return RoutePreview {
                    decision: RouteDecision::Abstain {
                        reason: reason.into(),
                    },
                    candidates: previews,
                };
            }
            None if eligible_ids.len() == 1 => Some(eligible_ids[0]),
            None => None,
        }
    };

    if let Some(id) = selected {
        if let Some(preview) = previews.iter_mut().find(|candidate| candidate.id == id) {
            preview.disposition = CandidateDisposition::Chosen;
        }
        return RoutePreview {
            decision: RouteDecision::Chosen {
                candidate_id: id.to_string(),
            },
            candidates: previews,
        };
    }

    let reason = if eligible_ids.is_empty() {
        "no candidate satisfies the hard requirements"
    } else {
        "multiple candidates are eligible and no preference order was supplied"
    };
    RoutePreview {
        decision: RouteDecision::Abstain {
            reason: reason.into(),
        },
        candidates: previews,
    }
}

fn exclusion_reasons(requirements: &RouteRequirements, candidate: &RouteCandidate) -> Vec<String> {
    let mut reasons = Vec::new();
    match candidate.available.value() {
        Some(true) => {}
        Some(false) => reasons.push("candidate is unavailable".into()),
        None => reasons.push("availability is unknown".into()),
    }

    let allowed = match &requirements.data_policy {
        DataPolicy::LocalOnly => [DataLocation::Local].into_iter().collect(),
        DataPolicy::Allow(locations) => locations.clone(),
    };
    match candidate.data_location.value() {
        Some(location) if allowed.contains(location) => {}
        Some(DataLocation::Hosted) if requirements.data_policy == DataPolicy::LocalOnly => {
            reasons.push("hosted data location violates local-only policy".into())
        }
        Some(_) => reasons.push("data location is not allowed by policy".into()),
        None => reasons.push("data location is unknown".into()),
    }

    check_limit(
        requirements.max_cost_microusd,
        candidate.max_cost_microusd.value(),
        |candidate_cost, limit| candidate_cost <= limit,
        "estimated cost exceeds the configured ceiling",
        "estimated cost is unknown",
        &mut reasons,
    );
    check_limit(
        requirements.max_seconds,
        candidate.max_seconds.value(),
        |candidate_seconds, limit| candidate_seconds <= limit,
        "time estimate exceeds the configured ceiling",
        "time estimate is unknown",
        &mut reasons,
    );
    check_limit(
        requirements.min_context_tokens,
        candidate.context_tokens.value(),
        |capacity, required| capacity >= required,
        "context capacity is below the required minimum",
        "context capacity is unknown",
        &mut reasons,
    );
    if requirements.require_context_fit {
        match (
            candidate.context_tokens.value(),
            candidate.input_context_upper_bound_tokens.value(),
        ) {
            (Some(capacity), Some(required)) if capacity >= required => {}
            (Some(capacity), Some(required)) => reasons.push(format!(
                "context fit failed: conservative UTF-8 upper-bound estimate {required} tokens exceeds operator-declared capacity {capacity} tokens"
            )),
            (None, _) => reasons.push(
                "context fit unavailable: operator-declared candidate capacity is unknown".into(),
            ),
            (_, None) => reasons.push(
                "context fit unavailable: conservative UTF-8 upper-bound estimate is unknown"
                    .into(),
            ),
        }
    }
    if let Some(minimum) = requirements.min_tokens_per_second_milli {
        match candidate.tokens_per_second_milli.value() {
            Some(throughput) if *throughput >= minimum => {}
            Some(_) => reasons.push("measured throughput is below the configured minimum".into()),
            None if requirements.allow_unknown_throughput_warmup => {}
            None => reasons.push("measured throughput is unknown".into()),
        }
    }
    if !requirements.required_tools.is_empty() {
        match candidate.tools.value() {
            Some(tools) => {
                let missing: Vec<_> = requirements.required_tools.difference(tools).collect();
                if !missing.is_empty() {
                    reasons.push(format!(
                        "required tools are unsupported: {}",
                        missing.into_iter().cloned().collect::<Vec<_>>().join(", ")
                    ));
                }
            }
            None => reasons.push("required-tool support is unknown".into()),
        }
    }
    if requirements.require_task_fit {
        match requirements.task_fit_by_candidate.get(&candidate.id) {
            Some(TaskFitEligibility::Qualified) => {}
            Some(TaskFitEligibility::Unknown(reason)) => reasons.push(match reason {
                TaskFitUnknownReason::InvalidPolicy => "task-fit policy is invalid".into(),
                TaskFitUnknownReason::InvalidRouteIdentity => {
                    "task-fit route identity is invalid".into()
                }
                TaskFitUnknownReason::MissingReport => {
                    "task-fit evidence is unknown: report is missing".into()
                }
                TaskFitUnknownReason::EvidenceStoreUnavailable => {
                    "task-fit evidence is unknown: local evidence store or reviewer identity is unavailable".into()
                }
                TaskFitUnknownReason::MissingLocalReview => {
                    "task-fit evidence is unknown: local review is missing".into()
                }
                TaskFitUnknownReason::StaleReport => {
                    "task-fit evidence is unknown: report is stale".into()
                }
                TaskFitUnknownReason::FutureReport => {
                    "task-fit evidence is unknown: report timestamp is in the future".into()
                }
                TaskFitUnknownReason::InsufficientCases => {
                    "task-fit evidence is unknown: too few distinct cases".into()
                }
                TaskFitUnknownReason::ModelIdentityUnobserved => {
                    "task-fit evidence is unknown: exact model identity was not observed".into()
                }
                TaskFitUnknownReason::DifferentTaskClass => {
                    "task-fit evidence is unknown: report covers a different task class".into()
                }
            }),
            Some(TaskFitEligibility::Rejected(reason)) => reasons.push(match reason {
                TaskFitRejectedReason::BindingMismatch => {
                    "task-fit evidence rejected: profile or candidate binding mismatch".into()
                }
                TaskFitRejectedReason::CandidateIdentityMismatch => {
                    "task-fit evidence rejected: candidate configuration mismatch".into()
                }
                TaskFitRejectedReason::BelowQualityFloor => {
                    "task-fit evidence rejected: measured quality is below the configured floor"
                        .into()
                }
            }),
            None => reasons.push("task-fit evidence is unknown: no decision was supplied".into()),
        }
    }
    reasons
}

fn check_limit<T>(
    limit: Option<T>,
    value: Option<&T>,
    within: impl FnOnce(&T, &T) -> bool,
    exceeded: &str,
    unknown: &str,
    reasons: &mut Vec<String>,
) {
    if let Some(limit) = limit {
        match value {
            Some(value) if within(value, &limit) => {}
            Some(_) => reasons.push(exceeded.into()),
            None => reasons.push(unknown.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known<T>(value: T) -> Evidence<T> {
        Evidence::Known {
            value,
            source: EvidenceSource::Measured,
        }
    }

    fn candidate(id: &str, location: DataLocation) -> RouteCandidate {
        RouteCandidate {
            id: id.into(),
            provider: Provider::OpenAi,
            model: "test-model".into(),
            available: known(true),
            data_location: known(location),
            max_cost_microusd: known(0),
            max_seconds: known(20),
            context_tokens: known(8_000),
            input_context_upper_bound_tokens: Evidence::Unknown,
            tokens_per_second_milli: known(25_000),
            tools: known(BTreeSet::from(["shell".into()])),
        }
    }

    fn profile_json() -> serde_json::Value {
        serde_json::json!({
            "version": 1,
            "preference_order": ["local-fast", "cloud-strong"],
            "candidates": [
                {
                    "id": "local-fast",
                    "provider": "openai",
                    "model": "qwen-local",
                    "data_location": "local",
                    "prompt_addendum": "Prefer concise code edits."
                },
                {
                    "id": "cloud-strong",
                    "provider": "deepseek",
                    "model": "deepseek-chat",
                    "data_location": "hosted"
                }
            ]
        })
    }

    fn test_config(provider: Provider, model: &str, prompt: String) -> Config {
        let endpoint = match provider {
            Provider::OpenAi => "http://127.0.0.1:8000/v1",
            Provider::DeepSeek => "https://api.deepseek.com",
            _ => "https://example.invalid/v1",
        };
        let mut config = Config::for_discovery(provider, "test-key".into(), endpoint.into(), None);
        config.model = model.into();
        config.system_prompt = prompt;
        config
    }

    fn select_test_profile_with_rates(
        profile: &RouteProfileDocument,
        rates: &std::collections::HashMap<String, u64>,
    ) -> Result<BoundRouteSelection, RouteProfileError> {
        select_profile_route_for_prompt_with_throughput(
            profile,
            "base instructions",
            &[ContentBlock::Text {
                text: "small request".into(),
            }],
            &[],
            |_| Vec::new(),
            |entry, _, _| {
                rates
                    .get(&entry.id)
                    .copied()
                    .map(|value| Evidence::Known {
                        value,
                        source: EvidenceSource::Measured,
                    })
                    .unwrap_or(Evidence::Unknown)
            },
            |provider, model, prompt| Ok(test_config(provider, model, prompt)),
        )
    }

    fn select_cost_profile(
        maximum_cost_microusd: u64,
        input_rate_microusd: Option<u64>,
        output_rate_microusd: Option<u64>,
    ) -> Result<BoundRouteSelection, RouteProfileError> {
        let mut profile_json = serde_json::json!({
            "version": 1,
            "data_policy": "local-only",
            "preference_order": ["local"],
            "max_turn_cost_microusd": maximum_cost_microusd,
            "candidates": [{
                "id": "local",
                "provider": "openai",
                "model": "test-model",
                "data_location": "local"
            }]
        });
        let candidate = profile_json["candidates"][0]
            .as_object_mut()
            .expect("candidate object");
        if let Some(rate) = input_rate_microusd {
            candidate.insert(
                "input_cost_microusd_per_million_tokens".into(),
                serde_json::json!(rate),
            );
        }
        if let Some(rate) = output_rate_microusd {
            candidate.insert(
                "output_cost_microusd_per_million_tokens".into(),
                serde_json::json!(rate),
            );
        }
        let profile = RouteProfileDocument::parse(profile_json.to_string().as_bytes())?;
        select_profile_route_for_prompt(
            &profile,
            "base",
            &[ContentBlock::Text {
                text: "small request".into(),
            }],
            &[],
            |_| Vec::new(),
            |provider, model, prompt| {
                let mut config = test_config(provider, model, prompt);
                config.max_output_tokens = 10;
                Ok(config)
            },
        )
    }

    #[test]
    fn estimated_cost_rounds_each_token_component_up() {
        assert_eq!(estimate_request_cost_microusd(1, 1, 1_000_001, 1), Some(3));
        assert_eq!(estimate_request_cost_microusd(0, 0, 0, 0), Some(0));
    }

    #[test]
    fn turn_cost_budget_reserves_each_call_before_dispatch() {
        let budget = RouteCostBudget {
            max_turn_cost_microusd: 100,
            input_cost_microusd_per_million_tokens: 1_000_000,
            output_cost_microusd_per_million_tokens: 1_000_000,
        };
        assert_eq!(budget.reserve_call(0, 40).unwrap(), 40);
        assert_eq!(budget.reserve_call(40, 60).unwrap(), 100);
        assert!(budget
            .reserve_call(40, 61)
            .unwrap_err()
            .contains("only 60 micro-USD remains"));
    }

    #[test]
    fn per_turn_cost_gate_uses_prompt_bound_and_output_limit() {
        let input_bound = estimate_prompt_context_upper_bound(
            "base",
            &[],
            &[ContentBlock::Text {
                text: "small request".into(),
            }],
            &[],
            10,
        )
        .unwrap()
        .checked_sub(10)
        .unwrap();
        let single_attempt = estimate_request_cost_microusd(input_bound, 10, 1_000_000, 2_000_000)
            .expect("bounded request cost");
        let expected = estimate_request_cost_with_retry_ceiling_microusd(
            input_bound,
            10,
            1_000_000,
            2_000_000,
        )
        .expect("bounded retry cost");
        assert_eq!(
            expected,
            single_attempt * u64::from(crate::llm::MAX_RETRIES)
        );
        let budget = RouteCostBudget {
            max_turn_cost_microusd: expected,
            input_cost_microusd_per_million_tokens: 1_000_000,
            output_cost_microusd_per_million_tokens: 2_000_000,
        };
        assert_eq!(
            budget.estimate_call_microusd(
                "base",
                &[HistoryItem::User("small request".into())],
                &[],
                10,
            ),
            Some(expected),
            "the turn-boundary estimate matches route preflight for the same prompt"
        );

        let at_limit = select_cost_profile(expected, Some(1_000_000), Some(2_000_000)).unwrap();
        assert!(matches!(
            at_limit,
            BoundRouteSelection::Chosen { selected, .. } if selected.candidate.id == "local"
        ));

        let below_limit =
            select_cost_profile(expected.saturating_sub(1), Some(1_000_000), Some(2_000_000))
                .unwrap();
        assert!(matches!(
            below_limit,
            BoundRouteSelection::Abstained { preview }
                if matches!(&preview.candidates[0].disposition,
                    CandidateDisposition::Excluded { reasons }
                        if reasons.iter().any(|reason| reason.contains("estimated cost exceeds")))
        ));
    }

    #[test]
    fn per_request_cost_gate_abstains_for_missing_or_incomplete_rates() {
        let result = select_cost_profile(1_000, None, None).unwrap();
        assert!(matches!(
            result,
            BoundRouteSelection::Abstained { preview }
                if matches!(&preview.candidates[0].disposition,
                    CandidateDisposition::Excluded { reasons }
                        if reasons.iter().any(|reason| reason == "estimated cost is unknown"))
        ));
        assert!(matches!(
            select_cost_profile(1_000, Some(1_000_000), None),
            Err(RouteProfileError::InvalidCandidatePricing { candidate_id })
                if candidate_id == "local"
        ));
    }

    #[test]
    fn route_profile_defaults_local_only_and_rejects_unknown_secret_fields() {
        let mut json = profile_json();
        json.as_object_mut()
            .expect("object")
            .remove("preference_order");
        let profile = RouteProfileDocument::parse(json.to_string().as_bytes()).unwrap();
        assert_eq!(profile.data_policy, RouteProfileDataPolicy::LocalOnly);
        assert!(profile.requirements().data_policy == DataPolicy::LocalOnly);
        assert!(!profile.strict_context_fit);
        assert!(!profile.prefer_fastest_measured);
        assert_eq!(profile.min_effective_output_tokens_per_second_milli, None);
        assert_eq!(profile.max_turn_cost_microusd, None);
        assert!(!profile.allow_preference_order_warmup);
        assert_eq!(profile.task_fit_policy, None);
        assert!(!profile.requirements().require_task_fit);

        let mut json = profile_json();
        json["candidates"][0]["api_key"] = serde_json::json!("must-not-be-stored");
        assert!(matches!(
            RouteProfileDocument::parse(json.to_string().as_bytes()),
            Err(RouteProfileError::InvalidJson(_))
        ));
    }

    #[test]
    fn measured_throughput_floor_skips_slow_candidate_and_unknown_fails_closed() {
        let mut json = profile_json();
        json["data_policy"] = serde_json::json!("allow-hosted");
        json["min_effective_output_tokens_per_second_milli"] = serde_json::json!(50_000);
        let profile = RouteProfileDocument::parse(json.to_string().as_bytes()).unwrap();
        let rates = std::collections::HashMap::from([
            ("local-fast".into(), 20_000),
            ("cloud-strong".into(), 60_000),
        ]);
        let selected = select_test_profile_with_rates(&profile, &rates).unwrap();
        let BoundRouteSelection::Chosen { selected, .. } = selected else {
            panic!("the faster candidate clears the floor");
        };
        assert_eq!(selected.candidate.id, "cloud-strong");

        let unknown = select_test_profile_with_rates(&profile, &Default::default()).unwrap();
        assert!(matches!(unknown, BoundRouteSelection::Abstained { .. }));
    }

    #[test]
    fn fastest_measured_selection_ranks_rates_and_explicit_warmup_uses_priority_order() {
        let mut json = profile_json();
        json["data_policy"] = serde_json::json!("allow-hosted");
        json["prefer_fastest_measured"] = serde_json::json!(true);
        let profile = RouteProfileDocument::parse(json.to_string().as_bytes()).unwrap();
        let rates = std::collections::HashMap::from([
            ("local-fast".into(), 20_000),
            ("cloud-strong".into(), 60_000),
        ]);
        let selected = select_test_profile_with_rates(&profile, &rates).unwrap();
        let BoundRouteSelection::Chosen { selected, .. } = selected else {
            panic!("known measured rates choose one candidate");
        };
        assert_eq!(selected.candidate.id, "cloud-strong");

        json["min_effective_output_tokens_per_second_milli"] = serde_json::json!(50_000);
        json["allow_preference_order_warmup"] = serde_json::json!(true);
        let warmup_profile = RouteProfileDocument::parse(json.to_string().as_bytes()).unwrap();
        let warmup = select_test_profile_with_rates(&warmup_profile, &Default::default()).unwrap();
        let BoundRouteSelection::Chosen { selected, .. } = warmup else {
            panic!("explicit warm-up should follow the saved preference order");
        };
        assert_eq!(selected.candidate.id, "local-fast");

        let partial = select_test_profile_with_rates(
            &warmup_profile,
            &std::collections::HashMap::from([("cloud-strong".into(), 60_000)]),
        )
        .unwrap();
        let BoundRouteSelection::Chosen { selected, .. } = partial else {
            panic!("unknown candidate should warm before measured candidates");
        };
        assert_eq!(selected.candidate.id, "local-fast");

        let all_measured = select_test_profile_with_rates(
            &warmup_profile,
            &std::collections::HashMap::from([
                ("local-fast".into(), 55_000),
                ("cloud-strong".into(), 60_000),
            ]),
        )
        .unwrap();
        let BoundRouteSelection::Chosen { selected, .. } = all_measured else {
            panic!("measured candidates should rank by speed after warm-up");
        };
        assert_eq!(selected.candidate.id, "cloud-strong");

        let below_floor = select_test_profile_with_rates(
            &warmup_profile,
            &std::collections::HashMap::from([("local-fast".into(), 40_000)]),
        )
        .unwrap();
        let BoundRouteSelection::Chosen { selected, preview } = below_floor else {
            panic!("unknown candidate should remain eligible during explicit warm-up");
        };
        assert_eq!(selected.candidate.id, "cloud-strong");
        assert!(matches!(
            &preview.candidates[0].disposition,
            CandidateDisposition::Excluded { reasons }
                if reasons.iter().any(|reason| reason.contains("below the configured minimum"))
        ));
    }

    #[test]
    fn warmup_with_only_a_throughput_floor_keeps_configured_preference_order() {
        let mut json = profile_json();
        json["data_policy"] = serde_json::json!("allow-hosted");
        json["preference_order"] = serde_json::json!(["cloud-strong", "local-fast"]);
        json["min_effective_output_tokens_per_second_milli"] = serde_json::json!(50_000);
        json["allow_preference_order_warmup"] = serde_json::json!(true);
        let profile = RouteProfileDocument::parse(json.to_string().as_bytes()).unwrap();
        let selection = select_test_profile_with_rates(
            &profile,
            &std::collections::HashMap::from([("local-fast".into(), 60_000)]),
        )
        .unwrap();
        let BoundRouteSelection::Chosen { selected, .. } = selection else {
            panic!("warm-up should admit unknown evidence");
        };
        assert_eq!(selected.candidate.id, "cloud-strong");
    }

    #[test]
    fn route_profile_rejects_invalid_measured_throughput_floor() {
        for minimum in [0, 1_000_000_001] {
            let mut json = profile_json();
            json["min_effective_output_tokens_per_second_milli"] = serde_json::json!(minimum);
            assert!(matches!(
                RouteProfileDocument::parse(json.to_string().as_bytes()),
                Err(RouteProfileError::InvalidMinimumThroughput)
            ));
        }
    }

    #[test]
    fn route_profile_rejects_invalid_task_fit_policy() {
        let mut json = profile_json();
        json["task_fit_policy"] = serde_json::json!({
            "taskClass": "coding",
            "taskClassTaxonomyVersion": "operator-defined-v1",
            "evaluationPolicyVersion": "task-fit-outcomes-v1",
            "minimumDistinctTasks": 20,
            "minimumWilsonLowerBound95": 1.1,
            "maximumAgeSeconds": 604800,
            "requireObservedModelIdentity": true
        });
        assert!(matches!(
            RouteProfileDocument::parse(json.to_string().as_bytes()),
            Err(RouteProfileError::InvalidTaskFitPolicy(_))
        ));
    }

    #[test]
    fn route_profile_rejects_invalid_cost_ceilings_and_partial_prices() {
        let mut json = profile_json();
        json["max_turn_cost_microusd"] = serde_json::json!(MAX_ROUTE_COST_MICROUSD + 1);
        assert!(matches!(
            RouteProfileDocument::parse(json.to_string().as_bytes()),
            Err(RouteProfileError::InvalidMaximumCost)
        ));

        let mut json = profile_json();
        json["candidates"][0]["input_cost_microusd_per_million_tokens"] =
            serde_json::json!(1_000_000);
        assert!(matches!(
            RouteProfileDocument::parse(json.to_string().as_bytes()),
            Err(RouteProfileError::InvalidCandidatePricing { candidate_id })
                if candidate_id == "local-fast"
        ));

        let mut json = profile_json();
        json["candidates"][0]["input_cost_microusd_per_million_tokens"] =
            serde_json::json!(1_000_000_000_001_u64);
        json["candidates"][0]["output_cost_microusd_per_million_tokens"] =
            serde_json::json!(1_000_000);
        assert!(matches!(
            RouteProfileDocument::parse(json.to_string().as_bytes()),
            Err(RouteProfileError::InvalidCandidatePricing { candidate_id })
                if candidate_id == "local-fast"
        ));
    }

    #[test]
    fn route_profile_rejects_stale_order_ids_and_duplicate_candidate_ids() {
        let mut json = profile_json();
        json["preference_order"] = serde_json::json!(["missing"]);
        assert!(matches!(
            RouteProfileDocument::parse(json.to_string().as_bytes()),
            Err(RouteProfileError::UnknownPreference(id)) if id == "missing"
        ));

        let mut json = profile_json();
        json["candidates"][1]["id"] = serde_json::json!("local-fast");
        assert!(matches!(
            RouteProfileDocument::parse(json.to_string().as_bytes()),
            Err(RouteProfileError::DuplicateCandidateId(id)) if id == "local-fast"
        ));

        let mut json = profile_json();
        json["preference_order"] = serde_json::json!(["local-fast", "local-fast"]);
        assert!(matches!(
            RouteProfileDocument::parse(json.to_string().as_bytes()),
            Err(RouteProfileError::DuplicatePreference(id)) if id == "local-fast"
        ));
    }

    #[test]
    fn route_profile_rejects_unsupported_version_bad_model_and_too_many_candidates() {
        let mut json = profile_json();
        json["version"] = serde_json::json!(2);
        assert!(matches!(
            RouteProfileDocument::parse(json.to_string().as_bytes()),
            Err(RouteProfileError::UnsupportedVersion(2))
        ));

        let mut json = profile_json();
        json["candidates"][0]["model"] = serde_json::json!(" model-with-padding ");
        assert!(matches!(
            RouteProfileDocument::parse(json.to_string().as_bytes()),
            Err(RouteProfileError::InvalidModel { candidate_id }) if candidate_id == "local-fast"
        ));

        let candidates: Vec<_> = (0..=ROUTE_PROFILE_MAX_CANDIDATES)
            .map(|index| {
                serde_json::json!({
                    "id": format!("route-{index}"),
                    "provider": "openai",
                    "model": "model",
                    "data_location": "local"
                })
            })
            .collect();
        let json = serde_json::json!({ "version": 1, "candidates": candidates });
        assert!(matches!(
            RouteProfileDocument::parse(json.to_string().as_bytes()),
            Err(RouteProfileError::CandidateCount)
        ));
    }

    #[test]
    fn route_profile_binding_revalidates_programmatically_built_documents() {
        let mut profile =
            RouteProfileDocument::parse(profile_json().to_string().as_bytes()).unwrap();
        profile.candidates[0].provider = "unknown-provider".into();

        let result = profile.bind_candidates("base", |_, _, _| unreachable!());

        assert!(matches!(
            result,
            Err(RouteProfileError::InvalidProvider { .. })
        ));
    }

    #[test]
    fn route_profile_enforces_size_candidate_and_prompt_caps() {
        assert!(matches!(
            RouteProfileDocument::parse(&vec![b' '; ROUTE_PROFILE_MAX_BYTES + 1]),
            Err(RouteProfileError::TooLarge)
        ));

        let mut json = profile_json();
        json["candidates"][0]["prompt_addendum"] =
            serde_json::json!("x".repeat(ROUTE_PROMPT_ADDENDUM_MAX_BYTES + 1));
        assert!(matches!(
            RouteProfileDocument::parse(json.to_string().as_bytes()),
            Err(RouteProfileError::PromptAddendumTooLarge { candidate_id })
                if candidate_id == "local-fast"
        ));
    }

    #[test]
    fn route_profile_binds_target_prompts_and_loopback_locality() {
        let profile = RouteProfileDocument::parse(profile_json().to_string().as_bytes()).unwrap();
        let bound = profile
            .bind_candidates("base persona prompt", |provider, model, prompt| {
                Ok(test_config(provider, model, prompt))
            })
            .unwrap();
        assert_eq!(bound[0].candidate.provider, Provider::OpenAi);
        assert_eq!(bound[0].candidate.model, "qwen-local");
        assert!(bound[0]
            .config()
            .unwrap()
            .system_prompt
            .contains("Prefer concise code edits."));
        assert_eq!(bound[1].candidate.provider, Provider::DeepSeek);
        assert_eq!(
            bound[1].config().unwrap().system_prompt,
            "base persona prompt"
        );

        let mut json = profile_json();
        json["candidates"][0]["provider"] = serde_json::json!("deepseek");
        json["candidates"][0]["model"] = serde_json::json!("deepseek-chat");
        let profile = RouteProfileDocument::parse(json.to_string().as_bytes()).unwrap();
        assert!(matches!(
            profile.bind_candidates("base", |provider, model, prompt| {
                Ok(test_config(provider, model, prompt))
            }),
            Err(RouteProfileError::LocalityMismatch { candidate_id })
                if candidate_id == "local-fast"
        ));
    }

    #[test]
    fn profile_route_uses_local_default_and_hosted_only_after_explicit_opt_in() {
        let mut profile =
            RouteProfileDocument::parse(profile_json().to_string().as_bytes()).unwrap();
        profile.preference_order = vec!["cloud-strong".into(), "local-fast".into()];
        let selected = select_profile_route(&profile, "base", |provider, model, prompt| {
            Ok(test_config(provider, model, prompt))
        })
        .unwrap();
        assert!(matches!(
            selected,
            BoundRouteSelection::Chosen { selected, .. }
                if selected.candidate.id == "local-fast"
        ));

        profile.data_policy = RouteProfileDataPolicy::AllowHosted;
        let selected = select_profile_route(&profile, "base", |provider, model, prompt| {
            Ok(test_config(provider, model, prompt))
        })
        .unwrap();
        assert!(matches!(
            selected,
            BoundRouteSelection::Chosen { selected, .. }
                if selected.candidate.id == "cloud-strong"
        ));
    }

    #[test]
    fn strict_profile_selects_only_when_declared_capacity_covers_conservative_bound() {
        let mut json = profile_json();
        json["strict_context_fit"] = serde_json::json!(true);
        json["candidates"][0]["context_capacity_tokens"] = serde_json::json!(1_000_000);
        let profile = RouteProfileDocument::parse(json.to_string().as_bytes()).unwrap();
        let history = vec![HistoryItem::User("prior session text".into())];
        let prompt = vec![ContentBlock::Text {
            text: "incoming task".into(),
        }];
        let tools = vec![ToolDef {
            name: "shell".into(),
            description: "Run a command".into(),
            input_schema: serde_json::json!({"type":"object","properties":{"cmd":{"type":"string"}}}),
        }];

        let selected = select_profile_route_for_prompt(
            &profile,
            "base system instructions",
            &prompt,
            &history,
            |_| tools.clone(),
            |provider, model, prompt| Ok(test_config(provider, model, prompt)),
        )
        .unwrap();

        let BoundRouteSelection::Chosen { selected, .. } = selected else {
            panic!("declared capacity should cover this request");
        };
        assert_eq!(
            selected.candidate.context_tokens,
            Evidence::Known {
                value: 1_000_000,
                source: EvidenceSource::OperatorConfig,
            }
        );
        let fit = selected.candidate.context_fit_summary().unwrap();
        assert_eq!(
            fit.estimate_method,
            ContextEstimateMethod::Utf8BytesPlusFramingAndOutputReserveV1
        );
        assert!(fit.input_tokens_upper_bound > 13);
        assert!(fit.input_tokens_upper_bound <= fit.capacity_tokens);
    }

    #[test]
    fn strict_profile_abstains_for_unknown_capacity_or_unbounded_input() {
        let mut json = profile_json();
        json["strict_context_fit"] = serde_json::json!(true);
        let profile = RouteProfileDocument::parse(json.to_string().as_bytes()).unwrap();
        let history = Vec::new();
        let tools = Vec::new();
        let prompt = vec![ContentBlock::Text {
            text: "hello".into(),
        }];

        let unknown_capacity = select_profile_route_for_prompt(
            &profile,
            "system",
            &prompt,
            &history,
            |_| tools.clone(),
            |provider, model, prompt| Ok(test_config(provider, model, prompt)),
        )
        .unwrap();
        assert!(matches!(
            unknown_capacity,
            BoundRouteSelection::Abstained { preview }
                if preview.decision == RouteDecision::Abstain {
                    reason: "strict context fit is unavailable".into()
                }
        ));

        let mut json = profile_json();
        json["strict_context_fit"] = serde_json::json!(true);
        json["candidates"][0]["context_capacity_tokens"] = serde_json::json!(1_000_000);
        let profile = RouteProfileDocument::parse(json.to_string().as_bytes()).unwrap();
        let unsupported = select_profile_route_for_prompt(
            &profile,
            "system",
            &[ContentBlock::Unsupported],
            &history,
            |_| tools.clone(),
            |provider, model, prompt| Ok(test_config(provider, model, prompt)),
        )
        .unwrap();
        assert!(matches!(
            unsupported,
            BoundRouteSelection::Abstained { preview }
                if preview.decision == RouteDecision::Abstain {
                    reason: "strict context fit is unavailable".into()
                }
        ));
    }

    #[test]
    fn strict_context_request_guard_covers_history_tools_output_and_rejects_multimodal() {
        let history = vec![HistoryItem::User("abc".into())];
        let tools = vec![ToolDef {
            name: "tool".into(),
            description: "description".into(),
            input_schema: serde_json::json!({"type":"object"}),
        }];
        let fit = check_context_fit(10_000, "system", &history, &tools, 128).unwrap();
        assert!(fit.input_tokens_upper_bound >= 256 + 3 + 6 + 11 + 128);
        assert!(matches!(
            check_context_fit(1, "system", &history, &tools, 128),
            Err(ContextFitCheckError::CapacityExceeded { .. })
        ));

        let multimodal_history = vec![HistoryItem::ToolResult(crate::types::ToolResult {
            provider_id: "call-1".into(),
            content: vec![ToolResultContent::Image {
                data: "base64".into(),
                mime_type: "image/png".into(),
            }],
            is_error: false,
        })];
        assert!(matches!(
            check_context_fit(10_000, "system", &multimodal_history, &tools, 128),
            Err(ContextFitCheckError::EstimateUnavailable(
                ContextEstimateError::MultimodalHistory
            ))
        ));
    }

    #[test]
    fn prompt_estimate_counts_borrowed_utf8_blocks_and_resource_links() {
        let incoming = [
            ContentBlock::Text { text: "é".into() },
            ContentBlock::ResourceLink { uri: "x".into() },
        ];
        let estimate = estimate_prompt_context_upper_bound("", &[], &incoming, &[], 5).unwrap();

        // UTF-8 bytes: "é" (2), join newline (1), and "[resource: x]" (13).
        // Framing: one system and one incoming user item, plus output reserve.
        assert_eq!(estimate, 256 + 2 + 1 + 13 + 256 + 5);
    }

    #[test]
    fn profile_route_skips_unconfigured_candidate_without_network_fallback() {
        let mut profile =
            RouteProfileDocument::parse(profile_json().to_string().as_bytes()).unwrap();
        profile.data_policy = RouteProfileDataPolicy::AllowHosted;
        profile.preference_order = vec!["cloud-strong".into(), "local-fast".into()];
        let selected = select_profile_route(&profile, "base", |provider, model, prompt| {
            if provider == Provider::DeepSeek {
                Err("missing secret should not escape into the preview".into())
            } else {
                Ok(test_config(provider, model, prompt))
            }
        })
        .unwrap();
        assert!(matches!(
            selected,
            BoundRouteSelection::Chosen { selected, preview }
                if selected.candidate.id == "local-fast"
                    && matches!(preview.candidates[1].disposition,
                        CandidateDisposition::Excluded { .. })
        ));
    }

    #[test]
    fn local_only_selects_sole_eligible_and_explains_hosted_exclusion() {
        let result = preview_route(
            &RouteRequirements::default(),
            &[
                candidate("local", DataLocation::Local),
                candidate("hosted", DataLocation::Hosted),
            ],
        );
        assert_eq!(
            result.decision,
            RouteDecision::Chosen {
                candidate_id: "local".into()
            }
        );
        assert_eq!(
            result.candidates[0].disposition,
            CandidateDisposition::Chosen
        );
        assert!(matches!(
            &result.candidates[1].disposition,
            CandidateDisposition::Excluded { reasons }
                if reasons.iter().any(|reason| reason.contains("local-only"))
        ));
    }

    #[test]
    fn task_fit_is_a_hard_gate_and_unknown_never_wins() {
        let mut requirements = RouteRequirements {
            require_task_fit: true,
            preference_order: vec!["unknown".into(), "failed".into(), "qualified".into()],
            ..Default::default()
        };
        requirements.task_fit_by_candidate.insert(
            "failed".into(),
            TaskFitEligibility::Rejected(TaskFitRejectedReason::BelowQualityFloor),
        );
        requirements
            .task_fit_by_candidate
            .insert("qualified".into(), TaskFitEligibility::Qualified);

        let result = preview_route(
            &requirements,
            &[
                candidate("unknown", DataLocation::Local),
                candidate("failed", DataLocation::Local),
                candidate("qualified", DataLocation::Local),
            ],
        );

        assert_eq!(
            result.decision,
            RouteDecision::Chosen {
                candidate_id: "qualified".into()
            }
        );
        assert!(matches!(
            &result.candidates[0].disposition,
            CandidateDisposition::Excluded { reasons }
                if reasons.iter().any(|reason| reason.contains("no decision was supplied"))
        ));
        assert!(matches!(
            &result.candidates[1].disposition,
            CandidateDisposition::Excluded { reasons }
                if reasons.iter().any(|reason| reason.contains("below the configured floor"))
        ));
        assert_eq!(
            result.candidates[2].disposition,
            CandidateDisposition::Chosen
        );
    }

    #[test]
    fn enabled_task_fit_profile_uses_exact_evidence_callback_and_fails_closed_by_default() {
        let policy = serde_json::json!({
            "taskClass": "coding",
            "taskClassTaxonomyVersion": "operator-defined-v1",
            "evaluationPolicyVersion": "task-fit-outcomes-v1",
            "minimumDistinctTasks": 20,
            "minimumWilsonLowerBound95": 0.8,
            "maximumAgeSeconds": 604800,
            "requireObservedModelIdentity": true
        });
        let mut profile_json = profile_json();
        profile_json["task_fit_policy"] = policy;
        let profile = RouteProfileDocument::parse(profile_json.to_string().as_bytes()).unwrap();
        assert!(profile.requirements().require_task_fit);

        let evidence_driven = select_profile_route_for_prompt_with_task_fit_evidence(
            &profile,
            "base instructions",
            &[ContentBlock::Text {
                text: "small request".into(),
            }],
            &[],
            |_| Vec::new(),
            |_, _, _| Evidence::Unknown,
            |candidate, _, policy| {
                assert_eq!(policy.task_class, "coding");
                if candidate.id == "local-fast" {
                    TaskFitEligibility::Qualified
                } else {
                    TaskFitEligibility::Unknown(TaskFitUnknownReason::MissingReport)
                }
            },
            |provider, model, prompt| Ok(test_config(provider, model, prompt)),
        )
        .unwrap();
        assert!(matches!(
            evidence_driven,
            BoundRouteSelection::Chosen { selected, .. }
                if selected.candidate.id == "local-fast"
        ));

        let without_evidence =
            select_test_profile_with_rates(&profile, &Default::default()).unwrap();
        assert!(matches!(
            without_evidence,
            BoundRouteSelection::Abstained { preview }
                if matches!(&preview.candidates[0].disposition,
                    CandidateDisposition::Excluded { reasons }
                        if reasons.iter().any(|reason| reason.contains("report is missing")))
        ));
    }

    #[test]
    fn unknown_hard_limit_evidence_excludes_candidate() {
        let mut candidate = candidate("local", DataLocation::Local);
        candidate.context_tokens = Evidence::Unknown;
        let result = preview_route(
            &RouteRequirements {
                min_context_tokens: Some(4_000),
                ..Default::default()
            },
            &[candidate],
        );
        assert!(matches!(result.decision, RouteDecision::Abstain { .. }));
        assert!(matches!(
            &result.candidates[0].disposition,
            CandidateDisposition::Excluded { reasons }
                if reasons.iter().any(|reason| reason.contains("context capacity is unknown"))
        ));
    }

    #[test]
    fn blank_model_id_is_ineligible() {
        let mut candidate = candidate("local", DataLocation::Local);
        candidate.model = "  ".into();
        let result = preview_route(&RouteRequirements::default(), &[candidate]);
        assert!(matches!(result.decision, RouteDecision::Abstain { .. }));
        assert!(matches!(
            &result.candidates[0].disposition,
            CandidateDisposition::Excluded { reasons }
                if reasons.iter().any(|reason| reason == "model id is blank")
        ));
    }

    #[test]
    fn all_resource_and_tool_limits_are_hard_gates() {
        let result = preview_route(
            &RouteRequirements {
                max_cost_microusd: Some(0),
                max_seconds: Some(10),
                min_context_tokens: Some(9_000),
                min_tokens_per_second_milli: Some(30_000),
                required_tools: BTreeSet::from(["filesystem".into()]),
                ..Default::default()
            },
            &[candidate("local", DataLocation::Local)],
        );
        let CandidateDisposition::Excluded { reasons } = &result.candidates[0].disposition else {
            panic!("candidate should be excluded");
        };
        assert_eq!(reasons.len(), 4);
    }

    #[test]
    fn multiple_eligible_candidates_abstain_without_quality_ranking() {
        let result = preview_route(
            &RouteRequirements::default(),
            &[
                candidate("local-a", DataLocation::Local),
                candidate("local-b", DataLocation::Local),
            ],
        );
        assert!(matches!(result.decision, RouteDecision::Abstain { .. }));
        assert!(result
            .candidates
            .iter()
            .all(|candidate| candidate.disposition == CandidateDisposition::Eligible));
    }

    #[test]
    fn preference_order_selects_first_eligible_candidate_not_input_order() {
        let result = preview_route(
            &RouteRequirements {
                preference_order: vec!["second".into(), "first".into()],
                ..Default::default()
            },
            &[
                candidate("first", DataLocation::Local),
                candidate("second", DataLocation::Local),
            ],
        );
        assert_eq!(
            result.decision,
            RouteDecision::Chosen {
                candidate_id: "second".into()
            }
        );
    }

    #[test]
    fn preference_order_skips_candidates_excluded_by_hard_gates() {
        let result = preview_route(
            &RouteRequirements {
                data_policy: DataPolicy::Allow(BTreeSet::from([DataLocation::Local])),
                preference_order: vec!["hosted".into(), "local".into()],
                ..Default::default()
            },
            &[
                candidate("hosted", DataLocation::Hosted),
                candidate("local", DataLocation::Local),
            ],
        );
        assert_eq!(
            result.decision,
            RouteDecision::Chosen {
                candidate_id: "local".into()
            }
        );
    }

    #[test]
    fn malformed_preference_order_abstains() {
        for preference_order in [vec!["local".into(), "local".into()], vec!["missing".into()]] {
            let result = preview_route(
                &RouteRequirements {
                    preference_order,
                    ..Default::default()
                },
                &[candidate("local", DataLocation::Local)],
            );
            assert!(matches!(result.decision, RouteDecision::Abstain { .. }));
        }
    }

    #[test]
    fn explicit_preference_cannot_override_exclusions() {
        let result = preview_route(
            &RouteRequirements {
                preferred_candidate_id: Some("hosted".into()),
                ..Default::default()
            },
            &[candidate("hosted", DataLocation::Hosted)],
        );
        assert!(matches!(result.decision, RouteDecision::Abstain { .. }));
        assert!(matches!(
            result.candidates[0].disposition,
            CandidateDisposition::Excluded { .. }
        ));
    }

    #[test]
    fn duplicate_ids_cannot_create_an_ambiguous_selection() {
        let result = preview_route(
            &RouteRequirements::default(),
            &[
                candidate("duplicate", DataLocation::Local),
                candidate("duplicate", DataLocation::Local),
            ],
        );
        assert!(matches!(result.decision, RouteDecision::Abstain { .. }));
        assert!(result.candidates.iter().all(|candidate| matches!(
            &candidate.disposition,
            CandidateDisposition::Excluded { reasons }
                if reasons.iter().any(|reason| reason.contains("not unique"))
        )));
    }

    #[test]
    fn safety_refusal_is_terminal_and_skips_candidate_evaluation() {
        let result = preview_route(
            &RouteRequirements {
                safety_refusal: true,
                ..Default::default()
            },
            &[candidate("local", DataLocation::Local)],
        );
        assert_eq!(
            result.decision,
            RouteDecision::Abstain {
                reason: "safety refusal is terminal".into()
            }
        );
        assert!(matches!(
            result.candidates[0].disposition,
            CandidateDisposition::NotEvaluated { .. }
        ));
    }
}
