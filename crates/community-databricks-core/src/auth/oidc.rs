//! OAuth endpoint discovery (Go: `u2m.BasicOAuthEndpointSupplier`).

use serde::Deserialize;

use crate::config::{Config, HostType};
use crate::error::{ApiError, Error, Result};

/// OAuth authorization-server endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub struct OAuthEndpoints {
    /// Where users are sent to authorise (U2M).
    #[serde(default)]
    pub authorization_endpoint: String,
    /// Where tokens are minted.
    pub token_endpoint: String,
}

impl OAuthEndpoints {
    /// Discover endpoints for `cfg`:
    ///
    /// * `discovery_url` set (explicitly or from host metadata) → fetch it;
    /// * account host → fixed `{host}/oidc/accounts/{account_id}/v1/…`;
    /// * workspace host → `{host}/oidc/.well-known/oauth-authorization-server`;
    /// * unified host → `{host}/oidc/accounts/{account_id}/.well-known/…`.
    pub async fn discover(cfg: &Config, http: &reqwest::Client) -> Result<Self> {
        if let Some(url) = cfg.discovery_url.as_deref().filter(|u| !u.is_empty()) {
            return fetch(http, url).await;
        }
        let host = cfg
            .host
            .as_deref()
            .filter(|h| !h.is_empty())
            .ok_or_else(|| Error::Config("no host configured".into()))?;
        let account = || {
            cfg.account_id
                .as_deref()
                .filter(|a| !a.is_empty())
                .ok_or_else(|| {
                    Error::Config("account_id is required for account-level OAuth".into())
                })
        };
        match cfg.host_type() {
            HostType::Account => {
                let acc = account()?;
                Ok(Self {
                    authorization_endpoint: format!("{host}/oidc/accounts/{acc}/v1/authorize"),
                    token_endpoint: format!("{host}/oidc/accounts/{acc}/v1/token"),
                })
            }
            HostType::Workspace => {
                fetch(
                    http,
                    &format!("{host}/oidc/.well-known/oauth-authorization-server"),
                )
                .await
            }
            HostType::Unified => {
                let acc = account()?;
                fetch(
                    http,
                    &format!("{host}/oidc/accounts/{acc}/.well-known/oauth-authorization-server"),
                )
                .await
            }
        }
    }
}

async fn fetch(http: &reqwest::Client, url: &str) -> Result<OAuthEndpoints> {
    let resp = http.get(url).send().await?;
    let status = resp.status();
    let body = resp.bytes().await?;
    if !status.is_success() {
        let path = reqwest::Url::parse(url)
            .map(|u| u.path().to_owned())
            .unwrap_or_default();
        return Err(ApiError::from_response(status.as_u16(), "GET", &path, &body).into());
    }
    serde_json::from_slice(&body).map_err(|e| Error::json("oauth endpoints", e))
}
