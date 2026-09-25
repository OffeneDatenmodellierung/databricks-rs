//! OAuth machine-to-machine auth (`oauth-m2m`): the client-credentials
//! grant for a service principal, with the token cached and refreshed
//! proactively.

use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use tokio::time::Instant;

use super::oidc::OAuthEndpoints;
use super::token::{CachedTokenSource, Token, TokenSource};
use super::{CredentialsProvider, CredentialsStrategy, Headers, bearer};
use crate::config::Config;
use crate::error::{ApiError, Error, Result};
use crate::http::{backoff, retry_after};

/// Retry budget for a single token request (Go: 1 minute).
const TOKEN_RETRY_TIMEOUT: Duration = Duration::from_mins(1);
/// Go's `retriableCodes` for token requests.
const RETRIABLE: &[u16] = &[429, 502, 503, 504];

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
                    auth_type: "oauth-m2m".into(),
                    message: format!("oidc: {e}"),
                })?;
            tracing::debug!(
                client_id = id,
                "generating Databricks OAuth token for service principal"
            );
            let source = ClientCredentials {
                http: http.clone(),
                token_url: endpoints.token_endpoint,
                client_id: id.to_owned(),
                client_secret: secret.clone(),
                scopes: cfg.scopes_or_default().join(" "),
            };
            Ok(Some(
                Arc::new(M2m(CachedTokenSource::new(source, true))) as Arc<dyn CredentialsProvider>
            ))
        })
    }
}

#[derive(Debug)]
struct M2m(CachedTokenSource);

impl CredentialsProvider for M2m {
    fn headers(&self) -> BoxFuture<'_, Result<Headers>> {
        Box::pin(async move { bearer(self.0.token().await?.secret()) })
    }
}

struct ClientCredentials {
    http: reqwest::Client,
    token_url: String,
    client_id: String,
    client_secret: SecretString,
    scopes: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

impl ClientCredentials {
    async fn attempt(&self) -> std::result::Result<Token, (Error, Option<Duration>)> {
        let resp = self
            .http
            .post(&self.token_url)
            .basic_auth(&self.client_id, Some(self.client_secret.expose_secret()))
            .form(&[
                ("grant_type", "client_credentials"),
                ("scope", self.scopes.as_str()),
            ])
            .send()
            .await;
        let resp = match resp {
            Ok(r) => r,
            Err(e) if e.is_connect() || e.is_timeout() => {
                return Err((e.into(), Some(Duration::ZERO)));
            }
            Err(e) => return Err((e.into(), None)),
        };
        let status = resp.status().as_u16();
        let wait = retry_after(resp.headers());
        let body = resp.bytes().await.map_err(|e| (e.into(), None))?;
        if !(200..300).contains(&status) {
            let path = reqwest::Url::parse(&self.token_url)
                .map(|u| u.path().to_owned())
                .unwrap_or_default();
            let err: Error = ApiError::from_response(status, "POST", &path, &body).into();
            let retry = RETRIABLE
                .contains(&status)
                .then(|| wait.unwrap_or(Duration::ZERO));
            return Err((err, retry));
        }
        let t: TokenResponse =
            serde_json::from_slice(&body).map_err(|e| (Error::json("token response", e), None))?;
        Ok(Token {
            access_token: SecretString::from(t.access_token),
            token_type: t.token_type.unwrap_or_else(|| "Bearer".into()),
            expiry: t
                .expires_in
                .filter(|s| *s > 0)
                .map(|s| Instant::now() + Duration::from_secs(s)),
        })
    }
}

impl TokenSource for ClientCredentials {
    fn token(&self) -> BoxFuture<'_, Result<Token>> {
        Box::pin(async move {
            let deadline = Instant::now() + TOKEN_RETRY_TIMEOUT;
            let mut attempt = 0;
            loop {
                attempt += 1;
                match self.attempt().await {
                    Ok(t) => return Ok(t),
                    Err((e, None)) => return Err(e),
                    Err((e, Some(hint))) => {
                        let wait = backoff(attempt).max(hint);
                        if Instant::now() + wait > deadline {
                            return Err(e);
                        }
                        tracing::debug!(attempt, ?wait, "retrying token request: {e}");
                        tokio::time::sleep(wait).await;
                    }
                }
            }
        })
    }
}
