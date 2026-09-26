//! Go's SCIM helpers on the V2 services, the v1 accessor names, and
//! `current_workspace_id` (#14).
//!
//! Run with `--all-features` (CI does).

#![cfg(feature = "iam")]

use community_databricks_sdk::core::config::HostMetadata;
use community_databricks_sdk::service::iam::{
    ListAccountGroupsRequest, ListAccountServicePrincipalsRequest, ListAccountUsersRequest,
    ListGroupsRequest, ListServicePrincipalsRequest, ListUsersRequest,
};
use community_databricks_sdk::{AccountClient, Config, WorkspaceClient};
use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
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
async fn by_id_helpers_and_v1_names() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/preview/scim/v2/Users/42"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id": "42", "userName": "a@x"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/2.0/preview/scim/v2/ServicePrincipals/7"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let w = workspace(&server).await;
    let u = w.users().get_by_id("42").await.unwrap();
    assert_eq!(u.user_name.as_deref(), Some("a@x"));
    w.service_principals().delete_by_id("7").await.unwrap();
}

#[tokio::test]
async fn name_lookups_list_every_scim_page() {
    let server = MockServer::start().await;
    for (start, items) in [
        (1, json!([{"id": "1", "userName": "a@x"}])),
        (2, json!([{"id": "2", "userName": "b@x"}])),
        (3, json!([])),
    ] {
        Mock::given(method("GET"))
            .and(path("/api/2.0/preview/scim/v2/Users"))
            .and(query_param("startIndex", start.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"Resources": items})))
            .mount(&server)
            .await;
    }
    let users = workspace(&server).await.users();
    let map = users
        .user_user_name_to_id_map(ListUsersRequest::default())
        .await
        .unwrap();
    assert_eq!(map["a@x"], "1");
    assert_eq!(map["b@x"], "2");
    assert_eq!(
        users.get_by_user_name("b@x").await.unwrap().id.as_deref(),
        Some("2")
    );
    assert!(
        users
            .get_by_user_name("c@x")
            .await
            .unwrap_err()
            .is_missing()
    );
}

#[tokio::test]
async fn account_and_workspace_groups_by_display_name() {
    let server = MockServer::start().await;
    let page = |items: serde_json::Value| {
        ResponseTemplate::new(200).set_body_json(json!({"Resources": items}))
    };
    for base in ["/api/2.0/accounts/acc/scim/v2", "/api/2.0/preview/scim/v2"] {
        for p in ["Groups", "ServicePrincipals"] {
            Mock::given(method("GET"))
                .and(path(format!("{base}/{p}")))
                .and(query_param("startIndex", "1"))
                .respond_with(page(json!([
                    {"id": "g1", "displayName": "admins"},
                    {"id": "g2", "displayName": "admins"}
                ])))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path(format!("{base}/{p}")))
                .and(query_param("startIndex", "3"))
                .respond_with(page(json!([])))
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path(format!("{base}/Users")))
            .respond_with(page(json!([])))
            .mount(&server)
            .await;
    }
    let a = AccountClient::new(cfg(&server).account("acc"))
        .await
        .unwrap();
    let e = a.groups().get_by_display_name("admins").await.unwrap_err();
    assert!(
        e.to_string()
            .contains("there are 2 instances of AccountGroup named 'admins'"),
        "{e}"
    );
    let e = a
        .groups()
        .group_display_name_to_id_map(ListAccountGroupsRequest::default())
        .await
        .unwrap_err();
    assert!(
        e.to_string().contains("duplicate display_name: admins"),
        "{e}"
    );
    assert!(
        a.service_principals()
            .service_principal_display_name_to_id_map(ListAccountServicePrincipalsRequest::default())
            .await
            .is_err()
    );
    assert!(
        a.service_principals()
            .get_by_display_name("x")
            .await
            .is_err()
    );
    assert!(
        a.users()
            .get_by_user_name("x")
            .await
            .unwrap_err()
            .is_missing()
    );
    assert!(
        a.users()
            .user_user_name_to_id_map(ListAccountUsersRequest::default())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(a.users().get_by_id("1").await.is_err());
    assert!(a.groups().delete_by_id("1").await.is_err());
    assert!(a.groups().get_by_id("1").await.is_err());
    assert!(a.service_principals().get_by_id("1").await.is_err());
    assert!(a.service_principals().delete_by_id("1").await.is_err());
    assert!(a.users().delete_by_id("1").await.is_err());

    let w = workspace(&server).await;
    let g = w.groups();
    assert!(g.get_by_display_name("admins").await.is_err());
    assert!(
        g.group_display_name_to_id_map(ListGroupsRequest::default())
            .await
            .is_err()
    );
    assert!(g.get_by_id("g1").await.is_err());
    assert!(g.delete_by_id("g1").await.is_err());
    let sp = w.service_principals();
    assert!(sp.get_by_display_name("admins").await.is_err());
    assert!(
        sp.service_principal_display_name_to_id_map(ListServicePrincipalsRequest::default())
            .await
            .is_err()
    );
    assert!(sp.get_by_id("s").await.is_err());
}

#[tokio::test]
async fn current_workspace_id_reads_the_org_id_header_once() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/preview/scim/v2/Me"))
        .and(query_param("excludedAttributes", "entitlements"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("X-Databricks-Org-Id", "1234567890")
                .set_body_json(json!({"userName": "a@x"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let w = workspace(&server).await;
    assert_eq!(w.current_workspace_id().await.unwrap(), 1_234_567_890);
    // Cached, including for clones.
    assert_eq!(
        w.clone().current_workspace_id().await.unwrap(),
        1_234_567_890
    );
}

#[tokio::test]
async fn current_workspace_id_needs_the_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/preview/scim/v2/Me"))
        .and(header("authorization", "Bearer dapi"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let e = workspace(&server)
        .await
        .current_workspace_id()
        .await
        .unwrap_err();
    assert!(e.to_string().contains("X-Databricks-Org-Id"), "{e}");
}
