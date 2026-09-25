//! Compute: clusters (Go: `service/compute`).

use std::collections::BTreeMap;

use databricks_core::http::Method;
use databricks_core::paging::{self, Paged};
use databricks_core::{ApiClient, Result, open_enum};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Clusters API.
#[derive(Debug, Clone)]
pub struct ClustersApi {
    api: ApiClient,
}

impl ClustersApi {
    pub(crate) fn new(api: ApiClient) -> Self {
        Self { api }
    }

    /// Pinned and active clusters, plus clusters terminated in the last 30
    /// days, as a lazily paginated stream.
    ///
    /// `GET /api/2.1/clusters/list`
    #[must_use]
    pub fn list(&self, request: ListClustersRequest) -> Paged<'static, ClusterDetails> {
        let api = self.api.clone();
        paging::paginate(
            request,
            move |req: &ListClustersRequest| {
                let api = api.clone();
                let req = req.clone();
                async move {
                    api.query::<_, ListClustersResponse>(
                        Method::GET,
                        "/api/2.1/clusters/list",
                        &req,
                    )
                    .await
                }
            },
            |resp| (resp.clusters, resp.next_page_token),
            |req, token| req.page_token = Some(token),
        )
    }

    /// Every page of [`list`](Self::list), collected.
    pub async fn list_all(&self, request: ListClustersRequest) -> Result<Vec<ClusterDetails>> {
        paging::collect(self.list(request)).await
    }
}

/// Request for [`ClustersApi::list`].
#[derive(Debug, Clone, Default, Serialize, bon::Builder)]
#[builder(on(String, into))]
#[non_exhaustive]
pub struct ListClustersRequest {
    /// Filters to apply.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter_by: Option<ListClustersFilterBy>,
    /// Maximum results per page (the server may return fewer).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_size: Option<i32>,
    /// Page token from a previous response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,
    /// Sort order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_by: Option<ListClustersSortBy>,
}

/// Filters for [`ListClustersRequest`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, bon::Builder)]
#[builder(on(String, into))]
#[non_exhaustive]
pub struct ListClustersFilterBy {
    /// Creation sources to include.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[builder(default)]
    pub cluster_sources: Vec<ClusterSource>,
    /// States to include.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[builder(default)]
    pub cluster_states: Vec<State>,
    /// Only pinned (or unpinned) clusters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_pinned: Option<bool>,
    /// Only clusters created with this policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_id: Option<String>,
}

/// Sort order for [`ListClustersRequest`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, bon::Builder)]
#[non_exhaustive]
pub struct ListClustersSortBy {
    /// Direction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<ListClustersSortByDirection>,
    /// Field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<ListClustersSortByField>,
}

/// Response for `clusters/list`.
#[derive(Debug, Clone, Default, Deserialize)]
#[non_exhaustive]
pub struct ListClustersResponse {
    /// Clusters on this page.
    #[serde(default)]
    pub clusters: Vec<ClusterDetails>,
    /// Token for the next page; empty when done.
    #[serde(default)]
    pub next_page_token: Option<String>,
    /// Token for the previous page.
    #[serde(default)]
    pub prev_page_token: Option<String>,
}

/// A cluster. The spike models the commonly used fields; everything else
/// is kept in [`other`](Self::other) until the generator emits the full type.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ClusterDetails {
    /// Autoscaling bounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autoscale: Option<AutoScale>,
    /// Minutes of inactivity before termination (0 = never).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autotermination_minutes: Option<i32>,
    /// Canonical cluster ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_id: Option<String>,
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_name: Option<String>,
    /// What created the cluster.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_source: Option<ClusterSource>,
    /// Creator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator_user_name: Option<String>,
    /// User tags.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_tags: BTreeMap<String, String>,
    /// Data governance mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_security_mode: Option<DataSecurityMode>,
    /// Tags added by Databricks.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub default_tags: BTreeMap<String, String>,
    /// Driver node type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver_node_type_id: Option<String>,
    /// Instance pool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_pool_id: Option<String>,
    /// Single-node cluster.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_single_node: Option<bool>,
    /// Worker node type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_type_id: Option<String>,
    /// Fixed worker count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num_workers: Option<i32>,
    /// Cluster policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_id: Option<String>,
    /// Dedicated user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub single_user_name: Option<String>,
    /// Spark config.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub spark_conf: BTreeMap<String, String>,
    /// Runtime version key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spark_version: Option<String>,
    /// Start time (epoch ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<i64>,
    /// Current state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<State>,
    /// Explanation of the current state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_message: Option<String>,
    /// Termination time (epoch ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminated_time: Option<i64>,
    /// Why the cluster terminated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub termination_reason: Option<TerminationReason>,
    /// Fields not yet modelled.
    #[serde(flatten)]
    pub other: BTreeMap<String, Value>,
}

/// Autoscaling bounds.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AutoScale {
    /// Maximum workers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_workers: Option<i32>,
    /// Minimum workers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_workers: Option<i32>,
}

/// Why a cluster terminated.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TerminationReason {
    /// Reason code (over 100 values; typed by the generator later).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Extra context.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, String>,
    /// `SUCCESS`, `CLIENT_ERROR`, `SERVICE_FAULT` or `CLOUD_FAILURE`.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
}

open_enum! {
    /// Cluster state.
    pub enum State {
        /// `ERROR`
        Error => "ERROR",
        /// `PENDING`
        Pending => "PENDING",
        /// `RESIZING`
        Resizing => "RESIZING",
        /// `RESTARTING`
        Restarting => "RESTARTING",
        /// `RUNNING`
        Running => "RUNNING",
        /// `TERMINATED`
        Terminated => "TERMINATED",
        /// `TERMINATING`
        Terminating => "TERMINATING",
        /// `UNKNOWN`
        StateUnknown => "UNKNOWN",
    }
}

open_enum! {
    /// What created a cluster.
    pub enum ClusterSource {
        /// `API`
        Api => "API",
        /// `JOB`
        Job => "JOB",
        /// `MODELS`
        Models => "MODELS",
        /// `PIPELINE`
        Pipeline => "PIPELINE",
        /// `PIPELINE_MAINTENANCE`
        PipelineMaintenance => "PIPELINE_MAINTENANCE",
        /// `SQL`
        Sql => "SQL",
        /// `UI`
        Ui => "UI",
    }
}

open_enum! {
    /// Data governance model.
    pub enum DataSecurityMode {
        /// `DATA_SECURITY_MODE_AUTO`
        DataSecurityModeAuto => "DATA_SECURITY_MODE_AUTO",
        /// `DATA_SECURITY_MODE_DEDICATED`
        DataSecurityModeDedicated => "DATA_SECURITY_MODE_DEDICATED",
        /// `DATA_SECURITY_MODE_STANDARD`
        DataSecurityModeStandard => "DATA_SECURITY_MODE_STANDARD",
        /// `LEGACY_PASSTHROUGH`
        LegacyPassthrough => "LEGACY_PASSTHROUGH",
        /// `LEGACY_SINGLE_USER`
        LegacySingleUser => "LEGACY_SINGLE_USER",
        /// `LEGACY_SINGLE_USER_STANDARD`
        LegacySingleUserStandard => "LEGACY_SINGLE_USER_STANDARD",
        /// `LEGACY_TABLE_ACL`
        LegacyTableAcl => "LEGACY_TABLE_ACL",
        /// `NONE`
        None => "NONE",
        /// `SINGLE_USER`
        SingleUser => "SINGLE_USER",
        /// `USER_ISOLATION`
        UserIsolation => "USER_ISOLATION",
    }
}

open_enum! {
    /// Sort direction.
    pub enum ListClustersSortByDirection {
        /// `ASC`
        Asc => "ASC",
        /// `DESC`
        Desc => "DESC",
    }
}

open_enum! {
    /// Sort field.
    pub enum ListClustersSortByField {
        /// `CLUSTER_NAME`
        ClusterName => "CLUSTER_NAME",
        /// `DEFAULT`
        Default => "DEFAULT",
    }
}
