//! Generated name lookups (Go's `XNameToIdMap` and list-based `GetByX`).
//!
//! Run with `--all-features` (CI does).

#![cfg(all(feature = "compute", feature = "jobs", feature = "provisioning"))]

use community_databricks_sdk::core::config::HostMetadata;
use community_databricks_sdk::service::compute::ListClustersRequest;
use community_databricks_sdk::service::jobs::ListJobsRequest;
use community_databricks_sdk::{AccountClient, Config, ErrorKind, WorkspaceClient};
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg(server: &MockServer) -> Config {
    let mut c = Config::with_host(server.uri()).token("dapi");
    c.host_metadata = Some(HostMetadata::default());
    c.rate_limit = Some(1000);
    c
}

async fn workspace(server: &MockServer) -> WorkspaceClient {
    WorkspaceClient::new(cfg(server)).await.unwrap()
}

#[tokio::test]
async fn name_maps_follow_pages_and_reject_duplicates() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.1/clusters/list"))
        .and(query_param("page_token", "p2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "clusters": [{"cluster_id": "c2", "cluster_name": "etl"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.1/clusters/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "clusters": [{"cluster_id": "c1", "cluster_name": "adhoc"}],
            "next_page_token": "p2"
        })))
        .mount(&server)
        .await;
    let clusters = workspace(&server).await.clusters();
    let map = clusters
        .cluster_details_cluster_name_to_cluster_id_map(ListClustersRequest::default())
        .await
        .unwrap();
    assert_eq!(map["adhoc"], "c1");
    assert_eq!(map["etl"], "c2");
    let etl = clusters.get_by_cluster_name("etl").await.unwrap();
    assert_eq!(etl.cluster_id.as_deref(), Some("c2"));
    let e = clusters.get_by_cluster_name("nope").await.unwrap_err();
    assert!(e.is_missing(), "{e}");
    assert!(
        e.to_string()
            .contains("ClusterDetails named 'nope' does not exist"),
        "{e}"
    );
}

#[tokio::test]
async fn nested_keys_and_integer_values() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.2/jobs/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jobs": [
                {"job_id": 1, "settings": {"name": "nightly"}},
                {"job_id": 2, "settings": {"name": "nightly"}},
                {"job_id": 3}
            ]
        })))
        .mount(&server)
        .await;
    let jobs = workspace(&server).await.jobs();
    // Two jobs share a name: the map refuses, and so does get_by.
    let e = jobs
        .base_job_settings_name_to_job_id_map(ListJobsRequest::default())
        .await
        .unwrap_err();
    assert!(e.is(ErrorKind::InvalidState), "{e}");
    assert!(
        e.to_string().contains("duplicate settings.name: nightly"),
        "{e}"
    );
    let e = jobs.get_by_settings_name("nightly").await.unwrap_err();
    assert!(
        e.to_string()
            .contains("there are 2 instances of BaseJob named 'nightly'"),
        "{e}"
    );
    // A job without settings has the empty name, as in Go.
    assert_eq!(jobs.get_by_settings_name("").await.unwrap().job_id, Some(3));
}

#[tokio::test]
async fn plain_list_lookups_on_the_account() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/accounts/acc/workspaces"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"workspace_id": 11, "workspace_name": "prod"},
            {"workspace_id": 12, "workspace_name": "dev"}
        ])))
        .mount(&server)
        .await;
    let a = AccountClient::new(cfg(&server).account("acc"))
        .await
        .unwrap();
    let ws = a.workspaces();
    let map = ws
        .workspace_workspace_name_to_workspace_id_map()
        .await
        .unwrap();
    assert_eq!(map["prod"], 11);
    assert_eq!(
        ws.get_by_workspace_name("dev").await.unwrap().workspace_id,
        Some(12)
    );
}
