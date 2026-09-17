use std::time::Duration;

use serde::Serialize;
use tauri::State;
use url::Url;

use crate::{
    app_state::AppState,
    relay::{classify_request_error, parse_json_response, relay_error_message},
};

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", tag = "status")]
pub enum EnterpriseLoginGateStatus {
    NotRequired,
    Required,
}

#[tauri::command]
pub async fn enterprise_login_gate(
    relay_url: String,
    state: State<'_, AppState>,
) -> Result<EnterpriseLoginGateStatus, String> {
    let http_url = crate::relay::relay_http_base_url(&relay_url);
    let url = format!("{}/info", http_url.trim_end_matches('/'));
    let response = state
        .http_client
        .get(url)
        .header("Accept", "application/nostr+json")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|error| classify_request_error(&error))?;

    if !response.status().is_success() {
        return Err(relay_error_message(response).await);
    }

    let document = parse_json_response::<serde_json::Value>(response).await?;
    evaluate_enterprise_login_gate(
        &relay_url,
        &document,
        option_env!("BUZZ_DESKTOP_BUILD_ENTERPRISE_AUTH_RELAYS"),
    )
}

#[derive(Debug, PartialEq, Eq)]
enum FederatedIdentityAdvertisement {
    NotAdvertised,
    Required,
    Malformed,
}

fn federated_identity_advertisement(
    document: &serde_json::Value,
) -> FederatedIdentityAdvertisement {
    let limitation_requires = match document
        .get("limitation")
        .and_then(|limitation| limitation.get("federated_identity"))
    {
        Some(serde_json::Value::Bool(value)) => *value,
        Some(_) => return FederatedIdentityAdvertisement::Malformed,
        None => false,
    };

    let Some(discovery) = document.get("federated_identity") else {
        return if limitation_requires {
            FederatedIdentityAdvertisement::Malformed
        } else {
            FederatedIdentityAdvertisement::NotAdvertised
        };
    };

    if !limitation_requires {
        return FederatedIdentityAdvertisement::Malformed;
    }

    let Some(discovery) = discovery.as_object() else {
        return FederatedIdentityAdvertisement::Malformed;
    };
    if discovery.get("core").and_then(serde_json::Value::as_str) != Some("client-attached") {
        return FederatedIdentityAdvertisement::Malformed;
    }

    let Some(assertion_freshness) = discovery
        .get("assertion_freshness")
        .and_then(serde_json::Value::as_object)
    else {
        return FederatedIdentityAdvertisement::Malformed;
    };
    if assertion_freshness
        .get("class")
        .and_then(serde_json::Value::as_str)
        != Some("offline-jwt")
    {
        return FederatedIdentityAdvertisement::Malformed;
    }

    match assertion_freshness.get("maximum_residual_upstream_revocation_seconds") {
        Some(serde_json::Value::Null) | None => FederatedIdentityAdvertisement::Required,
        Some(_) => FederatedIdentityAdvertisement::Malformed,
    }
}

fn evaluate_enterprise_login_gate(
    relay_url: &str,
    document: &serde_json::Value,
    trusted_relays: Option<&'static str>,
) -> Result<EnterpriseLoginGateStatus, String> {
    let advertisement = federated_identity_advertisement(document);
    let trusted = trusted_enterprise_relays(trusted_relays)?;
    let relay_is_trusted = relay_matches_any_trusted(relay_url, &trusted)?;

    match (advertisement, relay_is_trusted, trusted.is_empty()) {
        (FederatedIdentityAdvertisement::NotAdvertised, true, _) => Err(
            "This trusted enterprise community did not advertise supported enterprise login. Update the relay or choose another community."
                .to_owned(),
        ),
        (FederatedIdentityAdvertisement::NotAdvertised, false, _) => {
            Ok(EnterpriseLoginGateStatus::NotRequired)
        }
        (FederatedIdentityAdvertisement::Malformed, _, _) => Err(
            "This community advertised unsupported enterprise login requirements. Update Buzz or choose another community."
                .to_owned(),
        ),
        (FederatedIdentityAdvertisement::Required, true, _) => {
            Ok(EnterpriseLoginGateStatus::Required)
        }
        (FederatedIdentityAdvertisement::Required, false, true) => Err(
            "This community requires enterprise login, but this Buzz build does not include trusted enterprise authentication configuration."
                .to_owned(),
        ),
        (FederatedIdentityAdvertisement::Required, false, false) => Err(
            "This community requires enterprise login, but it does not match this Buzz build's trusted enterprise relay configuration."
                .to_owned(),
        ),
    }
}

fn trusted_enterprise_relays(raw: Option<&'static str>) -> Result<Vec<String>, String> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(Vec::new());
    };
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(canonical_relay_url)
        .collect()
}

fn relay_matches_any_trusted(relay_url: &str, trusted: &[String]) -> Result<bool, String> {
    if trusted.is_empty() {
        return Ok(false);
    }
    let relay = canonical_relay_url(relay_url)?;
    Ok(trusted.iter().any(|trusted| trusted == &relay))
}

fn canonical_relay_url(raw: &str) -> Result<String, String> {
    let mut url = Url::parse(raw.trim()).map_err(|error| format!("invalid relay URL: {error}"))?;
    let scheme = match url.scheme() {
        "ws" | "http" => "ws",
        "wss" | "https" => "wss",
        other => return Err(format!("unsupported relay URL scheme: {other}")),
    };
    url.set_scheme(scheme)
        .map_err(|_| "could not normalize relay URL scheme".to_owned())?;
    if url.host_str().is_none() {
        return Err("relay URL must include a host".to_owned());
    }
    url.set_fragment(None);
    url.set_query(None);
    let path = url.path().trim_end_matches('/').to_owned();
    url.set_path(&path);
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discovery_document() -> serde_json::Value {
        serde_json::json!({
            "limitation": { "federated_identity": true },
            "federated_identity": {
                "core": "client-attached",
                "assertion_freshness": {
                    "class": "offline-jwt",
                    "maximum_residual_upstream_revocation_seconds": null
                }
            }
        })
    }

    #[test]
    fn matching_trusted_relay_requires_enterprise_login() {
        assert_eq!(
            evaluate_enterprise_login_gate(
                "https://buzz.block.builderlab.xyz/",
                &discovery_document(),
                Some("wss://buzz.block.builderlab.xyz"),
            )
            .unwrap(),
            EnterpriseLoginGateStatus::Required,
        );
    }

    #[test]
    fn ordinary_relay_without_discovery_is_not_required() {
        assert_eq!(
            evaluate_enterprise_login_gate(
                "wss://community.example",
                &serde_json::json!({ "supported_nips": [1, 11] }),
                Some("wss://buzz.block.builderlab.xyz"),
            )
            .unwrap(),
            EnterpriseLoginGateStatus::NotRequired,
        );
    }

    #[test]
    fn trusted_relay_missing_discovery_fails_closed() {
        let error = evaluate_enterprise_login_gate(
            "wss://buzz.block.builderlab.xyz",
            &serde_json::json!({}),
            Some("wss://buzz.block.builderlab.xyz"),
        )
        .unwrap_err();
        assert!(error.contains("did not advertise supported enterprise login"));
    }

    #[test]
    fn advertised_enterprise_without_trusted_config_fails_closed() {
        let error = evaluate_enterprise_login_gate(
            "wss://buzz.block.builderlab.xyz",
            &discovery_document(),
            None,
        )
        .unwrap_err();
        assert!(error.contains("does not include trusted enterprise authentication configuration"));
    }

    #[test]
    fn advertised_enterprise_on_untrusted_relay_fails_closed() {
        let error = evaluate_enterprise_login_gate(
            "wss://evil.example",
            &discovery_document(),
            Some("wss://buzz.block.builderlab.xyz"),
        )
        .unwrap_err();
        assert!(error
            .contains("does not match this Buzz build's trusted enterprise relay configuration"));
    }

    #[test]
    fn malformed_discovery_fails_closed() {
        let mut document = discovery_document();
        document["federated_identity"]["assertion_freshness"]["class"] =
            serde_json::Value::String("current-status".to_owned());
        let error = evaluate_enterprise_login_gate(
            "wss://buzz.block.builderlab.xyz",
            &document,
            Some("wss://buzz.block.builderlab.xyz"),
        )
        .unwrap_err();
        assert!(error.contains("unsupported enterprise login requirements"));
    }

    #[test]
    fn finite_offline_jwt_residual_bound_fails_closed() {
        let mut document = discovery_document();
        document["federated_identity"]["assertion_freshness"]
            ["maximum_residual_upstream_revocation_seconds"] = serde_json::Value::from(60);
        let error = evaluate_enterprise_login_gate(
            "wss://buzz.block.builderlab.xyz",
            &document,
            Some("wss://buzz.block.builderlab.xyz"),
        )
        .unwrap_err();
        assert!(error.contains("unsupported enterprise login requirements"));
    }
}
