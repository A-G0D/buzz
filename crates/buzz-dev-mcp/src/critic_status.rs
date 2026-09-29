//! Read one critic record from the current identity-scoped local journal.

use buzz_run_journal::{CriticRoundRecord, RunJournal};
use rmcp::{model::CallToolResult, model::Content, ErrorData};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CriticStatusParams {
    /// Stable round ID returned when `run_critics` saved the local record.
    pub round_id: String,
}

#[derive(Debug, Serialize)]
struct CriticStatusResult {
    status: &'static str,
    round: Option<CriticRoundRecord>,
    error_code: Option<&'static str>,
}

pub fn run(params: CriticStatusParams) -> Result<CallToolResult, ErrorData> {
    let round_id = Uuid::parse_str(&params.round_id)
        .map_err(|_| ErrorData::invalid_params("round_id must be a UUID.", None))?
        .to_string();
    let result = match RunJournal::open_default_scoped() {
        Ok(journal) => read_round(&journal, &round_id),
        Err(_) => CriticStatusResult {
            status: "unavailable",
            round: None,
            error_code: Some("local_identity_or_storage_unavailable"),
        },
    };
    let body = serde_json::to_string(&result)
        .map_err(|_| ErrorData::internal_error("Could not encode critic status.", None))?;
    Ok(CallToolResult::success(vec![Content::text(body)]))
}

fn read_round(journal: &RunJournal, round_id: &str) -> CriticStatusResult {
    match journal.critic_round(round_id) {
        Ok(Some(round)) => CriticStatusResult {
            status: "found",
            round: Some(round),
            error_code: None,
        },
        Ok(None) => CriticStatusResult {
            status: "not_found",
            round: None,
            error_code: None,
        },
        Err(_) => CriticStatusResult {
            status: "unavailable",
            round: None,
            error_code: Some("local_journal_unavailable"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn open_journal(nest: &Path) -> RunJournal {
        RunJournal::open_scoped(nest, "ws://localhost:3000", &"d".repeat(64)).unwrap()
    }

    #[test]
    fn rejects_non_uuid_round_ids_without_opening_the_journal() {
        let error = run(CriticStatusParams {
            round_id: "not-a-round-id".into(),
        })
        .unwrap_err();
        assert_eq!(error.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn reads_a_saved_round_from_only_the_supplied_identity_journal() {
        use buzz_run_journal::{CriticReviewerRecord, CriticRoundSettings};

        let nest = tempfile::tempdir().unwrap();
        let journal = open_journal(nest.path());
        let round_id = "123e4567-e89b-12d3-a456-426614174005";
        journal
            .record_critic_round(
                round_id,
                &"a".repeat(64),
                &"b".repeat(64),
                &"c".repeat(64),
                CriticRoundSettings {
                    max_output_tokens: 512,
                    time_limit_seconds: 45,
                    thinking_effort_requested: None,
                    estimated_round_cost_budget_microusd: None,
                    route_profile: None,
                    coordinator_guide: None,
                },
                vec![CriticReviewerRecord {
                    role: "security".into(),
                    status: "completed".into(),
                    output: Some("One finding.".into()),
                    output_truncated: false,
                    stop_reason: Some("end_turn".into()),
                    candidate_id: None,
                    provider_id: None,
                    model_id: None,
                    route_profile: None,
                    estimated_cost_limit_microusd: None,
                    elapsed_ms: Some(2_000),
                    error_code: None,
                }],
            )
            .unwrap();

        let found = read_round(&journal, round_id);
        assert_eq!(found.status, "found");
        assert_eq!(
            found.round.unwrap().reviewers[0].output.as_deref(),
            Some("One finding.")
        );

        let other =
            RunJournal::open_scoped(nest.path(), "ws://localhost:3000", &"e".repeat(64)).unwrap();
        let not_found = read_round(&other, round_id);
        assert_eq!(not_found.status, "not_found");
        assert!(not_found.round.is_none());
    }
}
