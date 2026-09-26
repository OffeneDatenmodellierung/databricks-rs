//! Long-running operations: the generated calls return a `LongRunning`
//! handle that polls the service's `GetOperation` (Go's
//! `XOperationInterface`).
//!
//! Run with `--all-features` (CI does).

#![cfg(all(feature = "apps", feature = "ml", feature = "postgres"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use community_databricks_sdk::core::config::HostMetadata;
use community_databricks_sdk::service::apps::{CreateSpaceRequest, Space};
use community_databricks_sdk::service::ml::{
    BackfillFeaturesRequest, BackfillOperationMetadataState,
};
use community_databricks_sdk::service::postgres::{
    Branch, CreateBranchRequest, DeleteBranchRequest,
};
use community_databricks_sdk::{Config, Error, ErrorKind, WorkspaceClient};
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, method, path, query_param};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn cfg(server: &MockServer) -> Config {
    let mut c = Config::with_host(server.uri()).token("dapi");
    c.host_metadata = Some(HostMetadata::default());
    c.rate_limit = Some(1000);
    c
}

async fn workspace(server: &MockServer) -> WorkspaceClient {
    WorkspaceClient::new(cfg(server)).await.unwrap()
}

const FAST: (Duration, Duration) = (Duration::from_millis(1), Duration::from_millis(5));

/// `pending` for the first `n` polls, then `done`.
struct AfterPolls {
    n: usize,
    pending: Value,
    done: Value,
    seen: Arc<AtomicUsize>,
}

impl Respond for AfterPolls {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let i = self.seen.fetch_add(1, Ordering::SeqCst);
        let body = if i < self.n {
            &self.pending
        } else {
            &self.done
        };
        ResponseTemplate::new(200).set_body_json(body)
    }
}

#[tokio::test]
async fn create_branch_polls_the_operation_until_done() {
    let server = MockServer::start().await;
    let op = "projects/p/branches/dev/operations/o1";
    Mock::given(method("POST"))
        .and(path("/api/2.0/postgres/projects/p/branches"))
        .and(query_param("branch_id", "dev"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": op, "done": false, "metadata": {"@type": "BranchOperationMetadata"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let polls = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path(format!("/api/2.0/postgres/{op}")))
        .respond_with(AfterPolls {
            n: 2,
            pending: json!({"name": op, "done": false}),
            done: json!({"name": op, "done": true,
                "response": {"name": "projects/p/branches/dev", "branch_id": "dev"}}),
            seen: polls.clone(),
        })
        .expect(3)
        .mount(&server)
        .await;
    let lro = workspace(&server)
        .await
        .postgres()
        .create_branch(
            CreateBranchRequest::default()
                .with_branch(Branch::default())
                .with_branch_id("dev")
                .with_parent("projects/p"),
        )
        .await
        .unwrap()
        .with_poll_interval(FAST.0, FAST.1);
    assert_eq!(lro.name(), op);
    // Metadata is decoded into the typed message (fields it doesn't model
    // are kept in `other`).
    let md = lro.metadata().unwrap().unwrap();
    assert_eq!(md.other["@type"], "BranchOperationMetadata");
    let branch = lro.wait().await.unwrap();
    assert_eq!(branch.branch_id.as_deref(), Some("dev"));
    assert_eq!(polls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn a_finished_delete_returns_without_polling() {
    let server = MockServer::start().await;
    let op = "projects/p/branches/dev/operations/o2";
    Mock::given(method("DELETE"))
        .and(path("/api/2.0/postgres/projects/p/branches/dev"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"name": op, "done": true})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let lro = workspace(&server)
        .await
        .postgres()
        .delete_branch(DeleteBranchRequest::new("projects/p/branches/dev"))
        .await
        .unwrap();
    lro.wait().await.unwrap();
}

#[tokio::test]
async fn a_failed_operation_is_an_api_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/2.0/app-spaces"))
        .and(body_partial_json(json!({"name": "analytics"})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"name": "analytics", "done": false})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/app-spaces/analytics/operation"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "analytics", "done": true,
            "error": {"error_code": "INVALID_PARAMETER_VALUE", "message": "bad space"}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let e = workspace(&server)
        .await
        .apps()
        .create_space(CreateSpaceRequest::new(Space::new("analytics")))
        .await
        .unwrap()
        .with_poll_interval(FAST.0, FAST.1)
        .wait()
        .await
        .unwrap_err();
    assert!(e.is(ErrorKind::InvalidParameterValue), "{e}");
    assert!(
        e.to_string()
            .contains("[INVALID_PARAMETER_VALUE] bad space"),
        "{e}"
    );
}

#[tokio::test]
async fn backfill_can_be_cancelled_and_times_out() {
    let server = MockServer::start().await;
    let op = "operations/b1";
    Mock::given(method("POST"))
        .and(path("/api/2.0/feature-engineering/features:backfill"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": op, "done": false,
            "metadata": {"state": "RUNNING", "feature_full_names": ["c.s.f"]}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/2.0/feature-engineering/{op}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"name": op, "done": false})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/api/2.0/feature-engineering/{op}:cancel")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let lro = workspace(&server)
        .await
        .feature_engineering()
        .backfill_features(BackfillFeaturesRequest::new(
            Vec::new(),
            vec!["c.s.f".to_owned()],
        ))
        .await
        .unwrap()
        .with_poll_interval(FAST.0, FAST.1)
        .with_timeout(Duration::from_millis(50));
    let md = lro.metadata().unwrap().unwrap();
    assert_eq!(md.state, Some(BackfillOperationMetadataState::Running));
    assert_eq!(md.feature_full_names, ["c.s.f"]);
    lro.cancel().await.unwrap();
    let e = lro.wait().await.unwrap_err();
    assert!(matches!(e, Error::Timeout { .. }), "{e}");
}
