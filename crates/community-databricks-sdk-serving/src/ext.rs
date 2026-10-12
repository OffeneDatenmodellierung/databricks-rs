//! `ServingEndpointsDataPlane` from Go's `service/serving` (hand-written
//! there too, so it isn't in the extracted IR).
//!
//! [`ServingEndpointsDataPlaneApi::query`] sends a query straight to the
//! endpoint's data plane instead of through the workspace: it looks up the
//! endpoint's `data_plane_info.query_info` once, exchanges the client's
//! Databricks OAuth token for one scoped to the endpoint, and posts the
//! request to the data-plane URL. Both are cached by the
//! [`ApiClient`] (see `community_databricks_core::dataplane`).

use community_databricks_core::dataplane::DataPlaneTarget;
use community_databricks_core::http::{Call, Method, header};
use community_databricks_core::{ApiClient, Error, Result};

use crate::{
    GetServingEndpointRequest, QueryEndpointInput, QueryEndpointResponse, ServingEndpointsApi,
};

/// Serving endpoints DataPlane provides a set of operations to interact
/// with data plane endpoints for Serving endpoints service.
///
/// Needs Databricks OAuth credentials (for example `oauth-m2m` or
/// `databricks-cli`); a PAT gets an [`Error::Auth`].
#[derive(Debug, Clone)]
pub struct ServingEndpointsDataPlaneApi {
    api: ApiClient,
}

impl ServingEndpointsDataPlaneApi {
    /// Wrap an [`ApiClient`]. Its clones share the data-plane caches, so
    /// creating this per call is cheap.
    #[must_use]
    pub fn new(api: ApiClient) -> Self {
        Self { api }
    }

    /// Query a serving endpoint through its data plane.
    ///
    /// The response's `served_model_name` comes from the
    /// `served-model-name` header, as for the control-plane
    /// [`ServingEndpointsApi::query`].
    ///
    /// `POST <data_plane_info.query_info.endpoint_url>`
    pub async fn query(&self, request: QueryEndpointInput) -> Result<QueryEndpointResponse> {
        let target = self.query_target(&request.name).await?;
        let call = Call::new(Method::POST, String::new())
            .workspace()
            .json(&request)?;
        let (mut resp, headers) = self
            .api
            .send_data_plane_with_headers::<QueryEndpointResponse>(&target, call)
            .await?;
        resp.served_model_name = header(&headers, "served-model-name");
        Ok(resp)
    }

    /// Go: `dataPlaneInfoQuery`, cached per endpoint name.
    async fn query_target(&self, name: &str) -> Result<DataPlaneTarget> {
        let key = format!("Query/{name}");
        self.api
            .data_plane_target(&key, || async {
                let endpoint = ServingEndpointsApi::new(self.api.clone())
                    .get(GetServingEndpointRequest::new(name))
                    .await?;
                let info = endpoint
                    .data_plane_info
                    .and_then(|d| d.query_info)
                    .filter(|q| q.endpoint_url.as_deref().is_some_and(|u| !u.is_empty()))
                    .ok_or_else(|| {
                        Error::Config(format!(
                            "serving endpoint {name:?} has no data-plane query info"
                        ))
                    })?;
                Ok(DataPlaneTarget::new(
                    info.endpoint_url.unwrap_or_default(),
                    info.authorization_details.unwrap_or_default(),
                ))
            })
            .await
    }
}
