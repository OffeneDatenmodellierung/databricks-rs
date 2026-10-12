//! `w.serving_endpoints_data_plane().query` (Go: `ServingEndpointsDataPlane`).
//!
//! Run with `--all-features` (CI does).

#![cfg(feature = "serving")]

use community_databricks_sdk::core::Error;
use community_databricks_sdk::core::config::HostMetadata;
use community_databricks_sdk::service::serving::QueryEndpointInput;
use community_databricks_sdk::{Config, WorkspaceClient};
use serde_json::json;
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg(server: &MockServer) -> Config {
    let mut c = Config::with_host(server.uri());
    c.host_metadata = Some(HostMetadata::default());
    c.rate_limit = Some(1000);
    c
}

/// M2M discovery and token endpoint, the jwt-bearer exchange, and a
/// control-plane `Get` for endpoint `ep` whose data plane is `data`.
async fn mount_control(control: &MockServer, data: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/oidc/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token_endpoint": format!("{}/oidc/v1/token", control.uri()),
        })))
        .mount(control)
        .await;
    Mock::given(method("POST"))
        .and(path("/oidc/v1/token"))
        .and(body_string_contains("grant_type=client_credentials"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "cp-token", "token_type": "Bearer", "expires_in": 3600,
        })))
        .mount(control)
        .await;
    Mock::given(method("POST"))
        .and(path("/oidc/v1/token"))
        .and(body_string_contains("jwt-bearer"))
        .and(body_string_contains(
            "authorization_details=%5B%22ep-details%22%5D",
        ))
        .and(body_string_contains("assertion=cp-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "dp-token", "token_type": "Bearer", "expires_in": 3600,
        })))
        .expect(1)
        .mount(control)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/serving-endpoints/ep"))
        .and(header("authorization", "Bearer cp-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "ep",
            "data_plane_info": {"query_info": {
                "endpoint_url": format!("{}/serving-endpoints/ep/invocations", data.uri()),
                "authorization_details": "[\"ep-details\"]",
            }},
        })))
        .expect(1)
        .mount(control)
        .await;
}

#[tokio::test]
async fn query_goes_to_the_data_plane_and_caches_details_and_tokens() {
    let control = MockServer::start().await;
    let data = MockServer::start().await;
    mount_control(&control, &data).await;
    Mock::given(method("POST"))
        .and(path("/serving-endpoints/ep/invocations"))
        .and(header("authorization", "Bearer dp-token"))
        .and(body_string_contains("\"dataframe_records\""))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("served-model-name", "model-a")
                .set_body_json(json!({"predictions": [0.5]})),
        )
        .expect(2)
        .mount(&data)
        .await;
    let w = WorkspaceClient::new(cfg(&control).client_credentials("id", "secret"))
        .await
        .unwrap();

    for _ in 0..2 {
        // A new accessor each time: the caches live on the client.
        let mut req = QueryEndpointInput::new("ep");
        req.dataframe_records = vec![json!({"x": 1})];
        let resp = w.serving_endpoints_data_plane().query(req).await.unwrap();
        assert_eq!(resp.predictions, vec![json!(0.5)]);
        assert_eq!(resp.served_model_name.as_deref(), Some("model-a"));
    }
}

#[tokio::test]
async fn an_endpoint_without_data_plane_info_is_an_error() {
    let control = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/serving-endpoints/plain"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"name": "plain"})))
        .mount(&control)
        .await;
    let w = WorkspaceClient::new(cfg(&control).token("dapi"))
        .await
        .unwrap();
    let err = w
        .serving_endpoints_data_plane()
        .query(QueryEndpointInput::new("plain"))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("no data-plane query info"),
        "{err}"
    );
}

#[tokio::test]
async fn pat_is_refused_before_any_data_plane_request() {
    let control = MockServer::start().await;
    let data = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/serving-endpoints/ep"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "ep",
            "data_plane_info": {"query_info": {
                "endpoint_url": format!("{}/serving-endpoints/ep/invocations", data.uri()),
                "authorization_details": "[]",
            }},
        })))
        .mount(&control)
        .await;
    let w = WorkspaceClient::new(cfg(&control).token("dapi"))
        .await
        .unwrap();
    let err = w
        .serving_endpoints_data_plane()
        .query(QueryEndpointInput::new("ep"))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Auth { .. }), "{err:?}");
    assert_eq!(data.received_requests().await.unwrap().len(), 0);
}
