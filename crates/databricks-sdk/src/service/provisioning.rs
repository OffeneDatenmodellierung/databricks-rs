//! Provisioning: account-level workspaces (Go: `service/provisioning`).

use std::collections::BTreeMap;

use databricks_core::http::Method;
use databricks_core::{ApiClient, Result, open_enum};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Workspaces API (account level).
#[derive(Debug, Clone)]
pub struct WorkspacesApi {
    api: ApiClient,
    account_id: String,
}

impl WorkspacesApi {
    pub(crate) fn new(api: ApiClient, account_id: String) -> Self {
        Self { api, account_id }
    }

    /// All workspaces in the account.
    ///
    /// `GET /api/2.0/accounts/{account_id}/workspaces`
    pub async fn list(&self) -> Result<Vec<Workspace>> {
        let path = format!("/api/2.0/accounts/{}/workspaces", self.account_id);
        self.api.query(Method::GET, &path, &()).await
    }
}

/// A workspace. Unmodelled fields are kept in [`other`](Self::other).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Workspace {
    /// Workspace ID.
    #[serde(default)]
    pub workspace_id: i64,
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_name: Option<String>,
    /// Owning account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// Deployment name (the `<name>` in `<name>.cloud.databricks.com`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment_name: Option<String>,
    /// Cloud (`aws`, `azure`, `gcp`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud: Option<String>,
    /// AWS region.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aws_region: Option<String>,
    /// GCP/Azure location.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// Pricing tier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing_tier: Option<String>,
    /// Creation time (epoch ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creation_time: Option<i64>,
    /// Status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_status: Option<WorkspaceStatus>,
    /// Status detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_status_message: Option<String>,
    /// Tags.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_tags: BTreeMap<String, String>,
    /// Fields not yet modelled.
    #[serde(flatten)]
    pub other: BTreeMap<String, Value>,
}

open_enum! {
    /// Workspace provisioning status.
    pub enum WorkspaceStatus {
        /// `BANNED`
        Banned => "BANNED",
        /// `CANCELLING`
        Cancelling => "CANCELLING",
        /// `FAILED`
        Failed => "FAILED",
        /// `NOT_PROVISIONED`
        NotProvisioned => "NOT_PROVISIONED",
        /// `PROVISIONING`
        Provisioning => "PROVISIONING",
        /// `RUNNING`
        Running => "RUNNING",
    }
}
