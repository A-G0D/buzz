//! Selector eligibility and shared directed-attention rules. Aggregate SQL applies
//! the same eligibility before grouping, covered by the PostgreSQL parity test.
use super::model::ELIGIBLE_KINDS;

pub(super) fn eligible(
    kind: i32,
    own: bool,
    deleted: bool,
    received_ms: i64,
    cutoff_ms: i64,
) -> bool {
    ELIGIBLE_KINDS.contains(&kind) && !own && !deleted && received_ms >= cutoff_ms
}

pub(super) fn directed(channel_type: &str, actor_hex: &str, tags: &[Vec<String>]) -> bool {
    channel_type == "dm"
        || tags.iter().any(|tag| {
            tag.len() >= 2
                && ((tag[0] == "p" && tag[1].eq_ignore_ascii_case(actor_hex))
                    || (tag[0] == "broadcast" && tag[1] == "1"))
        })
}
