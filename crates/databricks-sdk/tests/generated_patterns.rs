//! One test per generated code shape, so a generator change that breaks a
//! pattern fails here rather than against a live workspace.
//!
//! Run with `--all-features` (CI does).

#![cfg(all(
    feature = "catalog",
    feature = "compute",
    feature = "files",
    feature = "iam",
    feature = "provisioning",
    feature = "sql"
))]

use std::time::Duration;

use databricks_sdk::core::config::HostMetadata;
use databricks_sdk::service::catalog::{McpService, UpdateMcpServiceRequest};
use databricks_sdk::service::compute::{CreateCluster, StartCluster, State};
use databricks_sdk::service::files::{GetDirectoryMetadataRequest, GetMetadataRequest};
use databricks_sdk::service::iam::ListUsersRequest;
use databricks_sdk::service::provisioning::GetWorkspaceRequest;
use databricks_sdk::service::sql::ListDashboardsRequest;
use databricks_sdk::{AccountClient, Config, WorkspaceClient};
use serde_json::json;
use wiremock::matchers::{body_json, method, path, query_param, query_param_is_missing};
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
async fn offset_pagination_scim() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/preview/scim/v2/Users"))
        .and(query_param_is_missing("startIndex"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "Resources": [{"id": "1", "userName": "a@x"}, {"id": "2", "userName": "b@x"}],
            "startIndex": 1, "itemsPerPage": 2, "totalResults": 3
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/preview/scim/v2/Users"))
        .and(query_param("startIndex", "3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "Resources": [{"id": "3", "userName": "c@x"}], "startIndex": 3
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/preview/scim/v2/Users"))
        .and(query_param("startIndex", "4"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"startIndex": 4})))
        .mount(&server)
        .await;
    let users = workspace(&server)
        .await
        .users_v2()
        .list_all(ListUsersRequest::default().with_filter("active eq true"))
        .await
        .unwrap();
    let names: Vec<_> = users
        .iter()
        .filter_map(|u| u.user_name.as_deref())
        .collect();
    assert_eq!(names, ["a@x", "b@x", "c@x"]);
    let first = &server.received_requests().await.unwrap()[0];
    assert!(first.url.query().unwrap().contains("filter=active+eq+true"));
}

#[tokio::test]
async fn offset_pagination_without_response_cursor() {
    // A page that omits `startIndex` must continue after the page, not
    // restart from zero.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/preview/scim/v2/Users"))
        .and(query_param_is_missing("startIndex"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "Resources": [{"id": "1"}, {"id": "2"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/preview/scim/v2/Users"))
        .and(query_param("startIndex", "3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"Resources": [{"id": "3"}]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/preview/scim/v2/Users"))
        .and(query_param("startIndex", "4"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let users = workspace(&server)
        .await
        .users_v2()
        .list_all(ListUsersRequest::default())
        .await
        .unwrap();
    let ids: Vec<_> = users.iter().filter_map(|u| u.id.as_deref()).collect();
    assert_eq!(ids, ["1", "2", "3"]);
}

#[tokio::test]
async fn response_headers_populate_header_fields() {
    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .and(path("/api/2.0/fs/files/Volumes/c/s/v/f.csv"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/csv")
                .insert_header("last-modified", "Wed, 24 Sep 2026 10:00:00 GMT"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let meta = workspace(&server)
        .await
        .files()
        .get_metadata(GetMetadataRequest::new("/Volumes/c/s/v/f.csv"))
        .await
        .unwrap();
    assert_eq!(meta.content_type.as_deref(), Some("text/csv"));
    assert_eq!(
        meta.last_modified.as_deref(),
        Some("Wed, 24 Sep 2026 10:00:00 GMT")
    );
}

#[tokio::test]
async fn page_number_pagination() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/preview/sql/dashboards"))
        .and(query_param_is_missing("page"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "page": 1, "results": [{"id": "d1"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/preview/sql/dashboards"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"page": 2, "results": []})))
        .mount(&server)
        .await;
    let d = workspace(&server)
        .await
        .dashboards()
        .list_all(ListDashboardsRequest::default())
        .await
        .unwrap();
    assert_eq!(d.len(), 1);
}

#[tokio::test]
async fn explicit_query_field_mask_and_sub_field_body() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/2.1/unity-catalog/mcp-services/svc"))
        .and(query_param("etag", "e1"))
        .and(query_param("update_mask", "comment,config"))
        .and(body_json(json!({"comment": "hi"})))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"name": "mcp-services/svc", "comment": "hi"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut req = UpdateMcpServiceRequest::default()
        .with_name("mcp-services/svc")
        .with_update_mask("comment,config")
        .with_etag("e1");
    req.mcp_service = McpService::default().with_comment("hi");
    let out = workspace(&server)
        .await
        .ai_gateway()
        .update_mcp_service(req)
        .await
        .unwrap();
    assert_eq!(out.comment.as_deref(), Some("hi"));
}

#[tokio::test]
async fn multi_segment_path_is_escaped_per_segment() {
    let server = MockServer::start().await;
    Mock::given(method("HEAD"))
        .and(path("/api/2.0/fs/directories/Volumes/main/my%20vol/a%23b"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    workspace(&server)
        .await
        .files()
        .get_directory_metadata(GetDirectoryMetadataRequest::new("/Volumes/main/my vol/a#b"))
        .await
        .unwrap();
}

#[tokio::test]
async fn waiters_bind_from_response_and_from_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/2.1/clusters/create"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"cluster_id": "c1"})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/2.1/clusters/start"))
        .and(body_json(json!({"cluster_id": "c2"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.1/clusters/get"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"cluster_id": "cX", "state": "RUNNING"})),
        )
        .mount(&server)
        .await;
    let w = workspace(&server).await;
    let created = w
        .clusters()
        .create(CreateCluster::new("15.4.x-scala2.12"))
        .await
        .unwrap();
    assert_eq!(created.cluster_id, "c1");
    let running = created
        .timeout(Duration::from_secs(30))
        .wait()
        .await
        .unwrap();
    assert_eq!(running.state, Some(State::Running));

    let started = w.clusters().start(StartCluster::new("c2")).await.unwrap();
    assert_eq!(started.cluster_id, "c2");
    started.wait().await.unwrap();
    let gets = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path() == "/api/2.1/clusters/get")
        .map(|r| r.url.query().unwrap_or_default().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(gets, ["cluster_id=c1", "cluster_id=c2"]);
}

#[tokio::test]
async fn account_paths_and_no_workspace_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/accounts/acc-1/workspaces/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"workspace_id": 42})))
        .expect(1)
        .mount(&server)
        .await;
    let mut c = cfg(&server).account("acc-1");
    c.workspace_id = Some("999".into());
    let a = AccountClient::new(c).await.unwrap();
    let ws = a
        .workspaces()
        .get(GetWorkspaceRequest::new(42))
        .await
        .unwrap();
    assert_eq!(ws.workspace_id, Some(42));
    let req = &server.received_requests().await.unwrap()[0];
    assert!(req.headers.get("x-databricks-workspace-id").is_none());
}
