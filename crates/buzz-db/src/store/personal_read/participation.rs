//! Bounded actor participation evidence from the existing conversation store.
use super::model::ELIGIBLE_KINDS;
use crate::Result;
use buzz_core::CommunityId;
use sqlx::{Acquire, PgConnection, Row};
use std::collections::HashMap;
use uuid::Uuid;

const MAX_THREAD_SCAN: i64 = 256;
// Bound multiplicative work independently of the receipt evidence window.
const MAX_ROOTS: usize = 1024;

/// Positive evidence survives truncation; absence requires exhausting the thread.
/// Participation is independent of unread retention and read frontiers.
pub(super) async fn resolve(
    conn: &mut PgConnection,
    community: CommunityId,
    actor: &[u8],
    targets: &[(Uuid, Vec<u8>)],
) -> Result<HashMap<(Uuid, Vec<u8>), Option<bool>>> {
    if targets.is_empty() {
        return Ok(HashMap::new());
    }
    let mut targets = targets.to_vec();
    targets.sort_unstable();
    targets.dedup();
    targets.truncate(MAX_ROOTS);
    let channels: Vec<_> = targets.iter().map(|(channel, _)| *channel).collect();
    let roots: Vec<_> = targets.iter().map(|(_, root)| root.clone()).collect();
    // Optional inference must not abort authoritative unread/frontier reads.
    // A nested transaction is a savepoint; rollback also restores the caller's
    // statement timeout. No state is written in this read-only inference.
    let mut budget = conn.begin().await?;
    sqlx::query("SET LOCAL statement_timeout = '500ms'")
        .execute(&mut *budget)
        .await?;
    sqlx::query("SET LOCAL jit = off")
        .execute(&mut *budget)
        .await?;
    let result = sqlx::query(
        "SELECT t.channel_id,t.root_id,
            CASE WHEN COALESCE(root.participated,false) OR COALESCE(replies.participated,false)
                THEN true WHEN replies.candidates <= $5 THEN false ELSE NULL END AS participated
         FROM unnest($3::uuid[],$4::bytea[]) t(channel_id,root_id)
         LEFT JOIN LATERAL (
            SELECT e.pubkey=$2 AND e.deleted_at IS NULL AND e.kind=ANY($6) AS participated
            FROM events e WHERE e.community_id=$1 AND e.channel_id=t.channel_id AND e.id=t.root_id
            ORDER BY e.created_at DESC LIMIT 1
         ) root ON true
         LEFT JOIN LATERAL (
            WITH candidates AS MATERIALIZED (
                SELECT event_created_at,event_id FROM thread_metadata
                WHERE community_id=$1 AND root_event_id=t.root_id
                ORDER BY event_created_at DESC,event_id LIMIT $5+1
            )
            SELECT count(*) AS candidates,
                bool_or(e.pubkey=$2 AND e.deleted_at IS NULL AND e.kind=ANY($6)) AS participated
            FROM candidates tm LEFT JOIN events e ON e.community_id=$1
                AND e.channel_id=t.channel_id AND e.created_at=tm.event_created_at AND e.id=tm.event_id
         ) replies ON true",
    )
    .bind(community.as_uuid())
    .bind(actor)
    .bind(channels)
    .bind(roots)
    .bind(MAX_THREAD_SCAN)
    .bind(ELIGIBLE_KINDS.as_slice())
    .fetch_all(&mut *budget)
    .await;
    budget.rollback().await?;
    let rows = match result {
        Ok(rows) => rows,
        Err(sqlx::Error::Database(error))
            if matches!(error.code().as_deref(), Some("57014" | "55P03")) =>
        {
            return Ok(HashMap::new());
        }
        Err(error) => return Err(error.into()),
    };
    rows.into_iter()
        .map(|row| {
            Ok((
                (row.try_get("channel_id")?, row.try_get("root_id")?),
                row.try_get("participated")?,
            ))
        })
        .collect()
}
