//! List running clusters in a workspace.
//!
//! ```sh
//! DATABRICKS_HOST=https://<workspace> DATABRICKS_TOKEN=dapi... \
//!   cargo run --example list_clusters
//! ```

use databricks_sdk::WorkspaceClient;
use databricks_sdk::service::compute::{ListClustersFilterBy, ListClustersRequest, State};
use futures_util::TryStreamExt;

#[tokio::main]
async fn main() -> databricks_sdk::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let w = WorkspaceClient::from_env().await?;
    let request = ListClustersRequest::default()
        .with_page_size(50)
        .with_filter_by(
            ListClustersFilterBy::default().with_cluster_states([State::Running, State::Pending]),
        );

    let mut clusters = w.clusters().list(request);
    while let Some(c) = clusters.try_next().await? {
        println!(
            "{:<24} {:<12} {}",
            c.cluster_id.unwrap_or_default(),
            c.state.map(|s| s.to_string()).unwrap_or_default(),
            c.cluster_name.unwrap_or_default(),
        );
    }
    println!("auth: {}", w.api_client().auth_type().unwrap_or("-"));
    Ok(())
}
