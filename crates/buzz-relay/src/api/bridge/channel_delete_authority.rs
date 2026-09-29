//! Narrow, authenticated ownership probe. Never an authorization token or a
//! persisted event; unsupported relays' ordinary query arrays cannot satisfy it.

use std::{sync::Arc, time::Duration};

use axum::{http::StatusCode, Json};
use buzz_core::TenantContext;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{api::api_error, handlers::side_effects::has_delete_authority, state::AppState};

pub(super) fn requested(filters: &[Value]) -> bool {
    filters
        .iter()
        .any(|f| f.get("channel_delete_authority").is_some())
}

fn channel(filters: &[Value], viewer: &str) -> Option<Uuid> {
    let id = filters.first()?.get("#h")?.get(0)?.as_str()?;
    let id = Uuid::parse_str(id).ok()?;
    let expected = json!({
        "kinds": [buzz_core::kind::KIND_NIP29_DELETE_GROUP], "#h": [id.to_string()], "#p": [viewer],
        "channel_delete_authority": 1,
    });
    (filters == [expected]).then_some(id)
}

pub(super) async fn query(
    state: &Arc<AppState>,
    tenant: &TenantContext,
    viewer: &nostr::PublicKey,
    filters: &[Value],
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let id = channel(filters, &viewer.to_hex()).ok_or_else(|| {
        api_error(
            StatusCode::BAD_REQUEST,
            "invalid channel_delete_authority filter",
        )
    })?;
    let unavailable = || {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Delete authority unavailable",
        )
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        // Both the active roster and persisted ownership mapping use the writer,
        // exactly as the Delete validator does, never profile tags or read replicas.
        let members = state
            .db
            .get_members(tenant.community(), id)
            .await
            .map_err(|_| unavailable())?;
        if !members.iter().any(|m| m.pubkey == viewer.to_bytes()) {
            // No existence/owner information for a nonmember, including other tenants.
            return Err(api_error(
                StatusCode::FORBIDDEN,
                "channel access unavailable",
            ));
        }
        let channel = state
            .db
            .get_channel_for_event_write(tenant.community(), id)
            .await
            .map_err(|_| unavailable())?;
        let can_delete = channel.archived_at.is_none()
            && has_delete_authority(state, tenant.community(), &members, &viewer.to_bytes())
                .await
                .map_err(|_| unavailable())?;
        Ok(Json(json!({
            "channel_delete_authority": 1,
            "community_id": tenant.community().as_uuid(),
            "pubkey": viewer.to_hex(),
            "channel_id": id,
            "can_delete": can_delete,
        })))
    })
    .await
    .map_err(|_| unavailable())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_single_channel_and_authenticated_viewer_only() {
        let viewer = "a".repeat(64);
        let id = Uuid::new_v4();
        let filter =
            json!({"kinds":[9008], "#h":[id], "#p":[viewer], "channel_delete_authority":1});
        assert_eq!(channel(std::slice::from_ref(&filter), &viewer), Some(id));
        assert_eq!(channel(&[], &viewer), None);
        assert_eq!(channel(&[filter.clone(), filter.clone()], &viewer), None);
        for (key, value) in [
            ("#p", json!(["b".repeat(64)])),
            ("#h", json!([id, id])),
            ("#h", json!(["not-a-channel"])),
            ("kinds", json!([9008, 9])),
            ("channel_delete_authority", json!(2)),
            ("limit", json!(1)),
        ] {
            let mut invalid = filter.clone();
            invalid[key] = value;
            assert!(requested(&[invalid.clone()]));
            assert_eq!(channel(&[invalid], &viewer), None, "{key}");
        }
    }
}

#[cfg(test)]
#[path = "channel_delete_authority_tests.rs"]
mod postgres_tests;
