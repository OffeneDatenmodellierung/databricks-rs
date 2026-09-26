//! End-to-end tests for the auth strategies beyond PAT and M2M:
//! `databricks-cli`, the OIDC/WIF family (`github-oidc`, `env-oidc`,
//! `file-oidc`, `mem-oidc`), `azure-msi` and `oauth-m2m-gcp`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use databricks_core::auth::{DefaultCredentials, IdToken, IdTokenFn};
use databricks_core::config::HostMetadata;
use databricks_core::{ApiClient, Config};
use reqwest::Method;
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cfg(server: &MockServer) -> Config {
    let mut c = Config::with_host(server.uri());
    c.host_metadata = Some(HostMetadata::default());
    c.rate_limit = Some(1000);
    c
}

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + Send + Sync + use<> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    move |k| map.get(k).cloned()
}

async fn client(c: Config, e: &[(&str, &str)]) -> ApiClient {
    let c = c.resolve_with(env(e), None).await.unwrap();
    ApiClient::from_resolved(c, DefaultCredentials::default()).unwrap()
}

/// Serve `/api/me`, requiring `Authorization: Bearer <token>`.
async fn mount_api(server: &MockServer, token: &str) {
    Mock::given(method("GET"))
        .and(path("/api/me"))
        .and(header("authorization", format!("Bearer {token}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!("ok")))
        .mount(server)
        .await;
}

async fn call(api: &ApiClient) {
    let v: Value = api.query(Method::GET, "/api/me", &()).await.unwrap();
    assert_eq!(v, json!("ok"));
}

/// Workspace OIDC discovery plus a token endpoint that expects a token
/// exchange for `subject`.
async fn mount_exchange(server: &MockServer, subject: &str, extra: &[&str]) {
    Mock::given(method("GET"))
        .and(path("/oidc/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token_endpoint": format!("{}/oidc/v1/token", server.uri()),
        })))
        .mount(server)
        .await;
    let mut m = Mock::given(method("POST"))
        .and(path("/oidc/v1/token"))
        .and(body_string_contains(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Atoken-exchange",
        ))
        .and(body_string_contains(
            "subject_token_type=urn%3Aietf%3Aparams%3Aoauth%3Atoken-type%3Ajwt",
        ))
        .and(body_string_contains(format!("subject_token={subject}&")))
        .and(body_string_contains("scope=all-apis"));
    for e in extra {
        m = m.and(body_string_contains(*e));
    }
    m.respond_with(ResponseTemplate::new(200).set_body_json(json!({
        "access_token": "wif-token", "token_type": "Bearer", "expires_in": 3600,
    })))
    .expect(1)
    .mount(server)
    .await;
}

#[tokio::test]
async fn env_oidc_exchanges_the_variable_with_group_and_client_id() {
    let server = MockServer::start().await;
    mount_exchange(&server, "env-jwt", &["client_id=sp-1", "assume_group=grp"]).await;
    mount_api(&server, "wif-token").await;
    let mut c = cfg(&server);
    c.client_id = Some("sp-1".into());
    c.group_id = Some("grp".into());
    let api = client(c, &[("DATABRICKS_OIDC_TOKEN", "env-jwt")]).await;
    call(&api).await;
    assert_eq!(api.auth_type(), Some("env-oidc"));
}

#[tokio::test]
async fn file_oidc_reads_the_file_and_account_wide_federation_omits_client_id() {
    let server = MockServer::start().await;
    mount_exchange(&server, "file-jwt", &[]).await;
    mount_api(&server, "wif-token").await;
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("jwt");
    std::fs::write(&f, "file-jwt").unwrap();
    let mut c = cfg(&server);
    c.set_attribute("databricks_id_token_filepath", f.to_string_lossy())
        .unwrap();
    let api = client(c, &[]).await;
    call(&api).await;
    assert_eq!(api.auth_type(), Some("file-oidc"));
    let reqs = server.received_requests().await.unwrap();
    let exchange = reqs
        .iter()
        .find(|r| r.url.path() == "/oidc/v1/token")
        .unwrap();
    assert!(!String::from_utf8_lossy(&exchange.body).contains("client_id"));
}

#[tokio::test]
async fn github_oidc_requests_an_id_token_for_the_audience() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/gh"))
        .and(query_param("x", "1"))
        .and(query_param("audience", "my-aud"))
        .and(header("authorization", "Bearer gh-request-token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"count": 1, "value": "gh-jwt"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    mount_exchange(&server, "gh-jwt", &[]).await;
    mount_api(&server, "wif-token").await;
    let mut c = cfg(&server);
    c.set_attribute("audience", "my-aud").unwrap();
    let url = format!("{}/gh?x=1", server.uri());
    let api = client(
        c,
        &[
            ("ACTIONS_ID_TOKEN_REQUEST_URL", &url),
            ("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "gh-request-token"),
        ],
    )
    .await;
    call(&api).await;
    assert_eq!(api.auth_type(), Some("github-oidc"));
}

#[tokio::test]
async fn mem_oidc_uses_the_in_memory_source_and_default_audience() {
    let server = MockServer::start().await;
    mount_exchange(&server, "mem-jwt", &[]).await;
    mount_api(&server, "wif-token").await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let c = cfg(&server).id_tokens(IdTokenFn(move |audience: String| {
        log.lock().unwrap().push(audience);
        async { Ok(IdToken::new("mem-jwt")) }
    }));
    assert!(format!("{c:?}").contains("IdTokenFn"));
    let api = client(c, &[]).await;
    call(&api).await;
    assert_eq!(api.auth_type(), Some("mem-oidc"));
    // Workspace host with no configured audience: the token endpoint.
    assert_eq!(
        *seen.lock().unwrap(),
        [format!("{}/oidc/v1/token", server.uri())]
    );
}

#[tokio::test]
async fn mem_oidc_on_an_account_host_uses_the_account_id_as_audience() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oidc/accounts/acc-1/v1/token"))
        .and(body_string_contains("subject_token=static-jwt&"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token": "acc-token", "expires_in": 60})),
        )
        .mount(&server)
        .await;
    let seen = Arc::new(Mutex::new(String::new()));
    let log = Arc::clone(&seen);
    let mut c = cfg(&server)
        .account("acc-1")
        .id_tokens(IdTokenFn(move |aud: String| {
            *log.lock().unwrap() = aud;
            async { Ok(IdToken::new("static-jwt")) }
        }));
    c.host_metadata = Some(serde_json::from_value(json!({"host_type": "ACCOUNT_HOST"})).unwrap());
    let api = client(c, &[]).await;
    assert_eq!(api.authenticate().await.unwrap(), "mem-oidc");
    assert_eq!(*seen.lock().unwrap(), "acc-1");
    assert!(format!("{:?}", IdToken::new("secret")).contains("***"));
}

#[tokio::test]
async fn explicit_oidc_auth_types_report_what_is_missing() {
    let server = MockServer::start().await;
    for (auth_type, want) in [
        ("env-oidc", "missing env var \"DATABRICKS_OIDC_TOKEN\""),
        ("file-oidc", "missing path"),
        ("github-oidc", "missing ActionsIDTokenRequestURL"),
    ] {
        let mut c = cfg(&server);
        c.auth_type = Some(auth_type.into());
        let e = client(c, &[]).await.authenticate().await.unwrap_err();
        assert!(e.to_string().contains(want), "{auth_type}: {e}");
    }
    Mock::given(method("GET"))
        .and(path("/oidc/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"token_endpoint": "x"})))
        .mount(&server)
        .await;
    let mut c = cfg(&server);
    c.auth_type = Some("file-oidc".into());
    c.set_attribute("databricks_id_token_filepath", "/no/such/file")
        .unwrap();
    let e = client(c, &[]).await.authenticate().await;
    assert!(e.unwrap_err().to_string().contains("does not exist"));
    let mut c = cfg(&server);
    c.auth_type = Some("mem-oidc".into());
    let e = client(c, &[]).await.authenticate().await.unwrap_err();
    assert!(e.to_string().contains("not configured"), "{e}");
}

#[tokio::test]
async fn azure_devops_oidc_exchanges_the_pipeline_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(
            "/org/proj-1/_apis/distributedtask/hubs/build/plans/plan-1/jobs/job-1/oidctoken",
        ))
        .and(query_param("api-version", "7.2-preview.1"))
        .and(header("authorization", "Bearer ado-access"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"oidcToken": "ado-jwt"})))
        .expect(1)
        .mount(&server)
        .await;
    mount_exchange(&server, "ado-jwt", &["client_id=sp-ado"]).await;
    mount_api(&server, "wif-token").await;
    let mut c = cfg(&server);
    c.client_id = Some("sp-ado".into());
    let collection = format!("{}/org/", server.uri());
    let api = client(
        c,
        &[
            ("SYSTEM_ACCESSTOKEN", "ado-access"),
            ("SYSTEM_TEAMFOUNDATIONCOLLECTIONURI", &collection),
            ("SYSTEM_TEAMPROJECTID", "proj-1"),
            ("SYSTEM_HOSTTYPE", "build"),
            ("SYSTEM_PLANID", "plan-1"),
            ("SYSTEM_JOBID", "job-1"),
        ],
    )
    .await;
    call(&api).await;
    assert_eq!(api.auth_type(), Some("azure-devops-oidc"));

    // Outside a pipeline the explicit type says what is missing.
    let mut c = cfg(&server);
    c.auth_type = Some("azure-devops-oidc".into());
    let e = client(c.clone(), &[])
        .await
        .authenticate()
        .await
        .unwrap_err();
    assert!(e.to_string().contains("SYSTEM_ACCESSTOKEN"), "{e}");
    let e = client(c, &[("SYSTEM_ACCESSTOKEN", "x")])
        .await
        .authenticate()
        .await
        .unwrap_err();
    assert!(
        e.to_string()
            .contains("missing env var SYSTEM_TEAMFOUNDATIONCOLLECTIONURI"),
        "{e}"
    );
}

#[tokio::test]
async fn azure_types_select_only_on_azure_and_refuse_group_roles() {
    let server = MockServer::start().await;
    // Explicitly selected on a non-Azure host: not configured.
    for t in [
        "azure-msi",
        "azure-client-secret",
        "github-oidc-azure",
        "azure-cli",
    ] {
        let mut c = cfg(&server);
        c.auth_type = Some(t.into());
        c.set_attribute("azure_use_msi", "true").unwrap();
        let e = client(c, &[]).await.authenticate().await.unwrap_err();
        assert!(e.to_string().contains("not configured"), "{t}: {e}");
    }
    // Managed identity builds without a network call; tokens are fetched
    // on the first request.
    let mut c = cfg(&server);
    c.auth_type = Some("azure-msi".into());
    c.cloud = Some("AZURE".into());
    c.set_attribute("azure_use_msi", "1").unwrap();
    assert_eq!(
        client(c.clone(), &[]).await.authenticate().await.unwrap(),
        "azure-msi"
    );
    c.group_id = Some("g".into());
    let e = client(c, &[]).await.authenticate().await.unwrap_err();
    assert!(e.to_string().contains("group role"), "{e}");
}

#[tokio::test]
async fn oauth_m2m_gcp_adds_the_google_access_token() {
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
        .and(body_string_contains("grant_type=client_credentials"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"access_token": "dbx", "expires_in": 3600})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/google/token"))
        .and(body_string_contains("\"grant_type\":\"refresh_token\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"access_token": "ya29.g", "expires_in": 3600, "token_type": "Bearer"}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/me"))
        .and(header("authorization", "Bearer dbx"))
        .and(header("x-databricks-gcp-sa-access-token", "ya29.g"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!("ok")))
        .expect(1)
        .mount(&server)
        .await;
    let google = json!({
        "type": "authorized_user", "client_id": "c", "client_secret": "s",
        "refresh_token": "r", "token_uri": format!("{}/google/token", server.uri()),
    })
    .to_string();
    let mut c = cfg(&server).client_credentials("sp", "secret");
    c.cloud = Some("GCP".into());
    c.set_attribute("google_credentials", google.clone())
        .unwrap();

    // oauth + google attributes conflict unless the auth type is explicit.
    let e = c.clone().resolve_with(env(&[]), None).await.unwrap_err();
    assert!(
        e.to_string().contains("more than one authorization method"),
        "{e}"
    );

    c.auth_type = Some("oauth-m2m-gcp".into());
    let api = client(c, &[]).await;
    call(&api).await;
    assert_eq!(api.auth_type(), Some("oauth-m2m-gcp"));
}

#[tokio::test]
async fn google_types_need_gcp_and_valid_credentials() {
    let server = MockServer::start().await;
    let mut c = cfg(&server).client_credentials("sp", "secret");
    c.auth_type = Some("oauth-m2m-gcp".into());
    // Not GCP: not configured.
    let e = client(c.clone(), &[])
        .await
        .authenticate()
        .await
        .unwrap_err();
    assert!(e.to_string().contains("not configured"), "{e}");
    // Unparseable or unsupported credentials fail before any network call.
    c.cloud = Some("GCP".into());
    c.set_attribute("google_credentials", "not json").unwrap();
    let e = client(c.clone(), &[])
        .await
        .authenticate()
        .await
        .unwrap_err();
    assert!(
        e.to_string().contains("could not read GoogleCredentials"),
        "{e}"
    );
    c.set_attribute("google_credentials", "{\"type\":\"gdch_service_account\"}")
        .unwrap();
    let e = client(c, &[]).await.authenticate().await.unwrap_err();
    assert!(
        e.to_string()
            .contains("unsupported Google credentials type"),
        "{e}"
    );

    // google-credentials: user credentials have no ID token for the host.
    let mut c = cfg(&server);
    c.auth_type = Some("google-credentials".into());
    c.cloud = Some("GCP".into());
    c.set_attribute(
        "google_credentials",
        json!({"type": "authorized_user", "client_id": "c", "client_secret": "s", "refresh_token": "r"}).to_string(),
    )
    .unwrap();
    let e = client(c.clone(), &[])
        .await
        .authenticate()
        .await
        .unwrap_err();
    assert!(e.to_string().contains("cannot mint ID tokens"), "{e}");
    c.group_id = Some("g".into());
    let e = client(c, &[]).await.authenticate().await.unwrap_err();
    assert!(e.to_string().contains("group role"), "{e}");

    // google-id: needs GCP and a service account to impersonate.
    let mut c = cfg(&server);
    c.auth_type = Some("google-id".into());
    c.set_attribute("google_service_account", "sa@p.iam.gserviceaccount.com")
        .unwrap();
    let e = client(c.clone(), &[])
        .await
        .authenticate()
        .await
        .unwrap_err();
    assert!(e.to_string().contains("not configured"), "{e}");
    c.group_id = Some("g".into());
    let e = client(c, &[]).await.authenticate().await.unwrap_err();
    assert!(e.to_string().contains("group role"), "{e}");
}

#[cfg(unix)]
mod cli {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// A fake `databricks` binary: a shell script padded past the 1 MiB
    /// legacy-CLI threshold. It logs its arguments next to itself.
    fn fake_cli(dir: &std::path::Path, version: &str, token_json: &str) {
        let log = dir.join("args.log");
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = version ]; then echo 'Databricks CLI {version}'; exit 0; fi\n\
             echo \"$@\" >> '{}'\ncat <<'EOF'\n{token_json}\nEOF\nexit 0\n",
            log.display()
        );
        let mut body = script.into_bytes();
        body.resize(1024 * 1024 + 16, b'#');
        let p = dir.join("databricks");
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[tokio::test]
    async fn databricks_cli_token_is_used_with_capability_flags() {
        let server = MockServer::start().await;
        mount_api(&server, "cli-token").await;
        let dir = tempfile::tempdir().unwrap();
        fake_cli(
            dir.path(),
            "v0.300.0",
            r#"{"access_token":"cli-token","token_type":"Bearer","expiry":"2099-01-01T00:00:00.123Z"}"#,
        );
        let path = dir.path().to_string_lossy().into_owned();
        let api = client(cfg(&server), &[("PATH", &path)]).await;
        call(&api).await;
        assert_eq!(api.auth_type(), Some("databricks-cli"));
        let args = std::fs::read_to_string(dir.path().join("args.log")).unwrap();
        assert_eq!(
            args.trim(),
            format!("auth token --host {} --force-refresh", server.uri())
        );
    }

    #[tokio::test]
    async fn databricks_cli_failures() {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_string_lossy().into_owned();
        let explicit = |c: &mut Config| c.auth_type = Some("databricks-cli".into());

        // Not installed.
        let mut c = cfg(&server);
        explicit(&mut c);
        let e = client(c, &[("PATH", &path)])
            .await
            .authenticate()
            .await
            .unwrap_err();
        assert!(e.to_string().contains("databricks CLI not found"), "{e}");

        // The legacy Python CLI is a small script.
        std::fs::write(dir.path().join("databricks"), "#!/bin/sh\n").unwrap();
        let mut c = cfg(&server);
        explicit(&mut c);
        let e = client(c, &[("PATH", &path)])
            .await
            .authenticate()
            .await
            .unwrap_err();
        assert!(e.to_string().contains("legacy databricks CLI"), "{e}");

        // Unparseable output, custom scopes, group roles.
        fake_cli(dir.path(), "v0.300.0", "not json");
        let mut c = cfg(&server);
        explicit(&mut c);
        let e = client(c, &[("PATH", &path)])
            .await
            .authenticate()
            .await
            .unwrap_err();
        assert!(e.to_string().contains("cannot parse CLI response"), "{e}");
        let mut c = cfg(&server);
        explicit(&mut c);
        c.scopes = vec!["sql".into()];
        let e = client(c, &[("PATH", &path)])
            .await
            .authenticate()
            .await
            .unwrap_err();
        assert!(e.to_string().contains("custom scopes"), "{e}");
        let mut c = cfg(&server);
        explicit(&mut c);
        c.group_id = Some("g".into());
        let e = client(c, &[("PATH", &path)])
            .await
            .authenticate()
            .await
            .unwrap_err();
        assert!(e.to_string().contains("group role"), "{e}");

        // A CLI path given explicitly, with an unparseable expiry.
        fake_cli(
            dir.path(),
            "v0.1.0",
            r#"{"access_token":"t","expiry":"soon"}"#,
        );
        let mut c = cfg(&server);
        explicit(&mut c);
        c.set_attribute(
            "databricks_cli_path",
            dir.path().join("databricks").to_string_lossy(),
        )
        .unwrap();
        let e = client(c, &[]).await.authenticate().await.unwrap_err();
        assert!(e.to_string().contains("cannot parse token expiry"), "{e}");
    }
}
