use buzz_agent_pkg::route_preview::{RouteProfileCandidate, RoutePromptProfileRef};
use sha2::{Digest, Sha256};

use crate::commands::resolve_buzz_agent_api_prompt_profile;

use super::{BackendKind, ManagedAgentRecord, known_acp_runtime};

pub(crate) const ROUTE_PROFILE_ID_ENV: &str = "BUZZ_AGENT_ROUTE_PROFILE_ID";
pub(crate) const ROUTE_PROFILE_JSON_ENV: &str = "BUZZ_AGENT_ROUTE_PROFILE_JSON";
pub(crate) const ROUTE_PROFILE_PROVENANCE_ID_ENV: &str = "BUZZ_ACP_ROUTE_PROFILE_ID";
pub(crate) const ROUTE_PROFILE_PROVENANCE_VERSION_ENV: &str = "BUZZ_ACP_ROUTE_PROFILE_VERSION";
pub(crate) const ROUTE_PROFILE_PROVENANCE_HASH_ENV: &str = "BUZZ_ACP_ROUTE_PROFILE_HASH";
pub(crate) const ROUTE_COST_BUDGET_ENV: &str = "BUZZ_AGENT_ROUTE_COST_BUDGET_MICROUSD";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct AgentRouteProfileIdentity {
    pub id: String,
    pub version: u32,
    /// Fingerprint of the launch-resolved document, including exact target
    /// prompt-profile references, rather than only the saved editor document.
    pub hash: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedAgentRouteProfile {
    pub identity: AgentRouteProfileIdentity,
    pub serialized_document: String,
    pub document: buzz_agent_pkg::route_preview::RouteProfileDocument,
}

/// Resolve an explicitly assigned local route profile and pin any exact
/// provider/model prompt profiles into the immutable launch snapshot.
pub(crate) fn resolve_for_spawn(
    record: &ManagedAgentRecord,
    command: &str,
) -> Result<Option<ResolvedAgentRouteProfile>, String> {
    // Assignment is agent-local. A global or persona environment value must
    // never silently attach a route profile to unrelated agents.
    let profile_id = super::config_bridge::effort::get_ci(&record.env_vars, ROUTE_PROFILE_ID_ENV)
        .map(|value| value.trim())
        .filter(|id| !id.is_empty());
    let Some(profile_id) = profile_id else {
        return Ok(None);
    };
    if record.backend != BackendKind::Local
        || !known_acp_runtime(command).is_some_and(|runtime| runtime.id == "buzz-agent")
    {
        return Err("API route profiles apply only to local Buzz Agent runtimes.".into());
    }

    resolve_saved_profile(profile_id).map(Some)
}

/// Resolve one saved profile with the same target-prompt expansion and hash
/// used at managed-agent launch. Local task-fit reviews use this identity so
/// a later profile or prompt change invalidates the signed association.
pub(crate) fn resolve_saved_profile(profile_id: &str) -> Result<ResolvedAgentRouteProfile, String> {
    let profile = crate::commands::read_agent_route_profile(profile_id.to_string())?;
    let mut document = profile.document;
    for candidate in &mut document.candidates {
        apply_target_prompt_profile(candidate)?;
    }
    document.profile_id = Some(profile.id.clone());
    document.profile_version = Some(profile.version);
    document.profile_hash = Some(profile.document_hash.clone());
    document
        .validate()
        .map_err(|error| format!("Resolved route profile is invalid: {error}"))?;
    let serialized_document = serde_json::to_string(&document)
        .map_err(|error| format!("Serialize resolved route profile: {error}"))?;
    if serialized_document.len() > 64 * 1024 {
        return Err("Resolved route profile exceeds 64 KiB after prompt-profile matching.".into());
    }
    let resolved_hash = hex::encode(Sha256::digest(serialized_document.as_bytes()));
    Ok(ResolvedAgentRouteProfile {
        identity: AgentRouteProfileIdentity {
            id: profile.id.clone(),
            version: profile.version,
            hash: resolved_hash,
        },
        serialized_document,
        document,
    })
}

fn apply_target_prompt_profile(candidate: &mut RouteProfileCandidate) -> Result<(), String> {
    let Some(profile) =
        resolve_buzz_agent_api_prompt_profile(&candidate.provider, &candidate.model)?
    else {
        return Ok(());
    };
    let prompt_hash = hex::encode(Sha256::digest(profile.prompt.as_bytes()));
    if profile.prompt_hash != prompt_hash {
        return Err(format!(
            "Prompt profile '{}' changed while preparing the route; reload and retry.",
            profile.id
        ));
    }
    let section = format!("## Target-specific prompt profile\n\n{}", profile.prompt);
    if candidate.prompt_addendum.trim().is_empty() {
        candidate.prompt_addendum = section;
    } else {
        candidate.prompt_addendum = format!("{}\n\n{section}", candidate.prompt_addendum);
    }
    candidate.prompt_profile = Some(RoutePromptProfileRef {
        id: profile.id,
        version: profile.version,
        prompt_hash,
    });
    Ok(())
}

/// Scrub inherited/user copies and stamp the selected immutable route snapshot.
pub(crate) fn apply_route_profile_env(
    command: &mut std::process::Command,
    profile: Option<&ResolvedAgentRouteProfile>,
) {
    command
        .env_remove(ROUTE_PROFILE_ID_ENV)
        .env_remove(ROUTE_PROFILE_JSON_ENV)
        .env_remove(ROUTE_PROFILE_PROVENANCE_ID_ENV)
        .env_remove(ROUTE_PROFILE_PROVENANCE_VERSION_ENV)
        .env_remove(ROUTE_PROFILE_PROVENANCE_HASH_ENV)
        .env_remove(ROUTE_COST_BUDGET_ENV);
    if let Some(profile) = profile {
        command
            .env(ROUTE_PROFILE_JSON_ENV, &profile.serialized_document)
            .env(ROUTE_PROFILE_PROVENANCE_ID_ENV, &profile.identity.id)
            .env(
                ROUTE_PROFILE_PROVENANCE_VERSION_ENV,
                profile.identity.version.to_string(),
            )
            .env(ROUTE_PROFILE_PROVENANCE_HASH_ENV, &profile.identity.hash);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn route_profile_environment_is_scrubbed_and_pinned() {
        use std::ffi::OsStr;

        let mut command = std::process::Command::new("true");
        command
            .env(ROUTE_PROFILE_ID_ENV, "spoofed")
            .env(ROUTE_PROFILE_JSON_ENV, "spoofed-json")
            .env(ROUTE_PROFILE_PROVENANCE_ID_ENV, "spoofed-id")
            .env(ROUTE_COST_BUDGET_ENV, "spoofed-cost");
        let profile = ResolvedAgentRouteProfile {
            identity: AgentRouteProfileIdentity {
                id: "local-first".into(),
                version: 3,
                hash: "a".repeat(64),
            },
            serialized_document: "{\"version\":1}".into(),
            document: serde_json::from_str(r#"{"version":1,"candidates":[]}"#)
                .expect("route profile document"),
        };

        apply_route_profile_env(&mut command, Some(&profile));
        let env = command.get_envs().collect::<BTreeMap<_, _>>();
        assert_eq!(
            env.get(OsStr::new(ROUTE_PROFILE_JSON_ENV))
                .unwrap()
                .unwrap(),
            "{\"version\":1}"
        );
        assert_eq!(env.get(OsStr::new(ROUTE_PROFILE_ID_ENV)).unwrap(), &None);
        assert_eq!(env.get(OsStr::new(ROUTE_COST_BUDGET_ENV)).unwrap(), &None);
        assert_eq!(
            env.get(OsStr::new(ROUTE_PROFILE_PROVENANCE_VERSION_ENV))
                .unwrap()
                .unwrap(),
            "3"
        );

        apply_route_profile_env(&mut command, None);
        let env = command.get_envs().collect::<BTreeMap<_, _>>();
        assert!(
            env.get(OsStr::new(ROUTE_PROFILE_JSON_ENV))
                .unwrap()
                .is_none()
        );
        assert!(
            env.get(OsStr::new(ROUTE_PROFILE_PROVENANCE_ID_ENV))
                .unwrap()
                .is_none()
        );
    }
}
