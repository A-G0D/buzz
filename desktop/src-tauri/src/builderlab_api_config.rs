#[derive(Clone, Copy)]
pub(crate) enum ApiBaseUrlKind {
    Builderlab,
    EnterpriseAuthAdapter,
}

const DEFAULT_BUILDERLAB_API_BASE_URL: &str = "https://app.builderlab.xyz/api/goose";

fn env_name(kind: ApiBaseUrlKind) -> &'static str {
    match kind {
        ApiBaseUrlKind::Builderlab => "BUZZ_BUILD_BUILDERLAB_API_BASE_URL",
        ApiBaseUrlKind::EnterpriseAuthAdapter => "BUZZ_BUILD_ENTERPRISE_AUTH_ADAPTER_BASE_URL",
    }
}

pub(crate) fn validate_api_base_url(raw: &str, kind: ApiBaseUrlKind) -> Result<String, String> {
    let env = env_name(kind);
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(format!("{env} must not be empty when set"));
    }

    let url =
        url::Url::parse(trimmed).map_err(|error| format!("{env} is not a valid URL: {error}"))?;
    match url.scheme() {
        "https" | "http" => {}
        _ => {
            return Err(format!("{env} must use http:// or https://"));
        }
    }
    if url.host_str().is_none() {
        return Err(format!("{env} must include a host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!("{env} must not include userinfo"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(format!("{env} must not include a query or fragment"));
    }
    Ok(trimmed.to_owned())
}

pub(crate) fn validate_builderlab_api_base_url(raw: &str) -> Result<String, String> {
    validate_api_base_url(raw, ApiBaseUrlKind::Builderlab)
}

pub(crate) fn resolve_builderlab_api_base_url(
    configured: Option<&str>,
    _enterprise_relays: Option<&str>,
) -> Result<String, String> {
    if let Some(configured) = configured {
        return validate_builderlab_api_base_url(configured);
    }

    Ok(DEFAULT_BUILDERLAB_API_BASE_URL.to_owned())
}

pub(crate) fn resolve_enterprise_auth_adapter_base_url(
    configured: Option<&str>,
    enterprise_relays: Option<&str>,
) -> Result<Option<String>, String> {
    let configured = configured.map(str::trim).filter(|value| !value.is_empty());
    if let Some(configured) = configured {
        return validate_api_base_url(configured, ApiBaseUrlKind::EnterpriseAuthAdapter).map(Some);
    }

    if enterprise_relays
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
    {
        return Err(
            "BUZZ_BUILD_ENTERPRISE_AUTH_ADAPTER_BASE_URL must be set when BUZZ_BUILD_ENTERPRISE_AUTH_RELAYS is set"
                .to_owned(),
        );
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_builderlab_api_base_url_normalizes_trailing_slash() {
        assert_eq!(
            resolve_builderlab_api_base_url(Some(" https://login.example/api/goose/ "), None)
                .unwrap(),
            "https://login.example/api/goose",
        );
    }

    #[test]
    fn hosted_builderlab_api_keeps_default_when_enterprise_auth_uses_separate_adapter() {
        assert_eq!(
            resolve_builderlab_api_base_url(None, Some("wss://buzz.block.example")).unwrap(),
            DEFAULT_BUILDERLAB_API_BASE_URL,
        );
    }

    #[test]
    fn enterprise_auth_requires_configured_adapter_base_url() {
        let error =
            resolve_enterprise_auth_adapter_base_url(None, Some("wss://buzz.block.example"))
                .expect_err("enterprise builds must configure the browser-login adapter base");
        assert!(error.contains("BUZZ_BUILD_ENTERPRISE_AUTH_ADAPTER_BASE_URL must be set"));
    }

    #[test]
    fn configured_enterprise_adapter_base_url_normalizes_trailing_slash() {
        assert_eq!(
            resolve_enterprise_auth_adapter_base_url(
                Some(" https://identity.example/buzz-auth/ "),
                Some("wss://buzz.block.example"),
            )
            .unwrap(),
            Some("https://identity.example/buzz-auth".to_owned()),
        );
    }

    #[test]
    fn builderlab_api_base_url_rejects_ambiguous_urls() {
        for raw in [
            "",
            "ftp://login.example/api/goose",
            "https://user@login.example/api/goose",
            "https://login.example/api/goose?env=prod",
            "https://login.example/api/goose#prod",
        ] {
            assert!(
                validate_builderlab_api_base_url(raw).is_err(),
                "{raw:?} must fail closed",
            );
        }
    }
}
