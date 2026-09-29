//! Local review shelf for structurally validated Harbor task-fit reports.

use buzz_agent_pkg::task_fit_evidence::{
    TaskFitEvidenceBinding, TaskFitRouteAttestationPayload, ValidatedTaskFitReport,
    validate_task_fit_report,
};
use nostr::{Event, EventBuilder, Keys, Kind};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

use crate::{app_state::AppState, managed_agents::nest_dir};
use tauri::State;

const REPORT_DIR: &str = "task-fit-evidence";
const MAX_REPORT_BYTES: usize = 1024 * 1024;
const MAX_REPORTS: usize = 256;
const MAX_ROUTE_ATTESTATIONS_PER_REPORT: usize = 512;
const ATTESTATION_KIND: u16 = 30078;
const MAX_ATTESTATION_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentTaskFitEvidenceSummary {
    pub report_sha256: String,
    pub task_class: String,
    pub task_class_taxonomy_version: String,
    pub evaluation_policy_version: String,
    pub dataset: String,
    pub job_id: String,
    pub provider_id: String,
    pub model_id: String,
    pub endpoint_id: String,
    pub condition_id: String,
    pub manifest_sha256: String,
    pub generation: serde_json::Value,
    pub endpoint_config_sha256: String,
    pub prompt_sha256: String,
    pub runtime_binary_sha256: std::collections::BTreeMap<String, String>,
    pub case_set_sha256: String,
    pub task_count: usize,
    pub task_success_count: usize,
    pub task_success_rate: f64,
    pub wilson_lower_bound_95: f64,
    pub created_at: String,
    pub finished_at: String,
    pub model_identity_observed: bool,
    pub local_attestation: Option<AgentTaskFitLocalAttestation>,
    pub route_attestations: Vec<AgentTaskFitRouteAttestation>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentTaskFitLocalAttestation {
    pub public_key: String,
    pub event_id: String,
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentTaskFitRouteAttestation {
    pub public_key: String,
    pub event_id: String,
    pub created_at: u64,
    pub profile_id: String,
    pub profile_version: u32,
    pub profile_hash: String,
    pub candidate_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LocalAttestationPayload {
    schema_version: u32,
    report_sha256: String,
    action: String,
    task_class: String,
    task_class_taxonomy_version: String,
}

impl From<&ValidatedTaskFitReport> for AgentTaskFitEvidenceSummary {
    fn from(report: &ValidatedTaskFitReport) -> Self {
        Self {
            report_sha256: report.report_sha256().to_string(),
            task_class: report.task_class().to_string(),
            task_class_taxonomy_version: report.task_class_taxonomy_version().to_string(),
            evaluation_policy_version: report.evaluation_policy_version().to_string(),
            dataset: report.dataset().to_string(),
            job_id: report.job_id().to_string(),
            provider_id: report.provider_id().to_string(),
            model_id: report.model_id().to_string(),
            endpoint_id: report.endpoint_id().to_string(),
            condition_id: report.condition_id().to_string(),
            manifest_sha256: report.manifest_sha256().to_string(),
            generation: report.generation().clone(),
            endpoint_config_sha256: report.endpoint_config_sha256().to_string(),
            prompt_sha256: report.prompt_sha256().to_string(),
            runtime_binary_sha256: report.runtime_binary_sha256().clone(),
            case_set_sha256: report.case_set_sha256().to_string(),
            task_count: report.task_count(),
            task_success_count: report.task_success_count(),
            task_success_rate: report.task_success_rate(),
            wilson_lower_bound_95: report.wilson_lower_bound_95(),
            created_at: report.created_at().to_rfc3339(),
            finished_at: report.finished_at().to_rfc3339(),
            model_identity_observed: report.model_identity_observed(),
            local_attestation: None,
            route_attestations: Vec::new(),
        }
    }
}

fn ensure_real_directory(root: &Path, relative: &Path, create: bool) -> Result<PathBuf, String> {
    if root
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink() || !metadata.is_dir())
    {
        return Err("Buzz workspace must be a real directory.".into());
    }
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return Err("Invalid task-fit report directory path.".into());
        };
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "Refusing to follow a symlink at {}.",
                    current.display()
                ));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(format!("Expected a directory at {}.", current.display()));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
                fs::create_dir(&current)
                    .map_err(|error| format!("Create {}: {error}", current.display()))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&current, fs::Permissions::from_mode(0o700)).map_err(
                        |error| format!("Restrict {} permissions: {error}", current.display()),
                    )?;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(current),
            Err(error) => return Err(format!("Inspect {}: {error}", current.display())),
        }
    }
    Ok(current)
}

fn validate_report(bytes: &[u8]) -> Result<ValidatedTaskFitReport, String> {
    validate_task_fit_report(bytes).map_err(|error| error.to_string())
}

fn report_path(root: &Path, report_sha256: &str) -> Result<PathBuf, String> {
    if report_sha256.len() != 64
        || !report_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("Report hash must be a lowercase SHA-256 value.".into());
    }
    Ok(root.join(format!("{report_sha256}.json")))
}

fn attestation_path(
    report_directory: &Path,
    report_sha256: &str,
    public_key: &str,
) -> Result<PathBuf, String> {
    let _ = report_path(report_directory, report_sha256)?;
    if public_key.len() != 64
        || !public_key
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("Attestation identity must be a lowercase public-key hex value.".into());
    }
    let directory = ensure_real_directory(report_directory, Path::new("attestations"), false)?;
    Ok(directory.join(format!("{report_sha256}-{public_key}.json")))
}

fn route_attestation_path(
    report_directory: &Path,
    binding: &TaskFitEvidenceBinding,
    public_key: &str,
    create: bool,
) -> Result<PathBuf, String> {
    let _ = report_path(report_directory, &binding.report_sha256)?;
    let valid_id = |value: &str| {
        !value.is_empty()
            && value.len() <= 64
            && value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            && !value.ends_with('-')
            && !value.contains("--")
    };
    if !valid_id(&binding.profile_id)
        || binding.profile_version == 0
        || binding.profile_hash.len() != 64
        || !binding
            .profile_hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || !valid_id(&binding.candidate_id)
    {
        return Err("Route binding identity is invalid.".into());
    }
    if public_key.len() != 64
        || !public_key
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("Attestation identity must be a lowercase public-key hex value.".into());
    }
    let directory = ensure_real_directory(
        report_directory,
        &Path::new("attestations")
            .join("routes")
            .join(&binding.report_sha256),
        create,
    )?;
    Ok(directory.join(format!(
        "{}-v{}-{}-{}-{}.json",
        binding.profile_id,
        binding.profile_version,
        binding.profile_hash,
        binding.candidate_id,
        public_key
    )))
}

fn read_report_file(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Inspect task-fit report file: {error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("Task-fit report must be a regular file.".into());
    }
    if metadata.len() > MAX_REPORT_BYTES as u64 {
        return Err("Task-fit report exceeds the 1 MiB limit.".into());
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options
        .open(path)
        .map_err(|error| format!("Open task-fit report file: {error}"))?;
    let mut bytes = Vec::new();
    file.take((MAX_REPORT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Read task-fit report file: {error}"))?;
    if bytes.len() > MAX_REPORT_BYTES {
        return Err("Task-fit report exceeds the 1 MiB limit.".into());
    }
    Ok(Some(bytes))
}

fn write_report_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or("Task-fit report path has no parent directory.")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| format!("Create task-fit report temporary file: {error}"))?;
    temp.write_all(bytes)
        .map_err(|error| format!("Write task-fit report temporary file: {error}"))?;
    temp.as_file()
        .sync_all()
        .map_err(|error| format!("Sync task-fit report temporary file: {error}"))?;
    temp.persist_noclobber(path)
        .map(|_| ())
        .map_err(|error| format!("Store task-fit report without overwriting: {}", error.error))
}

fn read_attestation(path: &Path) -> Result<Option<Event>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Inspect local task-fit attestation: {error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("Local task-fit attestation must be a regular file.".into());
    }
    if metadata.len() > MAX_ATTESTATION_BYTES as u64 {
        return Err("Local task-fit attestation exceeds the size limit.".into());
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options
        .open(path)
        .map_err(|error| format!("Open local task-fit attestation: {error}"))?;
    let mut bytes = Vec::new();
    file.take((MAX_ATTESTATION_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Read local task-fit attestation: {error}"))?;
    if bytes.len() > MAX_ATTESTATION_BYTES {
        return Err("Local task-fit attestation exceeds the size limit.".into());
    }
    let event: Event = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Invalid local task-fit attestation: {error}"))?;
    event
        .verify()
        .map_err(|error| format!("Local task-fit attestation signature is invalid: {error}"))?;
    if event.kind != Kind::Custom(ATTESTATION_KIND) {
        return Err("Local task-fit attestation has an unsupported event kind.".into());
    }
    Ok(Some(event))
}

fn validate_attestation_for_report(
    event: &Event,
    report: &ValidatedTaskFitReport,
) -> Result<(), String> {
    let payload: LocalAttestationPayload = serde_json::from_str(&event.content)
        .map_err(|error| format!("Invalid local task-fit attestation content: {error}"))?;
    if payload.schema_version != 1
        || payload.report_sha256 != report.report_sha256()
        || payload.action != "reviewed_for_local_routing"
        || payload.task_class != report.task_class()
        || payload.task_class_taxonomy_version != report.task_class_taxonomy_version()
    {
        return Err("Local task-fit attestation does not match the reviewed report.".into());
    }
    Ok(())
}

fn route_binding_for_report(
    report: &ValidatedTaskFitReport,
    profile: &crate::managed_agents::agent_route_profile::ResolvedAgentRouteProfile,
    candidate_id: &str,
) -> Result<TaskFitEvidenceBinding, String> {
    let serialized_document = serde_json::to_string(&profile.document)
        .map_err(|error| format!("Serialize resolved route profile: {error}"))?;
    let resolved_hash = hex::encode(Sha256::digest(serialized_document.as_bytes()));
    if serialized_document != profile.serialized_document || resolved_hash != profile.identity.hash
    {
        return Err("Resolved route profile document does not match its identity hash.".into());
    }
    profile
        .document
        .validate()
        .map_err(|error| format!("Resolved route profile is invalid: {error}"))?;
    if profile.document.profile_id.as_deref() != Some(profile.identity.id.as_str())
        || profile.document.profile_version != Some(profile.identity.version)
        || profile.identity.hash.len() != 64
        || !profile
            .identity
            .hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("Resolved route profile identity is inconsistent.".into());
    }
    let policy = profile
        .document
        .task_fit_policy
        .as_ref()
        .ok_or("Enable task-fit policy on this route profile before binding evidence.")?;
    policy
        .validate()
        .map_err(|error| format!("Invalid route task-fit policy: {error}"))?;
    if policy.task_class != report.task_class()
        || policy.task_class_taxonomy_version != report.task_class_taxonomy_version()
        || policy.evaluation_policy_version != report.evaluation_policy_version()
    {
        return Err("The report does not match this profile's task-fit policy.".into());
    }
    let candidate = profile
        .document
        .candidates
        .iter()
        .find(|candidate| candidate.id == candidate_id)
        .ok_or("Route candidate was not found in the resolved profile.")?;
    if candidate.provider != report.provider_id() || candidate.model != report.model_id() {
        return Err("The report provider/model does not match this route candidate.".into());
    }
    Ok(TaskFitEvidenceBinding {
        report_sha256: report.report_sha256().to_string(),
        profile_id: profile.identity.id.clone(),
        profile_version: profile.identity.version,
        profile_hash: profile.identity.hash.clone(),
        candidate_id: candidate.id.clone(),
    })
}

fn route_binding_from_event(
    event: &Event,
    report: &ValidatedTaskFitReport,
) -> Result<TaskFitEvidenceBinding, String> {
    let payload: TaskFitRouteAttestationPayload = serde_json::from_str(&event.content)
        .map_err(|error| format!("Invalid route-bound task-fit attestation content: {error}"))?;
    if payload.schema_version != 1
        || payload.report_sha256 != report.report_sha256()
        || payload.action != "reviewed_for_local_route_candidate"
        || payload.task_class != report.task_class()
        || payload.task_class_taxonomy_version != report.task_class_taxonomy_version()
        || payload.binding.report_sha256 != report.report_sha256()
    {
        return Err("Route-bound attestation does not match the reviewed report.".into());
    }
    Ok(payload.binding)
}

fn local_attestation_summary(event: &Event) -> AgentTaskFitLocalAttestation {
    AgentTaskFitLocalAttestation {
        public_key: event.pubkey.to_hex(),
        event_id: event.id.to_hex(),
        created_at: event.created_at.as_secs(),
    }
}

fn route_attestation_summary(
    event: &Event,
    binding: TaskFitEvidenceBinding,
) -> AgentTaskFitRouteAttestation {
    AgentTaskFitRouteAttestation {
        public_key: event.pubkey.to_hex(),
        event_id: event.id.to_hex(),
        created_at: event.created_at.as_secs(),
        profile_id: binding.profile_id,
        profile_version: binding.profile_version,
        profile_hash: binding.profile_hash,
        candidate_id: binding.candidate_id,
    }
}

fn list_route_attestations_at(
    report_directory: &Path,
    report: &ValidatedTaskFitReport,
    current_public_key: Option<&str>,
) -> Result<Vec<AgentTaskFitRouteAttestation>, String> {
    let Some(public_key) = current_public_key else {
        return Ok(Vec::new());
    };
    let directory = ensure_real_directory(
        report_directory,
        &Path::new("attestations")
            .join("routes")
            .join(report.report_sha256()),
        false,
    )?;
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("List route-bound task-fit attestations: {error}")),
    };
    let mut attestations = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("Read route attestation entry: {error}"))?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        if attestations.len() >= MAX_ROUTE_ATTESTATIONS_PER_REPORT {
            return Err("Task-fit report exceeds 512 route attestations.".into());
        }
        let Some(event) = read_attestation(&path)? else {
            return Err("Route-bound task-fit attestation disappeared while listing.".into());
        };
        if event.pubkey.to_hex() != public_key {
            continue;
        }
        let binding = route_binding_from_event(&event, report)?;
        if route_attestation_path(report_directory, &binding, public_key, false)? != path {
            return Err("Route-bound attestation identity does not match its file name.".into());
        }
        attestations.push(route_attestation_summary(&event, binding));
    }
    attestations.sort_by(|left, right| {
        left.profile_id
            .cmp(&right.profile_id)
            .then(left.candidate_id.cmp(&right.candidate_id))
    });
    Ok(attestations)
}

fn lock_writes(root: &Path) -> Result<fs::File, String> {
    let agents_dir = ensure_real_directory(root, Path::new(".agents"), true)?;
    let lock_path = agents_dir.join(".task-fit-evidence-write.lock");
    if fs::symlink_metadata(&lock_path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err("Refusing to use a symlink as the task-fit evidence lock.".into());
    }
    let mut options = fs::OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        options.mode(0o600);
    }
    let lock = options
        .open(lock_path)
        .map_err(|error| format!("Open task-fit evidence lock: {error}"))?;
    if !lock
        .metadata()
        .map_err(|error| format!("Inspect task-fit evidence lock: {error}"))?
        .is_file()
    {
        return Err("Task-fit evidence lock must be a regular file.".into());
    }
    lock.lock()
        .map_err(|error| format!("Lock task-fit evidence: {error}"))?;
    Ok(lock)
}

fn import_report_at(
    root: &Path,
    bytes: &[u8],
    expected_report_sha256: &str,
) -> Result<AgentTaskFitEvidenceSummary, String> {
    let report = validate_report(bytes)?;
    if report.report_sha256() != expected_report_sha256 {
        return Err("The report changed after review. Inspect it again before importing.".into());
    }
    let _lock = lock_writes(root)?;
    let directory =
        ensure_real_directory(root, Path::new(".agents").join(REPORT_DIR).as_path(), true)?;
    let path = report_path(&directory, report.report_sha256())?;
    match read_report_file(&path)? {
        Some(existing) if existing == bytes => {}
        Some(_) => return Err("A different report already uses this SHA-256 identity.".into()),
        None => {
            write_report_file(&path, bytes)?;
        }
    }
    Ok((&report).into())
}

fn list_reports_at(
    root: &Path,
    current_public_key: Option<&str>,
) -> Result<Vec<AgentTaskFitEvidenceSummary>, String> {
    let directory =
        ensure_real_directory(root, Path::new(".agents").join(REPORT_DIR).as_path(), false)?;
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("List task-fit evidence: {error}")),
    };
    let mut reports: Vec<AgentTaskFitEvidenceSummary> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("Read task-fit evidence entry: {error}"))?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        if reports.len() >= MAX_REPORTS {
            return Err("Task-fit evidence library exceeds 256 reports.".into());
        }
        let bytes = read_report_file(&path)?
            .ok_or("Task-fit report disappeared while reading the evidence library.")?;
        let report = validate_report(&bytes)?;
        if path.file_stem().and_then(|stem| stem.to_str()) != Some(report.report_sha256()) {
            return Err("Task-fit report hash does not match its file name.".into());
        }
        let mut summary = AgentTaskFitEvidenceSummary::from(&report);
        if let Some(public_key) = current_public_key {
            let path = attestation_path(&directory, report.report_sha256(), public_key)?;
            if let Some(event) = read_attestation(&path)? {
                if event.pubkey.to_hex() != public_key {
                    return Err("Local task-fit attestation signer does not match its path.".into());
                }
                validate_attestation_for_report(&event, &report)?;
                summary.local_attestation = Some(local_attestation_summary(&event));
            }
        }
        summary.route_attestations =
            list_route_attestations_at(&directory, &report, current_public_key)?;
        reports.push(summary);
    }
    reports.sort_by(|left, right| right.finished_at.cmp(&left.finished_at));
    Ok(reports)
}

/// Validate a report and return a review summary. Validation is structural;
/// it does not authenticate the producer or make the report route-eligible.
#[tauri::command]
pub fn preview_agent_task_fit_report(
    file_bytes: Vec<u8>,
) -> Result<AgentTaskFitEvidenceSummary, String> {
    let report = validate_report(&file_bytes)?;
    Ok((&report).into())
}

/// Store the exact report bytes after the user confirms the reviewed hash.
#[tauri::command]
pub fn import_agent_task_fit_report(
    file_bytes: Vec<u8>,
    expected_report_sha256: String,
) -> Result<AgentTaskFitEvidenceSummary, String> {
    let root = nest_dir().ok_or("cannot resolve Buzz workspace")?;
    import_report_at(&root, &file_bytes, &expected_report_sha256)
}

/// Record a local, identity-signed review of one imported report. This signs
/// the operator's review decision only; it does not authenticate Harbor or
/// make the report eligible for routing by itself.
#[tauri::command]
pub fn attest_agent_task_fit_report(
    report_sha256: String,
    state: State<'_, AppState>,
) -> Result<AgentTaskFitLocalAttestation, String> {
    let root = nest_dir().ok_or("cannot resolve Buzz workspace")?;
    let keys = state.signing_keys()?;
    attest_report_at(&root, &report_sha256, &keys)
}

/// Sign a local report-to-candidate association for one exact resolved route
/// profile. This is an operator review, not producer authentication.
#[tauri::command]
pub fn attest_agent_task_fit_report_for_route(
    report_sha256: String,
    profile_id: String,
    candidate_id: String,
    state: State<'_, AppState>,
) -> Result<AgentTaskFitRouteAttestation, String> {
    let root = nest_dir().ok_or("cannot resolve Buzz workspace")?;
    let profile = crate::managed_agents::agent_route_profile::resolve_saved_profile(&profile_id)?;
    let keys = state.signing_keys()?;
    attest_report_for_route_at(&root, &report_sha256, &profile, &candidate_id, &keys)
}

fn attest_report_at(
    root: &Path,
    report_sha256: &str,
    keys: &Keys,
) -> Result<AgentTaskFitLocalAttestation, String> {
    let _lock = lock_writes(root)?;
    let directory =
        ensure_real_directory(root, Path::new(".agents").join(REPORT_DIR).as_path(), false)?;
    let path = report_path(&directory, report_sha256)?;
    let bytes =
        read_report_file(&path)?.ok_or("Import the task-fit report before attesting it.")?;
    let report = validate_report(&bytes)?;
    if report.report_sha256() != report_sha256 {
        return Err("Stored task-fit report hash does not match its identity.".into());
    }
    let public_key = keys.public_key().to_hex();
    let path = attestation_path(&directory, report_sha256, &public_key)?;
    if let Some(existing) = read_attestation(&path)? {
        if existing.pubkey != keys.public_key() {
            return Err("Local task-fit attestation signer does not match its path.".into());
        }
        validate_attestation_for_report(&existing, &report)?;
        return Ok(local_attestation_summary(&existing));
    }
    let payload = LocalAttestationPayload {
        schema_version: 1,
        report_sha256: report_sha256.to_string(),
        action: "reviewed_for_local_routing".into(),
        task_class: report.task_class().to_string(),
        task_class_taxonomy_version: report.task_class_taxonomy_version().to_string(),
    };
    let content = serde_json::to_string(&payload)
        .map_err(|error| format!("Serialize local task-fit attestation: {error}"))?;
    let event = EventBuilder::new(Kind::Custom(ATTESTATION_KIND), content)
        .sign_with_keys(keys)
        .map_err(|error| format!("Sign local task-fit attestation: {error}"))?;
    validate_attestation_for_report(&event, &report)?;
    let bytes = serde_json::to_vec(&event)
        .map_err(|error| format!("Serialize local task-fit attestation: {error}"))?;
    if bytes.len() > MAX_ATTESTATION_BYTES {
        return Err("Local task-fit attestation exceeds the size limit.".into());
    }
    let attestation_directory = ensure_real_directory(&directory, Path::new("attestations"), true)?;
    let path = attestation_directory.join(format!("{report_sha256}-{public_key}.json"));
    write_report_file(&path, &bytes)?;
    Ok(local_attestation_summary(&event))
}

fn attest_report_for_route_at(
    root: &Path,
    report_sha256: &str,
    profile: &crate::managed_agents::agent_route_profile::ResolvedAgentRouteProfile,
    candidate_id: &str,
    keys: &Keys,
) -> Result<AgentTaskFitRouteAttestation, String> {
    let _lock = lock_writes(root)?;
    let directory =
        ensure_real_directory(root, Path::new(".agents").join(REPORT_DIR).as_path(), false)?;
    let path = report_path(&directory, report_sha256)?;
    let bytes =
        read_report_file(&path)?.ok_or("Import the task-fit report before attesting it.")?;
    let report = validate_report(&bytes)?;
    if report.report_sha256() != report_sha256 {
        return Err("Stored task-fit report hash does not match its identity.".into());
    }
    let binding = route_binding_for_report(&report, profile, candidate_id)?;
    let public_key = keys.public_key().to_hex();
    let path = route_attestation_path(&directory, &binding, &public_key, true)?;
    if let Some(existing) = read_attestation(&path)? {
        if existing.pubkey != keys.public_key() {
            return Err("Route attestation signer does not match its path.".into());
        }
        let existing_binding = route_binding_from_event(&existing, &report)?;
        if existing_binding != binding {
            return Err("Existing route attestation does not match this route.".into());
        }
        return Ok(route_attestation_summary(&existing, existing_binding));
    }
    let payload = TaskFitRouteAttestationPayload {
        schema_version: 1,
        report_sha256: report_sha256.to_string(),
        action: "reviewed_for_local_route_candidate".into(),
        task_class: report.task_class().to_string(),
        task_class_taxonomy_version: report.task_class_taxonomy_version().to_string(),
        binding: binding.clone(),
    };
    let content = serde_json::to_string(&payload)
        .map_err(|error| format!("Serialize route-bound task-fit attestation: {error}"))?;
    let event = EventBuilder::new(Kind::Custom(ATTESTATION_KIND), content)
        .sign_with_keys(keys)
        .map_err(|error| format!("Sign route-bound task-fit attestation: {error}"))?;
    if route_binding_from_event(&event, &report)? != binding {
        return Err("Signed route attestation does not match the reviewed route.".into());
    }
    let bytes = serde_json::to_vec(&event)
        .map_err(|error| format!("Serialize route-bound task-fit attestation: {error}"))?;
    if bytes.len() > MAX_ATTESTATION_BYTES {
        return Err("Route-bound task-fit attestation exceeds the size limit.".into());
    }
    write_report_file(&path, &bytes)?;
    Ok(route_attestation_summary(&event, binding))
}

/// List device-local reports after revalidating their contents and file names.
#[tauri::command]
pub fn list_agent_task_fit_reports(
    state: State<'_, AppState>,
) -> Result<Vec<AgentTaskFitEvidenceSummary>, String> {
    let root = nest_dir().ok_or("cannot resolve Buzz workspace")?;
    let current_public_key = state
        .signing_keys()
        .ok()
        .map(|keys| keys.public_key().to_hex());
    list_reports_at(&root, current_public_key.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managed_agents::agent_route_profile::{
        AgentRouteProfileIdentity, ResolvedAgentRouteProfile,
    };

    fn fixture() -> Vec<u8> {
        include_bytes!("../../../../crates/buzz-agent/testdata/harbor-task-fit-v2.json").to_vec()
    }

    fn resolved_profile(report: &ValidatedTaskFitReport) -> ResolvedAgentRouteProfile {
        let document: buzz_agent_pkg::route_preview::RouteProfileDocument =
            serde_json::from_value(serde_json::json!({
                "version": 1,
                "data_policy": "allow-hosted",
                "preference_order": ["candidate-a"],
                "task_fit_policy": {
                    "taskClass": report.task_class(),
                    "taskClassTaxonomyVersion": report.task_class_taxonomy_version(),
                    "evaluationPolicyVersion": report.evaluation_policy_version(),
                    "minimumDistinctTasks": 1,
                    "minimumWilsonLowerBound95": 0.0,
                    "maximumAgeSeconds": 3600,
                    "requireObservedModelIdentity": false
                },
                "candidates": [{
                    "id": "candidate-a",
                    "provider": report.provider_id(),
                    "model": report.model_id(),
                    "data_location": "hosted",
                    "prompt_addendum": ""
                }],
                "profile_id": "coding-route",
                "profile_version": 2,
                "profile_hash": "c".repeat(64)
            }))
            .expect("route profile");
        let serialized_document =
            serde_json::to_string(&document).expect("serialize route profile");
        let hash = hex::encode(Sha256::digest(serialized_document.as_bytes()));
        ResolvedAgentRouteProfile {
            identity: AgentRouteProfileIdentity {
                id: "coding-route".into(),
                version: 2,
                hash,
            },
            serialized_document,
            document,
        }
    }

    #[test]
    fn preview_returns_exact_case_and_candidate_identity() {
        let report = validate_report(&fixture()).expect("fixture validates");
        let summary = AgentTaskFitEvidenceSummary::from(&report);
        assert_eq!(summary.task_count, 1);
        assert_eq!(summary.provider_id, "openai");
        assert_eq!(summary.model_id, "fixture-model-r1");
        assert_eq!(summary.case_set_sha256.len(), 64);
        assert!(summary.model_identity_observed);
    }

    #[test]
    fn import_requires_the_exact_previewed_hash_and_is_idempotent() {
        let root = tempfile::tempdir().expect("temporary workspace");
        fs::create_dir(root.path().join(".agents")).expect("agents directory");
        let bytes = fixture();
        let report = validate_report(&bytes).expect("fixture validates");
        let expected_hash = report.report_sha256().to_string();
        assert!(import_report_at(root.path(), &bytes, &"0".repeat(64)).is_err());
        let imported = import_report_at(root.path(), &bytes, &expected_hash).expect("import");
        let repeated = import_report_at(root.path(), &bytes, &expected_hash).expect("repeat");
        assert_eq!(imported, repeated);
        assert_eq!(list_reports_at(root.path(), None).expect("list").len(), 1);
    }

    #[test]
    fn report_store_rejects_tampered_files() {
        let root = tempfile::tempdir().expect("temporary workspace");
        fs::create_dir(root.path().join(".agents")).expect("agents directory");
        let bytes = fixture();
        let report = validate_report(&bytes).expect("fixture validates");
        let summary =
            import_report_at(root.path(), &bytes, report.report_sha256()).expect("import");
        let path = root
            .path()
            .join(".agents")
            .join(REPORT_DIR)
            .join(format!("{}.json", summary.report_sha256));
        fs::write(&path, b"{}").expect("tamper report");
        assert!(list_reports_at(root.path(), None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn import_rejects_symlinked_report_directory_and_file() {
        use std::os::unix::fs::symlink;

        let bytes = fixture();
        let report = validate_report(&bytes).expect("fixture validates");
        let hash = report.report_sha256().to_string();

        let root = tempfile::tempdir().expect("temporary workspace");
        let external = tempfile::tempdir().expect("external directory");
        fs::create_dir(root.path().join(".agents")).expect("agents directory");
        symlink(
            external.path(),
            root.path().join(".agents").join(REPORT_DIR),
        )
        .expect("directory symlink");
        assert!(import_report_at(root.path(), &bytes, &hash).is_err());
        assert_eq!(fs::read_dir(external.path()).unwrap().count(), 0);

        let root = tempfile::tempdir().expect("temporary workspace");
        let external = tempfile::tempdir().expect("external directory");
        let directory = root.path().join(".agents").join(REPORT_DIR);
        fs::create_dir_all(&directory).expect("evidence directory");
        let outside = external.path().join("outside.json");
        fs::write(&outside, b"leave unchanged").expect("external file");
        symlink(&outside, directory.join(format!("{hash}.json"))).expect("file symlink");
        assert!(import_report_at(root.path(), &bytes, &hash).is_err());
        assert_eq!(fs::read(outside).unwrap(), b"leave unchanged");
    }

    #[test]
    fn local_attestation_is_signed_bound_to_report_and_identity_scoped() {
        let root = tempfile::tempdir().expect("temporary workspace");
        fs::create_dir(root.path().join(".agents")).expect("agents directory");
        let bytes = fixture();
        let report = validate_report(&bytes).expect("fixture validates");
        let hash = report.report_sha256().to_string();
        import_report_at(root.path(), &bytes, &hash).expect("import report");

        let keys = Keys::generate();
        let attestation = attest_report_at(root.path(), &hash, &keys).expect("attest report");
        assert_eq!(attestation.public_key, keys.public_key().to_hex());
        assert_eq!(attestation.event_id.len(), 64);
        let current_key = keys.public_key().to_hex();
        let listed = list_reports_at(root.path(), Some(&current_key)).expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].local_attestation, Some(attestation.clone()));
        assert!(
            list_reports_at(root.path(), Some(&Keys::generate().public_key().to_hex()))
                .expect("other identity list")
                .first()
                .unwrap()
                .local_attestation
                .is_none()
        );

        let path = attestation_path(
            &root.path().join(".agents").join(REPORT_DIR),
            &hash,
            &current_key,
        )
        .expect("attestation path");
        let mut tampered: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("read signature")).unwrap();
        tampered["content"] = serde_json::json!("forged");
        fs::write(path, serde_json::to_vec(&tampered).unwrap()).expect("tamper signature");
        assert!(list_reports_at(root.path(), Some(&current_key)).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn local_attestation_rejects_symlinked_directory_and_file() {
        use std::os::unix::fs::symlink;

        let bytes = fixture();
        let report = validate_report(&bytes).expect("fixture validates");
        let hash = report.report_sha256().to_string();
        let keys = Keys::generate();

        let root = tempfile::tempdir().expect("temporary workspace");
        let external = tempfile::tempdir().expect("external directory");
        fs::create_dir(root.path().join(".agents")).expect("agents directory");
        import_report_at(root.path(), &bytes, &hash).expect("import report");
        symlink(
            external.path(),
            root.path()
                .join(".agents")
                .join(REPORT_DIR)
                .join("attestations"),
        )
        .expect("attestation directory symlink");
        assert!(attest_report_at(root.path(), &hash, &keys).is_err());
        assert_eq!(fs::read_dir(external.path()).unwrap().count(), 0);

        let root = tempfile::tempdir().expect("temporary workspace");
        let external = tempfile::tempdir().expect("external directory");
        fs::create_dir(root.path().join(".agents")).expect("agents directory");
        import_report_at(root.path(), &bytes, &hash).expect("import report");
        let report_directory = root.path().join(".agents").join(REPORT_DIR);
        let attestation_directory = report_directory.join("attestations");
        fs::create_dir(&attestation_directory).expect("attestation directory");
        let outside = external.path().join("outside.json");
        fs::write(&outside, b"leave unchanged").expect("external file");
        let link =
            attestation_directory.join(format!("{}-{}.json", hash, keys.public_key().to_hex()));
        symlink(&outside, &link).expect("attestation file symlink");
        assert!(attest_report_at(root.path(), &hash, &keys).is_err());
        assert_eq!(fs::read(outside).unwrap(), b"leave unchanged");
    }

    #[test]
    fn local_attestation_requires_an_imported_report_and_reuses_existing_signature() {
        let root = tempfile::tempdir().expect("temporary workspace");
        fs::create_dir(root.path().join(".agents")).expect("agents directory");
        let bytes = fixture();
        let report = validate_report(&bytes).expect("fixture validates");
        let hash = report.report_sha256().to_string();
        let keys = Keys::generate();
        assert!(
            attest_report_at(root.path(), &hash, &keys)
                .unwrap_err()
                .contains("Import the task-fit report")
        );
        import_report_at(root.path(), &bytes, &hash).expect("import report");
        let first = attest_report_at(root.path(), &hash, &keys).expect("attest");
        let second = attest_report_at(root.path(), &hash, &keys).expect("repeat attest");
        assert_eq!(first, second);
    }

    #[test]
    fn route_attestation_signs_exact_profile_candidate_and_lists_by_identity() {
        let root = tempfile::tempdir().expect("temporary workspace");
        fs::create_dir(root.path().join(".agents")).expect("agents directory");
        let bytes = fixture();
        let report = validate_report(&bytes).expect("fixture validates");
        let hash = report.report_sha256().to_string();
        import_report_at(root.path(), &bytes, &hash).expect("import report");

        let profile = resolved_profile(&report);
        let keys = Keys::generate();
        let attestation =
            attest_report_for_route_at(root.path(), &hash, &profile, "candidate-a", &keys)
                .expect("attest route binding");
        assert_eq!(attestation.profile_id, "coding-route");
        assert_eq!(attestation.profile_version, 2);
        assert_eq!(attestation.profile_hash, profile.identity.hash);
        assert_eq!(attestation.candidate_id, "candidate-a");
        assert_eq!(
            attest_report_for_route_at(root.path(), &hash, &profile, "candidate-a", &keys)
                .expect("repeat route attestation"),
            attestation
        );

        let listed = list_reports_at(root.path(), Some(&keys.public_key().to_hex()))
            .expect("list route-bound evidence");
        assert_eq!(listed[0].route_attestations, vec![attestation.clone()]);
        assert!(
            list_reports_at(root.path(), Some(&Keys::generate().public_key().to_hex()))
                .expect("list another identity")
                .first()
                .unwrap()
                .route_attestations
                .is_empty()
        );

        let wrong_candidate =
            attest_report_for_route_at(root.path(), &hash, &profile, "missing-candidate", &keys);
        assert!(wrong_candidate.is_err());

        let path = route_attestation_path(
            &root.path().join(".agents").join(REPORT_DIR),
            &TaskFitEvidenceBinding {
                report_sha256: hash,
                profile_id: attestation.profile_id,
                profile_version: attestation.profile_version,
                profile_hash: attestation.profile_hash,
                candidate_id: attestation.candidate_id,
            },
            &keys.public_key().to_hex(),
            false,
        )
        .expect("route attestation path");
        let mut tampered: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("read route signature")).unwrap();
        tampered["content"] = serde_json::json!("forged");
        fs::write(path, serde_json::to_vec(&tampered).unwrap()).expect("tamper route signature");
        assert!(list_reports_at(root.path(), Some(&keys.public_key().to_hex())).is_err());
    }
}
