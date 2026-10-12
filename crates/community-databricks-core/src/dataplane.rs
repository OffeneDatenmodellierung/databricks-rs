//! Direct data-plane access (Go: `config/experimental/auth/dataplane` and
//! the per-service `dataPlaneInfo*` caches).
//!
//! Some operations, such as querying a serving endpoint, can bypass the
//! control plane. The control plane names the data-plane URL and the
//! `authorization_details` a token for it must carry; the client then
//! exchanges its Databricks OAuth token for one scoped to that endpoint at
//! `/oidc/v1/token` (an RFC 7523 `jwt-bearer` grant with RFC 9396
//! authorization details) and sends the request there.
//!
//! [`ApiClient`](crate::ApiClient) keeps both caches, shared by its clones:
//!
//! * targets, per operation and request key, for the life of the client
//!   (as Go does; nothing invalidates them);
//! * one [`CachedTokenSource`] per target, so data-plane tokens are reused
//!   and refreshed ahead of expiry like control-plane ones
//!   (`min(TTL / 2, 20 min)`).
//!
//! Only Databricks OAuth credentials can be exchanged; PAT, Azure AD and
//! Google credentials get an error, as in Go.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use futures_util::future::BoxFuture;
use secrecy::SecretString;

use crate::auth::common::{parse_oauth_token, send_token_request};
use crate::auth::{CachedTokenSource, CredentialsProvider, Headers, Token, TokenSource, bearer};
use crate::error::Result;

/// `grant_type` for the exchange (Go: `httpclient.JWTGrantType`).
pub const JWT_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";

/// Where and how to reach one data-plane operation.
#[derive(Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct DataPlaneTarget {
    /// The data-plane URL the request is sent to.
    pub endpoint_url: String,
    /// The `authorization_details` the data-plane token must carry, as
    /// the control plane returned them (a JSON string).
    pub authorization_details: String,
}

impl DataPlaneTarget {
    /// A target from the control plane's `endpoint_url` and
    /// `authorization_details`.
    #[must_use]
    pub fn new(endpoint_url: impl Into<String>, authorization_details: impl Into<String>) -> Self {
        Self {
            endpoint_url: endpoint_url.into(),
            authorization_details: authorization_details.into(),
        }
    }
}

impl fmt::Debug for DataPlaneTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Authorization details name what the token may do; keep them out
        // of logs.
        f.debug_struct("DataPlaneTarget")
            .field("endpoint_url", &self.endpoint_url)
            .finish_non_exhaustive()
    }
}

/// The caches an [`ApiClient`](crate::ApiClient) keeps for data-plane calls.
#[derive(Default)]
pub(crate) struct DataPlane {
    targets: Mutex<HashMap<String, DataPlaneTarget>>,
    tokens: Mutex<HashMap<DataPlaneTarget, CachedTokenSource>>,
}

impl DataPlane {
    pub(crate) fn target(&self, key: &str) -> Option<DataPlaneTarget> {
        self.targets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .cloned()
    }

    /// Store `target` unless another caller got there first; returns the
    /// cached one.
    pub(crate) fn store_target(&self, key: &str, target: DataPlaneTarget) -> DataPlaneTarget {
        self.targets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(key.to_owned())
            .or_insert(target)
            .clone()
    }

    /// The token cache for `target`, creating it with `make` on first use.
    pub(crate) fn tokens(
        &self,
        target: &DataPlaneTarget,
        make: impl FnOnce() -> CachedTokenSource,
    ) -> CachedTokenSource {
        self.tokens
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(target.clone())
            .or_insert_with(make)
            .clone()
    }
}

/// Exchanges the control-plane token for a data-plane one (Go:
/// `ApiClient.GetOAuthToken`). Uncached; wrapped in a [`CachedTokenSource`].
pub(crate) struct Exchange {
    pub http: reqwest::Client,
    pub token_url: String,
    pub control: Arc<dyn TokenSource>,
    pub authorization_details: String,
}

impl TokenSource for Exchange {
    fn token(&self) -> BoxFuture<'_, Result<Token>> {
        Box::pin(async move {
            let control = self.control.token().await?;
            let body = send_token_request(&self.token_url, || {
                self.http
                    .post(&self.token_url)
                    .bearer_auth(control.secret())
                    .form(&[
                        ("grant_type", JWT_GRANT_TYPE),
                        ("authorization_details", self.authorization_details.as_str()),
                        ("assertion", control.secret()),
                    ])
            })
            .await?;
            parse_oauth_token(&body)
        })
    }
}

/// Authenticates one data-plane request with an already-minted token.
pub(crate) struct DataPlaneBearer(pub SecretString);

impl fmt::Debug for DataPlaneBearer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DataPlaneBearer(***)")
    }
}

impl CredentialsProvider for DataPlaneBearer {
    fn headers(&self) -> BoxFuture<'_, Result<Headers>> {
        Box::pin(async move { bearer(secrecy::ExposeSecret::expose_secret(&self.0)) })
    }
}
