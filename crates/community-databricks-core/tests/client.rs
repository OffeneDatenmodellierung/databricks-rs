//! `ApiClient` behaviour against a mock server: auth, retries, headers,
//! rate limiting and OAuth M2M.

use std::time::{Duration, Instant};

use community_databricks_core::auth::{DefaultCredentials, OAuthEndpoints};
use community_databricks_core::config::HostMetadata;
use community_databricks_core::{ApiClient, Config, Error, ErrorKind};
use reqwest::Method;
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, header, header_regex, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg(server: &MockServer) -> Config {
    let mut c = Config::with_host(server.uri());
    c.host_metadata = Some(HostMetadata::default());
    c.rate_limit = Some(1000);
    c
}

async fn client(c: Config) -> ApiClient {
    let c = c.resolve_with(|_| None, None).await.unwrap();
    ApiClient::from_resolved(c, DefaultCredentials::default()).unwrap()
}

#[tokio::test]
async fn pat_request_carries_auth_user_agent_and_workspace_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.1/clusters/list"))
        .and(query_param("page_size", "5"))
        .and(header("authorization", "Bearer dapi-1"))
        .and(header("accept", "application/json"))
        .and(header("x-databricks-workspace-id", "123"))
        .and(header_regex(
            "user-agent",
            r"^unknown/0\.0\.0 databricks-sdk-rust/\S+ rust/\S+ os/\S+ auth/pat",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(1)
        .mount(&server)
        .await;
    let mut c = cfg(&server).token("dapi-1");
    c.workspace_id = Some("123".into());
    let api = client(c).await;
    assert_eq!(api.auth_type(), None);
    let v: Value = api
        .query(
            Method::GET,
            "/api/2.1/clusters/list",
            &json!({"page_size": 5}),
        )
        .await
        .unwrap();
    assert_eq!(v, json!({"ok": true}));
    assert_eq!(api.auth_type(), Some("pat"));
    assert!(format!("{api:?}").contains("pat"));
}

#[tokio::test]
async fn custom_headers_are_sent_but_never_override_sdk_headers() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/x"))
        .and(header("x-trace", "abc"))
        .and(header("authorization", "Bearer dapi-1"))
        .and(header("accept", "application/json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let c = cfg(&server)
        .token("dapi-1")
        .header("X-Trace", "abc")
        .header("Authorization", "Bearer evil")
        .header("accept", "text/plain");
    let api = client(c).await;
    let _: Value = api
        .query(Method::GET, "/api/2.0/x", &json!({}))
        .await
        .unwrap();
    let reqs = server.received_requests().await.unwrap();
    let auth: Vec<_> = reqs[0].headers.get_all("authorization").iter().collect();
    assert_eq!(auth.len(), 1, "{auth:?}");
    assert_eq!(reqs[0].headers.get_all("accept").iter().count(), 1);
}

#[tokio::test]
async fn json_body_and_empty_responses() {
    #[derive(serde::Deserialize, Default)]
    struct Empty {
        #[serde(default)]
        items: Vec<u8>,
    }
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/2.0/thing"))
        .and(header("content-type", "application/json"))
        .and(body_string_contains(r#""name":"x""#))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let api = client(cfg(&server).token("t")).await;
    let (): () = api
        .json(Method::POST, "/api/2.0/thing", &json!({"name": "x"}))
        .await
        .unwrap();
    let e: Empty = api
        .json(Method::POST, "/api/2.0/thing", &json!({"name": "x"}))
        .await
        .unwrap();
    assert!(e.items.is_empty());
}

#[tokio::test]
async fn retries_429_honouring_retry_after_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/x"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "1")
                .set_body_json(json!({"error_code": "TOO_MANY_REQUESTS", "message": "slow down"})),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/x"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!(1)))
        .expect(1)
        .mount(&server)
        .await;
    let api = client(cfg(&server).token("t")).await;
    let started = Instant::now();
    let v: u32 = api.query(Method::GET, "/api/x", &()).await.unwrap();
    assert_eq!(v, 1);
    assert!(started.elapsed() >= Duration::from_secs(1));
}

#[tokio::test]
async fn retries_503_504_and_transient_messages() {
    let server = MockServer::start().await;
    for (status, body) in [
        (503, json!({"message": "No webapps"})),
        (504, json!({})),
        (
            400,
            json!({"error_code": "INVALID_STATE", "message": "ClusterNotReadyException: warming"}),
        ),
        (
            400,
            json!({"error_code": "REQUEST_LIMIT_EXCEEDED", "message": "limit"}),
        ),
    ] {
        Mock::given(method("GET"))
            .and(path("/api/y"))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .up_to_n_times(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/api/y"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!("done")))
        .mount(&server)
        .await;
    let api = client(cfg(&server).token("t")).await;
    let v: String = api.query(Method::GET, "/api/y", &()).await.unwrap();
    assert_eq!(v, "done");
    assert_eq!(server.received_requests().await.unwrap().len(), 5);
}

#[tokio::test]
async fn non_retriable_errors_fail_fast_with_typed_kind() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.1/clusters/get"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error_code": "INVALID_PARAMETER_VALUE",
            "message": "Cluster 0000 does not exist"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let api = client(cfg(&server).token("t")).await;
    let e = api
        .query::<_, Value>(
            Method::GET,
            "/api/2.1/clusters/get",
            &json!({"cluster_id": "0000"}),
        )
        .await
        .unwrap_err();
    assert!(e.is_missing(), "{e:?}");
    assert!(e.is(ErrorKind::ResourceDoesNotExist));
    assert_eq!(e.as_api().unwrap().status_code, 400);
}

#[tokio::test]
async fn retry_budget_exhaustion_returns_last_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let mut c = cfg(&server).token("t");
    c.retry_timeout_seconds = Some(1);
    let api = client(c).await;
    let e = api
        .query::<_, Value>(Method::GET, "/api/z", &())
        .await
        .unwrap_err();
    assert!(e.is(ErrorKind::TemporarilyUnavailable), "{e:?}");
}

#[tokio::test]
async fn rate_limit_spaces_requests() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!(null)))
        .mount(&server)
        .await;
    let mut c = cfg(&server).token("t");
    c.rate_limit = Some(10);
    let api = client(c).await;
    let started = Instant::now();
    for _ in 0..4 {
        let (): () = api.query(Method::GET, "/api/r", &()).await.unwrap();
    }
    assert!(
        started.elapsed() >= Duration::from_millis(290),
        "{:?}",
        started.elapsed()
    );
}

async fn mount_workspace_oidc(server: &MockServer, expected_token_calls: u64) {
    Mock::given(method("GET"))
        .and(path("/oidc/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "authorization_endpoint": format!("{}/oidc/v1/authorize", server.uri()),
            "token_endpoint": format!("{}/oidc/v1/token", server.uri()),
        })))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oidc/v1/token"))
        // base64("sp-id:sp-secret")
        .and(header("authorization", "Basic c3AtaWQ6c3Atc2VjcmV0"))
        .and(body_string_contains("grant_type=client_credentials"))
        .and(body_string_contains("scope=all-apis"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "m2m-token",
            "token_type": "Bearer",
            "expires_in": 3600
        })))
        .expect(expected_token_calls)
        .mount(server)
        .await;
}

#[tokio::test]
async fn oauth_m2m_discovers_endpoints_and_caches_the_token() {
    let server = MockServer::start().await;
    mount_workspace_oidc(&server, 1).await;
    Mock::given(method("GET"))
        .and(path("/api/me"))
        .and(header("authorization", "Bearer m2m-token"))
        .and(header_regex("user-agent", "auth/oauth-m2m"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!("hi")))
        .expect(3)
        .mount(&server)
        .await;
    let api = client(cfg(&server).client_credentials("sp-id", "sp-secret")).await;
    for _ in 0..3 {
        let s: String = api.query(Method::GET, "/api/me", &()).await.unwrap();
        assert_eq!(s, "hi");
    }
    assert_eq!(api.auth_type(), Some("oauth-m2m"));
}

#[tokio::test]
async fn group_role_assumption_is_sent_by_m2m_and_refused_by_pat() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oidc/v1/token"))
        .and(body_string_contains("assume_group=grp-7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "group-token",
            "expires_in": 3600
        })))
        .expect(1)
        .mount(&server)
        .await;
    mount_workspace_oidc(&server, 0).await;
    let mut c = cfg(&server).client_credentials("sp-id", "sp-secret");
    c.group_id = Some("grp-7".into());
    let api = client(c).await;
    assert_eq!(api.authenticate().await.unwrap(), "oauth-m2m");

    // PAT can only give normal access, so it refuses rather than ignoring
    // the group; the default chain then has nothing left.
    let mut c = cfg(&server).token("dapi-1");
    c.group_id = Some("grp-7".into());
    let e = client(c.clone()).await.authenticate().await.unwrap_err();
    assert!(
        e.to_string()
            .contains("cannot configure default credentials"),
        "{e}"
    );
    c.auth_type = Some("pat".into());
    let e = client(c).await.authenticate().await.unwrap_err();
    assert!(
        e.to_string()
            .contains("does not support group role assumption"),
        "{e}"
    );
}

#[tokio::test]
async fn oauth_token_request_retries_on_503() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oidc/v1/token"))
        .respond_with(ResponseTemplate::new(503).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    mount_workspace_oidc(&server, 1).await;
    let api = client(cfg(&server).client_credentials("sp-id", "sp-secret")).await;
    assert_eq!(api.authenticate().await.unwrap(), "oauth-m2m");
}

#[tokio::test]
async fn oauth_token_request_fails_fast_on_401() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/oidc/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token_endpoint": format!("{}/oidc/v1/token", server.uri()),
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oidc/v1/token"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error": "invalid_client"})))
        .expect(1)
        .mount(&server)
        .await;
    let mut c = cfg(&server).client_credentials("sp-id", "sp-secret");
    c.auth_type = Some("oauth-m2m".into());
    let api = client(c).await;
    let e = api
        .query::<_, Value>(Method::GET, "/api/me", &())
        .await
        .unwrap_err();
    assert!(
        matches!(e, Error::Api(ref a) if a.is(ErrorKind::Unauthenticated)),
        "{e:?}"
    );
}

#[tokio::test]
async fn discovery_url_from_host_metadata_drives_m2m() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/.well-known/databricks-config"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "oidc_endpoint": format!("{}/oidc/accounts/{{account_id}}", server.uri()),
            "account_id": "acc-1",
            "host_type": "UNIFIED_HOST"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(
            "/oidc/accounts/acc-1/.well-known/oauth-authorization-server",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token_endpoint": format!("{}/oidc/accounts/acc-1/v1/token", server.uri()),
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oidc/accounts/acc-1/v1/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"access_token": "u"})))
        .mount(&server)
        .await;
    let mut c = Config::with_host(server.uri()).client_credentials("a", "b");
    c.rate_limit = Some(1000);
    let api = client(c).await;
    assert_eq!(api.authenticate().await.unwrap(), "oauth-m2m");
}

#[tokio::test]
async fn account_endpoints_are_fixed_paths() {
    let c = Config::with_host("https://accounts.cloud.databricks.com").account("acc-9");
    let mut c = c;
    c.host_metadata = Some(HostMetadata::default());
    let c = c.resolve_with(|_| None, None).await.unwrap();
    let e = OAuthEndpoints::discover(&c, &reqwest::Client::new())
        .await
        .unwrap();
    assert_eq!(
        e.token_endpoint,
        "https://accounts.cloud.databricks.com/oidc/accounts/acc-9/v1/token"
    );
    assert_eq!(
        e.authorization_endpoint,
        "https://accounts.cloud.databricks.com/oidc/accounts/acc-9/v1/authorize"
    );
    let mut no_account = c.clone();
    no_account.account_id = None;
    assert!(
        OAuthEndpoints::discover(&no_account, &reqwest::Client::new())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn auth_selection_errors() {
    let server = MockServer::start().await;

    // Nothing configured.
    let api = client(cfg(&server)).await;
    let e = api.authenticate().await.unwrap_err();
    assert!(
        e.to_string()
            .contains("cannot configure default credentials"),
        "{e}"
    );

    // Explicit type that isn't configured.
    let mut c = cfg(&server);
    c.auth_type = Some("oauth-m2m".into());
    let e = client(c).await.authenticate().await.unwrap_err();
    assert!(
        e.to_string().starts_with("oauth-m2m auth: not configured"),
        "{e}"
    );

    // A Go auth type deliberately not ported gets a clear hint.
    let mut c = cfg(&server);
    c.auth_type = Some("basic".into());
    let e = client(c).await.authenticate().await.unwrap_err();
    assert!(e.to_string().contains("not ported to Rust"), "{e}");

    // auth_type picks PAT even when M2M is also configured.
    let mut c = cfg(&server).token("t").client_credentials("a", "b");
    c.auth_type = Some("pat".into());
    assert_eq!(client(c).await.authenticate().await.unwrap(), "pat");

    // M2M discovery failure is wrapped as an auth error.
    let mut c = cfg(&server).client_credentials("a", "b");
    c.auth_type = Some("oauth-m2m".into());
    let e = client(c).await.authenticate().await.unwrap_err();
    assert!(
        matches!(e, Error::Auth { ref message, .. } if message.contains("oidc")),
        "{e}"
    );
}

#[tokio::test]
async fn no_host_is_a_config_error() {
    let mut c = Config::default();
    c.host_metadata = Some(HostMetadata::default());
    let c = c.resolve_with(|_| None, None).await.unwrap();
    assert!(matches!(
        ApiClient::from_resolved(c, DefaultCredentials::default()),
        Err(Error::Config(_))
    ));
}

#[tokio::test]
async fn decode_errors_are_reported() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&server)
        .await;
    let api = client(cfg(&server).token("t")).await;
    let e = api
        .query::<_, Value>(Method::GET, "/api/j", &())
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Json { .. }), "{e:?}");
    let raw = api
        .execute_raw(Method::GET, "/api/j", &[], None)
        .await
        .unwrap();
    assert_eq!(&raw[..], b"not json");
}

mod custom_chain {
    use std::sync::Arc;

    use community_databricks_core::auth::{
        CredentialsProvider, CredentialsStrategy, DefaultCredentials, Headers, PatCredentials,
    };
    use community_databricks_core::{Config, Error, Result};
    use futures_util::future::BoxFuture;

    use super::{MockServer, cfg};

    struct Broken;
    impl CredentialsStrategy for Broken {
        fn name(&self) -> &'static str {
            "broken"
        }
        fn configure<'a>(
            &'a self,
            _: &'a Config,
            _: &'a reqwest::Client,
        ) -> BoxFuture<'a, Result<Option<Arc<dyn CredentialsProvider>>>> {
            Box::pin(async { Err(Error::Config("nope".into())) })
        }
    }

    #[derive(Debug)]
    struct NoHeaders;
    impl CredentialsProvider for NoHeaders {
        fn headers(&self) -> BoxFuture<'_, Result<Headers>> {
            Box::pin(async { Err(Error::OperationFailed("no token cached".into())) })
        }
    }

    struct DryRunFails;
    impl CredentialsStrategy for DryRunFails {
        fn name(&self) -> &'static str {
            "dry"
        }
        fn configure<'a>(
            &'a self,
            _: &'a Config,
            _: &'a reqwest::Client,
        ) -> BoxFuture<'a, Result<Option<Arc<dyn CredentialsProvider>>>> {
            Box::pin(async { Ok(Some(Arc::new(NoHeaders) as Arc<dyn CredentialsProvider>)) })
        }
    }

    fn chain() -> DefaultCredentials {
        DefaultCredentials::new(vec![
            Box::new(Broken),
            Box::new(DryRunFails),
            Box::new(PatCredentials),
        ])
    }

    async fn resolved(c: Config) -> Config {
        c.resolve_with(|_| None, None).await.unwrap()
    }

    #[tokio::test]
    async fn failing_strategies_are_skipped() {
        let server = MockServer::start().await;
        let c = resolved(cfg(&server).token("t")).await;
        let (name, _) = chain()
            .configure(&c, &reqwest::Client::new())
            .await
            .unwrap();
        assert_eq!(name, "pat");
    }

    #[tokio::test]
    async fn explicit_type_errors_become_auth_errors() {
        let server = MockServer::start().await;
        let mut c = cfg(&server);
        c.auth_type = Some("broken".into());
        let e = chain()
            .configure(&resolved(c).await, &reqwest::Client::new())
            .await
            .unwrap_err();
        assert!(
            matches!(e, Error::Auth { ref auth_type, .. } if auth_type == "broken"),
            "{e}"
        );

        let mut c = cfg(&server);
        c.auth_type = Some("bogus".into());
        let e = chain()
            .configure(&resolved(c).await, &reqwest::Client::new())
            .await
            .unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("\"bogus\" not found, please check"), "{msg}");
    }

    #[tokio::test]
    async fn pat_requires_a_host() {
        let mut c = Config::default().token("t");
        c.host_metadata = Some(community_databricks_core::config::HostMetadata::default());
        let c = resolved(c).await;
        let e = PatCredentials
            .configure(&c, &reqwest::Client::new())
            .await
            .unwrap_err();
        assert!(e.to_string().contains("host is required"), "{e}");
    }
}

#[test]
fn response_header_parsing() {
    use community_databricks_core::http::header;
    use reqwest::header::{HeaderMap, HeaderValue};
    let mut h = HeaderMap::new();
    h.insert("content-length", HeaderValue::from_static(" 1234 "));
    h.insert("x-bad", HeaderValue::from_static("abc"));
    assert_eq!(header::<i64>(&h, "content-length"), Some(1234));
    assert_eq!(
        header::<String>(&h, "content-length").as_deref(),
        Some("1234")
    );
    assert_eq!(header::<i64>(&h, "x-bad"), None);
    assert_eq!(header::<i64>(&h, "missing"), None);
}

#[test]
fn path_param_escaping_by_segment_type() {
    use community_databricks_core::http::path_param;
    // Single segment: `/` is data (databricks-sdk-go#1765).
    assert_eq!(path_param("main.sch.tbl/col", false), "main.sch.tbl%2Fcol");
    assert_eq!(path_param("a#b?c d", false), "a%23b%3Fc%20d");
    assert_eq!(path_param("café", false), "caf%C3%A9");
    // Multi segment: `/` separates, each segment escaped.
    assert_eq!(
        path_param("projects/p1/branches/b 1", true),
        "projects/p1/branches/b%201"
    );
    assert_eq!(path_param("/a//b/", true), "/a//b/");
    assert_eq!(path_param("", false), "");
}
