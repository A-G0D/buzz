#![forbid(unsafe_code)]
mod agent;
pub mod auth;
mod auth_http;
mod builtin;
pub mod catalog;
pub mod config;
pub mod databricks;
mod databricks_label_grammar;
mod handoff;
mod hints;
mod llm;
mod mcp;
pub mod model_capabilities;
mod permission;
pub mod route_preview;
pub mod task_fit_evidence;
pub mod types;
mod wire;

pub use catalog::{
    discover_databricks_models, discover_databricks_models_with_cache_dir,
    discover_deepseek_models, ModelEntry,
};
pub use config::Provider;
pub use types::AgentError;

/// Hard output cap for the fixed, one-request provider connection probe.
/// Reasoning models may spend tokens before emitting their final text.
pub const SYNTHETIC_PROBE_MAX_OUTPUT_TOKENS: u32 = 256;

/// Environment keys the Windows Git Bash resolver may inspect. `spawn_one()`
/// forwards every key in this list into its otherwise-cleared MCP child; Doctor
/// uses the same contract so a ready agent can always start its shell tool.
#[cfg(windows)]
pub const WINDOWS_SHELL_RESOLUTION_ENV: &[&str] = &[
    "PATH",
    "BUZZ_SHELL",
    "GIT_BASH",
    "SystemRoot",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "LOCALAPPDATA",
];

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::io::BufReader;
use tokio::sync::{mpsc, watch, Mutex};

use crate::agent::{RouteMeasurementIdentity, RunCtx};
use crate::config::{Config, ThinkingEffort, MAX_SYSTEM_PROMPT_BYTES, PROTOCOL_VERSION};
use crate::hints::SkillEntry;
use crate::llm::Llm;
use crate::mcp::McpRegistry;
use crate::route_preview::{
    select_profile_route_for_prompt_with_task_fit_evidence, BoundRouteSelection, Evidence,
    EvidenceSource, RouteCandidate, RouteContextFitSummary, RouteCostBudget, RouteDecision,
    RouteProfileDocument,
};
use crate::task_fit_evidence::{
    TaskFitEvidenceSnapshot, TaskFitRouteProfileIdentity, TaskFitUnknownReason,
};
use crate::types::{ContentBlock, HistoryItem};
use crate::wire::{
    classify, goose_session_update, Inbound, InitializeParams, SessionCancelParams,
    SessionNewParams, SessionPromptParams, SessionSetModelParams, SessionSteerParams, WireMsg,
    WireSender, INVALID_PARAMS, METHOD_NOT_FOUND, PARSE_ERROR,
};

const CRITIC_ROUTE_COST_BUDGET_ENV: &str = "BUZZ_AGENT_ROUTE_COST_BUDGET_MICROUSD";

struct App {
    cfg: Config,
    llm: Arc<Llm>,
    /// When enabled by the trusted local launcher, this ACP process accepts
    /// only one prompt per session and exposes no MCP, skill, or summarizer
    /// tools. Critic rounds use this for text-only review of a frozen input.
    review_only: bool,
    /// Optional non-secret profile that selects one configured API route for
    /// each ACP prompt. It is parsed once and bound to session prompts per run.
    route_profile: Option<RouteProfileDocument>,
    /// Verified local report reviews for the exact pinned task-fit route.
    route_task_fit_evidence: Option<TaskFitEvidenceSnapshot>,
    /// Local history is consulted only for profiles with an explicit
    /// throughput policy. Failure leaves evidence unknown and the policy may
    /// either abstain or use its explicit warm-up fallback.
    route_journal: Option<buzz_run_journal::RunJournal>,
    sessions: Mutex<HashMap<String, Session>>,
    /// ACP protocol version negotiated at `initialize`, stored for the whole
    /// connection lifetime. The `session/request_permission` wire shape derives
    /// from this value — never from a later mutable session field — so a strict
    /// client always receives exactly the shape it negotiated. Defaults to
    /// [`PROTOCOL_VERSION`] before `initialize`; no prompt (and thus no
    /// permission ask) can run before then.
    negotiated_version: AtomicU32,
    /// Owns the entire `session/request_permission` correlation lifecycle:
    /// process-wide admission, id allocation, response delivery, and abort-safe
    /// cleanup. See [`permission::PermissionBroker`].
    permissions: Arc<permission::PermissionBroker>,
    /// Cached model catalog for Databricks providers. Populated lazily on the
    /// first successful `session/new` discovery call. Failed discovery is never
    /// cached: static-token authentication errors reject session creation, while
    /// OAuth authentication and non-auth errors use the configured model for that
    /// response and retry on the next session.
    models_cache: tokio::sync::OnceCell<Vec<ModelEntry>>,
}

struct Session {
    id: String,
    mcp: Arc<McpRegistry>,
    /// Skills discovered at session creation; used by the built-in `load_skill` tool.
    skills: Vec<SkillEntry>,
    history: Vec<HistoryItem>,
    cancel_tx: watch::Sender<bool>,
    busy: bool,
    prompt_count: u32,
    /// Changes whenever prompt-relevant session state changes. Route preflight
    /// snapshots this value and must match it before reserving the turn.
    state_revision: u64,
    /// Run id of the in-flight prompt, set when a prompt starts and cleared
    /// when it ends. `None` means no active run — a steer request targeting
    /// this session is rejected. Steer-capable clients learn this value from
    /// the `params.update._meta.goose.activeRunId` field on `session/update`.
    active_run_id: Option<String>,
    /// Sender for mid-turn steer messages. Created fresh per prompt (like
    /// `cancel_tx`); the running prompt loop holds the matching receiver and
    /// drains queued steers at round boundaries. `None` when no prompt is in
    /// flight.
    steer_tx: Option<mpsc::UnboundedSender<Vec<ContentBlock>>>,
    original_task: Option<String>,
    handoff_count: usize,
    /// Cache-summed input tokens the provider reported for this session's most
    /// recent request, or `None` before the first response (or after a handoff
    /// resets the context). Drives the token-based handoff gate; see
    /// [`RunCtx::should_handoff`].
    last_request_input_tokens: Option<u64>,
    /// History byte size when `last_request_input_tokens` was measured, paired
    /// with it so the gate can account for history appended since.
    last_request_history_bytes: Option<usize>,
    effective_system_prompt: Arc<str>,
    /// Per-session model override set by `session/set_model`. When `Some`,
    /// overrides `App::cfg.model` for all LLM calls on this session. Persists
    /// across `session/prompt` calls until changed.
    effective_model: Option<String>,
    /// Session-cumulative input tokens across all turns. Sent in the
    /// `_goose/unstable/session/update` usage notification so buzz-acp's
    /// `UsageTracker` can compute per-turn deltas symmetrically with goose.
    /// `TurnIOState`: `Unseen` before any turn reports; `Exact(n)` while running;
    /// `Poisoned` if any turn's sum overflowed — permanently poisons the session.
    accumulated_input_tokens: crate::types::TurnIOState,
    /// Session-cumulative output tokens across all turns.
    /// Same `Unseen`/`Exact(n)`/`Poisoned` contract as `accumulated_input_tokens`.
    accumulated_output_tokens: crate::types::TurnIOState,
    /// Session-cumulative cache-served input tokens across all turns — a subset
    /// of `accumulated_input_tokens`, not an addition to it. Tri-state:
    ///
    /// - `Unseen`: no turn has ever reported this category.
    /// - `Exact(n)`: every usage-bearing response in every turn reported this
    ///   category; `n` is the cumulative sum.
    /// - `Unknown`: at least one usage-bearing response ever omitted the
    ///   category — permanently poisoned for this session.
    accumulated_cached_input_tokens: crate::types::CacheTotalState,
    /// Session-cumulative cache-written input tokens across all turns — also a
    /// subset of `accumulated_input_tokens`, not an addition to it.
    /// Same `Unseen`/`Exact`/`Unknown` tri-state contract as
    /// `accumulated_cached_input_tokens`.
    accumulated_cache_write_tokens: crate::types::CacheTotalState,
    /// Session-cumulative total-token state across all turns.
    ///
    /// Mirrors the per-turn `TurnTotalState` tri-state: starts `Unseen`,
    /// becomes `Exact(n)` as turns with genuine provider totals complete,
    /// transitions permanently to `Unknown` when any turn lacks a total or
    /// when the cumulative would otherwise decrease. Only emitted in the
    /// `usage_update` notification when `Exact`.
    accumulated_total_state: crate::types::TurnTotalState,
}

struct SessionPromptSnapshot {
    state_revision: u64,
    id: String,
    mcp: Arc<McpRegistry>,
    skills: Vec<SkillEntry>,
    history: Vec<HistoryItem>,
    original_task: Option<String>,
    handoff_count: usize,
    last_request_input_tokens: Option<u64>,
    last_request_history_bytes: Option<usize>,
    effective_system_prompt: Arc<str>,
    effective_model_override: Option<String>,
    usage_baseline: crate::types::SessionUsageBaseline,
}

struct ReservedSessionPrompt {
    snapshot: SessionPromptSnapshot,
    cancel_rx: watch::Receiver<bool>,
    steer_rx: mpsc::UnboundedReceiver<Vec<ContentBlock>>,
}

fn route_provider_id(provider: Provider) -> &'static str {
    match provider {
        Provider::Anthropic => "anthropic",
        Provider::OpenAi => "openai",
        Provider::Databricks => "databricks",
        Provider::DatabricksV2 => "databricks-v2",
        Provider::OpenRouter => "openrouter",
        Provider::DeepSeek => "deepseek",
    }
}

fn route_thinking_effort(effort: Option<ThinkingEffort>) -> &'static str {
    match effort {
        None => "default",
        Some(ThinkingEffort::None) => "none",
        Some(ThinkingEffort::Minimal) => "minimal",
        Some(ThinkingEffort::Low) => "low",
        Some(ThinkingEffort::Medium) => "medium",
        Some(ThinkingEffort::High) => "high",
        Some(ThinkingEffort::XHigh) => "xhigh",
        Some(ThinkingEffort::Max) => "max",
    }
}

fn route_profile_identity(
    profile: &RouteProfileDocument,
) -> (Option<String>, Option<u32>, Option<String>) {
    let id = profile.profile_id.clone();
    let version = profile.profile_version;
    let saved_hash = profile.profile_hash.clone();
    let resolved_id = std::env::var("BUZZ_ACP_ROUTE_PROFILE_ID").ok();
    let resolved_version = std::env::var("BUZZ_ACP_ROUTE_PROFILE_VERSION")
        .ok()
        .and_then(|value| value.parse::<u32>().ok());
    let resolved_hash = std::env::var("BUZZ_ACP_ROUTE_PROFILE_HASH").ok();
    if resolved_id == id
        && resolved_version == version
        && resolved_hash.as_deref().is_some_and(|hash| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    {
        return (id, version, resolved_hash);
    }
    (id, version, saved_hash)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RouteProfileOverrideError {
    ModelNotListed,
    AmbiguousModel,
}

impl RouteProfileOverrideError {
    const fn reason_code(self) -> &'static str {
        match self {
            Self::ModelNotListed => "manual_override_model_not_listed",
            Self::AmbiguousModel => "manual_override_model_ambiguous",
        }
    }

    const fn message(self) -> &'static str {
        match self {
            Self::ModelNotListed => {
                "session model override is not listed in the active route profile"
            }
            Self::AmbiguousModel => {
                "session model override matches multiple active route profile candidates"
            }
        }
    }
}

fn route_profile_for_model_override(
    profile: &RouteProfileDocument,
    provider_id: &str,
    model_id: &str,
) -> Result<(RouteProfileDocument, String), RouteProfileOverrideError> {
    let mut matching = profile
        .candidates
        .iter()
        .filter(|candidate| candidate.provider == provider_id && candidate.model == model_id);
    let Some(candidate) = matching.next() else {
        return Err(RouteProfileOverrideError::ModelNotListed);
    };
    if matching.next().is_some() {
        return Err(RouteProfileOverrideError::AmbiguousModel);
    }

    let candidate_id = candidate.id.clone();
    let mut constrained = profile.clone();
    constrained
        .candidates
        .retain(|candidate| candidate.id == candidate_id);
    constrained.preference_order = vec![candidate_id.clone()];
    if constrained.prefer_fastest_measured {
        // Explicit selection changes ranking, not eligibility. Keep the
        // measured-throughput floor that fastest-measured routing implies.
        constrained
            .min_effective_output_tokens_per_second_milli
            .get_or_insert(1);
        constrained.prefer_fastest_measured = false;
    }

    Ok((constrained, candidate_id))
}

fn apply_critic_route_cost_budget_override(
    profile: &mut Option<RouteProfileDocument>,
    raw_budget: Option<&str>,
    review_only: bool,
) -> Result<(), String> {
    let Some(raw_budget) = raw_budget else {
        return Ok(());
    };
    if !review_only {
        return Err(format!(
            "{CRITIC_ROUTE_COST_BUDGET_ENV} is reserved for bounded critic reviews"
        ));
    }
    let budget = raw_budget.parse::<u64>().map_err(|_| {
        format!("{CRITIC_ROUTE_COST_BUDGET_ENV} must be an unsigned integer in micro-USD")
    })?;
    let profile = profile
        .as_mut()
        .ok_or_else(|| "critic route cost budget requires a resolved route profile".to_owned())?;
    profile
        .lower_max_turn_cost_ceiling(budget)
        .map_err(|error| format!("invalid critic route cost budget: {error}"))
}

fn route_decision_notice(
    session_id: &str,
    attempt_id: &str,
    profile: &RouteProfileDocument,
    outcome: &str,
    candidate: Option<&RouteCandidate>,
    context_fit: Option<RouteContextFitSummary>,
    reason_code: Option<&str>,
) -> Value {
    let (profile_id, profile_version, profile_hash) = route_profile_identity(profile);
    let mut notice = json!({
        "sessionId": session_id,
        "attemptId": attempt_id,
        "profileId": profile_id,
        "profileVersion": profile_version,
        "profileHash": profile_hash,
        "outcome": outcome,
        "candidateId": candidate.map(|candidate| candidate.id.as_str()),
        "providerId": candidate.map(|candidate| route_provider_id(candidate.provider)),
        "modelId": candidate.map(|candidate| candidate.model.as_str()),
        "reasonCode": reason_code,
    });
    if let Some(context_fit) = context_fit {
        notice["contextFit"] = json!(context_fit);
    }
    notice
}

fn route_abstention_reason(decision: &RouteDecision) -> &'static str {
    let RouteDecision::Abstain { reason } = decision else {
        return "route_abstained";
    };
    match reason.as_str() {
        "safety refusal is terminal" => "safety_refusal",
        "route configuration has both a single preference and a preference order"
        | "route preference order contains a duplicate candidate ID" => {
            "invalid_preference_configuration"
        }
        "preferred candidate is unknown" => "unknown_preference",
        "preferred candidate is ineligible" => "ineligible_preference",
        "no candidate satisfies the hard requirements" => "no_eligible_candidate",
        "no candidate satisfies strict context fit" => "context_capacity_insufficient",
        "strict context fit is unavailable" => "context_fit_unavailable",
        "multiple candidates are eligible and no preference order was supplied" => {
            "multiple_eligible_without_preference"
        }
        value if value.starts_with("route preference names unknown candidate ") => {
            "unknown_preference"
        }
        _ => "route_abstained",
    }
}

fn die(msg: String) -> ! {
    tracing::error!("{msg}");
    std::process::exit(2);
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if matches!(args.get(1).map(String::as_str), Some("synthetic-probe")) {
        return tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(synthetic_probe_cli());
    }
    if matches!(args.get(1).map(String::as_str), Some("auth")) {
        return tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?
            .block_on(auth_subcommand(&args[2..]));
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async_main());
    Ok(())
}

async fn synthetic_probe_cli() -> Result<(), Box<dyn std::error::Error>> {
    const SYSTEM: &str =
        "You are checking a model connection. Do not use tools. Reply exactly with BUZZ_PROBE_OK.";
    const USER: &str = "Reply exactly with BUZZ_PROBE_OK.";
    let local = std::env::var("BUZZ_AGENT_PROBE_LOCAL").as_deref() == Ok("1");
    let receipt = match Config::from_env() {
        Err(_) => json!({
            "status": "failed",
            "provider": null,
            "requestedModel": null,
            "responseMarkerMatched": null,
            "inputTokens": null,
            "outputTokens": null,
            "totalTokens": null,
            "failureClass": "configuration_error"
        }),
        Ok(mut config) => {
            config.system_prompt = SYSTEM.into();
            config.max_output_tokens = SYNTHETIC_PROBE_MAX_OUTPUT_TOKENS;
            config.max_token_recoveries = 0;
            config.llm_timeout = std::time::Duration::from_secs(30);
            let provider = match config.provider {
                Provider::Anthropic => "anthropic",
                Provider::OpenAi => "openai",
                Provider::Databricks => "databricks",
                Provider::DatabricksV2 => "databricks_v2",
                Provider::OpenRouter => "openrouter",
                Provider::DeepSeek => "deepseek",
            };
            let model = config.model.clone();
            let result = if local && !crate::route_preview::is_loopback_endpoint(&config.base_url) {
                Err("local_endpoint_required")
            } else {
                match Llm::new_for_synthetic_probe(&config) {
                    Err(_) => Err("client_setup_error"),
                    Ok(llm) => llm
                        .complete(
                            &config,
                            SYSTEM,
                            &[HistoryItem::User(USER.into())],
                            &[],
                            &config.model,
                        )
                        .await
                        .map_err(|_| "provider_error"),
                }
            };
            match result {
                Err(failure_class) => json!({
                    "status": "failed",
                    "provider": provider,
                    "requestedModel": model,
                    "responseMarkerMatched": null,
                    "inputTokens": null,
                    "outputTokens": null,
                    "totalTokens": null,
                    "failureClass": failure_class
                }),
                Ok(response) => json!({
                    "status": "responded",
                    "provider": provider,
                    "requestedModel": response.request_model.unwrap_or(model),
                    "responseMarkerMatched": response.text.trim().contains("BUZZ_PROBE_OK"),
                    "inputTokens": response.input_tokens,
                    "outputTokens": response.output_tokens,
                    "totalTokens": response.total_tokens,
                    "failureClass": null
                }),
            }
        }
    };
    println!("{}", serde_json::to_string(&receipt)?);
    Ok(())
}

/// Authenticate to Databricks and store credentials under an optional explicit
/// cache root. `None` preserves buzz-agent's production cache location.
pub async fn authenticate_databricks_with_cache_dir(
    host: &str,
    cache_dir: Option<&std::path::Path>,
) -> Result<(), AgentError> {
    auth::PkceOAuthTokenSource::new(llm::databricks_pkce_config(
        host,
        cache_dir.map(std::path::Path::to_path_buf),
    ))?
    .interactive_login()
    .await
}

pub async fn authenticate_databricks(host: &str) -> Result<(), AgentError> {
    authenticate_databricks_with_cache_dir(host, None).await
}

/// `buzz-agent auth <provider>` — run the interactive auth flow for a
/// provider and persist the result, then exit. Today this supports Databricks
/// OAuth 2.0 PKCE. Reads `DATABRICKS_HOST` from env; needs a browser on the
/// machine.
async fn auth_subcommand(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let provider = args.first().map(String::as_str);
    match provider {
        Some("databricks" | "databricks_v2" | "databricks-v2") => {
            let host = std::env::var("DATABRICKS_HOST")
                .map_err(|_| "auth databricks: DATABRICKS_HOST required")?;
            authenticate_databricks(&host).await?;
            eprintln!("Authenticated. Token cached under ~/.config/buzz-agent/oauth/databricks/.");
            Ok(())
        }
        Some(other) => Err(format!("auth: unknown provider {other:?}").into()),
        None => Err("auth: provider required (try: buzz-agent auth databricks)".into()),
    }
}

async fn async_main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    let mut cfg = Config::from_env().unwrap_or_else(|e| die(e));
    let review_only = match std::env::var("BUZZ_AGENT_REVIEW_ONLY") {
        Ok(value) if value == "1" => true,
        Ok(_) => die("config: BUZZ_AGENT_REVIEW_ONLY accepts only the value '1'".into()),
        Err(std::env::VarError::NotPresent) => false,
        Err(std::env::VarError::NotUnicode(_)) => {
            die("config: BUZZ_AGENT_REVIEW_ONLY must be UTF-8".into())
        }
    };
    if review_only {
        // A critic does not inherit skills or invoke the nested status model.
        // Its only context is the explicit system and user text supplied by the
        // trusted round coordinator.
        cfg.summary_model = None;
        cfg.hints_enabled = false;
    }
    let mut route_profile = match std::env::var("BUZZ_AGENT_ROUTE_PROFILE_JSON") {
        Ok(raw) => Some(
            RouteProfileDocument::parse(raw.as_bytes())
                .unwrap_or_else(|error| die(format!("config: {error}"))),
        ),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            die("config: BUZZ_AGENT_ROUTE_PROFILE_JSON must be UTF-8".into())
        }
    };
    let route_cost_budget_override = match std::env::var(CRITIC_ROUTE_COST_BUDGET_ENV) {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => die(format!(
            "config: {CRITIC_ROUTE_COST_BUDGET_ENV} must be UTF-8"
        )),
    };
    apply_critic_route_cost_budget_override(
        &mut route_profile,
        route_cost_budget_override.as_deref(),
        review_only,
    )
    .unwrap_or_else(|error| die(format!("config: {error}")));
    if review_only
        && route_profile.as_ref().is_some_and(|profile| {
            profile.candidates.iter().any(|candidate| {
                candidate.data_location != crate::route_preview::RouteProfileLocation::Local
            })
        })
    {
        die("config: review-only mode accepts only local route candidates".into());
    }
    if review_only && !crate::route_preview::is_loopback_endpoint(&cfg.base_url) {
        die("config: review-only mode requires a loopback default model endpoint".into());
    }
    // Text supplied for a review may contain private project material. Require
    // the default endpoint to be loopback and forbid redirects/proxy routing;
    // route-profile candidates are separately constrained to local targets.
    let llm = Arc::new(
        if review_only {
            Llm::new_for_route(&cfg, true)
        } else {
            Llm::new(&cfg)
        }
        .unwrap_or_else(|e| die(e.to_string())),
    );
    let route_task_fit_evidence = route_profile
        .as_ref()
        .filter(|profile| profile.uses_task_fit_routing())
        .and_then(|profile| {
            let (Some(profile_id), Some(profile_version), Some(profile_hash)) =
                route_profile_identity(profile)
            else {
                tracing::warn!("task-fit route provenance is incomplete; routing will abstain");
                return None;
            };
            let Ok(reviewer_public_key) =
                std::env::var(crate::task_fit_evidence::TASK_FIT_REVIEW_PUBLIC_KEY_ENV)
            else {
                tracing::warn!("task-fit reviewer identity is unavailable; routing will abstain");
                return None;
            };
            let Ok(nest_dir) = std::env::var("BUZZ_NEST_DIR") else {
                tracing::warn!("task-fit evidence store path is unavailable; routing will abstain");
                return None;
            };
            match TaskFitEvidenceSnapshot::load_local(
                Path::new(&nest_dir),
                TaskFitRouteProfileIdentity {
                    profile_id,
                    profile_version,
                    profile_hash,
                },
                &reviewer_public_key,
            ) {
                Ok(snapshot) => Some(snapshot),
                Err(error) => {
                    tracing::warn!("task-fit evidence is unavailable: {error}");
                    None
                }
            }
        });
    let route_journal = if route_profile
        .as_ref()
        .is_some_and(RouteProfileDocument::uses_throughput_routing)
    {
        match buzz_run_journal::RunJournal::open_default_scoped() {
            Ok(journal) => Some(journal),
            Err(error) => {
                tracing::warn!("route throughput evidence is unavailable: {error}");
                None
            }
        }
    } else {
        None
    };
    let max_line = cfg.max_line_bytes;
    let permissions = Arc::new(permission::PermissionBroker::new(
        cfg.max_pending_permissions,
        cfg.permission_timeout,
    ));
    let app = Arc::new(App {
        cfg,
        llm,
        review_only,
        route_profile,
        route_task_fit_evidence,
        route_journal,
        sessions: Mutex::new(HashMap::new()),
        negotiated_version: AtomicU32::new(PROTOCOL_VERSION),
        permissions,
        models_cache: tokio::sync::OnceCell::new(),
    });
    let (wire_tx, wire_rx) = mpsc::channel::<WireMsg>(64);
    let mut writer = tokio::spawn(wire::writer_task(wire_rx));
    // Whichever ends first drives shutdown. The reader ending is the normal
    // path (stdin EOF/error). The writer ending while the reader still runs
    // means stdout is closed/broken: no reply can ever be written, so we must
    // stop reading and cancel every session rather than leave the process
    // reading input while outstanding permission asks wait out their full
    // deadline for a response that can never arrive.
    tokio::select! {
        r = read_loop(
            BufReader::new(tokio::io::stdin()),
            app.clone(),
            wire_tx,
            max_line,
        ) => {
            if let Err(e) = r {
                tracing::error!("io: reader: {e}");
            }
            cancel_all_sessions(&app).await;
            let _ = writer.await;
        }
        _ = &mut writer => {
            tracing::error!("io: writer exited (stdout closed); shutting down connection");
            cancel_all_sessions(&app).await;
        }
    }
}

/// Signal every live session to cancel. Run on connection teardown so in-flight
/// prompts — including any waiting on a `session/request_permission` response —
/// resolve promptly instead of waiting out their deadline.
async fn cancel_all_sessions(app: &Arc<App>) {
    for session in app.sessions.lock().await.values() {
        let _ = session.cancel_tx.send(true);
    }
}

async fn read_loop<R: tokio::io::AsyncBufRead + Unpin>(
    mut stdin: R,
    app: Arc<App>,
    wire_tx: WireSender,
    max_line: usize,
) -> std::io::Result<()> {
    while let Some(line) = wire::read_bounded_line(&mut stdin, max_line).await? {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(&line) {
            Ok(msg) => dispatch(&app, msg, &wire_tx).await,
            Err(e) => {
                wire::send(
                    &wire_tx,
                    wire::err(Value::Null, PARSE_ERROR, &format!("jsonrpc: parse: {e}")),
                )
                .await;
            }
        }
    }
    Ok(())
}

async fn dispatch(app: &Arc<App>, msg: Value, wire_tx: &WireSender) {
    match classify(&msg) {
        Inbound::Request { id, method, params } => {
            handle_request(app, id, method, params, wire_tx).await
        }
        Inbound::Notification { method, params } => handle_notification(app, &method, params).await,
        // Client's answer to a `session/request_permission` we issued. The
        // broker matches it to a live correlation id (waking that waiter) or
        // ignores an unknown/late id.
        Inbound::Response { id, result } => app.permissions.deliver(&id, result),
        Inbound::Invalid { id, code, message } => {
            wire::send(wire_tx, wire::err(id, code, &message)).await
        }
    }
}

async fn handle_request(
    app: &Arc<App>,
    id: Value,
    method: String,
    params: Value,
    wire_tx: &WireSender,
) {
    match method.as_str() {
        "initialize" => initialize(app, id, params, wire_tx).await,
        "session/new" => {
            let app = app.clone();
            let wire_tx = wire_tx.clone();
            tokio::spawn(async move { session_new(&app, id, params, &wire_tx).await });
        }
        "session/prompt" => spawn_prompt(app.clone(), id, params, wire_tx.clone()),
        "session/set_model" => {
            set_model_session(app, id, params, wire_tx).await;
        }
        "session/cancel" => {
            cancel_session(app, params).await;
            wire::send(wire_tx, wire::ok(id, Value::Null)).await;
        }
        // goose-compatible non-standard extension: inject user input into the
        // currently active prompt without starting a new one. Mirrors goose's
        // `_goose/unstable/session/steer` wire contract so a single client-side
        // delivery path serves both agents.
        "_goose/unstable/session/steer" => {
            steer_session(app, id, params, wire_tx).await;
        }
        _ => {
            wire::send(
                wire_tx,
                wire::err(
                    id,
                    METHOD_NOT_FOUND,
                    &format!("jsonrpc: method not found: {method}"),
                ),
            )
            .await
        }
    }
}

async fn handle_notification(app: &Arc<App>, method: &str, params: Value) {
    if method == "session/cancel" {
        cancel_session(app, params).await;
    }
}

async fn initialize(app: &Arc<App>, id: Value, params: Value, wire_tx: &WireSender) {
    let p: InitializeParams = match decode(params, "initialize") {
        Ok(p) => p,
        Err(m) => return reject(wire_tx, id, INVALID_PARAMS, &m).await,
    };
    // Honest negotiation: respond with the minimum of what the client
    // requested and what we support.
    // NOTE: gating `[Base]` injection on `protocol_version < 2` is a deliberate
    // temporary measure — we are squatting on ACP v2 ahead of the upstream ACP
    // RFD. Revisit when that RFD merges; otherwise a genuine upstream-v2 agent
    // would silently lose `[Base]`.
    let negotiated_version = p.protocol_version.min(PROTOCOL_VERSION);
    // Store the negotiated version for the connection lifetime: the
    // `session/request_permission` wire shape derives from this value, never
    // from a later mutable session field, so a strict client always receives
    // exactly the shape it negotiated at `initialize`.
    app.negotiated_version
        .store(negotiated_version, Ordering::Relaxed);
    wire::send(
        wire_tx,
        wire::ok(
            id,
            json!({
                "protocolVersion": negotiated_version,
                "agentCapabilities": {
                    "loadSession": false,
                    "promptCapabilities": { "image": false, "audio": false, "embeddedContext": false },
                    "mcpCapabilities": { "http": false, "sse": false },
                },
                "agentInfo": { "name": "buzz-agent", "version": env!("CARGO_PKG_VERSION") },
                "_meta": {
                    "buzz": {
                        "taskClass": {
                            "version": 1,
                            "required": app.route_profile.as_ref().is_some_and(|profile| profile.task_fit_policy.is_some()),
                        }
                    }
                },
            }),
        ),
    )
    .await;
}

/// Resolve a Databricks model catalog for one `session/new` call.
///
/// The active filter is part of the result's authority: discovery failure may
/// not fall back to a configured model when it is present, because that would
/// bypass the same restriction applied to a successful catalog.
///
/// Tries to use a previously cached successful discovery result. If the cache
/// is empty, runs `discover` and — on success — populates the cache. On failure
/// the error is returned and the cell remains empty so the next session retries.
///
/// Extracted from `session_new` so tests can drive this path with an injected
/// discovery future without requiring a full `App` / transport stack.
async fn resolve_models_catalog(
    cache: &tokio::sync::OnceCell<Vec<ModelEntry>>,
    discover: impl std::future::Future<Output = Result<Vec<ModelEntry>, AgentError>>,
) -> Result<Vec<ModelEntry>, AgentError> {
    cache.get_or_try_init(|| discover).await.cloned()
}

/// Return the configured model as an unfiltered discovery fallback.
///
/// This value is never written to `models_cache`; failed discovery must be retried by
/// the next session rather than pinning degraded state for the process lifetime.
///
/// Only reached from the Databricks provider arm below, so the curated label is
/// looked up from the Databricks manifest; `id` stays the raw configured value.
fn configured_model_fallback(model: &str) -> Vec<ModelEntry> {
    let model = model.trim().to_string();
    let name = crate::model_capabilities::databricks_registry_label(&model)
        .unwrap_or_else(|| model.clone());
    vec![ModelEntry { id: model, name }]
}

/// A discovery failure may use the configured model only when no visibility
/// filter is active. Returning that model under an active filter would silently
/// bypass the operator's authoritative catalog restriction.
fn discovery_error_fallback(cfg: &Config) -> Vec<ModelEntry> {
    if cfg.databricks_model_filter.is_some() {
        Vec::new()
    } else {
        configured_model_fallback(&cfg.model)
    }
}

fn deepseek_catalog_fallback(
    model: &str,
    error: AgentError,
) -> Result<Vec<ModelEntry>, AgentError> {
    if matches!(error, AgentError::LlmAuth(_)) {
        return Err(error);
    }
    let model = model.trim().to_owned();
    Ok(vec![ModelEntry {
        id: model.clone(),
        name: model,
    }])
}

async fn session_new(app: &Arc<App>, id: Value, params: Value, wire_tx: &WireSender) {
    let p: SessionNewParams = match decode(params, "session/new") {
        Ok(p) => p,
        Err(m) => return reject(wire_tx, id, INVALID_PARAMS, &m).await,
    };
    if p.cwd.is_empty() || !Path::new(&p.cwd).is_absolute() {
        return reject(
            wire_tx,
            id,
            INVALID_PARAMS,
            "session/new: cwd must be an absolute path",
        )
        .await;
    }
    if app.review_only && !p.mcp_servers.is_empty() {
        return reject(
            wire_tx,
            id,
            INVALID_PARAMS,
            "session/new: review-only sessions cannot start MCP tools",
        )
        .await;
    }
    // Check cap without holding lock across MCP spawn (which may be slow).
    {
        let sessions = app.sessions.lock().await;
        if sessions.len() >= app.cfg.max_sessions {
            return reject(
                wire_tx,
                id,
                INVALID_PARAMS,
                "session/new: max sessions reached",
            )
            .await;
        }
    }
    let (hints_text, skills) = if app.cfg.hints_enabled && !app.review_only {
        hints::build_hints_section(std::path::Path::new(&p.cwd))
    } else {
        (String::new(), Vec::new())
    };
    let effective_system_prompt: Arc<str> = {
        // When the harness provides a systemPrompt (base_prompt + persona), use
        // it as the primary content and suppress the default. The default is only
        // a fallback for legacy harnesses that don't send systemPrompt.
        let base = match p.system_prompt.as_deref() {
            Some(client_prompt) if !client_prompt.trim().is_empty() => client_prompt.to_owned(),
            _ => app.cfg.system_prompt.clone(),
        };
        let prompt = if hints_text.is_empty() {
            base
        } else {
            format!("{base}\n\n{hints_text}")
        };
        // Reject combined prompts exceeding 512KB.
        if prompt.len() > MAX_SYSTEM_PROMPT_BYTES {
            return reject(
                wire_tx,
                id,
                INVALID_PARAMS,
                &format!(
                    "session/new: combined system prompt exceeds {}KB limit ({} bytes)",
                    MAX_SYSTEM_PROMPT_BYTES / 1024,
                    prompt.len()
                ),
            )
            .await;
        }
        Arc::from(prompt)
    };
    // Resolve the model catalog before spawning MCP servers or registering a
    // session. A configured static credential cannot recover interactively, so
    // its authentication failure rejects before allocation. OAuth authentication
    // failures and other catalog failures use only the configured model for this
    // response, without caching, so session/prompt can run the existing PKCE flow.
    let available_models: Vec<Value> = {
        use crate::config::Provider;
        match app.cfg.provider {
            Provider::Databricks | Provider::DatabricksV2 => {
                let models = match resolve_models_catalog(
                    &app.models_cache,
                    discover_databricks_models(&app.cfg),
                )
                .await
                {
                    Ok(models) => models,
                    Err(error @ AgentError::LlmAuth(_)) if !app.cfg.api_key.is_empty() => {
                        return reject(wire_tx, id, error.json_rpc_code(), &error.to_string())
                            .await;
                    }
                    Err(error @ AgentError::LlmAuth(_)) => {
                        tracing::warn!(
                            error = %error,
                            filter_active = app.cfg.databricks_model_filter.is_some(),
                            "Databricks OAuth model catalog unavailable; using filter-aware fallback"
                        );
                        discovery_error_fallback(&app.cfg)
                    }
                    Err(error) => {
                        tracing::warn!(
                            error = %error,
                            filter_active = app.cfg.databricks_model_filter.is_some(),
                            "Databricks model catalog unavailable; using filter-aware fallback"
                        );
                        discovery_error_fallback(&app.cfg)
                    }
                };
                models
                    .iter()
                    .map(|m| json!({ "modelId": m.id, "name": m.name }))
                    .collect()
            }
            Provider::DeepSeek => {
                match resolve_models_catalog(&app.models_cache, discover_deepseek_models(&app.cfg))
                    .await
                {
                    Ok(models) => models
                        .iter()
                        .map(|m| json!({ "modelId": m.id, "name": m.name }))
                        .collect(),
                    Err(error) => {
                        let models = match deepseek_catalog_fallback(&app.cfg.model, error) {
                            Ok(models) => models,
                            Err(error) => {
                                return reject(
                                    wire_tx,
                                    id,
                                    error.json_rpc_code(),
                                    &error.to_string(),
                                )
                                .await;
                            }
                        };
                        tracing::warn!(
                            "DeepSeek model catalog unavailable; using configured model"
                        );
                        models
                            .iter()
                            .map(|m| json!({ "modelId": m.id, "name": m.name }))
                            .collect()
                    }
                }
            }
            _ => vec![json!({ "modelId": app.cfg.model, "name": app.cfg.model })],
        }
    };

    let mcp = match McpRegistry::spawn_all(&app.cfg, &p.mcp_servers, &p.cwd).await {
        Ok(m) => Arc::new(m),
        Err(e) => return reject(wire_tx, id, e.json_rpc_code(), &e.to_string()).await,
    };
    let session_id = match session_token() {
        Ok(t) => format!("ses_{t}"),
        Err(e) => return reject(wire_tx, id, -32000, &e).await,
    };
    let (cancel_tx, _) = watch::channel(false);
    let mut sessions = app.sessions.lock().await;
    // Re-check cap (another session may have been created while we spawned MCP).
    if sessions.len() >= app.cfg.max_sessions {
        return reject(
            wire_tx,
            id,
            INVALID_PARAMS,
            "session/new: max sessions reached",
        )
        .await;
    }
    sessions.insert(
        session_id.clone(),
        Session {
            id: session_id.clone(),
            mcp,
            skills,
            history: Vec::new(),
            cancel_tx,
            busy: false,
            prompt_count: 0,
            state_revision: 0,
            active_run_id: None,
            steer_tx: None,
            original_task: None,
            handoff_count: 0,
            last_request_input_tokens: None,
            last_request_history_bytes: None,
            effective_system_prompt,
            effective_model: None,
            accumulated_input_tokens: crate::types::TurnIOState::Unseen,
            accumulated_output_tokens: crate::types::TurnIOState::Unseen,
            accumulated_cached_input_tokens: crate::types::CacheTotalState::Unseen,
            accumulated_cache_write_tokens: crate::types::CacheTotalState::Unseen,
            accumulated_total_state: crate::types::TurnTotalState::Unseen,
        },
    );
    drop(sessions);

    wire::send(
        wire_tx,
        wire::ok(
            id,
            json!({
                "sessionId": session_id,
                "models": {
                    "currentModelId": app.cfg.model,
                    "availableModels": available_models,
                },
            }),
        ),
    )
    .await;
}

fn decode<T: serde::de::DeserializeOwned>(params: Value, stage: &str) -> Result<T, String> {
    serde_json::from_value(params).map_err(|e| format!("{stage}: {e}"))
}

async fn reject(wire_tx: &WireSender, id: Value, code: i32, message: &str) {
    wire::send(wire_tx, wire::err(id, code, message)).await;
}

async fn cancel_session(app: &Arc<App>, params: Value) {
    if let Ok(p) = serde_json::from_value::<SessionCancelParams>(params) {
        let mut sessions = app.sessions.lock().await;
        if let Some(s) = sessions.get_mut(&p.session_id) {
            // Invalidate snapshots that are still doing route preflight. The
            // watch receiver is installed only at admission, so send alone
            // would lose cancels received before reserve_session.
            s.state_revision = next_session_revision(s.state_revision);
            let _ = s.cancel_tx.send(true);
        }
    }
}

/// Handle `session/set_model`: apply a per-session model override immediately.
///
/// Validation:
/// - Unknown `sessionId` → `invalid_params`.
/// - Empty `modelId` → `invalid_params`.
///
/// On success: stores `model_id` on the session and responds `{ sessionId, modelId }`.
/// The override is picked up by the next `session/prompt` call on this session.
async fn set_model_session(app: &Arc<App>, id: Value, params: Value, wire_tx: &WireSender) {
    let p: SessionSetModelParams = match decode(params, "session/set_model") {
        Ok(p) => p,
        Err(m) => return reject(wire_tx, id, INVALID_PARAMS, &m).await,
    };
    if p.model_id.trim().is_empty() {
        return reject(
            wire_tx,
            id,
            INVALID_PARAMS,
            "session/set_model: modelId must not be empty",
        )
        .await;
    }
    if app.review_only {
        return reject(
            wire_tx,
            id,
            INVALID_PARAMS,
            "session/set_model: review-only model identity is pinned at launch",
        )
        .await;
    }
    let mut sessions = app.sessions.lock().await;
    let Some(s) = sessions.get_mut(&p.session_id) else {
        return reject(
            wire_tx,
            id,
            INVALID_PARAMS,
            "session/set_model: unknown session",
        )
        .await;
    };
    update_session_model_override(&mut s.effective_model, &mut s.state_revision, &p.model_id);
    tracing::info!(
        session_id = %p.session_id,
        model_id = %p.model_id,
        "session/set_model: model overridden"
    );
    drop(sessions);
    wire::send(
        wire_tx,
        wire::ok(
            id,
            json!({ "sessionId": p.session_id, "modelId": p.model_id }),
        ),
    )
    .await;
}

/// Handle `_goose/unstable/session/steer`: queue user input into the in-flight
/// prompt. Validation mirrors goose's `on_steer_session`:
///   - empty prompt → `invalid_params`
///   - no active run (no prompt in flight) → `invalid_params`
///   - `expectedRunId` mismatch → `invalid_params` (caller is steering a turn
///     that already ended or rotated; it must fall back to cancel+merge)
///
/// On success the message is queued for pickup at the next round boundary and
/// we reply `{ runId, messageId }`, then emit a `queuedSteer` session/update so
/// the client can correlate the accepted steer with its eventual pickup.
async fn steer_session(app: &Arc<App>, id: Value, params: Value, wire_tx: &WireSender) {
    let p: SessionSteerParams = match decode(params, "_goose/unstable/session/steer") {
        Ok(p) => p,
        Err(m) => return reject(wire_tx, id, INVALID_PARAMS, &m).await,
    };
    if p.prompt.is_empty() {
        return reject(
            wire_tx,
            id,
            INVALID_PARAMS,
            "steer: prompt must not be empty",
        )
        .await;
    }
    if p.expected_run_id.is_empty() {
        return reject(
            wire_tx,
            id,
            INVALID_PARAMS,
            "steer: expectedRunId must not be empty",
        )
        .await;
    }
    let message_id = format!("steer_{}", session_token().unwrap_or_else(|_| "x".into()));
    let run_id = {
        let sessions = app.sessions.lock().await;
        let Some(s) = sessions.get(&p.session_id) else {
            return reject(wire_tx, id, INVALID_PARAMS, "steer: unknown session").await;
        };
        let Some(active) = s.active_run_id.as_deref() else {
            return reject(wire_tx, id, INVALID_PARAMS, "steer: no active run to steer").await;
        };
        if active != p.expected_run_id {
            return reject(
                wire_tx,
                id,
                INVALID_PARAMS,
                &format!(
                    "steer: expected active run id `{}` but found `{active}`",
                    p.expected_run_id
                ),
            )
            .await;
        }
        // A live run always has a steer_tx; if the channel is gone the run is
        // tearing down — treat as no active run rather than queue into the void.
        match &s.steer_tx {
            Some(tx) if tx.send(p.prompt).is_ok() => active.to_owned(),
            _ => return reject(wire_tx, id, INVALID_PARAMS, "steer: no active run to steer").await,
        }
    };
    wire::send(
        wire_tx,
        wire::ok(id, json!({ "runId": run_id, "messageId": message_id })),
    )
    .await;
    // Best-effort correlation hint for the client; mirrors goose's
    // `send_queued_steer_update`. Not load-bearing for delivery.
    wire::send(
        wire_tx,
        wire::session_update_with_goose_meta(
            &p.session_id,
            json!({ "sessionUpdate": "session_info_update" }),
            json!({ "queuedSteer": { "messageId": message_id, "runId": run_id } }),
        ),
    )
    .await;
}

fn spawn_prompt(app: Arc<App>, id: Value, params: Value, wire_tx: WireSender) {
    tokio::spawn(async move { run_prompt(app, id, params, wire_tx).await });
}

async fn run_prompt(app: Arc<App>, id: Value, params: Value, wire_tx: WireSender) {
    let p: SessionPromptParams = match decode(params, "session/prompt") {
        Ok(p) => p,
        Err(m) => return reject(&wire_tx, id, INVALID_PARAMS, &m).await,
    };
    if let Some(reason) = strict_task_class_preflight_reason(
        app.route_profile.as_ref(),
        p.task_class_metadata().as_ref(),
    ) {
        // Reject before session snapshot/reservation: strict-fit abstentions must neither
        // touch a provider nor consume a review-only session's one-shot turn.
        return reject(&wire_tx, id, INVALID_PARAMS, reason).await;
    }
    let snapshot = match snapshot_session(&app, &p.session_id).await {
        Ok(v) => v,
        Err(reason) => {
            return reject(
                &wire_tx,
                id,
                INVALID_PARAMS,
                &format!("session/prompt: {reason}"),
            )
            .await;
        }
    };
    #[cfg(debug_assertions)]
    wait_for_route_preflight_test_barrier(&id).await;
    let sid = snapshot.id.clone();
    let effective_system_prompt = Arc::clone(&snapshot.effective_system_prompt);
    let effective_model_override = snapshot.effective_model_override.clone();
    let run_id = match session_token() {
        Ok(token) => format!("run_{token}"),
        Err(_) => {
            return reject(
                &wire_tx,
                id,
                INVALID_PARAMS,
                "session/prompt: rng failure; retry prompt",
            )
            .await;
        }
    };
    // Resolve the one route used for this whole ACP prompt. An explicit
    // session/set_model choice wins; otherwise the optional profile applies
    // hard data gates and its ordered preference before the first request.
    let mut active_cfg = app.cfg.clone();
    // Preserve the prompt supplied to session/new on the default route. If a
    // route candidate is selected below, its bound config carries this same
    // session prompt plus that candidate's route-specific additions.
    active_cfg.system_prompt = effective_system_prompt.to_string();
    let mut active_llm = Arc::clone(&app.llm);
    let mut active_model = effective_model_override
        .clone()
        .unwrap_or_else(|| app.cfg.model.clone());
    let mut route_preflight_error = None;
    let mut route_context_capacity_tokens = None;
    let mut route_cost_budget: Option<RouteCostBudget> = None;
    let route_profile_override = app.route_profile.as_ref().and_then(|profile| {
        effective_model_override.as_deref().map(|model| {
            route_profile_for_model_override(profile, route_provider_id(app.cfg.provider), model)
        })
    });
    let route_decision = if let Some(profile) = &app.route_profile {
        if let Some(Err(reason)) = route_profile_override.as_ref() {
            route_preflight_error = Some(reason.message().into());
            Some(route_decision_notice(
                &sid,
                &run_id,
                profile,
                "refused",
                None,
                None,
                Some(reason.reason_code()),
            ))
        } else {
            let selection_profile = route_profile_override
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .map(|(profile, _)| profile)
                .unwrap_or(profile);
            let is_explicit_override = effective_model_override.is_some();
            let route_profile_hash = route_profile_identity(profile).2;
            let selection = select_profile_route_for_prompt_with_task_fit_evidence(
                selection_profile,
                &effective_system_prompt,
                &p.prompt,
                &snapshot.history,
                |config| {
                    crate::agent::request_tool_definitions(
                        &snapshot.mcp,
                        !snapshot.skills.is_empty(),
                        config.summary_model.is_some(),
                    )
                },
                |entry, config, input_tokens| {
                    let Some(journal) = app.route_journal.as_ref() else {
                        return Evidence::Unknown;
                    };
                    let Some(endpoint_hash) =
                        crate::agent::device_keyed_endpoint_fingerprint(&config.base_url)
                    else {
                        return Evidence::Unknown;
                    };
                    let Some(profile_hash) = route_profile_hash.as_ref() else {
                        return Evidence::Unknown;
                    };
                    let query = buzz_run_journal::RouteThroughputQuery {
                        profile_hash: profile_hash.clone(),
                        endpoint_hash,
                        candidate_id: entry.id.clone(),
                        provider_id: route_provider_id(config.provider).into(),
                        model_id: config.model.clone(),
                        thinking_effort: route_thinking_effort(config.thinking_effort).into(),
                        input_tokens,
                    };
                    match journal.route_throughput_summary(&query) {
                        Ok(summary) => summary
                            .effective_output_tokens_per_second_milli
                            .map(|value| Evidence::Known {
                                value,
                                source: EvidenceSource::Measured,
                            })
                            .unwrap_or(Evidence::Unknown),
                        Err(error) => {
                            tracing::debug!("route throughput lookup failed: {error}");
                            Evidence::Unknown
                        }
                    }
                },
                |entry, config, policy| {
                    let Some(evidence) = app.route_task_fit_evidence.as_ref() else {
                        return crate::task_fit_evidence::TaskFitEligibility::Unknown(
                            TaskFitUnknownReason::EvidenceStoreUnavailable,
                        );
                    };
                    evidence.evaluate_candidate(
                        &entry.id,
                        route_provider_id(config.provider),
                        &config.model,
                        policy,
                        chrono::Utc::now(),
                    )
                },
                |provider, model, prompt| app.cfg.for_route_target(provider, model, prompt),
            );
            match selection {
                Ok(BoundRouteSelection::Chosen { selected, preview }) => {
                    route_cost_budget = profile.cost_budget_for_candidate(&selected.candidate.id);
                    if profile.max_turn_cost_microusd.is_some() && route_cost_budget.is_none() {
                        route_preflight_error =
                            Some("selected route has no complete per-turn pricing".into());
                    }
                    let context_fit = selected.candidate.context_fit_summary();
                    if profile.strict_context_fit {
                        route_context_capacity_tokens = context_fit.map(|fit| fit.capacity_tokens);
                    }
                    if let Some(config) = selected.config() {
                        match Llm::new_for_route(
                            config,
                            selected.data_location()
                                == Some(crate::route_preview::DataLocation::Local),
                        ) {
                            Ok(llm) => {
                                active_model = selected.candidate.model.clone();
                                active_cfg = config.clone();
                                active_llm = Arc::new(llm);
                                tracing::info!(
                                    route_profile_version = profile.version,
                                    ?preview,
                                    route_candidate_id = %selected.candidate.id,
                                    provider = ?selected.candidate.provider,
                                    model = %selected.candidate.model,
                                    "selected provider route for ACP prompt"
                                );
                                Some(route_decision_notice(
                                    &sid,
                                    &run_id,
                                    profile,
                                    if is_explicit_override {
                                        "overridden"
                                    } else {
                                        "selected"
                                    },
                                    Some(&selected.candidate),
                                    context_fit,
                                    is_explicit_override
                                        .then_some("explicit_session_model_override"),
                                ))
                            }
                            Err(error) => {
                                route_preflight_error = Some(error.to_string());
                                Some(route_decision_notice(
                                    &sid,
                                    &run_id,
                                    profile,
                                    "refused",
                                    Some(&selected.candidate),
                                    context_fit,
                                    Some("provider_client_initialization_failed"),
                                ))
                            }
                        }
                    } else {
                        route_preflight_error =
                            Some("selected candidate has no configured provider connection".into());
                        Some(route_decision_notice(
                            &sid,
                            &run_id,
                            profile,
                            "refused",
                            Some(&selected.candidate),
                            context_fit,
                            Some("provider_connection_unconfigured"),
                        ))
                    }
                }
                Ok(BoundRouteSelection::Abstained { preview }) => {
                    tracing::info!(
                        route_profile_version = profile.version,
                        ?preview,
                        "route profile abstained"
                    );
                    route_preflight_error = Some(format!("no eligible route: {preview:?}"));
                    let reason_code = route_abstention_reason(&preview.decision);
                    Some(route_decision_notice(
                        &sid,
                        &run_id,
                        profile,
                        "abstained",
                        None,
                        None,
                        Some(reason_code),
                    ))
                }
                Err(error) => {
                    route_preflight_error = Some(error.to_string());
                    Some(route_decision_notice(
                        &sid,
                        &run_id,
                        profile,
                        "refused",
                        None,
                        None,
                        Some("route_configuration_invalid"),
                    ))
                }
            }
        }
    } else {
        None
    };
    if let Some(error) = route_preflight_error.as_deref() {
        if let Err(reason) = validate_session_snapshot(&app, &snapshot).await {
            return reject(
                &wire_tx,
                id,
                INVALID_PARAMS,
                &format!("session/prompt: {reason}"),
            )
            .await;
        }
        if let Some(route_decision) = route_decision {
            wire::send(
                &wire_tx,
                wire::session_update_with_meta(
                    &sid,
                    json!({ "sessionUpdate": "session_info_update" }),
                    json!({ "buzz": { "routeDecisionV1": route_decision } }),
                ),
            )
            .await;
        }
        return reject(
            &wire_tx,
            id,
            INVALID_PARAMS,
            &format!("route preflight abstained: {error}"),
        )
        .await;
    }
    let reservation = match reserve_session(&app, snapshot, &run_id).await {
        Ok(reservation) => reservation,
        Err(reason) => {
            return reject(
                &wire_tx,
                id,
                INVALID_PARAMS,
                &format!("session/prompt: {reason}"),
            )
            .await;
        }
    };
    let ReservedSessionPrompt {
        snapshot: admitted,
        mut cancel_rx,
        mut steer_rx,
    } = reservation;
    let mcp = admitted.mcp;
    let skills = admitted.skills;
    let mut history = admitted.history;
    let mut original_task = admitted.original_task;
    let mut handoff_count = admitted.handoff_count;
    let mut last_request_input_tokens = admitted.last_request_input_tokens;
    let mut last_request_history_bytes = admitted.last_request_history_bytes;
    let usage_baseline = admitted.usage_baseline;
    // Advertise the active run ID so steer-capable clients can target this turn
    // via `expectedRunId`. Buzz's namespaced route record shares the standard
    // opaque ACP `_meta` field and is joined to this active run by buzz-acp.
    let route_measurement_identity = route_decision.as_ref().and_then(|notice| {
        if notice["outcome"].as_str() != Some("selected") {
            return None;
        }
        Some(RouteMeasurementIdentity {
            profile_id: notice["profileId"].as_str()?.to_owned(),
            profile_version: u32::try_from(notice["profileVersion"].as_u64()?).ok()?,
            profile_hash: notice["profileHash"].as_str()?.to_owned(),
            candidate_id: notice["candidateId"].as_str()?.to_owned(),
            provider_id: notice["providerId"].as_str()?.to_owned(),
            model_id: notice["modelId"].as_str()?.to_owned(),
        })
    });
    let mut notification_meta = json!({ "goose": { "activeRunId": run_id } });
    if let Some(route_decision) = route_decision {
        notification_meta["buzz"] = json!({ "routeDecisionV1": route_decision });
    }
    wire::send(
        &wire_tx,
        wire::session_update_with_meta(
            &sid,
            json!({ "sessionUpdate": "session_info_update" }),
            notification_meta,
        ),
    )
    .await;
    let mut turn_input_tokens: crate::types::TurnIOState = crate::types::TurnIOState::Unseen;
    let mut turn_output_tokens: crate::types::TurnIOState = crate::types::TurnIOState::Unseen;
    let mut turn_cached_input_tokens: crate::types::CacheTotalState =
        crate::types::CacheTotalState::Unseen;
    let mut turn_cache_write_tokens: crate::types::CacheTotalState =
        crate::types::CacheTotalState::Unseen;
    let mut turn_total_state = crate::types::TurnTotalState::Unseen;
    // Per-turn billing identity accumulator — three-state:
    //   None          = no usage-bearing response seen yet (initial)
    //   Some(Some(pi))= all usage-bearing responses carry the same proven identity
    //   Some(None)    = poisoned (mixed identities, unproven response, etc.)
    // Not stored in Session (not session-cumulative); used only for the final
    // end-of-turn wire emission.
    let mut turn_pricing_identity: Option<Option<crate::types::PricingIdentity>> = None;
    let mut ctx = RunCtx {
        cfg: &active_cfg,
        effective_model: &active_model,
        route_preflight_error: None,
        context_fit_capacity_tokens: route_context_capacity_tokens,
        route_cost_budget,
        route_cost_reserved_microusd: 0,
        session_id: &sid,
        system_prompt: &active_cfg.system_prompt,
        llm: &active_llm,
        mcp: &mcp,
        permissions: &app.permissions,
        protocol_version: app.negotiated_version.load(Ordering::Relaxed),
        skills: &skills,
        wire: &wire_tx,
        cancel: &mut cancel_rx,
        steer: &mut steer_rx,
        history: &mut history,
        original_task: &mut original_task,
        handoff_count: &mut handoff_count,
        run_id,
        route_measurement_identity,
        route_measurement_sequence: 0,
        last_request_input_tokens: &mut last_request_input_tokens,
        last_request_history_bytes: &mut last_request_history_bytes,
        turn_input_tokens: &mut turn_input_tokens,
        turn_output_tokens: &mut turn_output_tokens,
        turn_cached_input_tokens: &mut turn_cached_input_tokens,
        turn_cache_write_tokens: &mut turn_cache_write_tokens,
        turn_total_state: &mut turn_total_state,
        turn_pricing_identity: &mut turn_pricing_identity,
        usage_baseline,
    };
    let result = ctx.run(p.prompt).await;
    if let Some(s) = app.sessions.lock().await.get_mut(&sid) {
        // Clear run state so a late steer can't queue into a finished turn.
        s.active_run_id = None;
        s.steer_tx = None;
        s.history = history;
        s.original_task = original_task;
        s.handoff_count = handoff_count;
        s.last_request_input_tokens = last_request_input_tokens;
        s.last_request_history_bytes = last_request_history_bytes;
    }
    // Update session-cumulative token counters and emit the usage notification
    // BEFORE sending the session/prompt response. buzz-acp's UsageTracker
    // processes the notification while the turn is still in-flight (i.e. before
    // the response triggers take_turn_usage()), which is required for the
    // begin_turn gate to recognise it as publishable.
    //
    // Only emit when at least one token count was observed — a turn with no
    // provider response (validation failure, pre-response cancellation) carries
    // no information and must not produce a kind 44200 record per NIP-AM.
    if !matches!(turn_input_tokens, crate::types::TurnIOState::Unseen)
        || !matches!(turn_output_tokens, crate::types::TurnIOState::Unseen)
    {
        let accumulated = {
            let mut sessions = app.sessions.lock().await;
            if let Some(s) = sessions.get_mut(&sid) {
                // merge_session: Poisoned poisons permanently; Exact sums with
                // overflow-check → Poisoned on wrap; Unseen leaves unchanged.
                s.accumulated_input_tokens =
                    s.accumulated_input_tokens.merge_session(turn_input_tokens);
                s.accumulated_output_tokens = s
                    .accumulated_output_tokens
                    .merge_session(turn_output_tokens);
                // D1 tri-state merge: merge_session propagates Unknown when
                // the turn was poisoned (any usage-bearing round omitted the
                // category), and is a no-op when the turn was Unseen (no
                // usage-bearing response at all).
                s.accumulated_cached_input_tokens = s
                    .accumulated_cached_input_tokens
                    .merge_session(turn_cached_input_tokens);
                s.accumulated_cache_write_tokens = s
                    .accumulated_cache_write_tokens
                    .merge_session(turn_cache_write_tokens);
                // Fold the per-turn total state into the session cumulative.
                // Unknown poisons the session permanently; Exact adds to running sum;
                // Unseen (turn emitted no usage) leaves the cumulative unchanged.
                // Uses TurnTotalState::merge_session, which applies the same
                // checked-add / overflow-poisons contract as the per-response fold.
                s.accumulated_total_state =
                    s.accumulated_total_state.merge_session(turn_total_state);
                Some((
                    s.accumulated_input_tokens,
                    s.accumulated_output_tokens,
                    s.accumulated_cached_input_tokens,
                    s.accumulated_cache_write_tokens,
                    s.accumulated_total_state,
                ))
            } else {
                // Session is gone — the accumulated baseline no longer exists, so
                // there is nothing correct to emit. Skip the usage notification.
                None
            }
        };
        if let Some((
            accumulated_in,
            accumulated_out,
            accumulated_cached,
            accumulated_written,
            accumulated_total,
        )) = accumulated
        {
            // Same builder the run loop uses for its per-round reports, so the
            // final notification is shape-identical to the ones that preceded
            // it and a consumer taking the high-water mark lands on this one.
            let update = wire::usage_update_payload(
                accumulated_in.exact_value(),
                accumulated_out.exact_value(),
                accumulated_cached.exact_value(),
                accumulated_written.exact_value(),
                accumulated_total,
                &active_model,
                // Pass the proven per-turn identity if consistent; absent otherwise.
                turn_pricing_identity
                    .as_ref()
                    .and_then(|inner| inner.as_ref()),
            );
            wire::send(&wire_tx, goose_session_update(&sid, update)).await;
        }
    }
    if let Some(s) = app.sessions.lock().await.get_mut(&sid) {
        s.busy = false;
        s.state_revision = next_session_revision(s.state_revision);
    }
    match result {
        Ok(stop) => {
            wire::send(
                &wire_tx,
                wire::ok(id, json!({ "stopReason": stop.as_wire() })),
            )
            .await
        }
        Err(e) => wire::send(&wire_tx, wire::err(id, e.json_rpc_code(), &e.to_string())).await,
    }
}

fn strict_task_class_preflight_reason(
    profile: Option<&RouteProfileDocument>,
    metadata: Option<&wire::TaskClassMetadata>,
) -> Option<&'static str> {
    let policy = profile?.task_fit_policy.as_ref()?;
    let Some(metadata) = metadata.filter(|metadata| metadata.validate()) else {
        return Some("strict_task_fit_task_class_unknown");
    };
    if metadata.task_class != policy.task_class
        || metadata.taxonomy_version != policy.task_class_taxonomy_version
    {
        return Some("strict_task_fit_task_class_mismatch");
    }
    None
}

async fn snapshot_session(
    app: &Arc<App>,
    session_id: &str,
) -> Result<SessionPromptSnapshot, &'static str> {
    let sessions = app.sessions.lock().await;
    let s = sessions.get(session_id).ok_or("unknown session")?;
    if s.busy {
        return Err("prompt already in flight");
    }
    if app.review_only && s.prompt_count > 0 {
        return Err("review-only session accepts one prompt");
    }
    Ok(SessionPromptSnapshot {
        state_revision: s.state_revision,
        id: s.id.clone(),
        mcp: Arc::clone(&s.mcp),
        skills: s.skills.clone(),
        history: s.history.clone(),
        original_task: s.original_task.clone(),
        handoff_count: s.handoff_count,
        last_request_input_tokens: s.last_request_input_tokens,
        last_request_history_bytes: s.last_request_history_bytes,
        effective_system_prompt: Arc::clone(&s.effective_system_prompt),
        effective_model_override: s.effective_model.clone(),
        usage_baseline: crate::types::SessionUsageBaseline {
            input_tokens: s.accumulated_input_tokens,
            output_tokens: s.accumulated_output_tokens,
            cached_input_tokens: s.accumulated_cached_input_tokens,
            cache_write_tokens: s.accumulated_cache_write_tokens,
            total_state: s.accumulated_total_state,
        },
    })
}

/// Deterministic barrier for integration tests that exercise route-preflight
/// races. It exists only in debug builds and is inactive unless the child test
/// process explicitly provides a private temporary directory.
#[cfg(debug_assertions)]
async fn wait_for_route_preflight_test_barrier(id: &Value) {
    const BARRIER_ENV: &str = "BUZZ_AGENT_TEST_ROUTE_PREFLIGHT_BARRIER_DIR";
    let Some(directory) = std::env::var_os(BARRIER_ENV) else {
        return;
    };
    let Some(request_id) = id.as_i64() else {
        return;
    };
    let directory = std::path::PathBuf::from(directory);
    let ready = directory.join(format!("{request_id}.ready"));
    let release = directory.join(format!("{request_id}.release"));
    if let Err(error) = std::fs::write(&ready, b"ready") {
        tracing::warn!(%error, "could not signal route-preflight test barrier");
        return;
    }

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if release.exists() {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!(request_id, "route-preflight test barrier timed out");
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

async fn reserve_session(
    app: &Arc<App>,
    snapshot: SessionPromptSnapshot,
    run_id: &str,
) -> Result<ReservedSessionPrompt, &'static str> {
    let mut sessions = app.sessions.lock().await;
    let s = sessions.get_mut(&snapshot.id).ok_or("unknown session")?;
    if let Some(reason) = session_admission_error(
        snapshot.state_revision,
        s.state_revision,
        s.busy,
        s.prompt_count,
        app.review_only,
    ) {
        return Err(reason);
    }

    s.busy = true;
    s.prompt_count = s.prompt_count.saturating_add(1);
    s.state_revision = next_session_revision(s.state_revision);
    let (cancel_tx, cancel_rx) = watch::channel(false);
    s.cancel_tx = cancel_tx;
    // Revision equality proves these values are still current. Move the
    // session-owned history only after the reservation succeeds.
    drop(std::mem::take(&mut s.history));
    s.original_task = None;
    s.active_run_id = Some(run_id.to_owned());
    let (steer_tx, steer_rx) = mpsc::unbounded_channel();
    s.steer_tx = Some(steer_tx);
    Ok(ReservedSessionPrompt {
        snapshot,
        cancel_rx,
        steer_rx,
    })
}

async fn validate_session_snapshot(
    app: &Arc<App>,
    snapshot: &SessionPromptSnapshot,
) -> Result<(), &'static str> {
    let sessions = app.sessions.lock().await;
    let s = sessions.get(&snapshot.id).ok_or("unknown session")?;
    session_admission_error(
        snapshot.state_revision,
        s.state_revision,
        s.busy,
        s.prompt_count,
        app.review_only,
    )
    .map_or(Ok(()), Err)
}

fn next_session_revision(revision: u64) -> u64 {
    revision.saturating_add(1)
}

fn update_session_model_override(
    effective_model: &mut Option<String>,
    state_revision: &mut u64,
    model_id: &str,
) {
    *effective_model = Some(model_id.to_owned());
    *state_revision = next_session_revision(*state_revision);
}

fn session_admission_error(
    snapshot_revision: u64,
    current_revision: u64,
    busy: bool,
    prompt_count: u32,
    review_only: bool,
) -> Option<&'static str> {
    if busy {
        Some("prompt already in flight")
    } else if review_only && prompt_count > 0 {
        Some("review-only session accepts one prompt")
    } else if snapshot_revision != current_revision {
        Some("session changed during route preflight; retry prompt")
    } else {
        None
    }
}

fn session_token() -> Result<String, String> {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).map_err(|e| format!("rng: getrandom failed: {e}"))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::{
        apply_critic_route_cost_budget_override, route_abstention_reason, route_decision_notice,
        route_profile_for_model_override, session_admission_error, update_session_model_override,
        RouteContextFitSummary,
    };
    use crate::catalog::ModelEntry;
    use crate::route_preview::{
        DataLocation, Evidence, RouteProfileDataPolicy, RouteProfileDocument,
    };
    use crate::types::AgentError;
    use serde_json::json;

    #[test]
    fn set_model_revision_invalidates_an_older_route_snapshot() {
        let snapshot_revision = 0;
        let mut effective_model = None;
        let mut current_revision = snapshot_revision;
        update_session_model_override(&mut effective_model, &mut current_revision, "changed-model");
        assert_eq!(effective_model.as_deref(), Some("changed-model"));
        assert_eq!(current_revision, 1);
        assert_eq!(
            session_admission_error(snapshot_revision, current_revision, false, 0, false,),
            Some("session changed during route preflight; retry prompt")
        );
    }

    #[test]
    fn critic_round_budget_only_lowers_the_reviewers_resolved_route_ceiling() {
        let mut profile = Some(
            RouteProfileDocument::parse(
                br#"{
                    "version":1,
                    "max_turn_cost_microusd":100,
                    "candidates":[{"id":"local","provider":"openai","model":"local-model","data_location":"local"}],
                    "profile_id":"local-review",
                    "profile_version":2,
                    "profile_hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                }"#,
            )
            .unwrap(),
        );

        apply_critic_route_cost_budget_override(&mut profile, Some("40"), true).unwrap();
        assert_eq!(profile.as_ref().unwrap().max_turn_cost_microusd, Some(40));
        assert_eq!(profile.as_ref().unwrap().profile_version, Some(2));
        assert_eq!(
            profile.as_ref().unwrap().profile_hash.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );

        apply_critic_route_cost_budget_override(&mut profile, Some("80"), true).unwrap();
        assert_eq!(profile.as_ref().unwrap().max_turn_cost_microusd, Some(40));
    }

    #[test]
    fn critic_round_budget_rejects_untrusted_or_malformed_overrides() {
        let mut profile = Some(RouteProfileDocument::parse(
            br#"{"version":1,"candidates":[{"id":"local","provider":"openai","model":"local-model","data_location":"local"}]}"#,
        ).unwrap());
        assert!(apply_critic_route_cost_budget_override(&mut profile, Some("20"), false).is_err());
        assert!(
            apply_critic_route_cost_budget_override(&mut profile, Some("20usd"), true).is_err()
        );
        assert!(apply_critic_route_cost_budget_override(&mut None, Some("20"), true).is_err());
        assert!(apply_critic_route_cost_budget_override(&mut profile, None, false).is_ok());
    }

    #[test]
    fn model_override_pin_preserves_profile_gates_and_requires_unique_exact_match() {
        let profile = RouteProfileDocument::parse(
            br#"{
                "version":1,
                "data_policy":"local-only",
                "strict_context_fit":true,
                "max_turn_cost_microusd":1000000,
                "prefer_fastest_measured":true,
                "profile_id":"profile-a",
                "profile_version":2,
                "profile_hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "candidates":[
                    {
                        "id":"pinned",
                        "provider":"openai",
                        "model":"target-model",
                        "data_location":"local",
                        "context_capacity_tokens":10000,
                        "input_cost_microusd_per_million_tokens":100,
                        "output_cost_microusd_per_million_tokens":100
                    },
                    {
                        "id":"other",
                        "provider":"deepseek",
                        "model":"other-model",
                        "data_location":"local"
                    }
                ]
            }"#,
        )
        .unwrap();

        let (pinned, candidate_id) =
            route_profile_for_model_override(&profile, "openai", "target-model").unwrap();
        assert_eq!(candidate_id, "pinned");
        assert_eq!(pinned.candidates.len(), 1);
        assert_eq!(pinned.candidates[0].id, "pinned");
        assert_eq!(pinned.preference_order, ["pinned"]);
        assert_eq!(pinned.data_policy, RouteProfileDataPolicy::LocalOnly);
        assert!(pinned.strict_context_fit);
        assert_eq!(pinned.max_turn_cost_microusd, Some(1_000_000));
        assert!(!pinned.prefer_fastest_measured);
        assert_eq!(pinned.min_effective_output_tokens_per_second_milli, Some(1));
        assert_eq!(pinned.profile_id, profile.profile_id);
        assert_eq!(pinned.profile_version, profile.profile_version);
        assert_eq!(pinned.profile_hash, profile.profile_hash);
        assert_eq!(
            route_profile_for_model_override(&profile, "openai", "missing-model")
                .err()
                .unwrap()
                .reason_code(),
            "manual_override_model_not_listed"
        );

        let mut ambiguous = profile;
        let mut duplicate_model = ambiguous.candidates[0].clone();
        duplicate_model.id = "same-model-second-id".into();
        ambiguous.candidates.push(duplicate_model);
        assert_eq!(
            route_profile_for_model_override(&ambiguous, "openai", "target-model")
                .err()
                .unwrap()
                .reason_code(),
            "manual_override_model_ambiguous"
        );
    }

    #[test]
    fn route_decision_notice_contains_only_join_and_decision_provenance() {
        let profile = RouteProfileDocument {
            version: 1,
            data_policy: RouteProfileDataPolicy::default(),
            preference_order: vec![],
            candidates: vec![],
            profile_id: Some("local-first".into()),
            profile_version: Some(3),
            profile_hash: Some("a".repeat(64)),
            strict_context_fit: false,
            max_turn_cost_microusd: None,
            prefer_fastest_measured: false,
            min_effective_output_tokens_per_second_milli: None,
            allow_preference_order_warmup: false,
            task_fit_policy: None,
        };
        let candidate = crate::route_preview::RouteCandidate {
            id: "local-fast".into(),
            provider: crate::config::Provider::OpenAi,
            model: "gpt-test".into(),
            available: Evidence::Unknown,
            data_location: Evidence::Known {
                value: DataLocation::Local,
                source: crate::route_preview::EvidenceSource::OperatorConfig,
            },
            max_cost_microusd: Evidence::Unknown,
            max_seconds: Evidence::Unknown,
            context_tokens: Evidence::Unknown,
            input_context_upper_bound_tokens: Evidence::Unknown,
            tokens_per_second_milli: Evidence::Unknown,
            tools: Evidence::Unknown,
        };
        let notice = route_decision_notice(
            "session-1",
            "run_attempt-1",
            &profile,
            "selected",
            Some(&candidate),
            Some(RouteContextFitSummary {
                estimate_method: crate::route_preview::ContextEstimateMethod::Utf8BytesPlusFramingAndOutputReserveV1,
                capacity_source: crate::route_preview::ContextCapacitySource::OperatorDeclared,
                input_tokens_upper_bound: 512,
                capacity_tokens: 2048,
            }),
            None,
        );
        assert_eq!(notice["sessionId"], "session-1");
        assert_eq!(notice["attemptId"], "run_attempt-1");
        assert_eq!(notice["profileId"], "local-first");
        assert_eq!(notice["profileVersion"], 3);
        assert_eq!(notice["candidateId"], "local-fast");
        assert_eq!(notice["providerId"], "openai");
        assert_eq!(notice["modelId"], "gpt-test");
        assert_eq!(notice["contextFit"]["capacitySource"], "operator_declared");
        assert_eq!(notice["contextFit"]["inputTokensUpperBound"], 512);
        assert_eq!(notice["contextFit"]["capacityTokens"], 2048);
        assert!(notice.get("prompt").is_none());
        assert!(notice.get("apiKey").is_none());
        assert!(notice.get("error").is_none());
        let message = crate::wire::session_update_with_meta(
            "session-1",
            json!({ "sessionUpdate": "session_info_update" }),
            json!({
                "goose": { "activeRunId": "run_attempt-1" },
                "buzz": { "routeDecisionV1": notice.clone() },
            }),
        );
        assert_eq!(
            message["params"]["update"]["_meta"]["buzz"]["routeDecisionV1"]["attemptId"],
            "run_attempt-1"
        );

        let unsafe_reason = crate::route_preview::RouteDecision::Abstain {
            reason: "provider error with secret text".into(),
        };
        assert_eq!(route_abstention_reason(&unsafe_reason), "route_abstained");
    }

    /// Regression: a discovery error must not pin the models_cache for the process lifetime.
    ///
    /// `resolve_models_catalog` uses `get_or_try_init` so an `Err` leaves the `OnceCell`
    /// empty and the next `session/new` retries discovery. This test calls
    /// `resolve_models_catalog` directly — the same function `session_new` calls — so
    /// reverting `session_new` to `get_or_init` (or any other cache-on-error variant) would
    /// break this test, not just the standalone `OnceCell` semantics.
    #[tokio::test]
    async fn models_cache_does_not_pin_on_discovery_error() {
        let cache: tokio::sync::OnceCell<Vec<ModelEntry>> = tokio::sync::OnceCell::new();

        // First call — discovery failure is surfaced and leaves the cell empty.
        let error = crate::resolve_models_catalog(&cache, async {
            Err::<Vec<ModelEntry>, AgentError>(AgentError::Llm("transient failure".into()))
        })
        .await
        .unwrap_err();
        assert!(matches!(error, AgentError::Llm(_)));

        // Second call — discovery succeeds. Cell is now populated and returned.
        let discovered = vec![ModelEntry {
            id: "databricks-meta-llama-3-1-70b-instruct".into(),
            name: "databricks-meta-llama-3-1-70b-instruct".into(),
        }];
        let discovered_clone = discovered.clone();
        let second = crate::resolve_models_catalog(&cache, async move {
            Ok::<Vec<ModelEntry>, AgentError>(discovered_clone)
        })
        .await
        .unwrap();
        assert_eq!(
            second, discovered,
            "second call must return the discovered catalog"
        );
        assert!(
            cache.get().is_some(),
            "cell must be populated after successful discovery"
        );
        assert_eq!(
            cache.get().unwrap(),
            &discovered,
            "cache must hold the successful discovery result"
        );
    }

    #[tokio::test]
    async fn models_catalog_does_not_cache_oauth_auth_fallback() {
        let cache: tokio::sync::OnceCell<Vec<ModelEntry>> = tokio::sync::OnceCell::new();
        let error = crate::resolve_models_catalog(&cache, async {
            Err::<Vec<ModelEntry>, AgentError>(AgentError::LlmAuth("sign in again".into()))
        })
        .await
        .unwrap_err();

        assert!(matches!(error, AgentError::LlmAuth(_)));
        assert!(cache.get().is_none());

        let discovered = vec![ModelEntry {
            id: "authenticated-model".into(),
            name: "authenticated-model".into(),
        }];
        let result = crate::resolve_models_catalog(&cache, async {
            Ok::<Vec<ModelEntry>, AgentError>(discovered.clone())
        })
        .await
        .unwrap();

        assert_eq!(result, discovered);
        assert_eq!(cache.get(), Some(&discovered));
    }

    #[test]
    fn configured_model_fallback_is_trimmed_and_singular() {
        // Unknown id: trimmed, and the raw id passes through as the name.
        assert_eq!(
            crate::configured_model_fallback("  configured-model  "),
            vec![ModelEntry {
                id: "configured-model".into(),
                name: "configured-model".into(),
            }]
        );
    }

    #[test]
    fn configured_model_fallback_curates_known_databricks_id() {
        // A configured Databricks id known to the manifest gets its curated
        // label; `id` stays the raw wire/config value.
        assert_eq!(
            crate::configured_model_fallback("databricks-gpt-5-5"),
            vec![ModelEntry {
                id: "databricks-gpt-5-5".into(),
                name: "GPT-5.5".into(),
            }]
        );
    }

    #[test]
    fn deepseek_invalid_key_auth_error_does_not_fall_back() {
        for error in [
            AgentError::LlmAuth("DeepSeek model catalog HTTP 401".into()),
            crate::catalog::classify_deepseek_catalog_error(AgentError::Llm(
                "DeepSeek model catalog failed HTTP 403: forbidden".into(),
            )),
        ] {
            let result = crate::deepseek_catalog_fallback("deepseek-chat", error);
            assert!(matches!(result, Err(AgentError::LlmAuth(_))));
        }
    }

    #[test]
    fn deepseek_catalog_outage_uses_configured_model_fallback() {
        let models = crate::deepseek_catalog_fallback(
            "deepseek-chat",
            AgentError::Llm("503: catalog unavailable".into()),
        )
        .unwrap();
        assert_eq!(
            models,
            vec![ModelEntry {
                id: "deepseek-chat".into(),
                name: "deepseek-chat".into(),
            }]
        );
    }
}
