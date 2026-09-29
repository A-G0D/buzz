//! Device-local prompt profile library.
//!
//! Profiles are editable prompt artifacts for distinct API, harness, and
//! consumer-app targets. Exact local Buzz Agent API and DSH ACP model matches
//! are automatically applied at launch; other targets remain stored artifacts.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
};

use crate::managed_agents::{
    nest_dir, storage::atomic_write_json_restricted, validate_visible_text,
};

const MAX_PROFILE_BYTES: usize = 128 * 1024;
const PROFILE_DIR: &str = "prompt-profiles";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// A prompt destination category; it does not imply a connected adapter.
pub enum AgentPromptProfileTargetKind {
    BuzzAgentApi,
    AcpHarness,
    CliHarness,
    ConsumerApp,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// Exact destination metadata stored with a prompt profile.
pub struct AgentPromptProfileTarget {
    pub kind: AgentPromptProfileTargetKind,
    /// Buzz Agent provider ID, harness ID, or consumer-app ID.
    pub target_id: String,
    /// Exact model ID for API profiles or the DSH ACP harness; `None` means
    /// provider-wide for API profiles or harness-wide for DSH.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// A local, versioned prompt and its declared destination.
pub struct AgentPromptProfile {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub version: u32,
    pub target: AgentPromptProfileTarget,
    pub prompt: String,
    pub prompt_hash: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
/// Prompt-profile metadata returned by the library listing command.
pub struct AgentPromptProfileSummary {
    pub id: String,
    pub name: String,
    pub version: u32,
    pub target: AgentPromptProfileTarget,
    pub prompt_hash: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
/// Input for creating or compare-and-swap updating a prompt profile.
pub struct SaveAgentPromptProfileInput {
    pub id: String,
    pub name: String,
    pub target: AgentPromptProfileTarget,
    pub prompt: String,
    /// `None` creates a profile; `Some(version)` performs a compare-and-swap
    /// update so one editor cannot silently overwrite another.
    pub expected_version: Option<u32>,
}

fn read_profiles(root: &Path) -> Result<Vec<AgentPromptProfile>, String> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("List prompt profiles: {error}")),
    };
    let mut profiles = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("Read prompt profile entry: {error}"))?;
        if entry
            .file_type()
            .map_err(|error| format!("Inspect prompt profile: {error}"))?
            .is_symlink()
            || entry.path().extension().and_then(|ext| ext.to_str()) != Some("json")
        {
            continue;
        }
        profiles.push(read_profile_file(&entry.path())?);
    }
    profiles.sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
    Ok(profiles)
}

fn ensure_unique_buzz_agent_target(
    root: &Path,
    id: &str,
    target: &AgentPromptProfileTarget,
) -> Result<(), String> {
    if target.kind == AgentPromptProfileTargetKind::AcpHarness && target.target_id.trim() == "dsh" {
        let duplicate = read_profiles(root)?.into_iter().find(|profile| {
            profile.id != id
                && profile.target.kind == AgentPromptProfileTargetKind::AcpHarness
                && profile.target.target_id.trim() == "dsh"
                && profile.target.model_id == target.model_id
        });
        if let Some(profile) = duplicate {
            return Err(format!(
                "DSH ACP target already has prompt profile '{}'; edit that profile instead.",
                profile.name
            ));
        }
        return Ok(());
    }
    if target.kind != AgentPromptProfileTargetKind::BuzzAgentApi {
        return Ok(());
    }
    let duplicate = read_profiles(root)?.into_iter().find(|profile| {
        profile.id != id
            && profile.target.kind == AgentPromptProfileTargetKind::BuzzAgentApi
            && same_buzz_agent_provider(&profile.target.target_id, &target.target_id)
            && profile.target.model_id == target.model_id
    });
    if let Some(profile) = duplicate {
        return Err(format!(
            "Buzz Agent API target already has prompt profile '{}'; edit that profile instead.",
            profile.name
        ));
    }
    Ok(())
}

fn canonical_buzz_agent_provider(provider_id: &str) -> Option<&'static str> {
    match provider_id.trim().to_ascii_lowercase().as_str() {
        "anthropic" => Some("anthropic"),
        "openai" | "openai-compat" => Some("openai"),
        "deepseek" => Some("deepseek"),
        "databricks" => Some("databricks"),
        "databricks_v2" | "databricks-v2" => Some("databricks_v2"),
        "openrouter" => Some("openrouter"),
        _ => None,
    }
}

fn same_buzz_agent_provider(left: &str, right: &str) -> bool {
    match (
        canonical_buzz_agent_provider(left),
        canonical_buzz_agent_provider(right),
    ) {
        (Some(left), Some(right)) => left == right,
        _ => left.trim().eq_ignore_ascii_case(right.trim()),
    }
}

fn env_value<'a>(env: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    env.iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
        .map(|(_, value)| value.as_str())
        .map(str::trim)
}

/// Return Buzz Agent's configured API provider/model when both are explicit
/// in the effective managed-agent environment.
pub(crate) fn buzz_agent_api_target_from_env(
    env: &BTreeMap<String, String>,
) -> Option<(String, String)> {
    let provider = canonical_buzz_agent_provider(env_value(env, "BUZZ_AGENT_PROVIDER")?)?;
    let model = match env_value(env, "BUZZ_AGENT_MODEL") {
        Some(model) if !model.is_empty() => model,
        Some(_) => return None,
        None => {
            let model_key = match provider {
                "anthropic" => "ANTHROPIC_MODEL",
                "openai" => "OPENAI_COMPAT_MODEL",
                "deepseek" => "DEEPSEEK_MODEL",
                "databricks" | "databricks_v2" => "DATABRICKS_MODEL",
                "openrouter" => "OPENROUTER_MODEL",
                _ => return None,
            };
            let model = env_value(env, model_key)?;
            if model.is_empty() {
                return None;
            }
            model
        }
    };
    Some((provider.to_string(), model.to_string()))
}

/// Resolve the single local Buzz Agent API prompt for an exact provider/model.
/// An exact-model profile wins over the provider-wide fallback.
pub(crate) fn resolve_buzz_agent_api_prompt_profile(
    provider_id: &str,
    model_id: &str,
) -> Result<Option<AgentPromptProfile>, String> {
    let provider_id = provider_id.trim();
    let model_id = model_id.trim();
    if provider_id.is_empty() || model_id.is_empty() {
        return Ok(None);
    }
    let root = profiles_root(false)?;
    let profiles = read_profiles(&root)?;
    let matches = |profile: &&AgentPromptProfile| {
        profile.target.kind == AgentPromptProfileTargetKind::BuzzAgentApi
            && same_buzz_agent_provider(&profile.target.target_id, provider_id)
    };
    let exact: Vec<_> = profiles
        .iter()
        .filter(matches)
        .filter(|profile| profile.target.model_id.as_deref() == Some(model_id))
        .collect();
    let provider_wide: Vec<_> = profiles
        .iter()
        .filter(matches)
        .filter(|profile| profile.target.model_id.is_none())
        .collect();
    let selected = if exact.is_empty() {
        provider_wide
    } else {
        exact
    };
    match selected.as_slice() {
        [] => Ok(None),
        [profile] => Ok(Some((*profile).clone())),
        _ => Err(format!(
            "Multiple Buzz Agent API prompt profiles match provider '{}' and model '{}'. Remove the duplicate profile before starting this agent.",
            provider_id, model_id
        )),
    }
}

/// Resolve the exact DSH ACP model profile, falling back to the harness-wide
/// profile when no exact match exists.
fn select_dsh_acp_prompt_profile(
    profiles: Vec<AgentPromptProfile>,
    model_id: Option<&str>,
) -> Result<Option<AgentPromptProfile>, String> {
    let matches = |profile: &&AgentPromptProfile| {
        profile.target.kind == AgentPromptProfileTargetKind::AcpHarness
            && profile.target.target_id.trim() == "dsh"
    };
    let exact: Vec<_> = profiles
        .iter()
        .filter(matches)
        .filter(|profile| {
            profile.target.model_id.as_deref() == model_id.filter(|id| !id.trim().is_empty())
        })
        .collect();
    let harness_wide: Vec<_> = profiles
        .iter()
        .filter(matches)
        .filter(|profile| profile.target.model_id.is_none())
        .collect();
    let selected = if exact.is_empty() {
        harness_wide
    } else {
        exact
    };
    match selected.as_slice() {
        [] => Ok(None),
        [profile] => Ok(Some((*profile).clone())),
        _ => Err("Multiple DSH ACP prompt profiles match; remove the duplicate before starting this agent.".to_string()),
    }
}

pub(crate) fn resolve_dsh_acp_prompt_profile(
    model_id: Option<&str>,
) -> Result<Option<AgentPromptProfile>, String> {
    let root = profiles_root(false)?;
    select_dsh_acp_prompt_profile(read_profiles(&root)?, model_id)
}

/// Append a matched target prompt after the agent's existing instructions.
pub(crate) fn append_buzz_agent_api_prompt_profile(
    base_prompt: Option<&str>,
    profile: &AgentPromptProfile,
) -> String {
    let profile_section = format!("## Target-specific prompt profile\n\n{}", profile.prompt);
    match base_prompt.filter(|prompt| !prompt.trim().is_empty()) {
        Some(base_prompt) => format!("{base_prompt}\n\n{profile_section}"),
        None => profile_section,
    }
}

fn ensure_directory(root: &Path, relative: &Path, create: bool) -> Result<PathBuf, String> {
    if root
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(format!(
            "Refusing to use a symlink as the Buzz workspace: {}",
            root.display()
        ));
    }
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return Err("Invalid prompt profile directory path".to_string());
        };
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "Refusing to follow a symlink at {}",
                    current.display()
                ));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(format!("Expected a directory at {}", current.display()));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
                fs::create_dir(&current)
                    .map_err(|e| format!("Create {}: {e}", current.display()))?;
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
        return Err(
            "Profile IDs must use lowercase letters, numbers, and single hyphens, and be 1–64 bytes long."
                .to_string(),
        );
    }
    Ok(())
}

fn validate_input(input: &SaveAgentPromptProfileInput) -> Result<(), String> {
    validate_id(&input.id)?;
    if input.name.trim().is_empty() || input.name.chars().count() > 120 {
        return Err("Profile names must contain 1–120 characters.".to_string());
    }
    validate_visible_text(&input.name, "Prompt profile name", false)?;
    if input.target.target_id.trim().is_empty() || input.target.target_id.chars().count() > 128 {
        return Err("Target IDs must contain 1–128 characters.".to_string());
    }
    validate_visible_text(&input.target.target_id, "Prompt profile target", false)?;
    if let Some(model_id) = input.target.model_id.as_deref() {
        if model_id.trim().is_empty() || model_id.chars().count() > 256 {
            return Err("Model IDs must contain 1–256 characters when provided.".to_string());
        }
        validate_visible_text(model_id, "Prompt profile model ID", false)?;
    }
    let dsh_model_profile = input.target.kind == AgentPromptProfileTargetKind::AcpHarness
        && input.target.target_id.trim() == "dsh";
    if input.target.kind != AgentPromptProfileTargetKind::BuzzAgentApi
        && !dsh_model_profile
        && input.target.model_id.is_some()
    {
        return Err(
            "Only Buzz Agent API profiles and DSH ACP profiles can target a model ID.".to_string(),
        );
    }
    if input.prompt.trim().is_empty() {
        return Err("Prompt text must not be empty.".to_string());
    }
    if input.prompt.len() > MAX_PROFILE_BYTES {
        return Err(format!(
            "Prompt text must be smaller than {} KiB.",
            MAX_PROFILE_BYTES / 1024
        ));
    }
    for line in input.prompt.lines() {
        validate_visible_text(line, "Prompt profile text", true)?;
    }
    Ok(())
}

fn profile_path(root: &Path, id: &str) -> Result<PathBuf, String> {
    validate_id(id)?;
    Ok(root.join(format!("{id}.json")))
}

fn read_profile_file(path: &Path) -> Result<AgentPromptProfile, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("Read prompt profile {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "Prompt profile is not a regular file: {}",
            path.display()
        ));
    }
    if metadata.len() > (MAX_PROFILE_BYTES + 16 * 1024) as u64 {
        return Err("Prompt profile file exceeds the size limit.".to_string());
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
        .map_err(|error| format!("Open prompt profile {}: {error}", path.display()))?;
    let mut bytes = Vec::new();
    file.take((MAX_PROFILE_BYTES + 16 * 1024 + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Read prompt profile {}: {error}", path.display()))?;
    if bytes.len() > MAX_PROFILE_BYTES + 16 * 1024 {
        return Err("Prompt profile file exceeds the size limit.".to_string());
    }
    let profile: AgentPromptProfile = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Invalid prompt profile {}: {error}", path.display()))?;
    if profile.id
        != path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
    {
        return Err("Prompt profile ID does not match its file name.".to_string());
    }
    if profile.schema_version != 1 || profile.version == 0 {
        return Err("Prompt profile schema or version is unsupported.".to_string());
    }
    validate_id(&profile.id)?;
    if profile.prompt_hash != hex::encode(Sha256::digest(profile.prompt.as_bytes())) {
        return Err("Prompt profile hash does not match its prompt text.".to_string());
    }
    validate_input(&SaveAgentPromptProfileInput {
        id: profile.id.clone(),
        name: profile.name.clone(),
        target: profile.target.clone(),
        prompt: profile.prompt.clone(),
        expected_version: Some(profile.version),
    })?;
    Ok(profile)
}

fn acquire_write_lock(workspace: &Path) -> Result<fs::File, String> {
    let agents_dir = ensure_directory(workspace, Path::new(".agents"), true)?;
    let lock_path = agents_dir.join(".prompt-profiles-write.lock");
    if fs::symlink_metadata(&lock_path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err("Refusing to use a symlink as the prompt profile write lock.".to_string());
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
        .open(&lock_path)
        .map_err(|error| format!("Open prompt profile write lock: {error}"))?;
    lock.lock()
        .map_err(|error| format!("Lock prompt profile library: {error}"))?;
    Ok(lock)
}

/// List device-local prompt profiles without exposing their prompt text.
#[tauri::command]
pub fn list_agent_prompt_profiles() -> Result<Vec<AgentPromptProfileSummary>, String> {
    let root = profiles_root(false)?;
    Ok(read_profiles(&root)?
        .into_iter()
        .map(|profile| AgentPromptProfileSummary {
            id: profile.id,
            name: profile.name,
            version: profile.version,
            target: profile.target,
            prompt_hash: profile.prompt_hash,
            updated_at: profile.updated_at,
        })
        .collect())
}

/// Read one device-local prompt profile, including its literal prompt text.
#[tauri::command]
pub fn read_agent_prompt_profile(id: String) -> Result<AgentPromptProfile, String> {
    let root = profiles_root(false)?;
    read_profile_file(&profile_path(&root, &id)?)
}

/// Create or compare-and-swap update one device-local prompt profile.
#[tauri::command]
pub fn save_agent_prompt_profile(
    input: SaveAgentPromptProfileInput,
) -> Result<AgentPromptProfile, String> {
    validate_input(&input)?;
    let workspace = nest_dir().ok_or("cannot resolve Buzz workspace")?;
    let _lock = acquire_write_lock(&workspace)?;
    let root = profiles_root(true)?;
    let path = profile_path(&root, &input.id)?;
    let current = match fs::symlink_metadata(&path) {
        Ok(_) => Some(read_profile_file(&path)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(format!(
                "Inspect prompt profile {}: {error}",
                path.display()
            ))
        }
    };
    ensure_unique_buzz_agent_target(&root, &input.id, &input.target)?;
    let version = match (&current, input.expected_version) {
        (None, None) => 1,
        (Some(profile), Some(expected)) if profile.version == expected => profile
            .version
            .checked_add(1)
            .ok_or("Prompt profile version is exhausted")?,
        (None, Some(_)) => {
            return Err("Prompt profile was removed; reload before saving.".to_string())
        }
        (Some(_), None) => {
            return Err("Prompt profile already exists; reload before saving.".to_string())
        }
        (Some(profile), Some(_)) => {
            return Err(format!(
                "Prompt profile changed to version {}; reload before saving.",
                profile.version
            ));
        }
    };
    let prompt_hash = hex::encode(Sha256::digest(input.prompt.as_bytes()));
    let profile = AgentPromptProfile {
        schema_version: 1,
        id: input.id,
        name: input.name,
        version,
        target: input.target,
        prompt: input.prompt,
        prompt_hash,
        updated_at: crate::util::now_iso(),
    };
    let payload = serde_json::to_vec_pretty(&profile)
        .map_err(|error| format!("Serialize prompt profile: {error}"))?;
    atomic_write_json_restricted(&path, &payload)?;
    Ok(profile)
}

#[cfg(test)]
mod provider_tests {
    use super::*;

    fn dsh_profile(id: &str, model_id: Option<&str>) -> AgentPromptProfile {
        AgentPromptProfile {
            schema_version: 1,
            id: id.into(),
            name: id.into(),
            version: 1,
            target: AgentPromptProfileTarget {
                kind: AgentPromptProfileTargetKind::AcpHarness,
                target_id: "dsh".into(),
                model_id: model_id.map(str::to_owned),
            },
            prompt: id.into(),
            prompt_hash: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn deepseek_prompt_profile_identity_and_model_fallback_are_exact() {
        assert_eq!(canonical_buzz_agent_provider("deepseek"), Some("deepseek"));
        assert!(!same_buzz_agent_provider("deepseek", "openai"));
        let env = BTreeMap::from([
            ("BUZZ_AGENT_PROVIDER".to_string(), "deepseek".to_string()),
            ("DEEPSEEK_MODEL".to_string(), "deepseek-chat".to_string()),
        ]);
        assert_eq!(
            buzz_agent_api_target_from_env(&env),
            Some(("deepseek".into(), "deepseek-chat".into()))
        );
    }

    #[test]
    fn dsh_prompt_profile_prefers_exact_model_then_harness_wide_fallback() {
        let profiles = vec![
            dsh_profile("dsh-default", None),
            dsh_profile("dsh-fast", Some("deepseek-v4-flash")),
        ];

        assert_eq!(
            select_dsh_acp_prompt_profile(profiles.clone(), Some("deepseek-v4-flash"))
                .unwrap()
                .unwrap()
                .id,
            "dsh-fast"
        );
        assert_eq!(
            select_dsh_acp_prompt_profile(profiles.clone(), Some("another-model"))
                .unwrap()
                .unwrap()
                .id,
            "dsh-default"
        );
        assert_eq!(
            select_dsh_acp_prompt_profile(profiles, None)
                .unwrap()
                .unwrap()
                .id,
            "dsh-default"
        );
    }

    #[test]
    fn only_dsh_acp_and_buzz_api_profiles_accept_model_targets() {
        let input = |kind, target_id: &str| SaveAgentPromptProfileInput {
            id: "model-profile".into(),
            name: "Model profile".into(),
            target: AgentPromptProfileTarget {
                kind,
                target_id: target_id.into(),
                model_id: Some("model-a".into()),
            },
            prompt: "prompt".into(),
            expected_version: None,
        };

        assert!(validate_input(&input(AgentPromptProfileTargetKind::AcpHarness, "dsh")).is_ok());
        assert!(validate_input(&input(AgentPromptProfileTargetKind::AcpHarness, "codex")).is_err());
        assert!(validate_input(&input(
            AgentPromptProfileTargetKind::BuzzAgentApi,
            "deepseek"
        ))
        .is_ok());
    }
}
