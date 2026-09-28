//! Bounded evidence projection. The cap limits evidence, not the definition of
//! unread: an unexamined tail yields a positive lower bound or an unknown count.

use super::{model::*, participation, writes};
use buzz_core::CommunityId;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{Acquire, PgConnection, Row};
use std::collections::HashMap;
use uuid::Uuid;

use crate::{observability, Db, DbError, Result};

impl Db {
    /// Read a bounded joined roster from the writer. One SQL statement projects
    /// channels, event evidence and read authority at a compatible MVCC cut.
    /// Callers must recheck admission/resource access before releasing this data.
    pub async fn personal_read_sidebar(
        &self,
        community: CommunityId,
        actor: &nostr::PublicKey,
        retention_seconds: u32,
        limit: usize,
        after: Option<Uuid>,
    ) -> Result<SidebarPage> {
        if !(1..=MAX_CHANNELS).contains(&limit) {
            return Err(DbError::InvalidData("invalid sidebar limit".into()));
        }
        let mut conn = observability::acquire_writer(
            &self.pool,
            observability::WriterOperation::SubscriptionHistory,
        )
        .await?;
        let mut tx = conn.begin().await?;
        // Import status and frontier evidence share a compatible read-only cut.
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        writes::deadlines(&mut tx).await?;
        sqlx::query("SET LOCAL jit = off").execute(&mut *tx).await?;
        let actor_bytes = actor.to_bytes();
        let account = read_account(&mut tx, community, &actor_bytes, retention_seconds).await?;
        // The inner event LIMIT is deliberately before eligibility filtering.
        // This bounds rows/joins even with long deleted or self-authored runs.
        // Aggregate equivalent eligible evidence before transfer. Multiplicity
        // preserves counts; raw scan count (not group count) proves exhaustion.
        // SQL eligibility mirrors classification::eligible (PostgreSQL parity test).
        // Canonical covered rows need no tags; missing ancestry stays unknown.
        // Validate relevant tag parts before compacting directed/ancestry facts.
        // PostgreSQL scalar "p" ->> 0 is "p": the type check must reject it too.
        // Canonical timeline roots share an empty (present) root sentinel.
        // Tags are bounded before transfer; oversized/corrupt evidence stays
        // unknown, never falsely top-level/unmentioned/read.
        let rows = sqlx::query(
            r#"WITH roster AS MATERIALIZED (
                SELECT c.id,c.name,c.channel_type::text AS channel_type,
                    c.archived_at IS NOT NULL AS archived, cm.hidden_at IS NOT NULL AS hidden
                FROM channel_members cm JOIN channels c
                    ON c.community_id=cm.community_id AND c.id=cm.channel_id
                WHERE cm.community_id=$1 AND cm.pubkey=$2 AND cm.removed_at IS NULL
                    AND c.deleted_at IS NULL AND ($3::uuid IS NULL OR c.id>$3)
                ORDER BY c.id LIMIT $4
             )
             SELECT r.*, latest.latest_message_id,
                (latest.latest_message_id IS NOT NULL OR latest.candidates <= $5-1) AS latest_message_complete,
                e.scanned,COALESCE(e.evidence,'[]'::jsonb) AS evidence FROM roster r
             LEFT JOIN LATERAL (
                WITH candidates AS MATERIALIZED (
                    SELECT id,created_at,kind,deleted_at FROM events
                    WHERE community_id=$1 AND channel_id=r.id
                    ORDER BY created_at DESC,id LIMIT $5
                )
                SELECT count(*) AS candidates,
                    (array_agg(encode(id,'hex') ORDER BY created_at DESC,id)
                        FILTER (WHERE kind=ANY($6) AND deleted_at IS NULL))[1] AS latest_message_id
                FROM candidates
             ) latest ON true
             LEFT JOIN personal_read_frontiers cf ON cf.community_id=$1 AND cf.actor=$2
                AND cf.channel_id=r.id AND cf.root_id=''::bytea
             LEFT JOIN LATERAL (
                WITH candidates AS MATERIALIZED (
                    SELECT id,pubkey,created_at,received_at,deleted_at,kind,tags
                    FROM events WHERE community_id=$1 AND channel_id=r.id AND received_at >= $7
                    ORDER BY received_at DESC,id,created_at LIMIT $8
                ), classified AS (
                    SELECT e.*, tm.root_event_id AS root,
                        COALESCE(tm.root_event_id<>e.id,false) AS is_reply,
                        COALESCE(extract(epoch FROM e.created_at)::bigint <=
                            CASE WHEN tm.root_event_id IS NOT NULL AND tm.root_event_id<>e.id
                                THEN tf.through_timestamp ELSE cf.through_timestamp END,false) AS covered
                    FROM (SELECT * FROM candidates ORDER BY received_at DESC,id,created_at LIMIT $8-1) e
                    LEFT JOIN thread_metadata tm ON tm.community_id=$1 AND tm.channel_id=r.id
                        AND tm.event_created_at=e.created_at AND tm.event_id=e.id
                    LEFT JOIN personal_read_frontiers tf ON tf.community_id=$1 AND tf.actor=$2
                        AND tf.channel_id=r.id AND tf.root_id=tm.root_event_id AND tm.root_event_id<>e.id
                    WHERE e.kind=ANY($6) AND e.pubkey<>$2 AND e.deleted_at IS NULL
                ), grouped AS (
                    SELECT CASE WHEN root IS NULL THEN NULL
                            WHEN is_reply THEN encode(root,'hex') ELSE '' END AS root,
                        is_reply, covered,
                        -- ->>0 also selects scalar "p"/"e": reject nonarrays first.
                        -- C collation matches Rust's ASCII case/hex rules. Reply
                        -- markers matter only without canonical ancestry; otherwise
                        -- metadata, not tag spelling, owns the context.
                        CASE WHEN octet_length(tags::text)<=8192
                            AND jsonb_typeof(tags)='array' THEN
                            (SELECT CASE WHEN bool_or(jsonb_typeof(tag)<>'array'
                                    OR jsonb_path_exists(tag,'strict $[*] ? (@.type() != "string")'))
                                THEN NULL ELSE jsonb_build_object(
                                    'directed',COALESCE(bool_or(
                                        (tag->>0='p' AND lower((tag->>1) COLLATE "C")=encode($2,'hex'))
                                        OR (tag->>0='broadcast' AND tag->>1='1')),false),
                                    'reply_marked',root IS NULL AND COALESCE(bool_or(tag->>0='e'
                                        AND tag->>3='reply'
                                        AND (tag->>1) COLLATE "C" ~ '^[0123456789abcdefABCDEF]{64}$'),false)) END
                             FROM jsonb_array_elements(tags) t(tag)
                             WHERE tag->>0 IN ('p','broadcast','e')) ELSE NULL END AS facts,
                        count(*) AS n
                    FROM classified
                    WHERE NOT covered OR root IS NULL
                    GROUP BY 1,2,3,4
                )
                SELECT (SELECT count(*) FROM candidates) AS scanned,
                    jsonb_agg(to_jsonb(grouped)) AS evidence FROM grouped
             ) e ON true ORDER BY r.id"#,
        ).bind(community.as_uuid()).bind(actor_bytes.as_slice()).bind(after)
            .bind((limit+1) as i64).bind((MAX_CHANNEL_SCAN+1) as i64)
            .bind(ELIGIBLE_KINDS.as_slice())
            .bind(DateTime::from_timestamp_millis(account.cutoff_ms)
                .ok_or_else(|| DbError::InvalidData("invalid receipt cutoff".into()))?)
            .bind((MAX_RECEIPT_SCAN+1) as i64)
            .fetch_all(&mut *tx).await?;
        let has_more = rows.len() > limit;
        let mut channels = Vec::new();
        let mut pending = Vec::new();
        for row in rows.into_iter().take(limit) {
            let evidence: Value = row.try_get("evidence")?;
            let evidence = evidence
                .as_array()
                .ok_or_else(|| DbError::InvalidData("invalid sidebar evidence".into()))?;
            let complete = row.try_get::<i64, _>("scanned")? <= MAX_RECEIPT_SCAN as i64;
            let channel_type: String = row.try_get("channel_type")?;
            let mut unread = 0;
            let mut attention = 0;
            let mut unread_complete = complete;
            let mut attention_complete = complete;
            let mut replies: HashMap<Vec<u8>, u32> = HashMap::new();
            for e in evidence {
                let n = e["n"]
                    .as_u64()
                    .filter(|n| *n <= MAX_RECEIPT_SCAN as u64)
                    .ok_or_else(|| DbError::InvalidData("invalid evidence multiplicity".into()))?
                    as u32;
                let Some(facts) = e["facts"].as_object() else {
                    unread_complete = false;
                    attention_complete = false;
                    continue;
                };
                // SQL's bounded ancestry fact is parity-tested against the shared
                // NIP-10 parser; no raw tag payload crosses the DB boundary.
                if facts.get("reply_marked") == Some(&Value::Bool(true)) && e["root"].is_null() {
                    unread_complete = false;
                    attention_complete = false;
                    continue;
                }
                // Roots are timeline messages; descendants belong exclusively
                // to their canonical thread. Never inherit the channel prefix.
                let is_reply = e["is_reply"] == true;
                if e["covered"] == true {
                    continue;
                }
                unread += n;
                let directed =
                    channel_type == "dm" || facts.get("directed") == Some(&Value::Bool(true));
                if directed {
                    attention += n;
                } else if is_reply {
                    if let Some(root) = e["root"].as_str().and_then(writes::event_id) {
                        *replies.entry(root).or_default() += n;
                    } else {
                        attention_complete = false;
                    }
                }
            }
            pending.push((replies, attention, attention_complete));
            channels.push(ChannelReadSummary {
                channel_id: row.try_get("id")?,
                name: row.try_get("name")?,
                channel_type,
                archived: row.try_get("archived")?,
                hidden: row.try_get("hidden")?,
                unread: ReadCount::from_evidence(unread, unread_complete),
                attention: ReadCount::from_evidence(attention, attention_complete),
                latest_message_id: row.try_get("latest_message_id")?,
                latest_message_complete: row.try_get("latest_message_complete")?,
            });
        }
        let targets: Vec<_> = channels
            .iter()
            .zip(&pending)
            .flat_map(|(channel, (replies, _, _))| {
                replies
                    .keys()
                    .map(|root| (channel.channel_id, root.clone()))
            })
            .collect();
        let participation =
            participation::resolve(&mut tx, community, &actor_bytes, &targets).await?;
        for (channel, (replies, mut attention, mut complete)) in channels.iter_mut().zip(pending) {
            for (root, count) in replies {
                match participation
                    .get(&(channel.channel_id, root))
                    .copied()
                    .flatten()
                {
                    Some(true) => attention += count,
                    Some(false) => {}
                    None => complete = false,
                }
            }
            channel.attention = ReadCount::from_evidence(attention, complete);
        }
        let next_cursor = if has_more {
            channels.last().map(|c| c.channel_id)
        } else {
            None
        };
        tx.commit().await?;
        Ok(SidebarPage {
            account,
            channels,
            next_cursor,
        })
    }
}

/// Read-time horizon only: frontier state is not discarded on expiry.
pub(super) async fn read_account(
    conn: &mut PgConnection,
    community: CommunityId,
    actor: &[u8],
    retention_seconds: u32,
) -> Result<ReadAccount> {
    let row = sqlx::query(
        "SELECT date_trunc('milliseconds',transaction_timestamp()-make_interval(secs=>$3::double precision)) AS cutoff,
            a.imported_at FROM (SELECT 1) singleton LEFT JOIN personal_read_accounts a
            ON a.community_id=$1 AND a.actor=$2"
    ).bind(community.as_uuid()).bind(actor).bind(f64::from(retention_seconds)).fetch_one(conn).await?;
    let cutoff: DateTime<Utc> = row.try_get("cutoff")?;
    let imported: Option<DateTime<Utc>> = row.try_get("imported_at")?;
    Ok(ReadAccount {
        retention_seconds,
        cutoff_ms: cutoff.timestamp_millis(),
        imported_at_ms: imported.map(|t| t.timestamp_millis()),
    })
}
