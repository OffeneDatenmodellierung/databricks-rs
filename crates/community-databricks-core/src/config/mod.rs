//! Unified client configuration.
//!
//! Resolution order matches `databricks-sdk-go` v0.182.0
//! (`Config.EnsureResolved`):
//!
//! 1. Values set in code.
//! 2. Environment variables (`DATABRICKS_HOST`, `DATABRICKS_TOKEN`, …).
//! 3. A `~/.databrickscfg` profile (see [`file`] rules).
//! 4. Validation: at most one auth method unless `auth_type` picks one.
//! 5. Host normalisation (`https://` added, path dropped, `?o=`/`?a=`
//!    query parameters lifted into `workspace_id`/`account_id`).
//! 6. Best-effort host metadata from `/.well-known/databricks-config`,
//!    back-filling `account_id`, `workspace_id`, `cloud`, the host type and
//!    the OIDC discovery URL.

pub(crate) mod attrs;
mod environment;
mod file;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use secrecy::SecretString;
use serde::Deserialize;
use url::Url;

pub use attrs::Source;
pub(crate) use environment::for_hostname as environment_for_hostname;
pub use environment::{AzureEnvironment, Cloud, Environment};

use crate::auth::IdTokenSource;
use crate::error::{Error, Result};

/// Default HTTP timeout (Go: 60s).
pub const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_mins(1);
/// Default retry budget (Go: 5 minutes).
pub const DEFAULT_RETRY_TIMEOUT: Duration = Duration::from_mins(5);
/// Default client-side rate limit (Go: 15 requests/second).
pub const DEFAULT_RATE_LIMIT: u32 = 15;

/// The kind of host a config points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub enum HostType {
    /// A workspace URL.
    #[serde(rename = "WORKSPACE_HOST")]
    Workspace,
    /// The accounts console (`accounts.*`).
    #[serde(rename = "ACCOUNT_HOST")]
    Account,
    /// A unified host serving both account- and workspace-level APIs.
    #[serde(rename = "UNIFIED_HOST")]
    Unified,
}

/// Parsed `/.well-known/databricks-config` response.
#[derive(Debug, Clone, Default, Deserialize)]
#[non_exhaustive]
pub struct HostMetadata {
    /// OIDC root; may contain an `{account_id}` placeholder.
    #[serde(default)]
    pub oidc_endpoint: String,
    /// Account ID associated with the host.
    #[serde(default)]
    pub account_id: String,
    /// Workspace ID associated with the host.
    #[serde(default)]
    pub workspace_id: String,
    /// Cloud provider (`AWS`, `AZURE`, `GCP`).
    #[serde(default)]
    pub cloud: String,
    /// Host type.
    #[serde(default)]
    pub host_type: Option<HostType>,
    /// Default OIDC audiences for token federation; the first becomes
    /// `audience` when none is configured.
    #[serde(default)]
    pub token_federation_default_oidc_audiences: Vec<String>,
}

/// Attributes without a typed field. `Debug` masks the sensitive ones
/// (`password`, `google_credentials`, …) like [`Config::debug_string`].
#[derive(Clone, Default)]
pub(crate) struct OtherAttrs(BTreeMap<String, String>);

impl std::ops::Deref for OtherAttrs {
    type Target = BTreeMap<String, String>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for OtherAttrs {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl std::fmt::Debug for OtherAttrs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(self.0.iter().map(|(k, v)| {
                let sensitive = attrs::find(k).is_none_or(|a| a.sensitive);
                (k, if sensitive { "***" } else { v.as_str() })
            }))
            .finish()
    }
}

/// How a resolved config reads ambient environment variables, such as the
/// Azure managed-identity endpoint or the GitHub Actions ID-token request.
///
/// [`Config::resolve_with`] keeps the injected lookup, so tests never touch
/// process-global state; [`Config::resolve`] and unresolved configs read
/// the process environment.
#[derive(Clone, Default)]
pub(crate) struct EnvLookup(Option<EnvFn>);

type EnvFn = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

impl std::fmt::Debug for EnvLookup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "EnvLookup(injected)"
        } else {
            "EnvLookup(process)"
        })
    }
}

impl EnvLookup {
    fn get(&self, key: &str) -> Option<String> {
        match &self.0 {
            Some(f) => f(key),
            None => std::env::var(key).ok(),
        }
        .filter(|v| !v.is_empty())
    }
}

/// Configuration for a [`WorkspaceClient`] or [`AccountClient`].
///
/// Construct with [`Config::default()`] and set what you need, or leave it
/// empty to pick everything up from the environment and config file.
///
/// [`WorkspaceClient`]: https://docs.rs/community-databricks-sdk
/// [`AccountClient`]: https://docs.rs/community-databricks-sdk
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
// Independent on/off settings mirroring Go's config, not a state machine.
#[allow(clippy::struct_excessive_bools)]
pub struct Config {
    /// Workspace or account host, e.g. `https://adb-123.4.azuredatabricks.net`.
    pub host: Option<String>,
    /// Default cluster ID (not used by the SDK itself).
    pub cluster_id: Option<String>,
    /// Default SQL warehouse ID (not used by the SDK itself).
    pub warehouse_id: Option<String>,
    /// Account ID, required for account-level APIs.
    pub account_id: Option<String>,
    /// Workspace ID, sent as `X-Databricks-Workspace-Id` on unified hosts.
    pub workspace_id: Option<String>,
    /// ID of a Databricks group whose role the client assumes
    /// (`DATABRICKS_GROUP_ID`).
    ///
    /// This needs OAuth auth. `oauth-m2m` sends it to the token endpoint as
    /// `assume_group`; `pat` refuses to authenticate while it is set rather
    /// than quietly giving normal access. As of August 2026 Databricks
    /// honours it for workspace authorization only.
    pub group_id: Option<String>,
    /// Personal access token.
    pub token: Option<SecretString>,
    /// OAuth client (service principal) ID.
    pub client_id: Option<String>,
    /// OAuth client secret.
    pub client_secret: Option<SecretString>,
    /// Config-file profile name.
    pub profile: Option<String>,
    /// Config-file path (default `~/.databrickscfg`).
    pub config_file: Option<String>,
    /// Force a particular auth strategy (`pat`, `oauth-m2m`).
    pub auth_type: Option<String>,
    /// OAuth scopes; defaults to `all-apis`.
    pub scopes: Vec<String>,
    /// Explicit OIDC discovery URL.
    pub discovery_url: Option<String>,
    /// Cloud provider override.
    pub cloud: Option<String>,
    /// Skip TLS verification. Testing only.
    pub skip_verify: bool,
    /// Per-request HTTP timeout in seconds (default 60).
    pub http_timeout_seconds: Option<u64>,
    /// Total retry budget in seconds (default 300; negative = unbounded).
    pub retry_timeout_seconds: Option<i64>,
    /// Requests per second (default 15).
    pub rate_limit: Option<u32>,
    /// Log request/response headers at trace level.
    pub debug_headers: bool,
    /// Pre-fetched host metadata; when set, the discovery request is skipped.
    pub host_metadata: Option<HostMetadata>,
    /// An in-memory source of OIDC ID tokens for workload identity
    /// federation (auth type `mem-oidc`; databricks-sdk-go#1790).
    ///
    /// Use this when the token is minted in-process (from a cloud SDK or
    /// an RPC) and should never be written to a file or an environment
    /// variable. Set in code only.
    pub id_token_source: Option<Arc<dyn IdTokenSource>>,
    /// Extra HTTP headers sent on every request (Go: `Config.Headers`).
    ///
    /// Set in code only. Headers the SDK manages itself (`Authorization`,
    /// `User-Agent`, `Content-Type`, `Accept`, `X-Databricks-Workspace-Id`
    /// and any header the credentials provider sets) are never overridden;
    /// a custom header with one of those names is ignored.
    pub headers: Vec<(String, String)>,
    /// Retry POST requests without an idempotency token after failures the
    /// server may already have acted on (timeouts, 503/504), as Go does.
    /// Off by default because a retried create can duplicate the object.
    /// Set in code only.
    pub retry_non_idempotent: bool,

    /// Attributes without a typed field (Azure, Google, OIDC, CLI…); read
    /// them with [`Config::attribute`].
    pub(crate) other: OtherAttrs,
    pub(crate) sources: HashMap<&'static str, Source>,
    pub(crate) resolved_host_type: Option<HostType>,
    pub(crate) resolved: bool,
    pub(crate) env: EnvLookup,
}

/// Host-metadata discovery shares one client per TLS mode, so resolving
/// several configs reuses connections.
fn metadata_client(skip_verify: bool) -> Result<reqwest::Client> {
    static VERIFIED: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    static UNVERIFIED: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    let slot = if skip_verify { &UNVERIFIED } else { &VERIFIED };
    if let Some(c) = slot.get() {
        return Ok(c.clone());
    }
    let c = reqwest::Client::builder()
        .danger_accept_invalid_certs(skip_verify)
        .build()?;
    Ok(slot.get_or_init(|| c).clone())
}

impl Config {
    /// A config for `host`, everything else from the environment.
    #[must_use]
    pub fn with_host(host: impl Into<String>) -> Self {
        Self {
            host: Some(host.into()),
            ..Self::default()
        }
    }

    /// Set a PAT.
    #[must_use]
    pub fn token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(SecretString::from(token.into()));
        self
    }

    /// Set OAuth M2M credentials.
    #[must_use]
    pub fn client_credentials(
        mut self,
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
    ) -> Self {
        self.client_id = Some(client_id.into());
        self.client_secret = Some(SecretString::from(client_secret.into()));
        self
    }

    /// Set the account ID.
    #[must_use]
    pub fn account(mut self, account_id: impl Into<String>) -> Self {
        self.account_id = Some(account_id.into());
        self
    }

    /// Add a header sent on every request. See [`Config::headers`].
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Set an in-memory OIDC ID-token source. See [`Config::id_token_source`].
    #[must_use]
    pub fn id_tokens(mut self, source: impl IdTokenSource + 'static) -> Self {
        self.id_token_source = Some(Arc::new(source));
        self
    }

    /// Resolve using the process environment and home directory.
    pub async fn resolve(self) -> Result<Self> {
        self.resolve_with(|k| std::env::var(k).ok(), std::env::home_dir())
            .await
    }

    /// Resolve with an injected environment and home directory.
    ///
    /// Tests use this instead of mutating process-global state.
    pub async fn resolve_with(
        mut self,
        env: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
        home: Option<PathBuf>,
    ) -> Result<Self> {
        if self.resolved {
            return Ok(self);
        }
        let env: EnvFn = Arc::new(env);
        self.env = EnvLookup(Some(Arc::clone(&env)));
        for attr in attrs::ATTRIBUTES {
            if attr.is_set(&self) {
                self.sources.entry(attr.name).or_insert(Source::Code);
            }
        }
        self.load_env(&*env).map_err(|e| self.wrap(e))?;
        file::load(&mut self, home).map_err(|e| self.wrap(e))?;
        self.validate().map_err(|e| self.wrap(e))?;
        self.fix_host().map_err(|e| self.wrap(e))?;
        self.scopes.sort();
        self.scopes.dedup();
        self.resolve_host_metadata().await;
        self.resolved = true;
        Ok(self)
    }

    fn load_env(&mut self, env: &dyn Fn(&str) -> Option<String>) -> Result<()> {
        for attr in attrs::ATTRIBUTES {
            if attr.is_set(self) {
                continue;
            }
            let Some(var) = attr.env else { continue };
            let Some(v) = env(var).filter(|v| !v.is_empty()) else {
                continue;
            };
            (attr.set)(self, v).map_err(Error::Config)?;
            self.sources.insert(attr.name, Source::Env(var));
        }
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        let mut used: Vec<&str> = attrs::ATTRIBUTES
            .iter()
            .filter(|a| a.is_set(self))
            .filter_map(|a| a.auth)
            .collect();
        used.sort_unstable();
        used.dedup();
        if used.len() <= 1 || self.auth_type.as_deref().is_some_and(|t| !t.is_empty()) {
            return Ok(());
        }
        Err(Error::Config(format!(
            "more than one authorization method configured: {}",
            used.join(" and ")
        )))
    }

    /// Normalise `host` to `scheme://authority`, lifting workspace/account
    /// IDs out of the query string (`?o=`, `?w=`, `?workspace_id=`, `?a=`,
    /// `?account_id=`).
    pub(crate) fn fix_host(&mut self) -> Result<()> {
        let Some(raw) = self.host.as_deref().filter(|h| !h.is_empty()) else {
            return Ok(());
        };
        let with_scheme = if raw.contains("://") {
            raw.to_owned()
        } else {
            format!("https://{raw}")
        };
        let url = Url::parse(&with_scheme)
            .map_err(|e| Error::Config(format!("invalid host {raw:?}: {e}")))?;
        let Some(hostname) = url.host_str().filter(|h| !h.is_empty()) else {
            return Err(Error::Config("no host configured".into()));
        };
        let q: HashMap<_, _> = url.query_pairs().into_owned().collect();
        if self.workspace_id.is_none() {
            let numeric = |k: &str| q.get(k).filter(|v| v.parse::<i64>().is_ok()).cloned();
            self.workspace_id = q
                .get("w")
                .filter(|v| !v.is_empty())
                .cloned()
                .or_else(|| numeric("o"))
                .or_else(|| numeric("workspace_id"));
        }
        if self.account_id.is_none() {
            self.account_id = ["a", "account_id"]
                .iter()
                .find_map(|k| q.get(*k).filter(|v| !v.is_empty()).cloned());
        }
        let port = url.port().map(|p| format!(":{p}")).unwrap_or_default();
        self.host = Some(format!("{}://{hostname}{port}", url.scheme()));
        Ok(())
    }

    async fn resolve_host_metadata(&mut self) {
        let Some(host) = self.host.clone() else {
            return;
        };
        let meta = match self.host_metadata.clone() {
            Some(m) => m,
            None => match self.fetch_host_metadata(&host).await {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(
                        "failed to resolve host metadata: {e}; falling back to user config"
                    );
                    return;
                }
            },
        };
        let mut backfill = |name: &'static str, slot: &mut Option<String>, v: &str| {
            if slot.is_none() && !v.is_empty() {
                *slot = Some(v.to_owned());
                self.sources.insert(name, Source::HostMetadata);
            }
        };
        let (mut a, mut w, mut c) = (
            self.account_id.take(),
            self.workspace_id.take(),
            self.cloud.take(),
        );
        backfill("account_id", &mut a, &meta.account_id);
        backfill("workspace_id", &mut w, &meta.workspace_id);
        backfill("cloud", &mut c, &meta.cloud);
        (self.account_id, self.workspace_id, self.cloud) = (a, w, c);
        if self.resolved_host_type.is_none() {
            self.resolved_host_type = meta.host_type;
        }
        if self.attr("audience").is_none() {
            let audience = meta
                .token_federation_default_oidc_audiences
                .first()
                .filter(|a| !a.is_empty())
                .cloned()
                .or_else(|| {
                    meta.workspace_id
                        .is_empty()
                        .then(|| self.account_id.clone())
                        .flatten()
                });
            if let Some(a) = audience {
                self.other.insert("audience".to_owned(), a);
                self.sources.insert("audience", Source::HostMetadata);
            }
        }
        if self.discovery_url.is_none() && !meta.oidc_endpoint.is_empty() {
            let mut root = meta.oidc_endpoint.clone();
            if root.contains("{account_id}") {
                let Some(acc) = self.account_id.as_deref() else {
                    tracing::warn!("oidc_endpoint has {{account_id}} but account_id is unset");
                    return;
                };
                root = root.replace("{account_id}", acc);
            }
            self.discovery_url = Some(format!(
                "{}/.well-known/oauth-authorization-server",
                root.trim_end_matches('/')
            ));
            self.sources.insert("discovery_url", Source::HostMetadata);
        }
    }

    /// `/.well-known/databricks-config`, over one shared connection pool,
    /// retrying throttling, 5xx and connection errors a few times.
    async fn fetch_host_metadata(&self, host: &str) -> Result<HostMetadata> {
        const ATTEMPTS: u32 = 3;
        if self.skip_verify {
            tracing::warn!(
                host,
                "TLS certificate verification is disabled (skip_verify); use this only for testing"
            );
        }
        let client = metadata_client(self.skip_verify)?;
        let url = format!("{host}/.well-known/databricks-config");
        let mut attempt = 0;
        loop {
            attempt += 1;
            let result = client.get(&url).timeout(self.http_timeout()).send().await;
            let transient = match &result {
                Ok(r) => r.status().as_u16() == 429 || r.status().is_server_error(),
                Err(e) => e.is_connect() || e.is_timeout(),
            };
            if transient && attempt < ATTEMPTS {
                tokio::time::sleep(crate::http::backoff(attempt)).await;
                continue;
            }
            let resp = result?;
            let status = resp.status();
            let body = resp.bytes().await?;
            if !status.is_success() {
                return Err(crate::ApiError::from_response(
                    status.as_u16(),
                    "GET",
                    "/.well-known",
                    &body,
                )
                .into());
            }
            return serde_json::from_slice(&body).map_err(|e| Error::json("host metadata", e));
        }
    }

    /// The host type: from metadata if known, else inferred from the host
    /// name (`accounts.` / `accounts-dod.` prefixes are account hosts).
    #[must_use]
    pub fn host_type(&self) -> HostType {
        if let Some(t) = self.resolved_host_type {
            return t;
        }
        let host = self.host.as_deref().unwrap_or_default();
        let host = if host.contains("://") {
            host.to_owned()
        } else {
            format!("https://{host}")
        };
        if host.starts_with("https://accounts.") || host.starts_with("https://accounts-dod.") {
            HostType::Account
        } else {
            HostType::Workspace
        }
    }

    /// True if this config targets account-level APIs.
    #[must_use]
    pub fn is_account_client(&self) -> bool {
        self.host_type() == HostType::Account
    }

    /// OAuth scopes to request (`all-apis` when none configured).
    #[must_use]
    pub fn scopes_or_default(&self) -> Vec<String> {
        if self.scopes.is_empty() {
            vec!["all-apis".to_owned()]
        } else {
            self.scopes.clone()
        }
    }

    /// HTTP timeout.
    #[must_use]
    pub fn http_timeout(&self) -> Duration {
        self.http_timeout_seconds
            .filter(|s| *s > 0)
            .map_or(DEFAULT_HTTP_TIMEOUT, Duration::from_secs)
    }

    /// Retry budget; `None` means retry indefinitely.
    #[must_use]
    pub fn retry_timeout(&self) -> Option<Duration> {
        match self.retry_timeout_seconds {
            None | Some(0) => Some(DEFAULT_RETRY_TIMEOUT),
            Some(s) if s < 0 => None,
            Some(s) => Some(Duration::from_secs(s.unsigned_abs())),
        }
    }

    /// Where each attribute came from.
    #[must_use]
    pub fn source_of(&self, attribute: &str) -> Option<&Source> {
        self.sources.get(attribute)
    }

    /// `Config: host=…, token=***. Env: DATABRICKS_HOST, DATABRICKS_TOKEN`
    /// — the same shape Go appends to configuration errors.
    #[must_use]
    pub fn debug_string(&self) -> String {
        let mut used = Vec::new();
        let mut envs = Vec::new();
        for attr in attrs::ATTRIBUTES {
            let Some(v) = (attr.get)(self).filter(|v| !v.is_empty()) else {
                continue;
            };
            let shown = if attr.sensitive { "***".to_owned() } else { v };
            used.push(format!("{}={shown}", attr.name));
            if let Some(Source::Env(var)) = self.sources.get(attr.name) {
                envs.push(*var);
            }
        }
        let mut parts = Vec::new();
        if !used.is_empty() {
            parts.push(format!("Config: {}", used.join(", ")));
        }
        if !envs.is_empty() {
            parts.push(format!("Env: {}", envs.join(", ")));
        }
        parts.join(". ")
    }

    pub(crate) fn wrap(&self, err: Error) -> Error {
        let debug = self.debug_string();
        match err {
            Error::Config(m) if !debug.is_empty() => Error::Config(format!("{m}. {debug}")),
            Error::Auth { auth_type, message } if !debug.is_empty() => Error::Auth {
                auth_type,
                message: format!("{message}. {debug}"),
            },
            other => other,
        }
    }

    /// Set any recognised attribute by its config-file name.
    pub fn set_attribute(&mut self, name: &str, value: impl Into<String>) -> Result<()> {
        let attr = attrs::find(name)
            .ok_or_else(|| Error::Config(format!("unknown attribute {name:?}")))?;
        (attr.set)(self, value.into()).map_err(Error::Config)?;
        self.sources.insert(attr.name, Source::Code);
        Ok(())
    }

    /// Read a recognised attribute by its config-file name. Sensitive ones
    /// (`token`, `client_secret`, `password`, …) come back as `***` so the
    /// value is safe to log; use [`Config::secret_attribute`] for those.
    #[must_use]
    pub fn attribute(&self, name: &str) -> Option<String> {
        let attr = attrs::find(name)?;
        let v = (attr.get)(self)?;
        Some(if attr.sensitive { "***".to_owned() } else { v })
    }

    /// Read a sensitive attribute (or any attribute) as a [`SecretString`],
    /// which never shows its value in `Debug` output.
    #[must_use]
    pub fn secret_attribute(&self, name: &str) -> Option<SecretString> {
        attrs::find(name)
            .and_then(|a| (a.get)(self))
            .map(SecretString::from)
    }

    /// A non-empty attribute value, secrets included (crate use only).
    pub(crate) fn attr(&self, name: &str) -> Option<String> {
        attrs::find(name)
            .and_then(|a| (a.get)(self))
            .filter(|v| !v.is_empty())
    }

    /// Point a copy of an account config at one workspace host (Go:
    /// `Config.NewWithWorkspaceHost`): account-only settings and anything
    /// host metadata derived for the account host are dropped.
    pub(crate) fn for_workspace_host(&mut self, host: &str) {
        self.host = Some(host.to_owned());
        self.account_id = None;
        self.resolved_host_type = Some(HostType::Workspace);
        for name in ["discovery_url", "audience"] {
            if matches!(self.sources.get(name), Some(Source::HostMetadata)) {
                self.sources.remove(name);
                if name == "discovery_url" {
                    self.discovery_url = None;
                } else {
                    self.other.remove(name);
                }
            }
        }
        self.other.remove("azure_workspace_resource_id");
    }

    /// A boolean attribute (`true`, `1`, `yes`, `on`).
    pub(crate) fn attr_bool(&self, name: &str) -> bool {
        self.attr(name)
            .is_some_and(|v| attrs::parse_bool(name, &v).unwrap_or(false))
    }

    /// An ambient environment variable (see [`EnvLookup`]).
    pub(crate) fn getenv(&self, key: &str) -> Option<String> {
        self.env.get(key)
    }

    /// The Databricks environment (cloud, DNS zone, Azure endpoints) for
    /// this host. Go: `Config.Environment`.
    #[must_use]
    pub fn environment(&self) -> Environment {
        let host = self.host.as_deref().unwrap_or_default();
        if host.is_empty() && self.attr("azure_workspace_resource_id").is_some() {
            let name = self
                .attr("azure_environment")
                .unwrap_or_else(|| "PUBLIC".to_owned());
            if let Some(e) = environment::azure_by_name(&name) {
                return e;
            }
        }
        let hostname = Url::parse(host)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_default();
        environment::for_hostname(&hostname)
    }

    fn cloud_override(&self) -> Option<Cloud> {
        self.cloud.as_deref().and_then(Cloud::parse)
    }

    /// Databricks on Azure: a workspace resource ID is set, `cloud` says so,
    /// or the host is an Azure Databricks host.
    #[must_use]
    pub fn is_azure(&self) -> bool {
        if self.attr("azure_workspace_resource_id").is_some() {
            return true;
        }
        self.cloud_override()
            .unwrap_or_else(|| self.environment().cloud)
            == Cloud::Azure
    }

    /// The URL of the workspace called `deployment_name` next to this
    /// account host (Go: `workspaceHost`): `https://{deployment}{dns_zone}`
    /// when the account host is in a known Databricks DNS zone, otherwise
    /// `None`, meaning the account host itself serves the workspace (a
    /// unified host).
    #[must_use]
    pub fn workspace_host(&self, deployment_name: &str) -> Option<String> {
        let env = self.environment();
        let host = self.host.as_deref().unwrap_or_default();
        let hostname = Url::parse(host).ok()?.host_str()?.to_owned();
        (!env.dns_zone.is_empty()
            && hostname.ends_with(env.dns_zone)
            && !deployment_name.is_empty())
        .then(|| format!("https://{deployment_name}{}", env.dns_zone))
    }

    /// Databricks on Google Cloud.
    #[must_use]
    pub fn is_gcp(&self) -> bool {
        self.cloud_override()
            .unwrap_or_else(|| self.environment().cloud)
            == Cloud::Gcp
    }
}
