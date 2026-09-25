//! Unofficial Rust SDK for the Databricks REST APIs.
//!
//! **Milestone 1 spike.** The service code here is hand-written to settle
//! the shape the code generator will emit; only a few operations exist:
//!
//! | Client | Service | Operations |
//! |---|---|---|
//! | [`WorkspaceClient`] | [`clusters`](WorkspaceClient::clusters) | `list` (paginated stream), `list_all` |
//! | [`WorkspaceClient`] | [`jobs`](WorkspaceClient::jobs) | `run_now` (with waiter), `get_run`, `wait_get_run_job_terminated_or_skipped` |
//! | [`AccountClient`] | [`workspaces`](AccountClient::workspaces) | `list` |
//!
//! ```no_run
//! use databricks_sdk::WorkspaceClient;
//! use databricks_sdk::service::compute::ListClustersRequest;
//! use futures_util::TryStreamExt;
//!
//! # async fn run() -> databricks_sdk::Result<()> {
//! // Host and credentials from DATABRICKS_* env vars or ~/.databrickscfg.
//! let w = WorkspaceClient::from_env().await?;
//! let mut clusters = w.clusters().list(ListClustersRequest::default());
//! while let Some(c) = clusters.try_next().await? {
//!     println!("{} {:?}", c.cluster_name.unwrap_or_default(), c.state);
//! }
//! # Ok(()) }
//! ```

pub mod service;

use databricks_core::auth::DefaultCredentials;
pub use databricks_core::{self as core, ApiClient, ApiError, Config, Error, ErrorKind, Result};

/// Client for workspace-level APIs.
#[derive(Debug, Clone)]
pub struct WorkspaceClient {
    api: ApiClient,
}

impl WorkspaceClient {
    /// Configure entirely from the environment / `~/.databrickscfg`.
    pub async fn from_env() -> Result<Self> {
        Self::new(Config::default()).await
    }

    /// Build from a config; unset values are filled from the environment.
    pub async fn new(cfg: Config) -> Result<Self> {
        Ok(Self::from_api_client(ApiClient::new(cfg).await?))
    }

    /// Build with a custom credential chain.
    pub async fn with_credentials(cfg: Config, credentials: DefaultCredentials) -> Result<Self> {
        Ok(Self::from_api_client(
            ApiClient::with_credentials(cfg, credentials).await?,
        ))
    }

    /// Wrap an existing [`ApiClient`].
    #[must_use]
    pub fn from_api_client(api: ApiClient) -> Self {
        Self { api }
    }

    /// The underlying client, for calling endpoints the SDK doesn't cover.
    #[must_use]
    pub fn api_client(&self) -> &ApiClient {
        &self.api
    }

    /// The resolved configuration.
    #[must_use]
    pub fn config(&self) -> &Config {
        self.api.config()
    }

    /// Clusters API.
    #[cfg(feature = "compute")]
    #[must_use]
    pub fn clusters(&self) -> service::compute::ClustersApi {
        service::compute::ClustersApi::new(self.api.clone())
    }

    /// Jobs API.
    #[cfg(feature = "jobs")]
    #[must_use]
    pub fn jobs(&self) -> service::jobs::JobsApi {
        service::jobs::JobsApi::new(self.api.clone())
    }
}

/// Client for account-level APIs.
#[derive(Debug, Clone)]
pub struct AccountClient {
    api: ApiClient,
    account_id: String,
}

impl AccountClient {
    /// Configure entirely from the environment / `~/.databrickscfg`.
    pub async fn from_env() -> Result<Self> {
        Self::new(Config::default()).await
    }

    /// Build from a config; `account_id` must resolve to a value.
    pub async fn new(cfg: Config) -> Result<Self> {
        Self::from_api_client(ApiClient::new(cfg).await?)
    }

    /// Build with a custom credential chain.
    pub async fn with_credentials(cfg: Config, credentials: DefaultCredentials) -> Result<Self> {
        Self::from_api_client(ApiClient::with_credentials(cfg, credentials).await?)
    }

    /// Wrap an existing [`ApiClient`].
    pub fn from_api_client(api: ApiClient) -> Result<Self> {
        let account_id = api
            .config()
            .account_id
            .clone()
            .filter(|a| !a.is_empty())
            .ok_or_else(|| {
                Error::Config(
                    "invalid Databricks Account configuration - host incorrect or account_id missing"
                        .into(),
                )
            })?;
        Ok(Self { api, account_id })
    }

    /// The underlying client.
    #[must_use]
    pub fn api_client(&self) -> &ApiClient {
        &self.api
    }

    /// The account ID requests are made against.
    #[must_use]
    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    /// Workspaces API.
    #[cfg(feature = "provisioning")]
    #[must_use]
    pub fn workspaces(&self) -> service::provisioning::WorkspacesApi {
        service::provisioning::WorkspacesApi::new(self.api.clone(), self.account_id.clone())
    }
}
