use super::{classification, postgres_tests::fixture, *};
use serde_json::json;

#[tokio::test]
#[ignore = "requires Postgres"]
async fn sidebar_sql_eligibility_matches_selector_classifier() {
    let (db, pool, community, _, actor, event) = fixture().await;
    let now = chrono::Utc::now().timestamp_millis();
    for kind in [9, 40002, 45001, 45003, 1, 7, 39002] {
        for own in [false, true] {
            for deleted in [false, true] {
                for expired in [false, true] {
                    let received = now - if expired { 31 * 86_400_000 } else { 0 };
                    sqlx::query("UPDATE events SET kind=$2,pubkey=$3,deleted_at=CASE WHEN $4 THEN now() ELSE NULL END,received_at=to_timestamp($5::double precision/1000) WHERE community_id=$1")
                        .bind(community.as_uuid()).bind(kind)
                        .bind(if own { actor.public_key().to_bytes() } else { event.pubkey.to_bytes() }.as_slice())
                        .bind(deleted).bind(received as f64).execute(&pool).await.unwrap();
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
                    let expected = u32::from(classification::eligible(
                        kind,
                        own,
                        deleted,
                        received,
                        page.account.cutoff_ms,
                    ));
                    assert!(
                        matches!(page.channels[0].unread, ReadCount::Exact { value } if value == expected),
                        "kind={kind} own={own} deleted={deleted} expired={expired}"
                    );
                }
            }
        }
    }
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn sidebar_compacted_tags_preserve_directed_and_corruption_rules() {
    let (db, pool, community, _, actor, _) = fixture().await;
    let actor_hex = actor.public_key().to_hex().to_uppercase();
    for (tags, unread, attention) in [
        (json!([]), Some(1), Some(0)),
        (json!(["p"]), None, None),
        (json!(["e"]), None, None),
        (json!(["broadcast"]), None, None),
        (
            json!([["p", actor_hex, "relay", "petname"]]),
            Some(1),
            Some(1),
        ),
        (json!([["p", "00".repeat(32)]]), Some(1), Some(0)),
        (json!([["broadcast", "1", "extra"]]), Some(1), Some(1)),
        (json!([["broadcast", "0"]]), Some(1), Some(0)),
        (json!([["p", "00".repeat(32), 42]]), None, None),
        (json!([["broadcast", "0", 42]]), None, None),
        (json!([["e", "00".repeat(32), "", "root", 42]]), None, None),
        (json!([["p", actor_hex], ["p", "other", 42]]), None, None),
        (json!([["e", "00".repeat(32), "", "reply"]]), None, None),
        (json!([["x", 42]]), Some(1), Some(0)),
        (json!({"p":actor_hex}), None, None),
        (json!([["p", "x".repeat(8193)]]), None, None),
    ] {
        sqlx::query("UPDATE events SET tags=$2 WHERE community_id=$1")
            .bind(community.as_uuid())
            .bind(&tags)
            .execute(&pool)
            .await
            .unwrap();
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
        for (actual, expected) in [
            (&page.channels[0].unread, unread),
            (&page.channels[0].attention, attention),
        ] {
            assert!(
                match (actual, expected) {
                    (ReadCount::Exact { value }, Some(n)) => *value == n,
                    (ReadCount::Unknown, None) => true,
                    _ => false,
                },
                "tags={tags} actual={actual:?} expected={expected:?}"
            );
        }
    }
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn sidebar_ancestry_fact_matches_shared_nip10_parser() {
    let (db, pool, community, _, actor, _) = fixture().await;
    let ids = [
        "a".repeat(64),
        "A".repeat(64),
        "aB09".repeat(16),
        "a".repeat(63),
        "a".repeat(65),
        "g".repeat(64),
        "١".repeat(64),
        "Ａ".repeat(64),
        format!("{}\n", "a".repeat(64)),
    ];
    let mut cases: Vec<Vec<Vec<String>>> = Vec::new();
    for id in ids {
        for marker in ["reply", "Reply", "root", "mention"] {
            cases.push(vec![vec!["e".into(), id.clone(), "".into(), marker.into()]]);
        }
    }
    cases.push(vec![vec!["e".into(), "a".repeat(64), "reply".into()]]);
    cases.push(vec![
        vec!["e".into(), "g".repeat(64), "".into(), "reply".into()],
        vec!["e".into(), "a".repeat(64), "".into(), "reply".into()],
    ]);
    cases.push(vec![
        vec!["e".into(), "a".repeat(64), "".into(), "reply".into()],
        vec!["e".into(), "g".repeat(64), "".into(), "reply".into()],
    ]);
    for tags in cases {
        let reply =
            buzz_core::nip10::parse_thread_markers_from_parts(tags.iter().map(Vec::as_slice))
                .resolve()
                .is_some();
        sqlx::query("UPDATE events SET tags=$2 WHERE community_id=$1")
            .bind(community.as_uuid())
            .bind(json!(tags))
            .execute(&pool)
            .await
            .unwrap();
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
        assert!(
            if reply {
                matches!(page.channels[0].unread, ReadCount::Unknown)
            } else {
                matches!(page.channels[0].unread, ReadCount::Exact { value: 1 })
            },
            "tags={tags:?}"
        );
    }
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn sidebar_directed_fact_matches_selector_classifier() {
    let (db, pool, community, channel, actor, _) = fixture().await;
    let actor_hex = actor.public_key().to_hex();
    let fullwidth: String = actor_hex
        .chars()
        .map(|c| if c.is_ascii_alphabetic() { 'Ａ' } else { c })
        .collect();
    assert_ne!(fullwidth, actor_hex);
    let cases: Vec<Vec<Vec<String>>> = serde_json::from_value(json!([
        [],
        [["p", actor_hex]],
        [["p", actor_hex.to_uppercase()]],
        [["p", fullwidth]],
        [["p"]],
        [["broadcast", "1"]],
        [["broadcast", "true"]],
        [["p", "00".repeat(32)]],
        [["broadcast", "0"], ["p", actor_hex.to_uppercase()]]
    ]))
    .unwrap();
    for channel_type in ["stream", "dm"] {
        sqlx::query(
            "UPDATE channels SET channel_type=$3::channel_type WHERE community_id=$1 AND id=$2",
        )
        .bind(community.as_uuid())
        .bind(channel)
        .bind(channel_type)
        .execute(&pool)
        .await
        .unwrap();
        for tags in &cases {
            let expected = u32::from(classification::directed(channel_type, &actor_hex, tags));
            sqlx::query("UPDATE events SET tags=$2 WHERE community_id=$1")
                .bind(community.as_uuid())
                .bind(json!(tags))
                .execute(&pool)
                .await
                .unwrap();
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
                ReadCount::Exact { value: 1 }
            ));
            assert!(
                matches!(page.channels[0].attention, ReadCount::Exact { value } if value == expected),
                "channel_type={channel_type} tags={tags:?} expected={expected}"
            );
        }
    }
}
