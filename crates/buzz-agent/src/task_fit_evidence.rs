//! Structural validation for local Harbor task-fit reports.
//!
//! A validated report is still unauthenticated input. Validation recomputes
//! its aggregate metrics and case-set hash; it does not make the evidence
//! representative or eligible for routing.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use nostr::{Event, Kind};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_REPORT_BYTES: usize = 1024 * 1024;
const MAX_ROUTING_REPORTS: usize = 256;
const MAX_ROUTING_STORE_BYTES: usize = 16 * 1024 * 1024;
const MAX_ROUTE_ATTESTATION_BYTES: usize = 16 * 1024;
const MAX_ROUTE_ATTESTATIONS_PER_REPORT: usize = 512;
const ROUTE_ATTESTATION_KIND: u16 = 30078;
pub const TASK_FIT_REVIEW_PUBLIC_KEY_ENV: &str = "BUZZ_AGENT_TASK_FIT_REVIEW_PUBLIC_KEY";
const HARBOR_VERSION: &str = "0.16.1";
const EVIDENCE_KIND: &str = "harbor_verifier_outcomes";
const EVIDENCE_SCOPE: &str = "exact_single_agent_benchmark_condition";
const EVALUATOR_NAME: &str = "harbor_canonical_verifier_reward";
const REWARD_KEY: &str = "verifier_result.rewards.reward";
const TASK_CLASS_TAXONOMY_VERSION: &str = "operator-defined-v1";
const TASK_FIT_POLICY_VERSION: &str = "task-fit-outcomes-v1";
const MAX_TASK_FIT_AGE_SECONDS: u64 = 31_536_000;
const MAX_TASK_FIT_CASES: usize = 1_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskFitEvidenceError {
    TooLarge,
    InvalidJson(String),
    UnsupportedSchema(u32),
    Invalid(String),
    Store(String),
}

impl std::fmt::Display for TaskFitEvidenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => write!(f, "task-fit report exceeds {MAX_REPORT_BYTES} bytes"),
            Self::InvalidJson(error) => write!(f, "task-fit report JSON is invalid: {error}"),
            Self::UnsupportedSchema(version) => {
                write!(f, "task-fit report schema {version} is unsupported")
            }
            Self::Invalid(reason) => write!(f, "task-fit report is invalid: {reason}"),
            Self::Store(reason) => write!(f, "task-fit evidence store is invalid: {reason}"),
        }
    }
}

impl std::error::Error for TaskFitEvidenceError {}

/// A structurally validated report. This does not authenticate its producer.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedTaskFitReport {
    report_sha256: String,
    task_class: String,
    task_class_taxonomy_version: String,
    evaluation_policy_version: String,
    dataset: String,
    job_id: String,
    provider_id: String,
    model_id: String,
    endpoint_id: String,
    condition_id: String,
    manifest_sha256: String,
    generation: serde_json::Value,
    endpoint_config_sha256: String,
    prompt_sha256: String,
    runtime_binary_sha256: BTreeMap<String, String>,
    case_set_sha256: String,
    task_count: usize,
    task_success_count: usize,
    task_success_rate: f64,
    wilson_lower_bound_95: f64,
    created_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
    model_identity_observed: bool,
}

impl ValidatedTaskFitReport {
    pub fn report_sha256(&self) -> &str {
        &self.report_sha256
    }

    pub fn task_class(&self) -> &str {
        &self.task_class
    }

    pub fn task_class_taxonomy_version(&self) -> &str {
        &self.task_class_taxonomy_version
    }

    pub fn evaluation_policy_version(&self) -> &str {
        &self.evaluation_policy_version
    }

    pub fn dataset(&self) -> &str {
        &self.dataset
    }

    pub fn job_id(&self) -> &str {
        &self.job_id
    }

    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    pub fn endpoint_id(&self) -> &str {
        &self.endpoint_id
    }

    pub fn condition_id(&self) -> &str {
        &self.condition_id
    }

    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_sha256
    }

    pub fn generation(&self) -> &serde_json::Value {
        &self.generation
    }

    pub fn endpoint_config_sha256(&self) -> &str {
        &self.endpoint_config_sha256
    }

    pub fn prompt_sha256(&self) -> &str {
        &self.prompt_sha256
    }

    pub fn runtime_binary_sha256(&self) -> &BTreeMap<String, String> {
        &self.runtime_binary_sha256
    }

    pub fn case_set_sha256(&self) -> &str {
        &self.case_set_sha256
    }

    pub fn task_count(&self) -> usize {
        self.task_count
    }

    pub fn task_success_count(&self) -> usize {
        self.task_success_count
    }

    pub fn task_success_rate(&self) -> f64 {
        self.task_success_rate
    }

    pub fn wilson_lower_bound_95(&self) -> f64 {
        self.wilson_lower_bound_95
    }

    pub fn finished_at(&self) -> DateTime<Utc> {
        self.finished_at
    }

    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    /// True only when Harbor reported the exact configured model ID.
    pub fn model_identity_observed(&self) -> bool {
        self.model_identity_observed
    }
}

/// Hard task-fit thresholds for one explicitly named task class.
///
/// These thresholds only decide whether validated evidence is sufficient for
/// a route. They do not authenticate Harbor or prove that its source artifacts
/// were trustworthy.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskFitEligibilityPolicy {
    /// Task class to which this policy applies.
    pub task_class: String,
    /// Version of the task-class taxonomy.
    pub task_class_taxonomy_version: String,
    /// Version of the outcome-scoring rule.
    pub evaluation_policy_version: String,
    /// Minimum number of distinct task checksums required.
    pub minimum_distinct_tasks: usize,
    /// Minimum 95% Wilson lower bound, from 0.0 through 1.0.
    pub minimum_wilson_lower_bound_95: f64,
    /// Maximum age of a completed report, in seconds.
    pub maximum_age_seconds: u64,
    /// Require Harbor to have observed the exact configured model ID.
    #[serde(default)]
    pub require_observed_model_identity: bool,
}

impl TaskFitEligibilityPolicy {
    /// Validate bounded, supported task-fit thresholds.
    pub fn validate(&self) -> Result<(), TaskFitEvidenceError> {
        if !valid_id(&self.task_class)
            || self.task_class_taxonomy_version != TASK_CLASS_TAXONOMY_VERSION
            || self.evaluation_policy_version != TASK_FIT_POLICY_VERSION
        {
            return invalid("task-fit policy class or version is unsupported");
        }
        if !(1..=MAX_TASK_FIT_CASES).contains(&self.minimum_distinct_tasks)
            || !self.minimum_wilson_lower_bound_95.is_finite()
            || !(0.0..=1.0).contains(&self.minimum_wilson_lower_bound_95)
            || !(1..=MAX_TASK_FIT_AGE_SECONDS).contains(&self.maximum_age_seconds)
        {
            return invalid("task-fit policy threshold is outside its supported range");
        }
        Ok(())
    }
}

/// Live route identity compared with an operator-reviewed benchmark report.
/// The report hash preserves its full benchmark condition; this identity binds
/// the review to the active saved candidate and its provider/model.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskFitRouteIdentity {
    /// Buzz route-profile ID.
    pub profile_id: String,
    /// Buzz route-profile version.
    pub profile_version: u32,
    /// Hash of the launch-resolved route profile.
    pub profile_hash: String,
    /// Candidate ID within the route profile.
    pub candidate_id: String,
    /// Provider ID resolved by the active route candidate.
    pub provider_id: String,
    /// Exact model ID resolved by the active route candidate.
    pub model_id: String,
}

impl TaskFitRouteIdentity {
    /// Validate that all active route identity fields are present and bounded.
    pub fn validate(&self) -> Result<(), TaskFitEvidenceError> {
        if !valid_id(&self.profile_id)
            || self.profile_version == 0
            || !valid_sha256(&self.profile_hash)
            || !valid_id(&self.candidate_id)
            || self.provider_id.trim().is_empty()
            || self.model_id.trim().is_empty()
        {
            return invalid("task-fit route identity is incomplete or invalid");
        }
        Ok(())
    }
}

/// Local operator-reviewed binding from one report to one route candidate.
///
/// The caller must load this only after verifying the local Nostr attestation.
/// The binding records a review decision; it is not producer authentication.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskFitEvidenceBinding {
    /// Hash of the exact validated report.
    pub report_sha256: String,
    /// Route profile ID reviewed by the operator.
    pub profile_id: String,
    /// Route profile version reviewed by the operator.
    pub profile_version: u32,
    /// Launch-resolved route-profile hash reviewed by the operator.
    pub profile_hash: String,
    /// Candidate ID reviewed by the operator.
    pub candidate_id: String,
}

/// Signed payload that binds a report review to one saved route candidate.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskFitRouteAttestationPayload {
    pub schema_version: u32,
    pub report_sha256: String,
    pub action: String,
    pub task_class: String,
    pub task_class_taxonomy_version: String,
    pub binding: TaskFitEvidenceBinding,
}

/// Exact local profile identity used to find review attestations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskFitRouteProfileIdentity {
    pub profile_id: String,
    pub profile_version: u32,
    pub profile_hash: String,
}

/// Reports and route bindings verified from the local Buzz nest.
#[derive(Debug, Clone, Default)]
pub struct TaskFitEvidenceSnapshot {
    profile: Option<TaskFitRouteProfileIdentity>,
    reports: Vec<ValidatedTaskFitReport>,
    bindings: BTreeMap<(String, String), TaskFitEvidenceBinding>,
}

impl TaskFitEvidenceSnapshot {
    /// Load bounded report files and signatures for one current Buzz identity.
    /// Missing stores are an empty snapshot; malformed or oversized stores fail
    /// closed so routing can report unavailable evidence.
    pub fn load_local(
        nest_dir: &Path,
        profile: TaskFitRouteProfileIdentity,
        reviewer_public_key: &str,
    ) -> Result<Self, TaskFitEvidenceError> {
        if !valid_id(&profile.profile_id)
            || profile.profile_version == 0
            || !valid_sha256(&profile.profile_hash)
            || !valid_sha256(reviewer_public_key)
        {
            return invalid("task-fit route profile or reviewer identity is invalid");
        }
        let Some(report_dir) = task_fit_report_directory(nest_dir)? else {
            return Ok(Self {
                profile: Some(profile),
                ..Self::default()
            });
        };
        let mut snapshot = Self {
            profile: Some(profile.clone()),
            ..Self::default()
        };
        let entries = fs::read_dir(&report_dir)
            .map_err(|error| TaskFitEvidenceError::Store(error.to_string()))?;
        let mut report_count = 0usize;
        let mut total_bytes = 0usize;
        for entry in entries {
            let entry = entry.map_err(|error| TaskFitEvidenceError::Store(error.to_string()))?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            report_count += 1;
            if report_count > MAX_ROUTING_REPORTS {
                return Err(TaskFitEvidenceError::Store(
                    "more than 256 reports are present".into(),
                ));
            }
            let report_hash = path
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or_else(|| TaskFitEvidenceError::Store("invalid report file name".into()))?;
            if !valid_sha256(report_hash) {
                return Err(TaskFitEvidenceError::Store(
                    "report file name is not a SHA-256 hash".into(),
                ));
            }
            let bytes = read_store_file(&path, MAX_REPORT_BYTES)?;
            total_bytes = total_bytes
                .checked_add(bytes.len())
                .ok_or_else(|| TaskFitEvidenceError::Store("store size overflow".into()))?;
            if total_bytes > MAX_ROUTING_STORE_BYTES {
                return Err(TaskFitEvidenceError::Store(
                    "routing evidence exceeds 16 MiB".into(),
                ));
            }
            let report = validate_task_fit_report(&bytes)?;
            if report.report_sha256() != report_hash {
                return invalid("stored report hash does not match its file name");
            }
            let report_index = snapshot.reports.len();
            snapshot.reports.push(report);
            let report = &snapshot.reports[report_index];
            let Some(route_dir) = task_fit_route_attestation_directory(&report_dir, report_hash)?
            else {
                continue;
            };
            let attestations = fs::read_dir(&route_dir)
                .map_err(|error| TaskFitEvidenceError::Store(error.to_string()))?;
            let mut attestation_count = 0usize;
            for attestation in attestations {
                let attestation =
                    attestation.map_err(|error| TaskFitEvidenceError::Store(error.to_string()))?;
                let path = attestation.path();
                if path.extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                attestation_count += 1;
                if attestation_count > MAX_ROUTE_ATTESTATIONS_PER_REPORT {
                    return Err(TaskFitEvidenceError::Store(
                        "a report has more than 512 route attestations".into(),
                    ));
                }
                let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
                    return invalid("route attestation file name is invalid");
                };
                if !stem.ends_with(&format!("-{reviewer_public_key}")) {
                    continue;
                }
                let event_bytes = read_store_file(&path, MAX_ROUTE_ATTESTATION_BYTES)?;
                total_bytes = total_bytes
                    .checked_add(event_bytes.len())
                    .ok_or_else(|| TaskFitEvidenceError::Store("store size overflow".into()))?;
                if total_bytes > MAX_ROUTING_STORE_BYTES {
                    return Err(TaskFitEvidenceError::Store(
                        "routing evidence exceeds 16 MiB".into(),
                    ));
                }
                let event: Event = serde_json::from_slice(&event_bytes)
                    .map_err(|error| TaskFitEvidenceError::Store(error.to_string()))?;
                if event.pubkey.to_hex() != reviewer_public_key
                    || event.kind != Kind::Custom(ROUTE_ATTESTATION_KIND)
                {
                    return invalid("route attestation signer or event kind is invalid");
                }
                event
                    .verify()
                    .map_err(|error| TaskFitEvidenceError::Store(error.to_string()))?;
                let payload: TaskFitRouteAttestationPayload = serde_json::from_str(&event.content)
                    .map_err(|error| TaskFitEvidenceError::Store(error.to_string()))?;
                if payload.schema_version != 1
                    || payload.action != "reviewed_for_local_route_candidate"
                    || payload.report_sha256 != report.report_sha256()
                    || payload.task_class != report.task_class()
                    || payload.task_class_taxonomy_version != report.task_class_taxonomy_version()
                    || payload.binding.report_sha256 != report.report_sha256()
                {
                    return invalid("route attestation does not match its report");
                }
                if payload.binding.profile_id != profile.profile_id
                    || payload.binding.profile_version != profile.profile_version
                    || payload.binding.profile_hash != profile.profile_hash
                {
                    continue;
                }
                let expected_name = format!(
                    "{}-v{}-{}-{}-{}.json",
                    payload.binding.profile_id,
                    payload.binding.profile_version,
                    payload.binding.profile_hash,
                    payload.binding.candidate_id,
                    reviewer_public_key
                );
                if path.file_name().and_then(|value| value.to_str()) != Some(&expected_name) {
                    return invalid("route attestation identity does not match its file name");
                }
                snapshot.bindings.insert(
                    (
                        report.report_sha256().to_string(),
                        payload.binding.candidate_id.clone(),
                    ),
                    payload.binding,
                );
            }
        }
        snapshot
            .reports
            .sort_by_key(|report| std::cmp::Reverse(report.finished_at()));
        Ok(snapshot)
    }

    /// Evaluate the freshest signed report for one exact route candidate.
    pub fn evaluate_candidate(
        &self,
        candidate_id: &str,
        provider_id: &str,
        model_id: &str,
        policy: &TaskFitEligibilityPolicy,
        now: DateTime<Utc>,
    ) -> TaskFitEligibility {
        let Some(profile) = self.profile.as_ref() else {
            return TaskFitEligibility::Unknown(TaskFitUnknownReason::EvidenceStoreUnavailable);
        };
        let mut missing_review = false;
        let mut latest_decision = None;
        for report in self
            .reports
            .iter()
            .filter(|report| report.provider_id() == provider_id && report.model_id() == model_id)
        {
            let Some(binding) = self
                .bindings
                .get(&(report.report_sha256().to_string(), candidate_id.to_string()))
            else {
                missing_review |= report.task_class() == policy.task_class;
                continue;
            };
            let route = TaskFitRouteIdentity {
                profile_id: profile.profile_id.clone(),
                profile_version: profile.profile_version,
                profile_hash: profile.profile_hash.clone(),
                candidate_id: candidate_id.to_string(),
                provider_id: provider_id.to_string(),
                model_id: model_id.to_string(),
            };
            let decision =
                evaluate_task_fit_eligibility(Some(report), Some(binding), policy, &route, now);
            if decision == TaskFitEligibility::Qualified {
                return decision;
            }
            latest_decision.get_or_insert(decision);
        }
        latest_decision.unwrap_or({
            TaskFitEligibility::Unknown(if missing_review {
                TaskFitUnknownReason::MissingLocalReview
            } else {
                TaskFitUnknownReason::MissingReport
            })
        })
    }
}

fn task_fit_report_directory(nest_dir: &Path) -> Result<Option<PathBuf>, TaskFitEvidenceError> {
    let root = match nest_dir.canonicalize() {
        Ok(root) => root,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(TaskFitEvidenceError::Store(error.to_string())),
    };
    let mut path = root;
    for name in [".agents", "task-fit-evidence"] {
        path.push(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(TaskFitEvidenceError::Store(
                    "task-fit report directory must not be a symlink".into(),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(TaskFitEvidenceError::Store(error.to_string())),
        }
    }
    Ok(Some(path))
}

fn task_fit_route_attestation_directory(
    report_dir: &Path,
    report_sha256: &str,
) -> Result<Option<PathBuf>, TaskFitEvidenceError> {
    let mut path = report_dir.to_path_buf();
    for name in ["attestations", "routes", report_sha256] {
        path.push(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(TaskFitEvidenceError::Store(
                    "route attestation directory must not be a symlink".into(),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(TaskFitEvidenceError::Store(error.to_string())),
        }
    }
    Ok(Some(path))
}

fn read_store_file(path: &Path, maximum_bytes: usize) -> Result<Vec<u8>, TaskFitEvidenceError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| TaskFitEvidenceError::Store(error.to_string()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(TaskFitEvidenceError::Store(
            "task-fit evidence entries must be regular files".into(),
        ));
    }
    if metadata.len() > maximum_bytes as u64 {
        return Err(TaskFitEvidenceError::Store(
            "task-fit evidence entry exceeds its size limit".into(),
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    fs::File::open(path)
        .and_then(|file| {
            file.take((maximum_bytes + 1) as u64)
                .read_to_end(&mut bytes)
        })
        .map_err(|error| TaskFitEvidenceError::Store(error.to_string()))?;
    if bytes.len() > maximum_bytes {
        return Err(TaskFitEvidenceError::Store(
            "task-fit evidence entry exceeds its size limit".into(),
        ));
    }
    Ok(bytes)
}

/// Reason no usable task-fit conclusion can be drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskFitUnknownReason {
    /// The configured policy is invalid or unsupported.
    InvalidPolicy,
    /// The launch-resolved route identity is incomplete or malformed.
    InvalidRouteIdentity,
    /// No locally imported report was supplied.
    MissingReport,
    /// The local evidence store or expected reviewer identity is unavailable.
    EvidenceStoreUnavailable,
    /// No verified local operator binding was supplied.
    MissingLocalReview,
    /// The report is too old to support the configured policy.
    StaleReport,
    /// The report timestamp is later than the evaluation clock.
    FutureReport,
    /// Too few distinct cases were evaluated.
    InsufficientCases,
    /// The benchmark did not observe the exact model identity.
    ModelIdentityUnobserved,
    /// The report uses a different task class or supported policy version.
    DifferentTaskClass,
}

/// Reason exact route-bound evidence cannot satisfy a task-fit gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskFitRejectedReason {
    /// Report hash or local review binding does not match this route.
    BindingMismatch,
    /// The benchmarked provider/model differs from the active candidate.
    CandidateIdentityMismatch,
    /// Measured task-fit lower bound is below the configured floor.
    BelowQualityFloor,
}

/// Deterministic task-fit gate result. Only `Qualified` passes a hard gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskFitEligibility {
    /// Fresh, sufficiently sampled, exact-identity evidence meets the floor.
    Qualified,
    /// Evidence is missing or inconclusive; a hard gate must abstain.
    Unknown(TaskFitUnknownReason),
    /// Exact evidence contradicts this route or misses its quality floor.
    Rejected(TaskFitRejectedReason),
}

/// Evaluate a validated report against freshness, sampling, quality, and the
/// exact active profile candidate/provider/model. Only a signed local operator
/// binding for this report and candidate can qualify; the report's benchmark
/// condition remains provenance, not producer-authenticated evidence.
pub fn evaluate_task_fit_eligibility(
    report: Option<&ValidatedTaskFitReport>,
    binding: Option<&TaskFitEvidenceBinding>,
    policy: &TaskFitEligibilityPolicy,
    route: &TaskFitRouteIdentity,
    now: DateTime<Utc>,
) -> TaskFitEligibility {
    if policy.validate().is_err() {
        return TaskFitEligibility::Unknown(TaskFitUnknownReason::InvalidPolicy);
    }
    if route.validate().is_err() {
        return TaskFitEligibility::Unknown(TaskFitUnknownReason::InvalidRouteIdentity);
    }
    let Some(report) = report else {
        return TaskFitEligibility::Unknown(TaskFitUnknownReason::MissingReport);
    };
    let Some(binding) = binding else {
        return TaskFitEligibility::Unknown(TaskFitUnknownReason::MissingLocalReview);
    };
    if binding.report_sha256 != report.report_sha256()
        || binding.profile_id != route.profile_id
        || binding.profile_version != route.profile_version
        || binding.profile_hash != route.profile_hash
        || binding.candidate_id != route.candidate_id
        || !valid_id(&binding.profile_id)
        || binding.profile_version == 0
        || !valid_sha256(&binding.profile_hash)
    {
        return TaskFitEligibility::Rejected(TaskFitRejectedReason::BindingMismatch);
    }
    if report.task_class() != policy.task_class
        || report.task_class_taxonomy_version() != policy.task_class_taxonomy_version
        || report.evaluation_policy_version() != policy.evaluation_policy_version
    {
        return TaskFitEligibility::Unknown(TaskFitUnknownReason::DifferentTaskClass);
    }
    if report.finished_at() > now {
        return TaskFitEligibility::Unknown(TaskFitUnknownReason::FutureReport);
    }
    let age_seconds = now
        .signed_duration_since(report.finished_at())
        .num_seconds()
        .max(0) as u64;
    if age_seconds > policy.maximum_age_seconds {
        return TaskFitEligibility::Unknown(TaskFitUnknownReason::StaleReport);
    }
    if report.task_count() < policy.minimum_distinct_tasks {
        return TaskFitEligibility::Unknown(TaskFitUnknownReason::InsufficientCases);
    }
    if policy.require_observed_model_identity && !report.model_identity_observed() {
        return TaskFitEligibility::Unknown(TaskFitUnknownReason::ModelIdentityUnobserved);
    }
    if report.provider_id() != route.provider_id || report.model_id() != route.model_id {
        return TaskFitEligibility::Rejected(TaskFitRejectedReason::CandidateIdentityMismatch);
    }
    if report.wilson_lower_bound_95() < policy.minimum_wilson_lower_bound_95 {
        return TaskFitEligibility::Rejected(TaskFitRejectedReason::BelowQualityFloor);
    }
    TaskFitEligibility::Qualified
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReport {
    schema_version: u32,
    evidence_kind: String,
    evidence_scope: String,
    created_at_utc: DateTime<Utc>,
    task_class: RawTaskClass,
    source: RawSource,
    evaluator: RawEvaluator,
    candidate: RawCandidate,
    evaluation: RawEvaluation,
    routing_status: RawRoutingStatus,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTaskClass {
    id: String,
    taxonomy_version: String,
    classification_source: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSource {
    harness: String,
    version: String,
    job_id: String,
    job_result_sha256: String,
    dataset: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEvaluator {
    name: String,
    version: String,
    reward_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCandidate {
    provider_id: String,
    model_id: String,
    endpoint_id: String,
    condition_id: String,
    condition_sha256: String,
    prompt_sha256: String,
    generation: serde_json::Value,
    runtime_binary_sha256: BTreeMap<String, String>,
    endpoint_config_sha256: String,
    observed_model_ids: Vec<String>,
    model_identity_source: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEvaluation {
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
    attempts_per_task: usize,
    policy_version: String,
    sample_count: usize,
    task_name_count: usize,
    task_count: usize,
    case_set_sha256: String,
    pass_threshold: f64,
    trial_success_rule: String,
    trial_success_count: usize,
    trial_success_rate: f64,
    task_success_rule: String,
    task_success_count: usize,
    task_success_rate: f64,
    task_wilson_lower_bound_95: Option<f64>,
    mean_reward: f64,
    task_checksums: Vec<String>,
    trials: Vec<RawTrial>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawTrial {
    trial_name: String,
    task_name: String,
    task_checksum: String,
    reward: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRoutingStatus {
    eligible_for_routing: bool,
    reason: String,
}

/// Parse a bounded schema-v2 report and recompute its derived evidence.
///
/// This function validates internal consistency only. A report hash is not a
/// signature, and callers must apply freshness, exact candidate identity, task
/// taxonomy, and minimum-sample policy before considering any route decision.
pub fn validate_task_fit_report(
    bytes: &[u8],
) -> Result<ValidatedTaskFitReport, TaskFitEvidenceError> {
    if bytes.is_empty() || bytes.len() > MAX_REPORT_BYTES {
        return Err(TaskFitEvidenceError::TooLarge);
    }
    let report: RawReport = serde_json::from_slice(bytes)
        .map_err(|error| TaskFitEvidenceError::InvalidJson(error.to_string()))?;
    if report.schema_version != 2 {
        return Err(TaskFitEvidenceError::UnsupportedSchema(
            report.schema_version,
        ));
    }
    if report.evidence_kind != EVIDENCE_KIND || report.evidence_scope != EVIDENCE_SCOPE {
        return invalid("unsupported task-fit evidence kind or scope");
    }
    if report.task_class.classification_source != "operator_annotation"
        || report.task_class.taxonomy_version != TASK_CLASS_TAXONOMY_VERSION
        || !valid_id(&report.task_class.id)
    {
        return invalid("task class annotation is invalid");
    }
    if report.source.harness != "Harbor"
        || report.source.version != HARBOR_VERSION
        || report.source.job_id.is_empty()
        || report.source.dataset.is_empty()
        || !valid_sha256(&report.source.job_result_sha256)
    {
        return invalid("Harbor source identity is incomplete or unsupported");
    }
    if report.evaluator.name != EVALUATOR_NAME
        || report.evaluator.version != HARBOR_VERSION
        || report.evaluator.reward_key != REWARD_KEY
    {
        return invalid("verifier identity is incomplete or unsupported");
    }
    let candidate = report.candidate;
    if candidate.provider_id.trim().is_empty()
        || candidate.model_id.trim().is_empty()
        || candidate.endpoint_id.trim().is_empty()
        || candidate.condition_id.trim().is_empty()
        || !candidate.generation.is_object()
        || !valid_sha256(&candidate.condition_sha256)
        || !valid_sha256(&candidate.prompt_sha256)
        || !valid_sha256(&candidate.endpoint_config_sha256)
        || candidate.runtime_binary_sha256.is_empty()
        || candidate
            .runtime_binary_sha256
            .iter()
            .any(|(name, hash)| name.trim().is_empty() || !valid_sha256(hash))
    {
        return invalid("candidate configuration identity is incomplete");
    }
    if candidate
        .observed_model_ids
        .iter()
        .any(|model| model.trim().is_empty())
    {
        return invalid("observed model identity contains an empty value");
    }
    let model_identity_observed = candidate
        .observed_model_ids
        .iter()
        .any(|model| model == &candidate.model_id);
    let expected_identity_source = if candidate.observed_model_ids.is_empty() {
        "manifest_configuration_only"
    } else {
        "harbor_trial_result"
    };
    if candidate.model_identity_source != expected_identity_source {
        return invalid("model identity source does not match its observations");
    }

    let evaluation = report.evaluation;
    if evaluation.started_at > evaluation.finished_at
        || evaluation.attempts_per_task == 0
        || evaluation.sample_count == 0
        || !evaluation.pass_threshold.is_finite()
        || !(0.0..=1.0).contains(&evaluation.pass_threshold)
    {
        return invalid("evaluation timing or threshold is invalid");
    }
    if evaluation.policy_version != TASK_FIT_POLICY_VERSION {
        return invalid("scoring policy version is unsupported");
    }
    if report.routing_status.eligible_for_routing {
        return invalid("benchmark report cannot assert route eligibility");
    }
    if report.routing_status.reason.trim().is_empty() {
        return invalid("routing status reason is missing");
    }

    let mut seen_trials = BTreeSet::new();
    let mut labels = BTreeMap::<String, (String, usize)>::new();
    let mut cases = BTreeMap::<String, Vec<bool>>::new();
    let mut trial_success_count = 0usize;
    let mut reward_sum = 0.0;
    for trial in &evaluation.trials {
        if trial.trial_name.trim().is_empty()
            || trial.task_name.trim().is_empty()
            || !valid_sha256(&trial.task_checksum)
            || !trial.reward.is_finite()
            || !(0.0..=1.0).contains(&trial.reward)
            || !seen_trials.insert(trial.trial_name.as_str())
        {
            return invalid("trial identity, checksum, reward, or uniqueness is invalid");
        }
        let label = labels
            .entry(trial.task_name.clone())
            .or_insert_with(|| (trial.task_checksum.clone(), 0));
        if label.0 != trial.task_checksum {
            return invalid("one task label refers to inconsistent task checksums");
        }
        label.1 += 1;
        let passed = trial.reward >= evaluation.pass_threshold;
        trial_success_count += usize::from(passed);
        reward_sum += trial.reward;
        cases
            .entry(trial.task_checksum.clone())
            .or_default()
            .push(passed);
    }

    if evaluation.trials.len() != evaluation.sample_count
        || evaluation.task_name_count != labels.len()
        || evaluation.task_count != cases.len()
        || evaluation.trial_success_rule != "reward_at_or_above_threshold"
        || evaluation.task_success_rule != "all_repeats_for_unique_checksum_meet_threshold"
        || labels
            .values()
            .any(|(_, attempts)| *attempts != evaluation.attempts_per_task)
    {
        return invalid("trial and task denominators do not match the report");
    }

    let mut case_checksums: Vec<_> = cases.keys().cloned().collect();
    case_checksums.sort();
    if evaluation.task_checksums != case_checksums
        || !valid_sha256(&evaluation.case_set_sha256)
        || canonical_case_set_sha256(&case_checksums)? != evaluation.case_set_sha256
    {
        return invalid("task checksum list or case-set hash does not match trials");
    }
    let task_success_count = cases
        .values()
        .filter(|repeats| repeats.iter().all(|passed| *passed))
        .count();
    let task_count = cases.len();
    let wilson = lower_wilson_95(task_success_count, task_count);
    if evaluation.trial_success_count != trial_success_count
        || evaluation.task_success_count != task_success_count
        || !close_rate(
            evaluation.trial_success_rate,
            rate(trial_success_count, evaluation.sample_count),
        )
        || !close_rate(
            evaluation.task_success_rate,
            rate(task_success_count, task_count),
        )
        || evaluation
            .task_wilson_lower_bound_95
            .is_none_or(|reported| !close_rate(reported, wilson))
        || !evaluation.mean_reward.is_finite()
        || !close_rate(
            evaluation.mean_reward,
            round_six(reward_sum / evaluation.sample_count as f64),
        )
    {
        return invalid("reported outcome metrics do not match trial rewards");
    }

    Ok(ValidatedTaskFitReport {
        report_sha256: hex::encode(Sha256::digest(bytes)),
        task_class: report.task_class.id,
        task_class_taxonomy_version: report.task_class.taxonomy_version,
        evaluation_policy_version: evaluation.policy_version,
        dataset: report.source.dataset,
        job_id: report.source.job_id,
        provider_id: candidate.provider_id,
        model_id: candidate.model_id,
        endpoint_id: candidate.endpoint_id,
        condition_id: candidate.condition_id,
        manifest_sha256: candidate.condition_sha256,
        generation: candidate.generation,
        endpoint_config_sha256: candidate.endpoint_config_sha256,
        prompt_sha256: candidate.prompt_sha256,
        runtime_binary_sha256: candidate.runtime_binary_sha256,
        case_set_sha256: evaluation.case_set_sha256,
        task_count,
        task_success_count,
        task_success_rate: evaluation.task_success_rate,
        wilson_lower_bound_95: wilson,
        created_at: report.created_at_utc,
        finished_at: evaluation.finished_at,
        model_identity_observed,
    })
}

fn invalid<T>(reason: &str) -> Result<T, TaskFitEvidenceError> {
    Err(TaskFitEvidenceError::Invalid(reason.into()))
}

fn valid_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(byte))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn canonical_case_set_sha256(checksums: &[String]) -> Result<String, TaskFitEvidenceError> {
    let bytes = serde_json::to_vec(checksums)
        .map_err(|error| TaskFitEvidenceError::InvalidJson(error.to_string()))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn rate(successes: usize, samples: usize) -> f64 {
    round_six(successes as f64 / samples as f64)
}

fn round_six(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

fn close_rate(left: f64, right: f64) -> bool {
    left.is_finite() && (left - right).abs() <= 0.000001
}

fn lower_wilson_95(successes: usize, samples: usize) -> f64 {
    let n = samples as f64;
    let rate = successes as f64 / n;
    let z = 1.644_853_626_951_472_2_f64;
    let z2 = z * z;
    let denominator = 1.0 + z2 / n;
    let center = rate + z2 / (2.0 * n);
    let margin = z * (rate * (1.0 - rate) / n + z2 / (4.0 * n * n)).sqrt();
    round_six(((center - margin) / denominator).max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::{EventBuilder, Keys};
    use serde_json::{json, Value};

    fn valid_policy() -> TaskFitEligibilityPolicy {
        TaskFitEligibilityPolicy {
            task_class: "coding".into(),
            task_class_taxonomy_version: TASK_CLASS_TAXONOMY_VERSION.into(),
            evaluation_policy_version: TASK_FIT_POLICY_VERSION.into(),
            minimum_distinct_tasks: 1,
            minimum_wilson_lower_bound_95: 0.2,
            maximum_age_seconds: 3_600,
            require_observed_model_identity: true,
        }
    }

    fn validated_test_report() -> ValidatedTaskFitReport {
        validate_task_fit_report(&serde_json::to_vec(&valid_report()).expect("report JSON"))
            .expect("valid report")
    }

    fn valid_route(report: &ValidatedTaskFitReport) -> TaskFitRouteIdentity {
        TaskFitRouteIdentity {
            profile_id: "coding-route".into(),
            profile_version: 2,
            profile_hash: "9".repeat(64),
            candidate_id: "candidate-a".into(),
            provider_id: report.provider_id().into(),
            model_id: report.model_id().into(),
        }
    }

    fn valid_binding(
        report: &ValidatedTaskFitReport,
        route: &TaskFitRouteIdentity,
    ) -> TaskFitEvidenceBinding {
        TaskFitEvidenceBinding {
            report_sha256: report.report_sha256().into(),
            profile_id: route.profile_id.clone(),
            profile_version: route.profile_version,
            profile_hash: route.profile_hash.clone(),
            candidate_id: route.candidate_id.clone(),
        }
    }

    fn test_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-26T00:02:00Z")
            .expect("test timestamp")
            .with_timezone(&Utc)
    }

    fn valid_report() -> Value {
        let checksum = "a".repeat(64);
        let trial = |name: &str, label: &str| {
            json!({
                "trial_name": name,
                "task_name": label,
                "task_checksum": checksum,
                "reward": 1.0
            })
        };
        let case_set_sha256 =
            canonical_case_set_sha256(std::slice::from_ref(&checksum)).expect("case set hash");
        let wilson = lower_wilson_95(1, 1);
        json!({
            "schema_version": 2,
            "evidence_kind": EVIDENCE_KIND,
            "evidence_scope": EVIDENCE_SCOPE,
            "created_at_utc": "2026-09-26T00:02:00Z",
            "task_class": {"id": "coding", "taxonomy_version": TASK_CLASS_TAXONOMY_VERSION, "classification_source": "operator_annotation"},
            "source": {"harness": "Harbor", "version": HARBOR_VERSION, "job_id": "job-1", "job_result_sha256": "b".repeat(64), "dataset": "local/cases"},
            "evaluator": {"name": EVALUATOR_NAME, "version": HARBOR_VERSION, "reward_key": REWARD_KEY},
            "candidate": {
                "provider_id": "openai",
                "model_id": "model-r1",
                "endpoint_id": "endpoint-a",
                "condition_id": "solo-a",
                "condition_sha256": "c".repeat(64),
                "prompt_sha256": "d".repeat(64),
                "generation": {"temperature": 0},
                "runtime_binary_sha256": {"buzz-agent": "e".repeat(64)},
                "endpoint_config_sha256": "f".repeat(64),
                "observed_model_ids": ["model-r1"],
                "model_identity_source": "harbor_trial_result"
            },
            "evaluation": {
                "started_at": "2026-09-26T00:00:00Z",
                "finished_at": "2026-09-26T00:01:00Z",
                "attempts_per_task": 2,
                "policy_version": TASK_FIT_POLICY_VERSION,
                "sample_count": 4,
                "task_name_count": 2,
                "task_count": 1,
                "case_set_sha256": case_set_sha256,
                "pass_threshold": 1.0,
                "trial_success_rule": "reward_at_or_above_threshold",
                "trial_success_count": 4,
                "trial_success_rate": 1.0,
                "task_success_rule": "all_repeats_for_unique_checksum_meet_threshold",
                "task_success_count": 1,
                "task_success_rate": 1.0,
                "task_wilson_lower_bound_95": wilson,
                "mean_reward": 1.0,
                "task_checksums": [checksum],
                "trials": [trial("a-1", "task-a"), trial("a-2", "task-a"), trial("b-1", "task-b"), trial("b-2", "task-b")]
            },
            "routing_status": {"eligible_for_routing": false, "reason": "advisory report"}
        })
    }

    fn write_reviewed_report(root: &Path, keys: &Keys) -> (String, TaskFitRouteProfileIdentity) {
        let report_bytes = serde_json::to_vec(&valid_report()).expect("report JSON");
        let report = validate_task_fit_report(&report_bytes).expect("report validates");
        let report_hash = report.report_sha256().to_string();
        let profile = TaskFitRouteProfileIdentity {
            profile_id: "coding-route".into(),
            profile_version: 2,
            profile_hash: "9".repeat(64),
        };
        let binding = TaskFitEvidenceBinding {
            report_sha256: report_hash.clone(),
            profile_id: profile.profile_id.clone(),
            profile_version: profile.profile_version,
            profile_hash: profile.profile_hash.clone(),
            candidate_id: "candidate-a".into(),
        };
        let payload = TaskFitRouteAttestationPayload {
            schema_version: 1,
            report_sha256: report_hash.clone(),
            action: "reviewed_for_local_route_candidate".into(),
            task_class: report.task_class().into(),
            task_class_taxonomy_version: report.task_class_taxonomy_version().into(),
            binding,
        };
        let event = EventBuilder::new(
            Kind::Custom(ROUTE_ATTESTATION_KIND),
            serde_json::to_string(&payload).expect("attestation payload"),
        )
        .sign_with_keys(keys)
        .expect("sign route review");
        let public_key = keys.public_key().to_hex();
        let report_dir = root.join(".agents/task-fit-evidence");
        let route_dir = report_dir.join("attestations/routes").join(&report_hash);
        fs::create_dir_all(&route_dir).expect("create report store");
        fs::write(report_dir.join(format!("{report_hash}.json")), report_bytes)
            .expect("write report");
        fs::write(
            route_dir.join(format!(
                "{}-v{}-{}-candidate-a-{public_key}.json",
                profile.profile_id, profile.profile_version, profile.profile_hash
            )),
            serde_json::to_vec(&event).expect("event JSON"),
        )
        .expect("write route attestation");
        (report_hash, profile)
    }

    #[test]
    fn local_snapshot_requires_current_signed_profile_candidate_binding() {
        let root = tempfile::tempdir().expect("temporary nest");
        let keys = Keys::generate();
        let (report_hash, profile) = write_reviewed_report(root.path(), &keys);
        let snapshot = TaskFitEvidenceSnapshot::load_local(
            root.path(),
            profile.clone(),
            &keys.public_key().to_hex(),
        )
        .expect("load reviewed report");
        assert_eq!(
            snapshot.evaluate_candidate(
                "candidate-a",
                "openai",
                "model-r1",
                &valid_policy(),
                test_now(),
            ),
            TaskFitEligibility::Qualified
        );
        assert_eq!(
            snapshot.evaluate_candidate(
                "candidate-b",
                "openai",
                "model-r1",
                &valid_policy(),
                test_now(),
            ),
            TaskFitEligibility::Unknown(TaskFitUnknownReason::MissingLocalReview)
        );

        let other_identity = Keys::generate();
        let other_snapshot = TaskFitEvidenceSnapshot::load_local(
            root.path(),
            TaskFitRouteProfileIdentity {
                profile_id: "coding-route".into(),
                profile_version: 2,
                profile_hash: "9".repeat(64),
            },
            &other_identity.public_key().to_hex(),
        )
        .expect("load under another identity");
        assert_eq!(
            other_snapshot.evaluate_candidate(
                "candidate-a",
                "openai",
                "model-r1",
                &valid_policy(),
                test_now(),
            ),
            TaskFitEligibility::Unknown(TaskFitUnknownReason::MissingLocalReview)
        );

        let profile_changed = TaskFitEvidenceSnapshot::load_local(
            root.path(),
            TaskFitRouteProfileIdentity {
                profile_id: profile.profile_id,
                profile_version: profile.profile_version,
                profile_hash: "8".repeat(64),
            },
            &keys.public_key().to_hex(),
        )
        .expect("load under changed profile");
        assert_eq!(
            profile_changed.evaluate_candidate(
                "candidate-a",
                "openai",
                "model-r1",
                &valid_policy(),
                test_now(),
            ),
            TaskFitEligibility::Unknown(TaskFitUnknownReason::MissingLocalReview)
        );
        assert_eq!(report_hash.len(), 64);
    }

    #[test]
    fn local_snapshot_rejects_symlinked_report_store_and_tampered_signatures() {
        let outside = tempfile::tempdir().expect("outside directory");
        let root = tempfile::tempdir().expect("temporary nest");
        let keys = Keys::generate();
        let (_report_hash, profile) = write_reviewed_report(outside.path(), &keys);
        fs::create_dir_all(root.path().join(".agents")).expect("create agents directory");
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            outside.path().join(".agents/task-fit-evidence"),
            root.path().join(".agents/task-fit-evidence"),
        )
        .expect("create report-store symlink");
        #[cfg(unix)]
        assert!(matches!(
            TaskFitEvidenceSnapshot::load_local(
                root.path(),
                profile.clone(),
                &keys.public_key().to_hex()
            ),
            Err(TaskFitEvidenceError::Store(_))
        ));

        let clean_root = tempfile::tempdir().expect("clean temporary nest");
        let (report_hash, clean_profile) = write_reviewed_report(clean_root.path(), &keys);
        let attestations = clean_root
            .path()
            .join(".agents/task-fit-evidence/attestations/routes")
            .join(&report_hash);
        let path = fs::read_dir(&attestations)
            .expect("attestation directory")
            .next()
            .expect("attestation entry")
            .expect("attestation path")
            .path();
        let mut event: Event =
            serde_json::from_slice(&fs::read(&path).expect("event bytes")).expect("event JSON");
        event.content.push(' ');
        fs::write(
            &path,
            serde_json::to_vec(&event).expect("tampered event JSON"),
        )
        .expect("replace event");
        assert!(matches!(
            TaskFitEvidenceSnapshot::load_local(
                clean_root.path(),
                clean_profile,
                &keys.public_key().to_hex()
            ),
            Err(TaskFitEvidenceError::Store(_))
        ));
    }

    #[test]
    fn validates_and_recomputes_checksum_deduplicated_outcomes() {
        let bytes = serde_json::to_vec(&valid_report()).expect("report JSON");
        let report = validate_task_fit_report(&bytes).expect("valid report");

        assert_eq!(report.task_class(), "coding");
        assert_eq!(
            report.task_class_taxonomy_version(),
            TASK_CLASS_TAXONOMY_VERSION
        );
        assert_eq!(report.evaluation_policy_version(), TASK_FIT_POLICY_VERSION);
        assert_eq!(report.provider_id(), "openai");
        assert_eq!(report.model_id(), "model-r1");
        assert_eq!(report.endpoint_id(), "endpoint-a");
        assert_eq!(report.condition_id(), "solo-a");
        assert!(report.manifest_sha256().bytes().all(|byte| byte == b'c'));
        assert!(report.generation().is_object());
        assert_eq!(report.dataset(), "local/cases");
        assert_eq!(report.job_id(), "job-1");
        assert_eq!(report.task_count(), 1);
        assert_eq!(report.task_success_count(), 1);
        assert!(report.model_identity_observed());
        assert_eq!(report.report_sha256(), hex::encode(Sha256::digest(&bytes)));
    }

    #[test]
    fn accepts_report_from_python_producer_fixture() {
        let report =
            validate_task_fit_report(include_bytes!("../testdata/harbor-task-fit-v2.json"))
                .expect("Python producer report");

        assert_eq!(report.task_class(), "coding");
        assert_eq!(report.task_class_taxonomy_version(), "operator-defined-v1");
        assert_eq!(report.evaluation_policy_version(), "task-fit-outcomes-v1");
        assert_eq!(report.provider_id(), "openai");
        assert_eq!(report.model_id(), "fixture-model-r1");
        assert_eq!(report.endpoint_id(), "fixture-endpoint");
        assert_eq!(report.condition_id(), "fixture-solo-v1");
        assert_eq!(report.task_count(), 1);
        assert_eq!(report.task_success_count(), 1);
        assert!(report.model_identity_observed());
    }

    #[test]
    fn task_fit_gate_requires_fresh_exactly_bound_evidence() {
        let report = validated_test_report();
        let route = valid_route(&report);
        let binding = valid_binding(&report, &route);

        assert_eq!(
            evaluate_task_fit_eligibility(
                Some(&report),
                Some(&binding),
                &valid_policy(),
                &route,
                test_now(),
            ),
            TaskFitEligibility::Qualified
        );
        assert_eq!(
            evaluate_task_fit_eligibility(
                None,
                Some(&binding),
                &valid_policy(),
                &route,
                test_now(),
            ),
            TaskFitEligibility::Unknown(TaskFitUnknownReason::MissingReport)
        );
        assert_eq!(
            evaluate_task_fit_eligibility(Some(&report), None, &valid_policy(), &route, test_now(),),
            TaskFitEligibility::Unknown(TaskFitUnknownReason::MissingLocalReview)
        );
    }

    #[test]
    fn task_fit_gate_distinguishes_inconclusive_from_failed_quality() {
        let report = validated_test_report();
        let route = valid_route(&report);
        let binding = valid_binding(&report, &route);
        let now = test_now();

        let mut policy = valid_policy();
        policy.minimum_distinct_tasks = 2;
        assert_eq!(
            evaluate_task_fit_eligibility(Some(&report), Some(&binding), &policy, &route, now,),
            TaskFitEligibility::Unknown(TaskFitUnknownReason::InsufficientCases)
        );

        policy = valid_policy();
        policy.minimum_wilson_lower_bound_95 = 0.9;
        assert_eq!(
            evaluate_task_fit_eligibility(Some(&report), Some(&binding), &policy, &route, now,),
            TaskFitEligibility::Rejected(TaskFitRejectedReason::BelowQualityFloor)
        );

        policy = valid_policy();
        policy.maximum_age_seconds = 1;
        assert_eq!(
            evaluate_task_fit_eligibility(
                Some(&report),
                Some(&binding),
                &policy,
                &route,
                now + chrono::Duration::seconds(3),
            ),
            TaskFitEligibility::Unknown(TaskFitUnknownReason::StaleReport)
        );
        assert_eq!(
            evaluate_task_fit_eligibility(
                Some(&report),
                Some(&binding),
                &valid_policy(),
                &route,
                now - chrono::Duration::minutes(2),
            ),
            TaskFitEligibility::Unknown(TaskFitUnknownReason::FutureReport)
        );
    }

    #[test]
    fn task_fit_gate_rejects_route_and_review_binding_mismatches() {
        let report = validated_test_report();
        let route = valid_route(&report);
        let mut binding = valid_binding(&report, &route);
        binding.profile_version += 1;
        assert_eq!(
            evaluate_task_fit_eligibility(
                Some(&report),
                Some(&binding),
                &valid_policy(),
                &route,
                test_now(),
            ),
            TaskFitEligibility::Rejected(TaskFitRejectedReason::BindingMismatch)
        );

        let binding = valid_binding(&report, &route);
        for field in ["provider", "model"] {
            let mut wrong_route = route.clone();
            match field {
                "provider" => wrong_route.provider_id.push_str("-other"),
                "model" => wrong_route.model_id.push_str("-other"),
                _ => unreachable!("test case is enumerated"),
            }
            assert_eq!(
                evaluate_task_fit_eligibility(
                    Some(&report),
                    Some(&binding),
                    &valid_policy(),
                    &wrong_route,
                    test_now(),
                ),
                TaskFitEligibility::Rejected(TaskFitRejectedReason::CandidateIdentityMismatch),
                "route mismatch should fail for {field}"
            );
        }
    }

    #[test]
    fn task_fit_gate_reports_invalid_policy_and_route_identity_as_unknown() {
        let report = validated_test_report();
        let route = valid_route(&report);
        let binding = valid_binding(&report, &route);
        let mut policy = valid_policy();
        policy.maximum_age_seconds = 0;
        assert_eq!(
            evaluate_task_fit_eligibility(
                Some(&report),
                Some(&binding),
                &policy,
                &route,
                test_now(),
            ),
            TaskFitEligibility::Unknown(TaskFitUnknownReason::InvalidPolicy)
        );

        let mut invalid_route = route;
        invalid_route.model_id.clear();
        assert_eq!(
            evaluate_task_fit_eligibility(
                Some(&report),
                Some(&binding),
                &valid_policy(),
                &invalid_route,
                test_now(),
            ),
            TaskFitEligibility::Unknown(TaskFitUnknownReason::InvalidRouteIdentity)
        );
    }

    #[test]
    fn task_fit_gate_keeps_unobserved_model_identity_unknown() {
        let mut report_json = valid_report();
        report_json["candidate"]["observed_model_ids"] = json!([]);
        report_json["candidate"]["model_identity_source"] = json!("manifest_configuration_only");
        let report =
            validate_task_fit_report(&serde_json::to_vec(&report_json).expect("report JSON"))
                .expect("valid report without observed model identity");
        let route = valid_route(&report);
        let binding = valid_binding(&report, &route);

        assert_eq!(
            evaluate_task_fit_eligibility(
                Some(&report),
                Some(&binding),
                &valid_policy(),
                &route,
                test_now(),
            ),
            TaskFitEligibility::Unknown(TaskFitUnknownReason::ModelIdentityUnobserved)
        );
    }

    #[test]
    fn rejects_claimed_eligibility_and_tampered_derived_metrics() {
        let mut report = valid_report();
        report["routing_status"]["eligible_for_routing"] = json!(true);
        let bytes = serde_json::to_vec(&report).expect("report JSON");
        assert!(matches!(
            validate_task_fit_report(&bytes),
            Err(TaskFitEvidenceError::Invalid(reason))
                if reason.contains("cannot assert route eligibility")
        ));

        report["routing_status"]["eligible_for_routing"] = json!(false);
        report["evaluation"]["task_success_count"] = json!(0);
        let bytes = serde_json::to_vec(&report).expect("report JSON");
        assert!(matches!(
            validate_task_fit_report(&bytes),
            Err(TaskFitEvidenceError::Invalid(reason))
                if reason.contains("outcome metrics")
        ));
    }

    #[test]
    fn leaves_model_identity_unobserved_when_report_only_has_configuration() {
        let mut report = valid_report();
        report["candidate"]["observed_model_ids"] = json!([]);
        report["candidate"]["model_identity_source"] = json!("manifest_configuration_only");
        let bytes = serde_json::to_vec(&report).expect("report JSON");

        let validated = validate_task_fit_report(&bytes).expect("valid report");

        assert!(!validated.model_identity_observed());
    }

    #[test]
    fn rejects_unknown_task_taxonomy_and_scoring_policy_versions() {
        let mut report = valid_report();
        report["task_class"]["taxonomy_version"] = json!("unknown-v9");
        let bytes = serde_json::to_vec(&report).expect("report JSON");
        assert!(matches!(
            validate_task_fit_report(&bytes),
            Err(TaskFitEvidenceError::Invalid(reason))
                if reason.contains("task class annotation")
        ));

        report = valid_report();
        report["evaluation"]["policy_version"] = json!("unknown-v9");
        let bytes = serde_json::to_vec(&report).expect("report JSON");
        assert!(matches!(
            validate_task_fit_report(&bytes),
            Err(TaskFitEvidenceError::Invalid(reason))
                if reason.contains("scoring policy version")
        ));
    }

    #[test]
    fn rejects_case_set_mismatch_unknown_fields_and_oversize() {
        let mut report = valid_report();
        report["evaluation"]["case_set_sha256"] = json!("0".repeat(64));
        let bytes = serde_json::to_vec(&report).expect("report JSON");
        assert!(matches!(
            validate_task_fit_report(&bytes),
            Err(TaskFitEvidenceError::Invalid(reason))
                if reason.contains("case-set hash")
        ));

        let mut report = valid_report();
        report["evaluation"]["trials"][1]["task_checksum"] = json!("0".repeat(64));
        let bytes = serde_json::to_vec(&report).expect("report JSON");
        assert!(matches!(
            validate_task_fit_report(&bytes),
            Err(TaskFitEvidenceError::Invalid(reason))
                if reason.contains("inconsistent task checksums")
        ));

        let mut report = valid_report();
        report["untrusted"] = json!(true);
        let bytes = serde_json::to_vec(&report).expect("report JSON");
        assert!(matches!(
            validate_task_fit_report(&bytes),
            Err(TaskFitEvidenceError::InvalidJson(_))
        ));
        let mut report = valid_report();
        report["schema_version"] = json!(1);
        let bytes = serde_json::to_vec(&report).expect("report JSON");
        assert_eq!(
            validate_task_fit_report(&bytes),
            Err(TaskFitEvidenceError::UnsupportedSchema(1))
        );
        assert_eq!(
            validate_task_fit_report(&vec![b' '; MAX_REPORT_BYTES + 1]),
            Err(TaskFitEvidenceError::TooLarge)
        );
    }
}
