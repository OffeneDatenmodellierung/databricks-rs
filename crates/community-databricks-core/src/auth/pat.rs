//! Personal access token auth (`pat`).

use std::sync::Arc;

use futures_util::future::BoxFuture;
use secrecy::{ExposeSecret, SecretString};

use super::{CredentialsProvider, CredentialsStrategy, Headers, bearer, reject_group_role};
use crate::config::Config;
use crate::error::{Error, Result};

/// `Authorization: Bearer <token>` from `token` / `DATABRICKS_TOKEN`.
#[derive(Debug, Clone, Copy, Default)]
pub struct PatCredentials;

impl CredentialsStrategy for PatCredentials {
    fn name(&self) -> &'static str {
        "pat"
    }

    fn configure<'a>(
        &'a self,
        cfg: &'a Config,
        _http: &'a reqwest::Client,
    ) -> BoxFuture<'a, Result<Option<Arc<dyn CredentialsProvider>>>> {
        Box::pin(async move {
            reject_group_role(cfg, "pat")?;
            let Some(token) = cfg.token.as_ref().filter(|t| !t.expose_secret().is_empty()) else {
                return Ok(None);
            };
            if cfg.host.as_deref().unwrap_or_default().is_empty() {
                return Err(Error::Auth {
                    auth_type: "pat".into(),
                    message: "host is required for PAT authentication".into(),
                });
            }
            Ok(Some(
                Arc::new(Pat(token.clone())) as Arc<dyn CredentialsProvider>
            ))
        })
    }
}

#[derive(Debug)]
struct Pat(SecretString);

impl CredentialsProvider for Pat {
    fn headers(&self) -> BoxFuture<'_, Result<Headers>> {
        Box::pin(async move { bearer(self.0.expose_secret()) })
    }
}
