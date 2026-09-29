use serde::Serialize;

use super::types::{AgentExecutionProfileSnapshot, BackendKind};

pub const PROFILE_ID_ENV: &str = "BUZZ_ACP_EXECUTION_PROFILE_ID";
pub const PROFILE_VERSION_ENV: &str = "BUZZ_ACP_EXECUTION_PROFILE_VERSION";

/// Stamp the resolved profile on the managed ACP process, clearing any stale
/// inherited value when the agent has no selected profile.
pub fn apply_profile_provenance_env(
    command: &mut std::process::Command,
    profile: Option<&AgentExecutionProfileSnapshot>,
) {
    if let Some(profile) = profile {
        command.env(PROFILE_ID_ENV, &profile.id);
        command.env(PROFILE_VERSION_ENV, profile.version.to_string());
    } else {
        command.env_remove(PROFILE_ID_ENV);
        command.env_remove(PROFILE_VERSION_ENV);
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentArchetypeInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub snapshot: AgentExecutionProfileSnapshot,
}

#[derive(Debug, Clone, Copy)]
struct ArchetypeMetadata {
    id: &'static str,
    name: &'static str,
    description: &'static str,
}

#[derive(Debug, Clone, Copy)]
struct ArchetypeDefinition {
    info: ArchetypeMetadata,
    prompt_addendum: Option<&'static str>,
    parallelism: Option<u32>,
    idle_timeout_seconds: Option<u64>,
    max_turn_duration_seconds: Option<u64>,
}

const ARCHETYPES: &[ArchetypeDefinition] = &[
    ArchetypeDefinition {
        info: ArchetypeMetadata {
            id: "balanced",
            name: "Balanced",
            description: "Use the selected agent's normal behavior and Buzz limits.",
        },
        prompt_addendum: None,
        parallelism: None,
        idle_timeout_seconds: None,
        max_turn_duration_seconds: None,
    },
    ArchetypeDefinition {
        info: ArchetypeMetadata {
            id: "critic",
            name: "Critic",
            description: "Review assumptions and cite concrete evidence. This is review guidance, not a read-only tool sandbox.",
        },
        prompt_addendum: Some(
            "Review claims against the available evidence. Look for counterexamples, missing cases, and concrete regressions. Separate confirmed findings from uncertainty, and cite the exact evidence for each finding. Do not treat agreement or confidence as proof. Do not edit files or approve the work; return findings and unanswered questions. Keep the review within the user's stated scope.",
        ),
        parallelism: Some(1),
        idle_timeout_seconds: Some(300),
        max_turn_duration_seconds: Some(1_800),
    },
    ArchetypeDefinition {
        info: ArchetypeMetadata {
            id: "critic_correctness",
            name: "Correctness critic",
            description: "Check behavior, edge cases, and tests; report evidence-backed findings. Review guidance, not a read-only tool sandbox.",
        },
        prompt_addendum: Some(
            "Review the proposed change for concrete behavior defects, missing edge cases, and regressions in the production path. Trace important claims to the changed code and relevant callers. Check whether tests bind the real seam and would fail if the defect returned. Do not edit files or approve the work. Report each finding with severity, exact file/line or command evidence, impact, and uncertainty. If you find no issue, state what you inspected and what remains unverified.",
        ),
        parallelism: Some(1),
        idle_timeout_seconds: Some(300),
        max_turn_duration_seconds: Some(1_800),
    },
    ArchetypeDefinition {
        info: ArchetypeMetadata {
            id: "critic_security",
            name: "Security critic",
            description: "Trace trust boundaries, authorization, validation, and sensitive data flow. Review guidance, not a read-only tool sandbox.",
        },
        prompt_addendum: Some(
            "Review only the stated change and its directly affected boundaries. Trace untrusted input, identity, authorization, filesystem or process access, network destinations, and secret/data flow where relevant. Distinguish an exploitable path from a theoretical concern; cite the exact code and preconditions. Do not edit files or approve the work. Report severity, evidence, impact, and uncertainty. Do not recommend weakening a boundary to make a test pass.",
        ),
        parallelism: Some(1),
        idle_timeout_seconds: Some(300),
        max_turn_duration_seconds: Some(1_800),
    },
    ArchetypeDefinition {
        info: ArchetypeMetadata {
            id: "critic_ui_accessibility",
            name: "UI and accessibility critic",
            description: "Review hierarchy, responsive states, keyboard use, and assistive semantics. Review guidance, not a read-only tool sandbox.",
        },
        prompt_addendum: Some(
            "Review the actual user flow and rendered UI for hierarchy, spacing, loading/empty/error states, responsive behavior, keyboard operation, focus, screen-reader semantics, and reduced motion. Separate source-based findings from anything that requires a live visual/runtime check. Do not edit files or approve the work. Cite the exact component and observable failure; report severity and uncertainty.",
        ),
        parallelism: Some(1),
        idle_timeout_seconds: Some(240),
        max_turn_duration_seconds: Some(1_200),
    },
    ArchetypeDefinition {
        info: ArchetypeMetadata {
            id: "critic_performance",
            name: "Performance critic",
            description: "Look for resource, latency, and scaling regressions; separate measured from inferred. Review guidance, not a read-only tool sandbox.",
        },
        prompt_addendum: Some(
            "Review hot paths, resource bounds, query/process fan-out, allocations, retries, and cancellation behavior that the change affects. Prefer a reproducible measurement or a concrete complexity/path argument; label unmeasured concerns as hypotheses. Do not edit files or approve the work. Cite the exact code and workload assumptions, and report severity, likely impact, and uncertainty.",
        ),
        parallelism: Some(1),
        idle_timeout_seconds: Some(240),
        max_turn_duration_seconds: Some(1_200),
    },
    ArchetypeDefinition {
        info: ArchetypeMetadata {
            id: "critic_architecture",
            name: "Architecture critic",
            description: "Check ownership, duplication, migration, and failure recovery. Review guidance, not a read-only tool sandbox.",
        },
        prompt_addendum: Some(
            "Review whether the change follows existing source-of-truth boundaries, avoids duplicate abstractions, preserves compatibility, and has a recoverable failure/migration path. Identify concrete coupling or ownership problems rather than proposing broad rewrites. Do not edit files or approve the work. Cite affected interfaces and explain the consequence and uncertainty.",
        ),
        parallelism: Some(1),
        idle_timeout_seconds: Some(300),
        max_turn_duration_seconds: Some(1_800),
    },
    ArchetypeDefinition {
        info: ArchetypeMetadata {
            id: "critic_product",
            name: "Product critic",
            description: "Check user intent, acceptance criteria, failure states, and honest capability claims. Review guidance, not a read-only tool sandbox.",
        },
        prompt_addendum: Some(
            "Compare the user request and explicit acceptance criteria with the actual changed flow. Look for missing user-visible states, misleading promises, inaccessible recovery paths, and scope drift. Do not edit files or approve the work. Cite the specific request, UI or behavior, explain user impact, and mark assumptions or unverified behavior.",
        ),
        parallelism: Some(1),
        idle_timeout_seconds: Some(240),
        max_turn_duration_seconds: Some(1_200),
    },
    ArchetypeDefinition {
        info: ArchetypeMetadata {
            id: "fast",
            name: "Fast",
            description: "Favor a short path and concise output with a 15-minute turn cap.",
        },
        prompt_addendum: Some(
            "Prefer the shortest correct path. Avoid speculative work and unnecessary tool calls. Keep the final result concise, and state clearly when the time limit prevents completion.",
        ),
        parallelism: Some(1),
        idle_timeout_seconds: Some(180),
        max_turn_duration_seconds: Some(900),
    },
    ArchetypeDefinition {
        info: ArchetypeMetadata {
            id: "scope_minimal",
            name: "Scope-minimal",
            description: "Make the smallest complete change inside the explicit request.",
        },
        prompt_addendum: Some(
            "Make the smallest complete change that satisfies the request. Preserve unrelated work, avoid adjacent cleanup, and identify any necessary scope expansion before making it.",
        ),
        parallelism: Some(1),
        idle_timeout_seconds: Some(240),
        max_turn_duration_seconds: Some(1_200),
    },
    ArchetypeDefinition {
        info: ArchetypeMetadata {
            id: "research",
            name: "Research",
            description: "Separate source-backed facts, inference, and open questions.",
        },
        prompt_addendum: Some(
            "Separate source-backed facts from inference and open questions. Prefer primary evidence, retain useful source links, and do not present an unverified capability as available.",
        ),
        parallelism: Some(1),
        idle_timeout_seconds: Some(600),
        max_turn_duration_seconds: Some(3_600),
    },
    ArchetypeDefinition {
        info: ArchetypeMetadata {
            id: "cost_conscious",
            name: "Cost-conscious",
            description: "Limit turns to 15 minutes and avoid unnecessary tool calls.",
        },
        prompt_addendum: Some(
            "Use the smallest amount of tool work needed to complete the task. Avoid repeated exploration and retries without new evidence. If the available budget is insufficient, return the useful partial result and explain the limit.",
        ),
        parallelism: Some(1),
        idle_timeout_seconds: Some(180),
        max_turn_duration_seconds: Some(900),
    },
];

pub fn list_agent_archetypes() -> Vec<AgentArchetypeInfo> {
    ARCHETYPES
        .iter()
        .map(|definition| AgentArchetypeInfo {
            id: definition.info.id,
            name: definition.info.name,
            description: definition.info.description,
            snapshot: snapshot_for(definition),
        })
        .collect()
}

fn snapshot_for(definition: &ArchetypeDefinition) -> AgentExecutionProfileSnapshot {
    AgentExecutionProfileSnapshot {
        id: definition.info.id.to_string(),
        version: 1,
        name: definition.info.name.to_string(),
        prompt_addendum: definition.prompt_addendum.map(str::to_string),
        parallelism: definition.parallelism,
        idle_timeout_seconds: definition.idle_timeout_seconds,
        max_turn_duration_seconds: definition.max_turn_duration_seconds,
    }
}

pub fn validate_archetype_backend(id: Option<&str>, backend: &BackendKind) -> Result<(), String> {
    let selected = id.map(str::trim).filter(|value| !value.is_empty());
    if selected.is_some() && !matches!(backend, BackendKind::Local) {
        return Err("Agent archetypes currently apply only to local ACP agents.".to_string());
    }
    Ok(())
}

pub fn resolve_execution_profile(
    id: Option<&str>,
) -> Result<Option<AgentExecutionProfileSnapshot>, String> {
    let Some(id) = id.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };

    let definition = ARCHETYPES
        .iter()
        .find(|definition| definition.info.id == id)
        .ok_or_else(|| {
            format!("Unknown agent archetype '{id}'. Refresh Buzz and choose a listed archetype.")
        })?;

    Ok(Some(snapshot_for(definition)))
}

pub fn compose_system_prompt(
    base: Option<&str>,
    profile: Option<&AgentExecutionProfileSnapshot>,
) -> Option<String> {
    let base = base.map(str::trim).filter(|value| !value.is_empty());
    let addendum = profile
        .and_then(|profile| profile.prompt_addendum.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty());

    match (base, addendum) {
        (Some(base), Some(addendum)) => {
            Some(format!(
            "{base}\n\n<agent-archetype id=\"{}\" version=\"{}\">\n{addendum}\n</agent-archetype>",
            profile.map(|profile| profile.id.as_str()).unwrap_or("custom"),
            profile.map(|profile| profile.version).unwrap_or(1),
        ))
        }
        (Some(base), None) => Some(base.to_string()),
        (None, Some(addendum)) => Some(format!(
            "<agent-archetype id=\"{}\" version=\"{}\">\n{addendum}\n</agent-archetype>",
            profile
                .map(|profile| profile.id.as_str())
                .unwrap_or("custom"),
            profile.map(|profile| profile.version).unwrap_or(1),
        )),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_ids_resolve_to_versioned_local_snapshots() {
        for info in list_agent_archetypes() {
            let snapshot = resolve_execution_profile(Some(info.id))
                .expect("listed archetype should resolve")
                .expect("listed archetype should create a snapshot");
            assert_eq!(snapshot.id, info.id);
            assert_eq!(snapshot, info.snapshot);
            assert_eq!(snapshot.version, 1);
            assert!(!snapshot.name.is_empty());
        }
    }

    #[test]
    fn unknown_archetype_is_rejected_instead_of_silently_ignored() {
        assert!(resolve_execution_profile(Some("deepseek"))
            .unwrap_err()
            .contains("Unknown agent archetype"));
    }

    #[test]
    fn acp_archetype_is_rejected_for_a_remote_execution_backend() {
        let backend = BackendKind::Provider {
            id: "remote".to_string(),
            config: serde_json::json!({}),
        };
        assert!(validate_archetype_backend(Some("critic"), &backend)
            .unwrap_err()
            .contains("only to local ACP agents"));
        validate_archetype_backend(Some("critic"), &BackendKind::Local)
            .expect("local ACP agents support archetypes");
    }

    #[test]
    fn prompt_composition_preserves_base_and_scopes_the_overlay() {
        let snapshot = resolve_execution_profile(Some("critic")).unwrap().unwrap();
        let prompt = compose_system_prompt(Some("persona instructions"), Some(&snapshot)).unwrap();
        assert!(prompt
            .starts_with("persona instructions\n\n<agent-archetype id=\"critic\" version=\"1\">"));
        assert!(prompt.ends_with("</agent-archetype>"));
    }

    #[test]
    fn balanced_profile_does_not_add_instructions() {
        let snapshot = resolve_execution_profile(Some("balanced"))
            .unwrap()
            .unwrap();
        assert_eq!(
            compose_system_prompt(Some("persona instructions"), Some(&snapshot)),
            Some("persona instructions".to_string())
        );
    }

    #[test]
    fn fast_profile_uses_existing_enforced_turn_limits() {
        let snapshot = resolve_execution_profile(Some("fast")).unwrap().unwrap();
        assert_eq!(snapshot.parallelism, Some(1));
        assert_eq!(snapshot.idle_timeout_seconds, Some(180));
        assert_eq!(snapshot.max_turn_duration_seconds, Some(900));
    }

    #[test]
    fn focused_critic_profiles_are_versioned_bounded_and_review_only() {
        for id in [
            "critic",
            "critic_correctness",
            "critic_security",
            "critic_ui_accessibility",
            "critic_performance",
            "critic_architecture",
            "critic_product",
        ] {
            let profile = resolve_execution_profile(Some(id))
                .unwrap()
                .unwrap_or_else(|| panic!("critic profile {id} should resolve"));
            let prompt = profile.prompt_addendum.as_deref().unwrap_or_default();
            let info = list_agent_archetypes()
                .into_iter()
                .find(|info| info.id == id)
                .unwrap_or_else(|| panic!("critic profile {id} should be listed"));
            assert!(
                info.description.contains("not a read-only tool sandbox"),
                "{id} must not imply technical tool isolation"
            );
            assert!(
                prompt.contains("Do not edit files"),
                "{id} must be review-only guidance"
            );
            assert!(
                prompt.contains("uncertainty") || prompt.contains("unverified"),
                "{id} must disclose limits"
            );
            assert_eq!(profile.parallelism, Some(1), "{id} must remain serial");
            assert!(
                profile
                    .max_turn_duration_seconds
                    .is_some_and(|seconds| seconds <= 1_800),
                "{id} must have a bounded turn"
            );
        }
    }

    #[test]
    fn profile_provenance_env_is_desktop_owned_and_cleared_when_absent() {
        use std::ffi::OsStr;

        let profile = resolve_execution_profile(Some("critic_security"))
            .unwrap()
            .unwrap();
        let mut command = std::process::Command::new("true");
        command.env(PROFILE_ID_ENV, "spoofed");
        command.env(PROFILE_VERSION_ENV, "999");

        apply_profile_provenance_env(&mut command, Some(&profile));
        let env = command
            .get_envs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            env.get(OsStr::new(PROFILE_ID_ENV)).unwrap().unwrap(),
            "critic_security"
        );
        assert_eq!(
            env.get(OsStr::new(PROFILE_VERSION_ENV)).unwrap().unwrap(),
            "1"
        );

        apply_profile_provenance_env(&mut command, None);
        let env = command
            .get_envs()
            .collect::<std::collections::HashMap<_, _>>();
        assert!(env.get(OsStr::new(PROFILE_ID_ENV)).unwrap().is_none());
        assert!(env.get(OsStr::new(PROFILE_VERSION_ENV)).unwrap().is_none());
    }
}
