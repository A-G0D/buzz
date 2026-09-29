//! Device-local, versioned API route profiles for managed Buzz Agent runs.

use buzz_agent_pkg::route_preview::RouteProfileDocument;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
};
use tauri::State;

use crate::app_state::AppState;
use crate::managed_agents::{
    nest_dir, storage::atomic_write_json_restricted, validate_visible_text,
};

const PROFILE_DIR: &str = "route-profiles";
const MAX_PROFILE_BYTES: usize = 64 * 1024;
const MAX_PROFILE_FILE_BYTES: usize = MAX_PROFILE_BYTES + 16 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
/// A device-local route profile. Provider credentials remain in agent/runtime configuration.
pub struct AgentRouteProfile {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub version: u32,
    pub document: RouteProfileDocument,
    pub document_hash: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
/// Non-secret route-profile metadata shown in the library and agent selector.
pub struct AgentRouteProfileSummary {
    pub id: String,
    pub name: String,
    pub version: u32,
    pub data_policy: String,
    pub candidate_count: usize,
    pub document_hash: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
/// Input for creating or compare-and-swap updating a route profile.
pub struct SaveAgentRouteProfileInput {
    pub id: String,
    pub name: String,
    pub document: RouteProfileDocument,
    pub expected_version: Option<u32>,
}

fn ensure_directory(root: &Path, relative: &Path, create: bool) -> Result<PathBuf, String> {
    if root
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err("Refusing to use a symlink as the Buzz workspace.".into());
    }
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return Err("Invalid route-profile directory path.".into());
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

fn profiles_root(create: bool) -> Result<PathBuf, String> {
    let workspace = nest_dir().ok_or("cannot resolve Buzz workspace")?;
    ensure_directory(
        &workspace,
        Path::new(".agents").join(PROFILE_DIR).as_path(),
        create,
    )
}

fn validate_id(id: &str) -> Result<(), String> {
    let valid = !id.is_empty()
        && id.len() <= 64
        && id.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !id.ends_with('-')
        && !id.contains("--");
    if !valid {
        return Err("Route profile IDs must use lowercase letters, numbers, and single hyphens, and be 1–64 bytes long.".into());
    }
    Ok(())
}

fn validate_input(input: &SaveAgentRouteProfileInput) -> Result<(), String> {
    validate_id(&input.id)?;
    if input.name.trim().is_empty() || input.name.chars().count() > 120 {
        return Err("Route profile names must contain 1–120 characters.".into());
    }
    validate_visible_text(&input.name, "Route profile name", false)?;
    input
        .document
        .validate()
        .map_err(|error| format!("Invalid route profile: {error}"))?;
    if input.document.profile_id.is_some()
        || input.document.profile_version.is_some()
        || input.document.profile_hash.is_some()
        || input
            .document
            .candidates
            .iter()
            .any(|candidate| candidate.prompt_profile.is_some())
    {
        return Err("Route profile provenance is generated by Buzz and cannot be edited.".into());
    }
    let bytes = serde_json::to_vec(&input.document)
        .map_err(|error| format!("Serialize route profile: {error}"))?;
    if bytes.len() > MAX_PROFILE_BYTES {
        return Err(format!(
            "Route profile exceeds {} KiB.",
            MAX_PROFILE_BYTES / 1024
        ));
    }
    for candidate in &input.document.candidates {
        for line in candidate.prompt_addendum.lines() {
            validate_visible_text(line, "Route prompt addendum", true)?;
        }
    }
    Ok(())
}

fn profile_path(root: &Path, id: &str) -> Result<PathBuf, String> {
    validate_id(id)?;
    Ok(root.join(format!("{id}.json")))
}

fn document_hash(document: &RouteProfileDocument) -> Result<String, String> {
    let bytes = serde_json::to_vec(document)
        .map_err(|error| format!("Serialize route profile: {error}"))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn read_profile_file(path: &Path) -> Result<AgentRouteProfile, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("Read route profile {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("Route profile is not a regular file.".into());
    }
    if metadata.len() > MAX_PROFILE_FILE_BYTES as u64 {
        return Err("Route profile file exceeds the size limit.".into());
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
        .map_err(|error| format!("Open route profile {}: {error}", path.display()))?;
    let mut bytes = Vec::new();
    file.take((MAX_PROFILE_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Read route profile {}: {error}", path.display()))?;
    if bytes.len() > MAX_PROFILE_FILE_BYTES {
        return Err("Route profile file exceeds the size limit.".into());
    }
    let profile: AgentRouteProfile = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Invalid route profile JSON: {error}"))?;
    if profile.id
        != path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
    {
        return Err("Route profile ID does not match its file name.".into());
    }
    if profile.schema_version != 1 || profile.version == 0 {
        return Err("Route profile schema or version is unsupported.".into());
    }
    let input = SaveAgentRouteProfileInput {
        id: profile.id.clone(),
        name: profile.name.clone(),
        document: profile.document.clone(),
        expected_version: Some(profile.version),
    };
    validate_input(&input)?;
    if document_hash(&profile.document)? != profile.document_hash {
        return Err("Route profile hash does not match its contents.".into());
    }
    Ok(profile)
}

fn acquire_write_lock(workspace: &Path) -> Result<fs::File, String> {
    let agents_dir = ensure_directory(workspace, Path::new(".agents"), true)?;
    let lock_path = agents_dir.join(".route-profiles-write.lock");
    if fs::symlink_metadata(&lock_path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err("Refusing to use a symlink as the route profile write lock.".into());
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
        .map_err(|error| format!("Open route profile write lock: {error}"))?;
    lock.lock()
        .map_err(|error| format!("Lock route profile library: {error}"))?;
    Ok(lock)
}

/// List local route profiles without returning their prompt text.
#[tauri::command]
pub fn list_agent_route_profiles() -> Result<Vec<AgentRouteProfileSummary>, String> {
    let root = profiles_root(false)?;
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("List route profiles: {error}")),
    };
    let mut profiles = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("Read route profile entry: {error}"))?;
        if entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_symlink()
            || entry.path().extension().and_then(|ext| ext.to_str()) != Some("json")
        {
            continue;
        }
        profiles.push(read_profile_file(&entry.path())?);
    }
    profiles.sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
    Ok(profiles
        .into_iter()
        .map(|profile| AgentRouteProfileSummary {
            id: profile.id,
            name: profile.name,
            version: profile.version,
            data_policy: match profile.document.data_policy {
                buzz_agent_pkg::route_preview::RouteProfileDataPolicy::LocalOnly => {
                    "local-only".into()
                }
                buzz_agent_pkg::route_preview::RouteProfileDataPolicy::AllowHosted => {
                    "allow-hosted".into()
                }
            },
            candidate_count: profile.document.candidates.len(),
            document_hash: profile.document_hash,
            updated_at: profile.updated_at,
        })
        .collect())
}

/// Read one local route profile, including its target instructions.
#[tauri::command]
pub fn read_agent_route_profile(id: String) -> Result<AgentRouteProfile, String> {
    let root = profiles_root(false)?;
    read_profile_file(&profile_path(&root, &id)?)
}

/// Read fresh local throughput summaries for the current saved profile version.
#[tauri::command]
pub fn list_agent_route_throughput_summaries(
    profile_id: String,
    profile_version: u32,
    state: State<'_, AppState>,
) -> Result<Vec<buzz_run_journal::RouteThroughputGroupSummary>, String> {
    let profile = read_agent_route_profile(profile_id.clone())?;
    if profile.version != profile_version {
        return Err("Route profile changed; refresh it before reading measurements.".into());
    }
    let viewer = state
        .keys
        .lock()
        .map_err(|error| format!("Lock workspace identity: {error}"))?
        .public_key()
        .to_hex();
    let relay_url = crate::relay::relay_ws_url_with_override(&state);
    let workspace = nest_dir().ok_or("cannot resolve Buzz workspace")?;
    let journal = buzz_run_journal::RunJournal::open_scoped(workspace, &relay_url, &viewer)?;
    journal.route_throughput_summaries(&profile_id, profile_version)
}

/// Create or compare-and-swap update one local route profile.
#[tauri::command]
pub fn save_agent_route_profile(
    input: SaveAgentRouteProfileInput,
) -> Result<AgentRouteProfile, String> {
    validate_input(&input)?;
    let workspace = nest_dir().ok_or("cannot resolve Buzz workspace")?;
    let _lock = acquire_write_lock(&workspace)?;
    let root = profiles_root(true)?;
    let path = profile_path(&root, &input.id)?;
    let current = match fs::symlink_metadata(&path) {
        Ok(_) => Some(read_profile_file(&path)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("Inspect route profile: {error}")),
    };
    let version = match (&current, input.expected_version) {
        (None, None) => 1,
        (Some(profile), Some(expected)) if profile.version == expected => profile
            .version
            .checked_add(1)
            .ok_or("Route profile version is exhausted")?,
        (None, Some(_)) => return Err("Route profile was removed; reload before saving.".into()),
        (Some(_), None) => return Err("Route profile already exists; reload before saving.".into()),
        (Some(profile), Some(_)) => {
            return Err(format!(
                "Route profile changed to version {}; reload before saving.",
                profile.version
            ));
        }
    };
    let profile = AgentRouteProfile {
        schema_version: 1,
        id: input.id,
        name: input.name,
        version,
        document_hash: document_hash(&input.document)?,
        document: input.document,
        updated_at: crate::util::now_iso(),
    };
    let payload = serde_json::to_vec_pretty(&profile)
        .map_err(|error| format!("Serialize route profile: {error}"))?;
    if payload.len() > MAX_PROFILE_FILE_BYTES {
        return Err("Saved route profile exceeds the file size limit.".into());
    }
    atomic_write_json_restricted(&path, &payload)?;
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use buzz_agent_pkg::route_preview::{
        RouteProfileCandidate, RouteProfileDataPolicy, RouteProfileLocation,
    };

    fn input() -> SaveAgentRouteProfileInput {
        SaveAgentRouteProfileInput {
            id: "local-first".into(),
            name: "Local first".into(),
            document: RouteProfileDocument {
                version: 1,
                data_policy: RouteProfileDataPolicy::LocalOnly,
                preference_order: vec!["local".into()],
                strict_context_fit: false,
                max_turn_cost_microusd: None,
                prefer_fastest_measured: false,
                min_effective_output_tokens_per_second_milli: None,
                allow_preference_order_warmup: false,
                task_fit_policy: None,
                candidates: vec![RouteProfileCandidate {
                    id: "local".into(),
                    provider: "openai".into(),
                    model: "qwen3-coder".into(),
                    data_location: RouteProfileLocation::Local,
                    context_capacity_tokens: None,
                    input_cost_microusd_per_million_tokens: None,
                    output_cost_microusd_per_million_tokens: None,
                    prompt_addendum: "Stay local.".into(),
                    prompt_profile: None,
                }],
                profile_id: None,
                profile_version: None,
                profile_hash: None,
            },
            expected_version: None,
        }
    }

    #[test]
    fn route_profile_input_validates_policy_and_rejects_user_supplied_provenance() {
        validate_input(&input()).unwrap();
        let mut invalid = input();
        invalid.document.profile_id = Some("fake".into());
        invalid.document.profile_version = Some(1);
        invalid.document.profile_hash = Some("a".repeat(64));
        assert!(
            validate_input(&invalid)
                .unwrap_err()
                .contains("generated by Buzz")
        );
        let mut invalid = input();
        invalid.document.candidates[0].prompt_profile =
            Some(buzz_agent_pkg::route_preview::RoutePromptProfileRef {
                id: "deepseek".into(),
                version: 1,
                prompt_hash: "b".repeat(64),
            });
        assert!(
            validate_input(&invalid)
                .unwrap_err()
                .contains("generated by Buzz")
        );
        let mut invalid = input();
        invalid.document.preference_order = vec!["missing".into()];
        assert!(validate_input(&invalid).is_err());
    }

    #[test]
    fn profile_id_is_a_safe_single_path_component() {
        assert!(profile_path(Path::new("/tmp/routes"), "local-first").is_ok());
        assert!(profile_path(Path::new("/tmp/routes"), "../elsewhere").is_err());
    }
}
