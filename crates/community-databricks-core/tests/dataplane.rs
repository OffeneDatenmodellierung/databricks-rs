//! Data-plane token exchange, caching and requests (Go:
//! `config/experimental/auth/dataplane`, `ApiClient.GetOAuthToken`).

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use community_databricks_core::auth::DefaultCredentials;
use community_databricks_core::config::HostMetadata;
use community_databricks_core::dataplane::DataPlaneTarget;
use community_databricks_core::http::{Call, Method};
use community_databricks_core::{ApiClient, Config, Error};
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const DETAILS: &str = r#"[{"type":"workspace_permission","object_type":"serving-endpoints","object_path":"/serving-endpoints/ep","actions":["query_inference_endpoint"]}]"#;

async fn client(c: Config) -> ApiClient {
    let mut c = c;
    c.host_metadata = Some(HostMetadata::default());
    c.rate_limit = Some(1000);
    let c = c.resolve_with(|_| None, None).await.unwrap();
    ApiClient::from_resolved(c, DefaultCredentials::default()).unwrap()
}

/// OIDC discovery and an M2M token endpoint issuing `cp-token`.
async fn mount_m2m(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/oidc/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token_endpoint": format!("{}/oidc/v1/token", server.uri()),
        })))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oidc/v1/token"))
        .and(body_string_contains("grant_type=client_credentials"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "cp-token", "token_type": "Bearer", "expires_in": 3600,
        })))
        .mount(server)
        .await;
}

/// The jwt-bearer exchange for `details`, issuing `token`.
async fn mount_exchange(server: &MockServer, details: &str, token: &str, expires_in: u64) {
    let encoded: String = url::form_urlencoded::byte_serialize(details.as_bytes()).collect();
    Mock::given(method("POST"))
        .and(path("/oidc/v1/token"))
        .and(header("authorization", "Bearer cp-token"))
        .and(body_string_contains(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer",
        ))
        .and(body_string_contains(format!(
            "authorization_details={encoded}"
        )))
        .and(body_string_contains("assertion=cp-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": token, "token_type": "Bearer", "expires_in": expires_in,
        })))
        .mount(server)
        .await;
}

fn exchanges(requests: &[wiremock::Request]) -> usize {
    requests
        .iter()
        .filter(|r| String::from_utf8_lossy(&r.body).contains("jwt-bearer"))
        .count()
}

#[tokio::test]
async fn exchanges_the_oauth_token_and_caches_per_target() {
    let server = MockServer::start().await;
    mount_m2m(&server).await;
    mount_exchange(&server, DETAILS, "dp-token", 3600).await;
    mount_exchange(&server, "[]", "dp-other", 3600).await;
    let api = client(Config::with_host(server.uri()).client_credentials("id", "secret")).await;

    let target = DataPlaneTarget::new("https://dp.example/ep", DETAILS);
    assert_eq!(
        api.data_plane_token(&target).await.unwrap().secret(),
        "dp-token"
    );
    assert_eq!(
        api.data_plane_token(&target).await.unwrap().secret(),
        "dp-token"
    );
    // Clones share the cache.
    let clone = api.clone();
    assert_eq!(
        clone.data_plane_token(&target).await.unwrap().secret(),
        "dp-token"
    );
    // Another target gets its own token.
    let other = DataPlaneTarget::new("https://dp.example/other", "[]");
    assert_eq!(
        api.data_plane_token(&other).await.unwrap().secret(),
        "dp-other"
    );

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(exchanges(&reqs), 2);
    // Authorization details stay out of Debug output.
    let shown = format!("{target:?}");
    assert!(shown.contains("dp.example") && !shown.contains("serving-endpoints"));
}

#[tokio::test]
async fn expired_data_plane_tokens_are_exchanged_again() {
    let server = MockServer::start().await;
    mount_m2m(&server).await;
    mount_exchange(&server, DETAILS, "dp-token", 1).await;
    let api = client(Config::with_host(server.uri()).client_credentials("id", "secret")).await;
    let target = DataPlaneTarget::new("https://dp.example/ep", DETAILS);

    api.data_plane_token(&target).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    api.data_plane_token(&target).await.unwrap();

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(exchanges(&reqs), 2);
}

#[tokio::test]
async fn targets_are_fetched_once_per_key() {
    let server = MockServer::start().await;
    let api = client(Config::with_host(server.uri()).token("pat")).await;
    let fetches = Arc::new(AtomicU32::new(0));
    for _ in 0..3 {
        let f = Arc::clone(&fetches);
        let t = api
            .data_plane_target("Query/ep", || async move {
                f.fetch_add(1, Ordering::SeqCst);
                Ok(DataPlaneTarget::new("https://dp.example/ep", DETAILS))
            })
            .await
            .unwrap();
        assert_eq!(t.endpoint_url, "https://dp.example/ep");
    }
    assert_eq!(fetches.load(Ordering::SeqCst), 1);

    // A failed fetch isn't cached.
    let err = api
        .data_plane_target("Query/missing", || async {
            Err(Error::Config("no data plane".into()))
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no data plane"));
    let t = api
        .data_plane_target("Query/missing", || async {
            Ok(DataPlaneTarget::new("https://dp.example/m", "[]"))
        })
        .await
        .unwrap();
    assert_eq!(t.authorization_details, "[]");
}

#[tokio::test]
async fn pat_credentials_are_refused() {
    let server = MockServer::start().await;
    let api = client(Config::with_host(server.uri()).token("pat")).await;
    let target = DataPlaneTarget::new("https://dp.example/ep", DETAILS);
    let err = api.data_plane_token(&target).await.unwrap_err();
    match err {
        Error::Auth { auth_type, message } => {
            assert_eq!(auth_type, "pat");
            assert!(message.contains("OAuth token not supported"), "{message}");
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 0);
}

#[tokio::test]
async fn requests_go_to_the_data_plane_url_with_the_exchanged_token() {
    let control = MockServer::start().await;
    let data = MockServer::start().await;
    mount_m2m(&control).await;
    mount_exchange(&control, DETAILS, "dp-token", 3600).await;
    Mock::given(method("POST"))
        .and(path("/serving-endpoints/ep/invocations"))
        .and(header("authorization", "Bearer dp-token"))
        .and(header("x-databricks-workspace-id", "123"))
        .and(body_string_contains("\"inputs\""))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("served-model-name", "m-1")
                .set_body_json(json!({"predictions": [1]})),
        )
        .expect(1)
        .mount(&data)
        .await;
    let mut c = Config::with_host(control.uri()).client_credentials("id", "secret");
    c.workspace_id = Some("123".into());
    let api = client(c).await;
    let target = DataPlaneTarget::new(
        format!("{}/serving-endpoints/ep/invocations", data.uri()),
        DETAILS,
    );

    let call = Call::new(Method::POST, "ignored".into())
        .workspace()
        .json(&json!({"inputs": [1]}))
        .unwrap();
    let (body, headers): (Value, _) = api
        .send_data_plane_with_headers(&target, call)
        .await
        .unwrap();
    assert_eq!(body, json!({"predictions": [1]}));
    assert_eq!(
        community_databricks_core::http::header::<String>(&headers, "served-model-name").as_deref(),
        Some("m-1")
    );
}
