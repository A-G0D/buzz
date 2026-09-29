use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::commands::{
    append_buzz_agent_api_prompt_profile, buzz_agent_api_target_from_env,
    resolve_buzz_agent_api_prompt_profile, AgentPromptProfile,
};

use super::{known_acp_runtime, BackendKind, ManagedAgentRecord};

pub(crate) const PROFILE_ID_ENV: &str = "BUZZ_ACP_PROMPT_PROFILE_ID";
pub(crate) const PROFILE_VERSION_ENV: &str = "BUZZ_ACP_PROMPT_PROFILE_VERSION";
pub(crate) const PROFILE_HASH_ENV: &str = "BUZZ_ACP_PROMPT_PROFILE_HASH";

/// The exact Buzz Agent API destination and optional local prompt profile
/// resolved from the same effective environment that will be spawned.
#[derive(Clone)]
pub(crate) struct ResolvedAgentPromptProfile {
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
    pub dsh: bool,
    pub profile: Option<AgentPromptProfile>,
}

/// Resolve a profile only for a local, catalogued Buzz Agent runtime. Other
/// ACP harnesses and remote agents keep their own prompt configuration.
pub(crate) fn resolve_for_spawn(
    record: &ManagedAgentRecord,
    command: &str,
    env: &BTreeMap<String, String>,
    model_id: Option<&str>,
) -> Result<Option<ResolvedAgentPromptProfile>, String> {
    if record.backend != BackendKind::Local {
        return Ok(None);
    }
    if known_acp_runtime(command).is_some_and(|runtime| runtime.id == "dsh") {
        return Ok(
            crate::commands::resolve_dsh_acp_prompt_profile(model_id)?.map(|profile| {
                ResolvedAgentPromptProfile {
                    provider_id: None,
                    model_id: model_id.map(str::to_owned),
                    dsh: true,
                    profile: Some(profile),
                }
            }),
        );
    }
    if !known_acp_runtime(command).is_some_and(|runtime| runtime.id == "buzz-agent") {
        return Ok(None);
    }
    let Some((provider_id, model_id)) = buzz_agent_api_target_from_env(env) else {
        return Ok(None);
    };
    let profile = resolve_buzz_agent_api_prompt_profile(&provider_id, &model_id)?;
    Ok(Some(ResolvedAgentPromptProfile {
        provider_id: Some(provider_id),
        model_id: Some(model_id),
        dsh: false,
        profile,
    }))
}

/// Keep the saved agent instructions and append the exact matched profile.
pub(crate) fn compose_system_prompt(
    base_prompt: Option<&str>,
    resolved: Option<&ResolvedAgentPromptProfile>,
) -> Option<String> {
    match resolved.and_then(|resolved| resolved.profile.as_ref()) {
        Some(profile) => Some(append_buzz_agent_api_prompt_profile(base_prompt, profile)),
        None => base_prompt.map(str::to_owned),
    }
}

/// Stamp the exact API destination and matched profile after user env layers,
/// so the local run journal cannot be mislabeled by a saved env override.
pub(crate) fn apply_provenance_env(
    command: &mut Command,
    resolved: Option<&ResolvedAgentPromptProfile>,
) {
    if let Some(resolved) = resolved.filter(|resolved| !resolved.dsh) {
        command
            .env(
                "BUZZ_ACP_PROVIDER",
                resolved.provider_id.as_deref().unwrap_or_default(),
            )
            .env(
                "BUZZ_ACP_MODEL",
                resolved.model_id.as_deref().unwrap_or_default(),
            );
    }
    if let Some(profile) = resolved.and_then(|resolved| resolved.profile.as_ref()) {
        command
            .env(PROFILE_ID_ENV, &profile.id)
            .env(PROFILE_VERSION_ENV, profile.version.to_string())
            .env(PROFILE_HASH_ENV, &profile.prompt_hash);
    } else {
        command
            .env_remove(PROFILE_ID_ENV)
            .env_remove(PROFILE_VERSION_ENV)
            .env_remove(PROFILE_HASH_ENV);
    }
}

pub(crate) fn render_dsh_prompt_overlay(prompt: &str) -> Result<Vec<u8>, String> {
    let patch = serde_yaml::to_string(&serde_json::json!([{
        "id": "system-prompt",
        "config": {
            "includeHarnessIdentity": true,
            "includeRuntimeContext": true,
            "personaPrefix": prompt
        }
    }]))
    .map_err(|e| format!("Render DSH ACP prompt overlay: {e}"))?;
    Ok(patch.into_bytes())
}

pub(crate) fn dsh_overlay_path(
    root: &Path,
    profile: &AgentPromptProfile,
) -> Result<PathBuf, String> {
    if profile.id.is_empty()
        || !profile
            .id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("DSH prompt profile ID cannot be used as a safe overlay filename".into());
    }
    let path = root
        .join(".agents")
        .join("dsh-prompt-overlays")
        .join(format!("{}-v{}.patch.yml", profile.id, profile.version));
    if !path.starts_with(root) {
        return Err("DSH overlay path escaped the Buzz workspace".into());
    }
    Ok(path)
}

pub(crate) fn write_dsh_overlay(
    root: &Path,
    profile: &AgentPromptProfile,
    prompt: &str,
) -> Result<PathBuf, String> {
    let path = dsh_overlay_path(root, profile)?;
    let canonical_root =
        fs::canonicalize(root).map_err(|e| format!("Resolve Buzz workspace: {e}"))?;
    let agents = canonical_root.join(".agents");
    let dir = agents.join("dsh-prompt-overlays");
    for ancestor in [&agents, &dir] {
        match fs::symlink_metadata(ancestor) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                return Err("Refusing unsafe Buzz DSH overlay directory".into())
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir(ancestor)
                .map_err(|e| format!("Create Buzz DSH overlay directory: {e}"))?,
            Err(error) => return Err(format!("Inspect Buzz DSH overlay directory: {error}")),
        }
        if !fs::canonicalize(ancestor)
            .map_err(|e| format!("Resolve Buzz DSH overlay directory: {e}"))?
            .starts_with(&canonical_root)
        {
            return Err("DSH overlay directory escaped the Buzz workspace".into());
        }
    }
    if let Ok(meta) = fs::symlink_metadata(&path) {
        if meta.file_type().is_symlink() || !meta.is_file() {
            return Err("Refusing to replace a non-regular Buzz DSH overlay file".into());
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("Restrict Buzz DSH overlay directory: {e}"))?;
    }
    let bytes = render_dsh_prompt_overlay(prompt)?;
    super::storage::atomic_write_json_restricted(&path, &bytes)?;
    Ok(path)
}

#[cfg(test)]
mod dsh_overlay_tests {
    use super::*;

    fn profile(id: &str) -> AgentPromptProfile {
        AgentPromptProfile {
            schema_version: 1,
            id: id.into(),
            name: "DSH profile".into(),
            version: 2,
            target: crate::commands::AgentPromptProfileTarget {
                kind: crate::commands::AgentPromptProfileTargetKind::AcpHarness,
                target_id: "dsh".into(),
                model_id: None,
            },
            prompt: "hello".into(),
            prompt_hash: "hash".into(),
            updated_at: "now".into(),
        }
    }

    #[test]
    fn renders_only_verified_system_prompt_row_config() {
        let bytes = render_dsh_prompt_overlay("line one\nline two").unwrap();
        let value: serde_yaml::Value = serde_yaml::from_slice(&bytes).unwrap();
        assert_eq!(value[0]["id"].as_str(), Some("system-prompt"));
        let config = &value[0]["config"];
        assert_eq!(config["includeHarnessIdentity"].as_bool(), Some(true));
        assert_eq!(config["includeRuntimeContext"].as_bool(), Some(true));
        assert_eq!(config["personaPrefix"].as_str(), Some("line one\nline two"));
        assert_eq!(config.as_mapping().unwrap().len(), 3);
    }

    #[test]
    fn overlay_path_is_contained_and_write_is_atomic_buzz_owned_file() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(dsh_overlay_path(tmp.path(), &profile("../escape")).is_err());
        let path = write_dsh_overlay(tmp.path(), &profile("local-dsh"), "first").unwrap();
        assert!(path.starts_with(tmp.path()));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            String::from_utf8(render_dsh_prompt_overlay("first").unwrap()).unwrap()
        );
        write_dsh_overlay(tmp.path(), &profile("local-dsh"), "second").unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            String::from_utf8(render_dsh_prompt_overlay("second").unwrap()).unwrap()
        );
    }
}
