//! OAuth machine-to-machine auth (`oauth-m2m`): the client-credentials
//! grant for a service principal, with the token cached and refreshed
//! proactively.

use std::sync::Arc;

use futures_util::future::BoxFuture;
use secrecy::{ExposeSecret, SecretString};

use super::common::{TokenHeaders, parse_oauth_token, send_token_request};
use super::oidc::OAuthEndpoints;
use super::token::{CachedTokenSource, Token, TokenSource};
use super::{CredentialsProvider, CredentialsStrategy};
use crate::config::Config;
use crate::error::{Error, Result};

/// Service-principal OAuth from `client_id` + `client_secret`.
#[derive(Debug, Clone, Copy, Default)]
pub struct M2mCredentials;

impl CredentialsStrategy for M2mCredentials {
    fn name(&self) -> &'static str {
        "oauth-m2m"
    }

    fn configure<'a>(
        &'a self,
        cfg: &'a Config,
        http: &'a reqwest::Client,
    ) -> BoxFuture<'a, Result<Option<Arc<dyn CredentialsProvider>>>> {
        Box::pin(async move {
            let Some(source) = ClientCredentials::from_config(cfg, http, self.name()).await? else {
                return Ok(None);
            };
            Ok(Some(
                Arc::new(TokenHeaders::bearer(CachedTokenSource::new(source, true)))
                    as Arc<dyn CredentialsProvider>,
            ))
        })
    }
}

/// The uncached client-credentials grant. Go: `databricksOAuthTokenSource`.
pub(crate) struct ClientCredentials {
    http: reqwest::Client,
    token_url: String,
    client_id: String,
    client_secret: SecretString,
    scopes: String,
    /// Go: `EndpointParams{"assume_group": cfg.GroupID}`.
    assume_group: Option<String>,
}

impl ClientCredentials {
    /// `Ok(None)` when `client_id`/`client_secret` are not both set.
    pub(crate) async fn from_config(
        cfg: &Config,
        http: &reqwest::Client,
        auth_type: &str,
    ) -> Result<Option<Self>> {
        let (Some(id), Some(secret)) = (
            cfg.client_id.as_deref().filter(|s| !s.is_empty()),
            cfg.client_secret
                .as_ref()
                .filter(|s| !s.expose_secret().is_empty()),
        ) else {
            return Ok(None);
        };
        let endpoints = OAuthEndpoints::discover(cfg, http)
            .await
            .map_err(|e| Error::Auth {
                auth_type: auth_type.into(),
                message: format!("oidc: {e}"),
            })?;
        tracing::debug!(
            client_id = id,
            "generating Databricks OAuth token for service principal"
        );
        Ok(Some(Self {
            http: http.clone(),
            token_url: endpoints.token_endpoint,
            client_id: id.to_owned(),
            client_secret: secret.clone(),
            scopes: cfg.scopes_or_default().join(" "),
            assume_group: cfg.group_id.clone().filter(|g| !g.is_empty()),
        }))
    }
}

impl TokenSource for ClientCredentials {
    fn token(&self) -> BoxFuture<'_, Result<Token>> {
        Box::pin(async move {
            let body = send_token_request(&self.token_url, || {
                self.http
                    .post(&self.token_url)
                    .basic_auth(&self.client_id, Some(self.client_secret.expose_secret()))
                    .form(&[
                        ("grant_type", Some("client_credentials")),
                        ("scope", Some(self.scopes.as_str())),
                        ("assume_group", self.assume_group.as_deref()),
                    ])
            })
            .await?;
            parse_oauth_token(&body)
        })
    }
}
