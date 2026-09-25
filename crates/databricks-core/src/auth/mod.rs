//! Authentication.
//!
//! A [`CredentialsStrategy`] inspects a resolved [`Config`] and, if it has
//! what it needs, returns a [`CredentialsProvider`] that stamps headers onto
//! each request. [`DefaultCredentials`] tries strategies in the same order
//! as `databricks-sdk-go` and picks the first that works; setting
//! `auth_type` (or `DATABRICKS_AUTH_TYPE`) selects one explicitly.
//!
//! Implemented, in chain order: `pat`, `oauth-m2m`, `databricks-cli`,
//! `github-oidc`, `env-oidc`, `file-oidc`, `mem-oidc`, `azure-msi` and
//! `oauth-m2m-gcp`. `mem-oidc` is Rust-only (an in-memory ID-token source,
//! databricks-sdk-go#1790). The rest of Go's chain is listed in
//! [`PLANNED_AUTH_TYPES`] so a requested-but-missing type gives a clear
//! error.

mod azure_msi;
mod cli;
mod common;
mod gcp;
mod m2m;
mod oidc;
mod pat;
mod token;
mod wif;

use std::fmt::Debug;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use reqwest::header::{HeaderName, HeaderValue};

pub use azure_msi::AzureMsiCredentials;
pub(crate) use azure_msi::ensure_workspace_host;
pub use cli::DatabricksCliCredentials;
pub use gcp::GcpM2mCredentials;
pub use m2m::M2mCredentials;
pub use oidc::OAuthEndpoints;
pub use pat::PatCredentials;
pub use token::{CachedTokenSource, Token, TokenSource};
pub use wif::{
    EnvOidcCredentials, FileOidcCredentials, GithubOidcCredentials, IdToken, IdTokenFn,
    IdTokenSource, MemOidcCredentials,
};

use crate::config::Config;
use crate::error::{Error, Result};

const AUTH_DOC_URL: &str =
    "https://docs.databricks.com/en/dev-tools/auth.html#databricks-client-unified-authentication";

/// Auth types in Go's default chain that are not implemented yet.
pub const PLANNED_AUTH_TYPES: &[&str] = &[
    "basic",
    "metadata-service",
    "azure-devops-oidc",
    "github-oidc-azure",
    "azure-client-secret",
    "azure-cli",
    "google-credentials",
    "google-id",
];

/// Headers to add to a request.
pub type Headers = Vec<(HeaderName, HeaderValue)>;

/// Produces authentication headers for each request.
pub trait CredentialsProvider: Send + Sync + Debug {
    /// Headers for the next request (may refresh a token).
    fn headers(&self) -> BoxFuture<'_, Result<Headers>>;
}

/// Decides whether it can authenticate a [`Config`], and if so builds a
/// [`CredentialsProvider`].
pub trait CredentialsStrategy: Send + Sync {
    /// Auth type name (for example `pat`).
    fn name(&self) -> &'static str;

    /// `Ok(None)` means "not configured for this strategy — try the next".
    fn configure<'a>(
        &'a self,
        cfg: &'a Config,
        http: &'a reqwest::Client,
    ) -> BoxFuture<'a, Result<Option<Arc<dyn CredentialsProvider>>>>;
}

/// Tries strategies in order; honours `auth_type`.
pub struct DefaultCredentials {
    strategies: Vec<Box<dyn CredentialsStrategy>>,
}

impl Default for DefaultCredentials {
    fn default() -> Self {
        Self {
            // Go's order, minus the strategies not ported yet.
            strategies: vec![
                Box::new(PatCredentials),
                Box::new(M2mCredentials),
                Box::new(DatabricksCliCredentials),
                Box::new(GithubOidcCredentials),
                Box::new(EnvOidcCredentials),
                Box::new(FileOidcCredentials),
                Box::new(MemOidcCredentials),
                Box::new(AzureMsiCredentials),
                Box::new(GcpM2mCredentials),
            ],
        }
    }
}

impl DefaultCredentials {
    /// A chain of custom strategies.
    #[must_use]
    pub fn new(strategies: Vec<Box<dyn CredentialsStrategy>>) -> Self {
        Self { strategies }
    }

    /// Configure the first working strategy. Returns its name and provider.
    pub async fn configure(
        &self,
        cfg: &Config,
        http: &reqwest::Client,
    ) -> Result<(&'static str, Arc<dyn CredentialsProvider>)> {
        if let Some(wanted) = cfg.auth_type.as_deref().filter(|t| !t.is_empty()) {
            let Some(s) = self.strategies.iter().find(|s| s.name() == wanted) else {
                let hint = if PLANNED_AUTH_TYPES.contains(&wanted) {
                    " (supported by the Go SDK; not implemented in Rust yet)"
                } else {
                    ""
                };
                return Err(cfg.wrap(Error::Config(format!(
                    "auth type {wanted:?} not found{hint}, please check {AUTH_DOC_URL} for a list of supported auth types"
                ))));
            };
            return match s.configure(cfg, http).await {
                Ok(Some(p)) => Ok((s.name(), p)),
                Ok(None) => Err(cfg.wrap(Error::Auth {
                    auth_type: s.name().into(),
                    message: "not configured".into(),
                })),
                Err(e) => Err(cfg.wrap(as_auth(s.name(), e))),
            };
        }
        for s in &self.strategies {
            match s.configure(cfg, http).await {
                Ok(Some(p)) => {
                    // Dry run: some providers can only be validated by
                    // producing headers (Go does the same).
                    if let Err(e) = p.headers().await {
                        tracing::debug!(auth = s.name(), "dry run failed: {e}");
                        continue;
                    }
                    return Ok((s.name(), p));
                }
                Ok(None) => tracing::trace!(auth = s.name(), "not configured"),
                Err(e) => tracing::debug!(auth = s.name(), "failed to configure: {e}"),
            }
        }
        Err(cfg.wrap(Error::Auth {
            auth_type: "default".into(),
            message: format!(
                "cannot configure default credentials, please check {AUTH_DOC_URL} to configure credentials for your preferred authentication method"
            ),
        }))
    }
}

/// Go: `unsupportedGroupRoleAssumption`. Strategies that can only produce
/// normal-access credentials refuse to run when a group role is requested.
pub(crate) fn reject_group_role(cfg: &Config, auth_type: &str) -> Result<()> {
    if cfg.group_id.as_deref().is_none_or(str::is_empty) {
        return Ok(());
    }
    Err(Error::Auth {
        auth_type: auth_type.into(),
        message: format!(
            "auth type {auth_type:?} does not support group role assumption. Use Databricks OAuth authentication"
        ),
    })
}

fn as_auth(name: &str, e: Error) -> Error {
    match e {
        Error::Auth { .. } => e,
        other => Error::Auth {
            auth_type: name.into(),
            message: other.to_string(),
        },
    }
}

pub(crate) fn bearer(token: &str) -> Result<Headers> {
    let mut v = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| Error::Config("token contains invalid header characters".into()))?;
    v.set_sensitive(true);
    Ok(vec![(reqwest::header::AUTHORIZATION, v)])
}
