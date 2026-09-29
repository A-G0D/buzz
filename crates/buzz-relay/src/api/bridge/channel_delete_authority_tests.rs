//! Exercises the actual router and command validator with disposable tenant data.
use super::*;
use axum::{
    body::{to_bytes, Body},
    http::Request,
};
use base64::Engine;
use buzz_db::channel::{ChannelType, ChannelVisibility, MemberRole};
use nostr::{EventBuilder, Keys, Kind, Tag, Timestamp};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

fn filter(key: &Keys, id: Uuid) -> Value {
    json!([{"kinds":[9008], "#h":[id], "#p":[key.public_key().to_hex()], "channel_delete_authority":1}])
}
fn proof(key: &Keys, host: &str, body: &[u8]) -> String {
    let event = EventBuilder::new(Kind::Custom(27235), "")
        .tags([
            Tag::parse(["u", &format!("https://{host}/query")]).unwrap(),
            Tag::parse(["method", "POST"]).unwrap(),
            Tag::parse(["payload", &hex::encode(Sha256::digest(body))]).unwrap(),
            Tag::parse(["nonce", &Uuid::new_v4().to_string()]).unwrap(),
        ])
        .sign_with_keys(key)
        .unwrap();
    format!(
        "Nostr {}",
        base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&event).unwrap())
    )
}
async fn post(
    state: &Arc<AppState>,
    host: &str,
    auth: Option<&str>,
    body: Vec<u8>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri("/query")
        .header("host", host);
    if let Some(auth) = auth {
        request = request.header("authorization", auth);
    }
    let response = crate::router::build_router(state.clone())
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
async fn query_as(
    state: &Arc<AppState>,
    host: &str,
    key: &Keys,
    filter: Value,
) -> (StatusCode, Value) {
    let body = serde_json::to_vec(&filter).unwrap();
    post(state, host, Some(&proof(key, host, &body)), body).await
}
async fn setup() -> (Arc<AppState>, TenantContext, Keys, Uuid) {
    let fixture = super::super::postgres_tests::bridge_handler_test_state()
        .await
        .expect("Postgres and Redis required");
    let mut state = (*fixture).clone();
    let config = Arc::make_mut(&mut state.config);
    config.require_auth_token = true;
    config.require_relay_membership = true;
    state.nip98_replay = Arc::new(buzz_pubsub::RedisNip98ReplayGuard::new(
        state.redis_pool.clone(),
    ));
    let state = Arc::new(state);
    let host = format!("delete-authority-{}.local", Uuid::new_v4().simple());
    let community = state
        .db
        .ensure_configured_community(&host)
        .await
        .unwrap()
        .id;
    let owner = Keys::generate();
    let id = state
        .db
        .create_channel(
            community,
            "authority-fixture",
            ChannelType::Stream,
            ChannelVisibility::Private,
            None,
            &owner.public_key().to_bytes(),
            None,
        )
        .await
        .unwrap()
        .id;
    admit(&state, community, &owner).await;
    (state, TenantContext::resolved(community, host), owner, id)
}
async fn admit(state: &AppState, community: buzz_core::CommunityId, key: &Keys) {
    state
        .db
        .add_relay_member(community, &key.public_key().to_hex(), "member", None)
        .await
        .unwrap();
}
async fn agrees(
    state: &Arc<AppState>,
    tenant: &TenantContext,
    key: &Keys,
    id: Uuid,
    allowed: bool,
) {
    let (status, body) = query_as(state, tenant.host(), key, filter(key, id)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({"channel_delete_authority":1, "community_id":tenant.community().as_uuid(), "pubkey":key.public_key().to_hex(), "channel_id":id, "can_delete":allowed})
    );
    // Validate but never publish/execute the destructive command.
    let command = EventBuilder::new(Kind::Custom(9008), "")
        .tags([Tag::parse(["h", &id.to_string()]).unwrap()])
        .sign_with_keys(key)
        .unwrap();
    assert_eq!(
        crate::handlers::side_effects::validate_admin_event(tenant, 9008, &command, state)
            .await
            .is_ok(),
        allowed
    );
}

#[tokio::test]
#[ignore = "requires Postgres and Redis"]
async fn persisted_authority_matches_delete_not_replaced_profile_claims() {
    let (state, tenant, owner, id) = setup().await;
    let human = Keys::generate();
    let other = Keys::generate();
    let agent = Keys::generate();
    state
        .db
        .ensure_user(tenant.community(), &agent.public_key().to_bytes())
        .await
        .unwrap();
    for key in [&human, &other] {
        state
            .db
            .ensure_user(tenant.community(), &key.public_key().to_bytes())
            .await
            .unwrap();
        admit(&state, tenant.community(), key).await;
        state
            .db
            .add_member(
                tenant.community(),
                id,
                &key.public_key().to_bytes(),
                MemberRole::Admin,
                Some(&owner.public_key().to_bytes()),
            )
            .await
            .unwrap();
    }
    // A coowner, not the creator, is enough; an ordinary admin is not.
    state
        .db
        .add_member(
            tenant.community(),
            id,
            &agent.public_key().to_bytes(),
            MemberRole::Owner,
            Some(&owner.public_key().to_bytes()),
        )
        .await
        .unwrap();
    assert!(state
        .db
        .set_agent_owner(
            tenant.community(),
            &agent.public_key().to_bytes(),
            &human.public_key().to_bytes()
        )
        .await
        .unwrap());
    assert!(!state
        .db
        .set_agent_owner(
            tenant.community(),
            &agent.public_key().to_bytes(),
            &other.public_key().to_bytes()
        )
        .await
        .unwrap());
    agrees(&state, &tenant, &owner, id, true).await;
    agrees(&state, &tenant, &human, id, true).await;
    agrees(&state, &tenant, &other, id, false).await;
    // A valid current NIP-OA claim for a different human cannot rewrite the
    // first-write-wins mapping, nor does later removing the tag revoke it.
    let digest: [u8; 32] =
        Sha256::digest(format!("nostr:agent-auth:{}:", agent.public_key().to_hex())).into();
    let signature = other.sign_schnorr(&nostr::secp256k1::Message::from_digest(digest));
    let tag = Tag::parse([
        "auth",
        &other.public_key().to_hex(),
        "",
        &signature.to_string(),
    ])
    .unwrap();
    for (index, tags) in [vec![tag], vec![]].into_iter().enumerate() {
        let profile = EventBuilder::new(Kind::Metadata, "{}")
            .tags(tags)
            .custom_created_at(Timestamp::from(1_790_000_000 + index as u64))
            .sign_with_keys(&agent)
            .unwrap();
        state
            .db
            .replace_addressable_event(tenant.community(), &profile, None)
            .await
            .unwrap();
        agrees(&state, &tenant, &human, id, true).await;
        agrees(&state, &tenant, &other, id, false).await;
    }
    // Identical channel/agent/viewer keys in another tenant use that tenant's
    // persisted mapping, never the mapping above.
    let other_host = format!("mapping-{}.local", Uuid::new_v4().simple());
    let other_community = state
        .db
        .ensure_configured_community(&other_host)
        .await
        .unwrap()
        .id;
    state
        .db
        .create_channel_with_id(
            other_community,
            id,
            "other-mapping",
            ChannelType::Stream,
            ChannelVisibility::Private,
            None,
            &agent.public_key().to_bytes(),
            None,
        )
        .await
        .unwrap();
    for key in [&agent, &human, &other] {
        state
            .db
            .ensure_user(other_community, &key.public_key().to_bytes())
            .await
            .unwrap();
        admit(&state, other_community, key).await;
    }
    for key in [&human, &other] {
        state
            .db
            .add_member(
                other_community,
                id,
                &key.public_key().to_bytes(),
                MemberRole::Member,
                Some(&agent.public_key().to_bytes()),
            )
            .await
            .unwrap();
    }
    state
        .db
        .set_agent_owner(
            other_community,
            &agent.public_key().to_bytes(),
            &other.public_key().to_bytes(),
        )
        .await
        .unwrap();
    let other_tenant = TenantContext::resolved(other_community, other_host);
    agrees(&state, &other_tenant, &human, id, false).await;
    agrees(&state, &other_tenant, &other, id, true).await;
    // The existing mutation permits a nonmember owning human; the read remains
    // deliberately member-only. Extracting the predicate must not change that.
    state
        .db
        .remove_member(
            other_community,
            id,
            &other.public_key().to_bytes(),
            &agent.public_key().to_bytes(),
        )
        .await
        .unwrap();
    assert_eq!(
        query_as(&state, other_tenant.host(), &other, filter(&other, id))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let command = EventBuilder::new(Kind::Custom(9008), "")
        .tags([Tag::parse(["h", &id.to_string()]).unwrap()])
        .sign_with_keys(&other)
        .unwrap();
    crate::handlers::side_effects::validate_admin_event(&other_tenant, 9008, &command, &state)
        .await
        .unwrap();
    // Membership-role changes, unlike profile replacement, do change authority.
    state
        .db
        .add_member(
            tenant.community(),
            id,
            &agent.public_key().to_bytes(),
            MemberRole::Member,
            Some(&owner.public_key().to_bytes()),
        )
        .await
        .unwrap();
    agrees(&state, &tenant, &human, id, false).await;
    state
        .db
        .add_member(
            tenant.community(),
            id,
            &agent.public_key().to_bytes(),
            MemberRole::Owner,
            Some(&owner.public_key().to_bytes()),
        )
        .await
        .unwrap();
    agrees(&state, &tenant, &human, id, true).await;
    state
        .db
        .remove_member(
            tenant.community(),
            id,
            &agent.public_key().to_bytes(),
            &owner.public_key().to_bytes(),
        )
        .await
        .unwrap();
    agrees(&state, &tenant, &human, id, false).await;
    state
        .db
        .add_member(
            tenant.community(),
            id,
            &agent.public_key().to_bytes(),
            MemberRole::Owner,
            Some(&owner.public_key().to_bytes()),
        )
        .await
        .unwrap();
    state
        .db
        .archive_channel(tenant.community(), id)
        .await
        .unwrap();
    agrees(&state, &tenant, &human, id, false).await;
    agrees(&state, &tenant, &owner, id, false).await;
}

#[tokio::test]
#[ignore = "requires Postgres and Redis"]
async fn router_auth_replay_scope_discovery_and_strict_filters() {
    let (state, tenant, owner, id) = setup().await;
    let body = serde_json::to_vec(&filter(&owner, id)).unwrap();
    assert_eq!(
        post(&state, tenant.host(), None, body.clone()).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post(
            &state,
            tenant.host(),
            Some(&proof(&owner, "wrong.invalid", &body)),
            body.clone()
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post(
            &state,
            tenant.host(),
            Some(&proof(&owner, tenant.host(), b"[]")),
            body.clone()
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let auth = proof(&owner, tenant.host(), &body);
    assert_eq!(
        post(&state, tenant.host(), Some(&auth), body.clone())
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        post(&state, tenant.host(), Some(&auth), body).await.0,
        StatusCode::UNAUTHORIZED
    );
    let stranger = Keys::generate();
    let denied = query_as(&state, tenant.host(), &stranger, filter(&stranger, id)).await;
    assert_eq!(denied.0, StatusCode::FORBIDDEN);
    assert_eq!(denied.1["error"], "relay_membership_required");
    admit(&state, tenant.community(), &stranger).await;
    let denied = query_as(&state, tenant.host(), &stranger, filter(&stranger, id)).await;
    assert_eq!(denied.0, StatusCode::FORBIDDEN);
    assert!(denied.1.get("can_delete").is_none());
    assert_eq!(
        query_as(&state, tenant.host(), &stranger, filter(&owner, id))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        query_as(
            &state,
            tenant.host(),
            &owner,
            filter(&owner, Uuid::new_v4())
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    for (name, value) in [
        ("limit", json!(1)),
        ("channel_delete_authority", json!(2)),
        ("channel_delete_authority", Value::Null),
        ("#h", json!([id, id])),
    ] {
        let mut bad = filter(&owner, id);
        bad[0][name] = value;
        assert_eq!(
            query_as(&state, tenant.host(), &owner, bad).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    let mut mixed = filter(&owner, id);
    mixed.as_array_mut().unwrap().push(json!({"kinds":[0]}));
    assert_eq!(
        query_as(&state, tenant.host(), &owner, mixed).await.0,
        StatusCode::BAD_REQUEST
    );
    let info = crate::nip11::nip11_document(&state, tenant.host()).await;
    assert_eq!(
        info.channel_delete_authority,
        Some(json!({"version":1,"community_id":tenant.community().as_uuid()}))
    );
    assert!(crate::nip11::nip11_document(&state, "unmapped.invalid")
        .await
        .channel_delete_authority
        .is_none());
    assert_eq!(
        query_as(&state, "unmapped.invalid", &owner, filter(&owner, id))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let second_host = format!("other-{}.local", Uuid::new_v4().simple());
    let second = state
        .db
        .ensure_configured_community(&second_host)
        .await
        .unwrap()
        .id;
    admit(&state, second, &owner).await;
    assert_eq!(
        query_as(&state, &second_host, &owner, filter(&owner, id))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    // Same channel UUID and identities in another tenant: no ownership inheritance.
    state
        .db
        .create_channel_with_id(
            second,
            id,
            "other",
            ChannelType::Stream,
            ChannelVisibility::Private,
            None,
            &stranger.public_key().to_bytes(),
            None,
        )
        .await
        .unwrap();
    state
        .db
        .add_member(
            second,
            id,
            &owner.public_key().to_bytes(),
            MemberRole::Member,
            Some(&stranger.public_key().to_bytes()),
        )
        .await
        .unwrap();
    agrees(
        &state,
        &TenantContext::resolved(second, second_host),
        &owner,
        id,
        false,
    )
    .await;
}

#[tokio::test]
#[ignore = "requires Postgres and Redis"]
async fn database_failure_is_unavailable_not_an_authoritative_denial() {
    let (state, tenant, owner, id) = setup().await;
    let pool = sqlx::PgPool::connect(&crate::test_support::database_url())
        .await
        .unwrap();
    pool.close().await;
    let mut failed = (*state).clone();
    failed.db = buzz_db::Db::from_pool(pool);
    let filters = filter(&owner, id);
    let (status, body) = query(
        &Arc::new(failed),
        &tenant,
        &owner.public_key(),
        filters.as_array().unwrap(),
    )
    .await
    .unwrap_err();
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body.get("can_delete").is_none());
    agrees(&state, &tenant, &owner, id, true).await;
}
