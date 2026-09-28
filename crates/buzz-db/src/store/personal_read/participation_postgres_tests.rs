use super::*;
use crate::{
    channel::{ChannelType, ChannelVisibility},
    Db,
};
use buzz_core::CommunityId;
use nostr::{EventBuilder, Keys, Kind, Timestamp};
use sqlx::PgPool;
use uuid::Uuid;

async fn event(
    db: &Db,
    community: CommunityId,
    channel: Uuid,
    author: &Keys,
    root: Option<&nostr::Event>,
    kind: u16,
    time: u64,
) -> nostr::Event {
    let event = EventBuilder::new(Kind::Custom(kind), Uuid::new_v4().to_string())
        .custom_created_at(Timestamp::from(time))
        .sign_with_keys(author)
        .unwrap();
    db.insert_event(community, &event, Some(channel))
        .await
        .unwrap();
    if let Some(root) = root {
        sqlx::query("INSERT INTO thread_metadata (community_id,event_id,event_created_at,channel_id,root_event_id,parent_event_id,depth)
            VALUES ($1,$2,to_timestamp($3),$4,$5,$5,1)")
            .bind(community.as_uuid()).bind(event.id.as_bytes().as_slice()).bind(time as f64)
            .bind(channel).bind(root.id.as_bytes().as_slice()).execute(&db.pool).await.unwrap();
    }
    event
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn personal_read_attention_proves_participation_without_retention_or_frontier_inheritance() {
    let pool = PgPool::connect(&crate::test_support::database_url())
        .await
        .unwrap();
    let db = Db::from_pool(pool.clone());
    let community = db
        .ensure_configured_community(&format!("attention-{}.local", Uuid::new_v4()))
        .await
        .unwrap()
        .id;
    let actor = Keys::generate();
    let other = Keys::generate();
    let now = Timestamp::now().as_secs();
    // Root author, old participant, absent, deleted participant, auxiliary author,
    // overflowing thread without positive evidence, and overflowing own-root thread.
    for case in 0..8 {
        let channel = db
            .create_channel(
                community,
                &format!("case-{case}"),
                ChannelType::Stream,
                ChannelVisibility::Open,
                None,
                &actor.public_key().to_bytes(),
                None,
            )
            .await
            .unwrap()
            .id;
        let root_author = if case == 0 || case == 6 {
            &actor
        } else {
            &other
        };
        let root = event(
            &db,
            community,
            channel,
            root_author,
            None,
            9,
            now - 40 * 86400,
        )
        .await;
        if matches!(case, 1 | 3 | 4 | 7) {
            let own = event(
                &db,
                community,
                channel,
                &actor,
                Some(&root),
                if case == 4 { 7 } else { 9 },
                now - 39 * 86400,
            )
            .await;
            if case == 3 {
                sqlx::query("UPDATE events SET deleted_at=now() WHERE community_id=$1 AND id=$2")
                    .bind(community.as_uuid())
                    .bind(own.id.as_bytes().as_slice())
                    .execute(&pool)
                    .await
                    .unwrap();
            }
        }
        // Root and own reply receipts are old. Participation must not expire with unread.
        sqlx::query("UPDATE events SET received_at=now()-interval '35 days' WHERE community_id=$1 AND channel_id=$2")
            .bind(community.as_uuid()).bind(channel).execute(&pool).await.unwrap();
        let count = if case >= 5 { 258 } else { 1 };
        let mut last = root.clone();
        for i in 0..count {
            last = event(&db, community, channel, &other, Some(&root), 9, now + i).await;
        }
        let sidebar = db
            .personal_read_sidebar(
                community,
                &actor.public_key(),
                DEFAULT_RETENTION_SECONDS,
                20,
                None,
            )
            .await
            .unwrap();
        let summary = sidebar
            .channels
            .iter()
            .find(|s| s.channel_id == channel)
            .unwrap();
        assert!(matches!(summary.unread, ReadCount::Exact { value } if value == count as u32));
        let expected = match case {
            0 | 1 | 6 => Some(true),
            5 | 7 => None,
            _ => Some(false),
        };
        match expected {
            Some(true) => assert!(
                matches!(summary.attention, ReadCount::Exact { value } if value == count as u32),
                "case {case}: {:?}",
                summary.attention
            ),
            Some(false) => assert!(
                matches!(summary.attention, ReadCount::Exact { value: 0 }),
                "case {case}: {:?}",
                summary.attention
            ),
            None => assert!(
                matches!(summary.attention, ReadCount::Unknown),
                "case {case}: {:?}",
                summary.attention
            ),
        }
        let context = db
            .personal_read_contexts(
                community,
                &actor.public_key(),
                DEFAULT_RETENTION_SECONDS,
                &[ContextQuery {
                    target: ReadTarget {
                        channel_id: channel,
                        root_id: Some(root.id.to_hex()),
                    },
                    message_ids: vec![last.id.to_hex()],
                }],
            )
            .await
            .unwrap();
        let wire = serde_json::to_value(context).unwrap();
        assert_eq!(
            wire["contexts"][0]["messages"][0]["attention"],
            serde_json::json!(expected),
            "case {case}"
        );
    }
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn personal_read_attention_root_budget_does_not_fabricate_absence() {
    let pool = PgPool::connect(&crate::test_support::database_url())
        .await
        .unwrap();
    let db = Db::from_pool(pool.clone());
    let community = db
        .ensure_configured_community(&format!("root-budget-{}.local", Uuid::new_v4()))
        .await
        .unwrap()
        .id;
    let actor = Keys::generate();
    let other = Keys::generate();
    let channel = db
        .create_channel(
            community,
            "many roots",
            ChannelType::Stream,
            ChannelVisibility::Open,
            None,
            &actor.public_key().to_bytes(),
            None,
        )
        .await
        .unwrap()
        .id;
    let now = Timestamp::now().as_secs();
    for i in 0..1025 {
        let root = event(&db, community, channel, &other, None, 9, now).await;
        event(&db, community, channel, &other, Some(&root), 9, now + 1).await;
        if i == 1023 {
            let page = db
                .personal_read_sidebar(
                    community,
                    &actor.public_key(),
                    DEFAULT_RETENTION_SECONDS,
                    20,
                    None,
                )
                .await
                .unwrap();
            assert!(matches!(
                page.channels[0].attention,
                ReadCount::Exact { value: 0 }
            ));
        }
    }
    let page = db
        .personal_read_sidebar(
            community,
            &actor.public_key(),
            DEFAULT_RETENTION_SECONDS,
            20,
            None,
        )
        .await
        .unwrap();
    assert!(matches!(
        page.channels[0].unread,
        ReadCount::Exact { value: 2050 }
    ));
    assert!(matches!(page.channels[0].attention, ReadCount::Unknown));
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn personal_read_attention_timeout_preserves_sidebar_and_context_authority() {
    let pool = PgPool::connect(&crate::test_support::database_url())
        .await
        .unwrap();
    let db = Db::from_pool(pool.clone());
    let community = db
        .ensure_configured_community(&format!("attention-timeout-{}.local", Uuid::new_v4()))
        .await
        .unwrap()
        .id;
    let actor = Keys::generate();
    let other = Keys::generate();
    let channel = db
        .create_channel(
            community,
            "timeout",
            ChannelType::Stream,
            ChannelVisibility::Open,
            None,
            &actor.public_key().to_bytes(),
            None,
        )
        .await
        .unwrap()
        .id;
    let now = Timestamp::now().as_secs();
    let root = event(&db, community, channel, &other, None, 9, now).await;
    let reply = event(&db, community, channel, &other, Some(&root), 9, now + 1).await;
    let mut held = pool.begin().await.unwrap();
    // Force the real resolver to time out, then check that its outer snapshot
    // and original statement budget remain usable.
    sqlx::query("LOCK TABLE thread_metadata IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *held)
        .await
        .unwrap();
    let mut reader = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL statement_timeout='2000ms'")
        .execute(&mut *reader)
        .await
        .unwrap();
    for lock_timeout in ["0", "10ms"] {
        sqlx::query("SELECT set_config('lock_timeout',$1,true)")
            .bind(lock_timeout)
            .execute(&mut *reader)
            .await
            .unwrap();
        sqlx::query("SET LOCAL jit=on")
            .execute(&mut *reader)
            .await
            .unwrap();
        let result = participation::resolve(
            &mut reader,
            community,
            &actor.public_key().to_bytes(),
            &[(channel, root.id.as_bytes().to_vec())],
        )
        .await
        .unwrap();
        assert!(result.is_empty(), "timeout provides no negative evidence");
        let setting: String = sqlx::query_scalar("SHOW statement_timeout")
            .fetch_one(&mut *reader)
            .await
            .unwrap();
        assert_eq!(setting, "2s");
        let jit: String = sqlx::query_scalar("SHOW jit")
            .fetch_one(&mut *reader)
            .await
            .unwrap();
        assert_eq!(jit, "on", "optional inference restores caller settings");
    }
    reader.rollback().await.unwrap();
    held.rollback().await.unwrap();
    let contexts = db
        .personal_read_contexts(
            community,
            &actor.public_key(),
            DEFAULT_RETENTION_SECONDS,
            &[ContextQuery {
                target: ReadTarget {
                    channel_id: channel,
                    root_id: Some(root.id.to_hex()),
                },
                message_ids: vec![reply.id.to_hex()],
            }],
        )
        .await
        .unwrap();
    let wire = serde_json::to_value(contexts).unwrap();
    assert_eq!(wire["contexts"][0]["messages"][0]["attention"], false);
}
