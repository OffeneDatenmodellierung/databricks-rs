//! End-to-end tests of the spike services against a mock server.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use community_databricks_sdk::core::config::HostMetadata;
use community_databricks_sdk::service::compute::{
    ClusterSource, ListClustersFilterBy, ListClustersRequest, ListClustersSortBy,
    ListClustersSortByField, State,
};
use community_databricks_sdk::service::jobs::{
    GetRunRequest, QueueSettings, RunLifeCycleState, RunNow, RunResultState,
};
use community_databricks_sdk::service::provisioning::WorkspaceStatus;
use community_databricks_sdk::{AccountClient, Config, Error, WorkspaceClient};
use futures_util::TryStreamExt;
use serde_json::json;
use wiremock::matchers::{body_json, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

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
async fn clusters_list_streams_every_page_with_nested_query_params() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.1/clusters/list"))
        .and(query_param("page_size", "2"))
        .and(query_param("sort_by.field", "CLUSTER_NAME"))
        .and(query_param_is_missing("page_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "clusters": [
                {"cluster_id": "a", "cluster_name": "one", "state": "RUNNING",
                 "cluster_source": "UI", "autoscale": {"min_workers": 1, "max_workers": 4},
                 "brand_new_field": {"x": 1}},
                {"cluster_id": "b", "state": "HIBERNATING"}
            ],
            "next_page_token": "p2"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.1/clusters/list"))
        .and(query_param("page_token", "p2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "clusters": [{"cluster_id": "c", "state": "PENDING"}],
            "next_page_token": ""
        })))
        .expect(1)
        .mount(&server)
        .await;

    let w = workspace(&server).await;
    let req = ListClustersRequest::default()
        .with_page_size(2)
        .with_filter_by(
            ListClustersFilterBy::default().with_cluster_states([State::Running, State::Pending]),
        )
        .with_sort_by(
            ListClustersSortBy::default().with_field(ListClustersSortByField::ClusterName),
        );
    let all: Vec<_> = w.clusters().list(req.clone()).try_collect().await.unwrap();

    let ids: Vec<_> = all.iter().filter_map(|c| c.cluster_id.as_deref()).collect();
    assert_eq!(ids, ["a", "b", "c"]);
    assert_eq!(all[0].state, Some(State::Running));
    assert_eq!(all[0].cluster_source, Some(ClusterSource::Ui));
    assert_eq!(all[0].autoscale.as_ref().unwrap().max_workers, Some(4));
    // Fields this SDK version doesn't know ("brand_new_field") are ignored.
    assert_eq!(all[1].state, Some(State::Unknown("HIBERNATING".into())));

    // The repeated, dot-nested filter params Go sends.
    let reqs = server.received_requests().await.unwrap();
    let states: Vec<String> = reqs[0]
        .url
        .query_pairs()
        .filter(|(k, _)| k == "filter_by.cluster_states")
        .map(|(_, v)| v.into_owned())
        .collect();
    assert_eq!(states, ["RUNNING", "PENDING"]);

    // list_all is the same walk, collected.
    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    assert!(w.clusters().list_all(req).await.unwrap().is_empty());
}

fn run_state(life: &str, result: Option<&str>) -> serde_json::Value {
    json!({"run_id": 7, "job_id": 42, "state": {
        "life_cycle_state": life, "result_state": result, "state_message": format!("in {life}")
    }})
}

/// Serves `states` in order from runs/get, repeating the last one.
fn sequence(states: Vec<serde_json::Value>) -> impl Fn(&Request) -> ResponseTemplate {
    let n = AtomicUsize::new(0);
    move |_: &Request| {
        let i = n.fetch_add(1, Ordering::SeqCst).min(states.len() - 1);
        ResponseTemplate::new(200).set_body_json(states[i].clone())
    }
}

#[tokio::test]
async fn run_now_then_wait_until_terminated() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/2.2/jobs/run-now"))
        .and(body_json(json!({
            "job_id": 42,
            "job_parameters": {"env": "dev"},
            "queue": {"enabled": true}
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"run_id": 7, "number_in_job": 3})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.2/jobs/runs/get"))
        .and(query_param("run_id", "7"))
        .respond_with(sequence(vec![
            run_state("PENDING", None),
            run_state("TERMINATED", Some("SUCCESS")),
        ]))
        .mount(&server)
        .await;

    let w = workspace(&server).await;
    let waiter = w
        .jobs()
        .run_now(
            RunNow::new(42)
                .with_job_parameters([("env".to_owned(), "dev".to_owned())])
                .with_queue(QueueSettings::new(true)),
        )
        .await
        .unwrap();
    assert_eq!(waiter.run_id, 7);
    assert_eq!(waiter.response.number_in_job, Some(3));
    assert!(format!("{waiter:?}").contains("run_id: 7"));

    let seen = Arc::new(AtomicUsize::new(0));
    let s = seen.clone();
    let run = waiter
        .timeout(Duration::from_secs(30))
        .on_progress(move |_| {
            s.fetch_add(1, Ordering::SeqCst);
        })
        .wait()
        .await
        .unwrap();
    let state = run.state.unwrap();
    assert_eq!(state.life_cycle_state, Some(RunLifeCycleState::Terminated));
    assert_eq!(state.result_state, Some(RunResultState::Success));
    assert_eq!(seen.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn internal_error_halts_and_timeouts_surface() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.2/jobs/runs/get"))
        .and(query_param("run_id", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(run_state("INTERNAL_ERROR", None)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.2/jobs/runs/get"))
        .and(query_param("run_id", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(run_state("RUNNING", None)))
        .mount(&server)
        .await;
    let jobs = workspace(&server).await.jobs();

    let e = jobs
        .wait_get_run_job_terminated_or_skipped(1, Duration::from_secs(30), None)
        .await
        .unwrap_err();
    assert!(
        matches!(e, Error::OperationFailed(ref m) if m.contains("INTERNAL_ERROR")),
        "{e}"
    );

    let e = jobs
        .wait_get_run_job_terminated_or_skipped(2, Duration::from_millis(1500), None)
        .await
        .unwrap_err();
    assert!(
        matches!(e, Error::Timeout { ref last, .. } if last == "in RUNNING"),
        "{e}"
    );
}

#[tokio::test]
async fn get_run_merges_task_pages_like_go() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.2/jobs/runs/get"))
        .and(query_param_is_missing("page_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "run_id": 9, "tasks": [{"task_key": "a"}], "job_clusters": [{"k": 1}],
            "next_page_token": "n2"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.2/jobs/runs/get"))
        .and(query_param("page_token", "n2"))
        .and(query_param("include_history", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "run_id": 9, "tasks": [{"task_key": "b"}], "job_clusters": [{"k": 2}],
            "repair_history": [{"id": 1}]
        })))
        .mount(&server)
        .await;
    let run = workspace(&server)
        .await
        .jobs()
        .get_run(GetRunRequest::new(9).with_include_history(true))
        .await
        .unwrap();
    let keys: Vec<_> = run.tasks.iter().map(|t| t.task_key.as_str()).collect();
    assert_eq!(keys, ["a", "b"]);
    assert_eq!(run.job_clusters.len(), 2);
    assert_eq!(run.repair_history.len(), 1);
    assert!(run.next_page_token.is_none());
}

#[tokio::test]
async fn get_run_for_each_merges_iterations_not_tasks() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param_is_missing("page_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "run_id": 9, "tasks": [{"task_key": "loop"}], "iterations": [{"i": 0}],
            "next_page_token": "n2"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(query_param("page_token", "n2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "run_id": 9, "tasks": [{"task_key": "loop"}], "iterations": [{"i": 1}]
        })))
        .mount(&server)
        .await;
    let run = workspace(&server)
        .await
        .jobs()
        .get_run(GetRunRequest::new(9))
        .await
        .unwrap();
    assert_eq!(run.iterations.len(), 2);
    assert_eq!(run.tasks.len(), 1);
}

#[tokio::test]
async fn account_client_lists_workspaces() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/accounts/acc-1/workspaces"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"workspace_id": 11, "workspace_name": "prod", "workspace_status": "RUNNING",
             "deployment_name": "dbc-1"},
            {"workspace_id": 12, "workspace_status": "SUSPENDED"}
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let a = AccountClient::new(cfg(&server).account("acc-1"))
        .await
        .unwrap();
    assert_eq!(a.account_id(), "acc-1");
    let ws = a.workspaces().list().await.unwrap();
    assert_eq!(ws[0].workspace_status, Some(WorkspaceStatus::Running));
    assert_eq!(
        ws[1].workspace_status,
        Some(WorkspaceStatus::Unknown("SUSPENDED".into()))
    );
    assert_eq!(a.api_client().auth_type(), Some("pat"));
}

#[tokio::test]
async fn account_client_requires_account_id() {
    let server = MockServer::start().await;
    let e = AccountClient::new(cfg(&server)).await.unwrap_err();
    assert!(e.to_string().contains("account_id missing"), "{e}");
}

#[tokio::test]
async fn workspace_client_exposes_config_and_api_client() {
    let server = MockServer::start().await;
    let w = workspace(&server).await;
    assert_eq!(w.config().host.as_deref(), Some(server.uri().as_str()));
    assert!(w.api_client().auth_type().is_none());
    let w2 = WorkspaceClient::from_api_client(w.api_client().clone());
    assert!(format!("{w2:?}").contains("WorkspaceClient"));
}

#[test]
fn every_known_enum_value_round_trips() {
    use community_databricks_sdk::service::compute::{
        DataSecurityMode, ListClustersSortByDirection,
    };
    use community_databricks_sdk::service::jobs::PerformanceTarget;
    macro_rules! check {
        ($($t:ident),+) => {$(
            for s in $t::KNOWN {
                let v = $t::from(*s);
                assert_ne!(v, $t::Unknown((*s).to_owned()), "{s}");
                assert_eq!(v.as_str(), *s);
                assert_eq!(serde_json::to_value(&v).unwrap(), json!(s));
            }
        )+};
    }
    check!(
        State,
        ClusterSource,
        DataSecurityMode,
        ListClustersSortByDirection,
        ListClustersSortByField,
        RunLifeCycleState,
        RunResultState,
        PerformanceTarget,
        WorkspaceStatus
    );
}

#[tokio::test]
async fn clients_accept_a_custom_credential_chain() {
    use community_databricks_sdk::core::auth::{DefaultCredentials, PatCredentials};
    let server = MockServer::start().await;
    let chain = || DefaultCredentials::new(vec![Box::new(PatCredentials)]);
    let w = WorkspaceClient::with_credentials(cfg(&server), chain())
        .await
        .unwrap();
    assert_eq!(w.api_client().authenticate().await.unwrap(), "pat");
    let a = AccountClient::with_credentials(cfg(&server).account("a"), chain())
        .await
        .unwrap();
    assert_eq!(a.account_id(), "a");
}
