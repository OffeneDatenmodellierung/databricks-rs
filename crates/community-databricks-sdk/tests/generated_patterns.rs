//! One test per generated code shape, so a generator change that breaks a
//! pattern fails here rather than against a live workspace.
//!
//! Run with `--all-features` (CI does).

#![cfg(all(
    feature = "agentbricks",
    feature = "catalog",
    feature = "compute",
    feature = "files",
    feature = "iam",
    feature = "jobs",
    feature = "ml",
    feature = "provisioning",
    feature = "sql",
    feature = "tags"
))]

use std::time::Duration;

use community_databricks_sdk::core::config::HostMetadata;
use community_databricks_sdk::service::agentbricks::CancelCustomLlmOptimizationRunRequest;
use community_databricks_sdk::service::catalog::{
    GetSchemaRequest, McpService, UpdateMcpServiceRequest,
};
use community_databricks_sdk::service::compute::{
    ClusterDetails, CreateCluster, StartCluster, State,
};
use community_databricks_sdk::service::files::{GetDirectoryMetadataRequest, GetMetadataRequest};
use community_databricks_sdk::service::iam::ListUsersRequest;
use community_databricks_sdk::service::jobs::RunNow;
use community_databricks_sdk::service::ml::Metric;
use community_databricks_sdk::service::provisioning::{GetWorkspaceRequest, Workspace};
use community_databricks_sdk::service::sql::ListDashboardsRequest;
use community_databricks_sdk::service::tags::{
    GetTagPolicyRequest, ListTagPoliciesRequest, UpdateTagPolicyRequest,
};
use community_databricks_sdk::{AccountClient, Config, WorkspaceClient};
use serde_json::{Value, json};
use wiremock::matchers::{body_json, header, method, path, query_param};
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
        // As Go: SCIM starts at 1 and defaults `count` to 10000.
        .and(query_param("startIndex", "1"))
        .and(query_param("count", "10000"))
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
        .and(query_param("startIndex", "1"))
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
        .and(query_param("page", "1"))
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
async fn unmodelled_fields_of_a_single_field_body_request() {
    // The request's own `other` sits beside its path and query fields, so
    // it goes in the query string; the body field's `other` goes in the
    // body.
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/2.1/unity-catalog/mcp-services/svc"))
        .and(query_param("update_mask", "comment"))
        .and(query_param("dry_run", "true"))
        .and(body_json(json!({"comment": "hi", "future_body_field": 1})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"name": "mcp-services/svc"})))
        .expect(1)
        .mount(&server)
        .await;
    let req = UpdateMcpServiceRequest::default()
        .with_name("mcp-services/svc")
        .with_update_mask("comment")
        .with_other("dry_run", true)
        .with_mcp_service(
            McpService::default()
                .with_comment("hi")
                .with_other("future_body_field", 1),
        );
    workspace(&server)
        .await
        .ai_gateway()
        .update_mcp_service(req)
        .await
        .unwrap();
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
async fn single_segment_path_escapes_slashes() {
    // databricks-sdk-go#1765: a name that is one path segment must not
    // turn a `/` into a new segment.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.1/unity-catalog/schemas/main.a%2Fb"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"name": "a/b"})))
        .expect(1)
        .mount(&server)
        .await;
    workspace(&server)
        .await
        .schemas()
        .get(GetSchemaRequest::new("main.a/b"))
        .await
        .unwrap();
}

#[test]
fn int64_fields_accept_numeric_strings_and_null() {
    // databricks-sdk-go#1808: proto3 JSON encodes int64 as a string.
    let c: ClusterDetails = serde_json::from_value(json!({
        "num_workers": "4",
        "spark_context_id": "9007199254740993",
        "autotermination_minutes": null,
    }))
    .unwrap();
    assert_eq!(c.num_workers, Some(4));
    assert_eq!(c.spark_context_id, Some(9_007_199_254_740_993));
    assert_eq!(c.autotermination_minutes, None);
    // Serialisation stays numeric.
    assert_eq!(serde_json::to_value(&c).unwrap()["num_workers"], json!(4));
}

#[test]
fn float_fields_accept_non_finite_strings() {
    // databricks-sdk-go#1498: MLflow returns metric values as "NaN".
    let m: Metric = serde_json::from_value(json!({"key": "loss", "value": "NaN"})).unwrap();
    assert!(m.value.is_some_and(f64::is_nan));
    let m: Metric = serde_json::from_value(json!({"key": "loss", "value": 0.5})).unwrap();
    assert_eq!(m.value, Some(0.5));
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

#[tokio::test]
async fn bodyless_post_sends_empty_json_object_like_go() {
    // databricks-sdk-go marshals the request struct for every POST/PUT/PATCH,
    // so a request with only path fields is sent as `{}` with a JSON
    // content type. Match it rather than sending no body.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/2.0/custom-llms/llm-1/optimize/cancel"))
        .and(wiremock::matchers::header(
            "content-type",
            "application/json",
        ))
        .and(body_json(json!({})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    workspace(&server)
        .await
        .agent_bricks()
        .cancel_optimize(CancelCustomLlmOptimizationRunRequest::new("llm-1"))
        .await
        .unwrap();
}

#[tokio::test]
async fn unknown_fields_survive_read_modify_write() {
    // #11: a field this SDK version doesn't model, at the top level and
    // nested, must come back unchanged when the object is sent back.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.1/tag-policies/env"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tag_key": "env",
            "values": [{"name": "prod", "colour": "red"}],
            "future_field": {"a": 1},
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/api/2.1/tag-policies/env"))
        .and(query_param("update_mask", "description"))
        .and(body_json(json!({
            "tag_key": "env",
            "description": "deployment stage",
            "values": [{"name": "prod", "colour": "red"}],
            "future_field": {"a": 1},
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"tag_key": "env"})))
        .expect(1)
        .mount(&server)
        .await;
    let w = workspace(&server).await;
    let policy = w
        .tag_policies()
        .get_tag_policy(GetTagPolicyRequest::new("env"))
        .await
        .unwrap();
    assert_eq!(policy.other["future_field"], json!({"a": 1}));
    assert_eq!(policy.values[0].other["colour"], json!("red"));
    w.tag_policies()
        .update_tag_policy(
            UpdateTagPolicyRequest::default()
                .with_tag_key("env")
                .with_update_mask("description")
                .with_tag_policy(policy.with_description("deployment stage")),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn unmodelled_query_parameters_are_sent() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.1/tag-policies"))
        .and(query_param("page_size", "10"))
        .and(query_param("new_filter", "x"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"tag_policies": [{"tag_key": "a"}]})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let all = workspace(&server)
        .await
        .tag_policies()
        .list_tag_policies_all(
            ListTagPoliciesRequest::default()
                .with_page_size(10)
                .with_other("new_filter", "x"),
        )
        .await
        .unwrap();
    assert_eq!(all.len(), 1);
}

#[tokio::test]
async fn run_now_retries_with_the_same_generated_idempotency_token() {
    // #3: run_now is a POST; the SDK fills in an idempotency token so a
    // retry after a 503 can't start a second run.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/2.2/jobs/run-now"))
        .respond_with(
            ResponseTemplate::new(503)
                .set_body_json(json!({"error_code": "TEMPORARILY_UNAVAILABLE", "message": "busy"})),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/2.2/jobs/run-now"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"run_id": 5})))
        .mount(&server)
        .await;
    let waiter = workspace(&server)
        .await
        .jobs()
        .run_now(RunNow::new(9))
        .await
        .unwrap();
    assert_eq!(waiter.response.run_id, Some(5));
    let reqs = server.received_requests().await.unwrap();
    let tokens: Vec<Value> = reqs
        .iter()
        .map(|r| serde_json::from_slice::<Value>(&r.body).unwrap()["idempotency_token"].clone())
        .collect();
    assert_eq!(tokens.len(), 2);
    assert_eq!(tokens[0], tokens[1]);
    assert_eq!(tokens[0].as_str().map(str::len), Some(36));

    // A caller-supplied token is kept.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/2.2/jobs/run-now"))
        .and(body_json(json!({"job_id": 9, "idempotency_token": "mine"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"run_id": 6})))
        .expect(1)
        .mount(&server)
        .await;
    workspace(&server)
        .await
        .jobs()
        .run_now(RunNow::new(9).with_idempotency_token("mine"))
        .await
        .unwrap();
}

#[tokio::test]
async fn account_client_derives_a_workspace_client() {
    // #4. The mock is not in a Databricks DNS zone, so it is treated as a
    // unified host: the workspace client uses the same host and sends the
    // workspace ID header.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/accounts/acc/workspaces/42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "workspace_id": 42, "deployment_name": "dbc-42", "workspace_name": "ws",
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.1/clusters/list"))
        .and(header("x-databricks-workspace-id", "42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"clusters": []})))
        .expect(1)
        .mount(&server)
        .await;
    let mut c = cfg(&server).account("acc");
    c.host_metadata = Some(serde_json::from_value(json!({"host_type": "UNIFIED_HOST"})).unwrap());
    let a = AccountClient::new(c).await.unwrap();
    let ws = a
        .workspaces()
        .get(GetWorkspaceRequest::new(42))
        .await
        .unwrap();
    let w = a.get_workspace_client(&ws).unwrap();
    let clusters = w
        .clusters()
        .list_all(community_databricks_sdk::service::compute::ListClustersRequest::default())
        .await
        .unwrap();
    assert!(clusters.is_empty());
    assert!(a.get_workspace_client(&Workspace::default()).is_err());

    // An Azure workspace carries its ARM resource ID to the derived client.
    let azure: Workspace = serde_json::from_value(json!({
        "workspace_id": 42, "workspace_name": "ws",
        "azure_workspace_info": {"subscription_id": "sub", "resource_group": "rg"},
    }))
    .unwrap();
    let w = a.get_workspace_client(&azure).unwrap();
    assert_eq!(
        w.config()
            .attribute("azure_workspace_resource_id")
            .as_deref(),
        Some("/subscriptions/sub/resourceGroups/rg/providers/Microsoft.Databricks/workspaces/ws")
    );
}
