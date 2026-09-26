//! Tier-0 runtime behaviour: idempotency-safe retries (#3), workspace
//! derivation (#4), redirects and private link (#5), and hygiene (#6).

use community_databricks_core::auth::DefaultCredentials;
use community_databricks_core::config::HostMetadata;
use community_databricks_core::http::{Call, Method, idempotency_token};
use community_databricks_core::{ApiClient, Config, Error, ErrorKind};
use secrecy::ExposeSecret;
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg(server: &MockServer) -> Config {
    let mut c = Config::with_host(server.uri()).token("dapi");
    c.host_metadata = Some(HostMetadata::default());
    c.rate_limit = Some(1000);
    c
}

async fn client(c: Config) -> ApiClient {
    let c = c.resolve_with(|_| None, None).await.unwrap();
    ApiClient::from_resolved(c, DefaultCredentials::default()).unwrap()
}

/// `status` for the first `times` requests to `p`, then 200 `{}`.
async fn flaky(server: &MockServer, verb: &str, p: &str, status: u16, times: u64) {
    Mock::given(method(verb))
        .and(path(p))
        .respond_with(ResponseTemplate::new(status).set_body_json(
            json!({"error_code": "TEMPORARILY_UNAVAILABLE", "message": "try later"}),
        ))
        .up_to_n_times(times)
        .mount(server)
        .await;
    Mock::given(method(verb))
        .and(path(p))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "1"})))
        .mount(server)
        .await;
}

async fn send(api: &ApiClient, call: Call) -> Result<Value, Error> {
    api.send(call).await
}

fn post(p: &str) -> Call {
    Call::new(Method::POST, p.to_owned())
        .json(&json!({"name": "x"}))
        .unwrap()
}

#[tokio::test]
async fn a_create_that_may_have_been_applied_is_not_retried() {
    let server = MockServer::start().await;
    flaky(&server, "POST", "/api/2.0/things", 503, 1).await;
    let api = client(cfg(&server)).await;
    let e = send(&api, post("/api/2.0/things")).await.unwrap_err();
    assert!(
        matches!(e, Error::Api(ref a) if a.status_code == 503),
        "{e}"
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn throttled_creates_are_retried() {
    let server = MockServer::start().await;
    flaky(&server, "POST", "/api/2.0/things", 429, 1).await;
    let api = client(cfg(&server)).await;
    let v = send(&api, post("/api/2.0/things")).await.unwrap();
    assert_eq!(v, json!({"id": "1"}));
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn idempotent_calls_are_retried_after_503() {
    let server = MockServer::start().await;
    flaky(&server, "POST", "/api/2.0/keyed", 503, 1).await;
    flaky(&server, "PUT", "/api/2.0/put", 503, 1).await;
    flaky(&server, "PATCH", "/api/2.0/patch", 503, 1).await;
    let api = client(cfg(&server)).await;
    send(&api, post("/api/2.0/keyed").idempotent())
        .await
        .unwrap();
    let put = Call::new(Method::PUT, "/api/2.0/put".into())
        .json(&json!({}))
        .unwrap();
    send(&api, put).await.unwrap();
    let patch = Call::new(Method::PATCH, "/api/2.0/patch".into())
        .json(&json!({}))
        .unwrap();
    send(&api, patch).await.unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 6);
}

#[tokio::test]
async fn go_behaviour_is_opt_in() {
    let server = MockServer::start().await;
    flaky(&server, "POST", "/api/2.0/things", 503, 1).await;
    let mut c = cfg(&server);
    c.retry_non_idempotent = true;
    let api = client(c).await;
    send(&api, post("/api/2.0/things")).await.unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[test]
fn idempotency_tokens_are_uuid_v4() {
    let t = idempotency_token();
    assert_eq!(t.len(), 36);
    assert_eq!(&t[14..15], "4");
    assert!(matches!(&t[19..20], "8" | "9" | "a" | "b"), "{t}");
    assert_ne!(t, idempotency_token());
}

#[tokio::test]
async fn private_link_login_redirect_is_a_permission_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/clusters/list"))
        .respond_with(ResponseTemplate::new(302).insert_header(
            "location",
            "/login.html?error=private-link-validation-error",
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/login.html"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>login</html>"))
        .mount(&server)
        .await;
    let api = client(cfg(&server)).await;
    let e = api
        .query::<_, Value>(Method::GET, "/api/2.0/clusters/list", &())
        .await
        .unwrap_err();
    let Error::Api(a) = &e else { panic!("{e}") };
    assert_eq!(a.error_code, "PRIVATE_LINK_VALIDATION_ERROR");
    assert_eq!(a.status_code, 403);
    assert!(e.is(ErrorKind::PermissionDenied));
    assert!(a.message.contains("AWS PrivateLink"), "{}", a.message);
}

#[tokio::test]
async fn an_unfollowed_redirect_is_an_error_not_success() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/x"))
        .respond_with(ResponseTemplate::new(302))
        .mount(&server)
        .await;
    let api = client(cfg(&server)).await;
    let e = api
        .query::<_, Value>(Method::GET, "/api/2.0/x", &())
        .await
        .unwrap_err();
    let Error::Api(a) = &e else { panic!("{e}") };
    assert_eq!(
        (a.status_code, a.error_code.as_str()),
        (302, "UNEXPECTED_REDIRECT")
    );
    assert!(a.message.contains("no Location header"), "{}", a.message);
}

#[tokio::test]
async fn unified_host_workspace_client_shares_credentials() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/oidc/accounts/acc/.well-known/oauth-authorization-server",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token_endpoint": format!("{}/oidc/accounts/acc/v1/token", server.uri()),
        })))
        .mount(&server)
        .await;
    // One token request serves both clients.
    Mock::given(method("POST"))
        .and(path("/oidc/accounts/acc/v1/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"access_token": "t", "token_type": "Bearer", "expires_in": 3600}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/accounts/acc/workspaces"))
        .and(header("authorization", "Bearer t"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/clusters/list"))
        .and(header("authorization", "Bearer t"))
        .and(header("x-databricks-workspace-id", "42"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let mut c = Config::with_host(server.uri())
        .client_credentials("sp", "secret")
        .account("acc");
    c.rate_limit = Some(1000);
    c.host_metadata = Some(serde_json::from_value(json!({"host_type": "UNIFIED_HOST"})).unwrap());
    let account = client(c).await;
    let _: Value = account
        .query(Method::GET, "/api/2.0/accounts/acc/workspaces", &())
        .await
        .unwrap();
    let ws = account.for_workspace(None, "42", None).unwrap();
    assert_eq!(ws.config().workspace_id.as_deref(), Some("42"));
    assert_eq!(ws.auth_type(), Some("oauth-m2m"));
    let _: Value = ws
        .query(Method::GET, "/api/2.0/clusters/list", &())
        .await
        .unwrap();
}

#[tokio::test]
async fn separate_workspace_host_drops_account_settings() {
    let server = MockServer::start().await;
    let mut c = cfg(&server).account("acc");
    c.host_metadata = Some(
        serde_json::from_value(json!({"host_type": "ACCOUNT_HOST", "oidc_endpoint": "https://acc.example/oidc/accounts/{account_id}"})).unwrap(),
    );
    let account = client(c).await;
    assert!(account.config().discovery_url.is_some());
    assert_eq!(
        account.config().attribute("audience").as_deref(),
        Some("acc")
    );
    let ws = account
        .for_workspace(
            Some("https://dbc-1.cloud.databricks.com"),
            "7",
            Some("/subscriptions/s/x"),
        )
        .unwrap();
    let cfg = ws.config();
    assert_eq!(
        cfg.host.as_deref(),
        Some("https://dbc-1.cloud.databricks.com")
    );
    assert_eq!(cfg.workspace_id.as_deref(), Some("7"));
    assert!(cfg.account_id.is_none());
    assert!(cfg.discovery_url.is_none());
    assert!(cfg.attribute("audience").is_none());
    assert_eq!(
        cfg.attribute("azure_workspace_resource_id").as_deref(),
        Some("/subscriptions/s/x")
    );
    assert!(!cfg.is_account_client());
    assert!(account.for_workspace(Some("not a url"), "7", None).is_err());
}

#[test]
fn workspace_host_follows_the_account_dns_zone() {
    let c = Config::with_host("https://accounts.cloud.databricks.com");
    assert_eq!(
        c.workspace_host("dbc-a1b2").as_deref(),
        Some("https://dbc-a1b2.cloud.databricks.com")
    );
    assert_eq!(
        Config::with_host("https://accounts.azuredatabricks.net")
            .workspace_host("adb-1")
            .as_deref(),
        Some("https://adb-1.azuredatabricks.net")
    );
    // Unified or unknown hosts serve the workspace themselves.
    assert!(
        Config::with_host("https://unified.example")
            .workspace_host("x")
            .is_none()
    );
    assert!(c.workspace_host("").is_none());
}

#[test]
fn attribute_masks_secrets() {
    let mut c = Config::with_host("https://x").token("dapi-secret");
    c.set_attribute("azure_client_secret", "az-secret").unwrap();
    c.set_attribute("azure_client_id", "id").unwrap();
    assert_eq!(c.attribute("token").as_deref(), Some("***"));
    assert_eq!(c.attribute("azure_client_secret").as_deref(), Some("***"));
    assert_eq!(c.attribute("azure_client_id").as_deref(), Some("id"));
    assert_eq!(
        c.secret_attribute("token").unwrap().expose_secret(),
        "dapi-secret"
    );
    assert!(c.secret_attribute("unknown").is_none());
}

#[tokio::test]
async fn host_metadata_retries_transient_failures() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/.well-known/databricks-config"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/.well-known/databricks-config"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"workspace_id": "99"})))
        .mount(&server)
        .await;
    let c = Config::with_host(server.uri())
        .token("t")
        .resolve_with(|_| None, None)
        .await
        .unwrap();
    assert_eq!(c.workspace_id.as_deref(), Some("99"));
}

#[tokio::test]
async fn skip_verify_still_builds_clients() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/.well-known/databricks-config"))
        .and(query_param("x", "y"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let mut c = Config::with_host(server.uri()).token("t");
    c.skip_verify = true;
    let c = c.resolve_with(|_| None, None).await.unwrap();
    assert!(ApiClient::from_resolved(c, DefaultCredentials::default()).is_ok());
}

#[test]
fn debug_hides_custom_header_values() {
    let cfg = Config::with_host("https://x.cloud.databricks.com").header("X-Api-Key", "s3cret");
    let dbg = format!("{cfg:?}");
    assert!(dbg.contains("X-Api-Key"));
    assert!(!dbg.contains("s3cret"));
}
