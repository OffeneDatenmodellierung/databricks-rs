//! Config resolution: env, config file, validation, host normalisation and
//! host metadata. Behaviour is checked against databricks-sdk-go v0.182.0.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use databricks_core::config::{HostMetadata, HostType, Source};
use databricks_core::{Config, Error};
use pretty_assertions::assert_eq;
use secrecy::ExposeSecret;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    move |k| map.get(k).cloned()
}

/// A config that never touches the network for host metadata.
fn offline() -> Config {
    let mut c = Config::default();
    c.host_metadata = Some(HostMetadata::default());
    c
}

fn home_with(cfg: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".databrickscfg"), cfg).unwrap();
    let home = dir.path().to_path_buf();
    (dir, home)
}

async fn resolve(c: Config, e: &[(&str, &str)], home: Option<&Path>) -> Result<Config, Error> {
    c.resolve_with(env(e), home.map(Path::to_path_buf)).await
}

#[tokio::test]
async fn env_vars_fill_unset_attributes_and_are_reported() {
    let c = resolve(
        offline(),
        &[
            ("DATABRICKS_HOST", "adb-1.2.azuredatabricks.net"),
            ("DATABRICKS_TOKEN", "dapi123"),
        ],
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        c.host.as_deref(),
        Some("https://adb-1.2.azuredatabricks.net")
    );
    assert_eq!(c.token.as_ref().unwrap().expose_secret(), "dapi123");
    assert_eq!(c.source_of("token"), Some(&Source::Env("DATABRICKS_TOKEN")));
    assert_eq!(
        c.debug_string(),
        "Config: host=https://adb-1.2.azuredatabricks.net, token=***. Env: DATABRICKS_HOST, DATABRICKS_TOKEN"
    );
}

#[tokio::test]
async fn code_wins_over_env() {
    let mut c = offline();
    c.host = Some("https://code.example".into());
    let c = resolve(c, &[("DATABRICKS_HOST", "https://env.example")], None)
        .await
        .unwrap();
    assert_eq!(c.host.as_deref(), Some("https://code.example"));
    assert_eq!(c.source_of("host"), Some(&Source::Code));
}

#[tokio::test]
async fn default_profile_is_used_when_nothing_is_set() {
    let (_d, home) = home_with("[DEFAULT]\nhost = https://default.example\ntoken = t1 # comment\n");
    let c = resolve(offline(), &[], Some(&home)).await.unwrap();
    assert_eq!(c.host.as_deref(), Some("https://default.example"));
    assert_eq!(c.token.as_ref().unwrap().expose_secret(), "t1");
    assert_eq!(c.profile.as_deref(), Some("DEFAULT"));
    assert!(matches!(c.source_of("host"), Some(Source::File(p)) if p.ends_with(".databrickscfg")));
}

#[tokio::test]
async fn named_profile_from_env_and_settings_default_profile() {
    let file = "[__settings__]\ndefault_profile = dev\n\n[dev]\nhost = https://dev.example\ntoken = d\n\n[prod]\nhost = https://prod.example\nclient_id = id\nclient_secret = s\nscopes = sql, all-apis ,sql\n";
    let (_d, home) = home_with(file);

    let c = resolve(offline(), &[], Some(&home)).await.unwrap();
    assert_eq!(c.profile.as_deref(), Some("dev"));
    assert_eq!(c.host.as_deref(), Some("https://dev.example"));

    let c = resolve(
        offline(),
        &[("DATABRICKS_CONFIG_PROFILE", "prod")],
        Some(&home),
    )
    .await
    .unwrap();
    assert_eq!(c.client_id.as_deref(), Some("id"));
    assert_eq!(c.scopes, vec!["all-apis".to_owned(), "sql".to_owned()]);
}

#[tokio::test]
async fn missing_profiles() {
    let (_d, home) = home_with("[other]\nhost = https://x\n");
    // Missing DEFAULT is fine…
    let c = resolve(offline(), &[], Some(&home)).await.unwrap();
    assert!(c.host.is_none());
    // …a missing requested profile is not.
    let mut cfg = offline();
    cfg.profile = Some("nope".into());
    let e = resolve(cfg, &[], Some(&home)).await.unwrap_err();
    assert!(
        e.to_string().contains("has no nope profile configured"),
        "{e}"
    );
    // The settings section is reserved.
    let mut cfg = offline();
    cfg.profile = Some("__settings__".into());
    let e = resolve(cfg, &[], Some(&home)).await.unwrap_err();
    assert!(e.to_string().contains("reserved section name"), "{e}");
}

#[tokio::test]
async fn file_is_skipped_when_host_or_auth_already_set() {
    let (_d, home) = home_with("[DEFAULT]\nhost = https://file.example\ntoken = f\n");
    let c = resolve(
        offline(),
        &[("DATABRICKS_HOST", "https://env.example")],
        Some(&home),
    )
    .await
    .unwrap();
    assert_eq!(c.host.as_deref(), Some("https://env.example"));
    assert!(c.token.is_none());
}

#[tokio::test]
async fn missing_file_and_home_are_not_errors() {
    let dir = tempfile::tempdir().unwrap();
    assert!(resolve(offline(), &[], Some(dir.path())).await.is_ok());
    assert!(resolve(offline(), &[], None).await.is_ok());
    let mut c = offline();
    c.config_file = Some(dir.path().join("absent").display().to_string());
    assert!(resolve(c, &[], None).await.is_ok());
}

#[tokio::test]
async fn unparsable_values_are_reported() {
    let (_d, home) = home_with("[DEFAULT]\nhost = https://x\nrate_limit = fast\n");
    let e = resolve(offline(), &[], Some(&home)).await.unwrap_err();
    assert!(e.to_string().contains("rate_limit"), "{e}");
    let e = resolve(offline(), &[("DATABRICKS_DEBUG_HEADERS", "maybe")], None)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("debug_headers"), "{e}");
}

#[tokio::test]
async fn more_than_one_auth_method_is_rejected_unless_auth_type_is_set() {
    let e = resolve(
        offline(),
        &[
            ("DATABRICKS_HOST", "https://x.example"),
            ("DATABRICKS_TOKEN", "t"),
            ("DATABRICKS_CLIENT_ID", "id"),
            ("ARM_CLIENT_SECRET", "s"),
        ],
        None,
    )
    .await
    .unwrap_err();
    let msg = e.to_string();
    assert!(
        msg.contains("more than one authorization method configured: azure and oauth and pat"),
        "{msg}"
    );
    assert!(msg.contains("azure_client_secret=***"), "{msg}");

    let ok = resolve(
        offline(),
        &[
            ("DATABRICKS_TOKEN", "t"),
            ("DATABRICKS_CLIENT_ID", "id"),
            ("DATABRICKS_AUTH_TYPE", "pat"),
        ],
        None,
    )
    .await;
    assert!(ok.is_ok());
}

#[tokio::test]
async fn host_normalisation_lifts_ids_from_query() {
    let mut c = offline();
    c.host = Some("adb-1.2.azuredatabricks.net/some/path?o=123456".into());
    let c = resolve(c, &[], None).await.unwrap();
    assert_eq!(
        c.host.as_deref(),
        Some("https://adb-1.2.azuredatabricks.net")
    );
    assert_eq!(c.workspace_id.as_deref(), Some("123456"));

    let mut c = offline();
    c.host = Some("https://unified.example:8443/?a=acc-1&w=ws-9".into());
    let c = resolve(c, &[], None).await.unwrap();
    assert_eq!(c.host.as_deref(), Some("https://unified.example:8443"));
    assert_eq!(c.account_id.as_deref(), Some("acc-1"));
    assert_eq!(c.workspace_id.as_deref(), Some("ws-9"));

    // Non-numeric ?o= is ignored.
    let mut c = offline();
    c.host = Some("https://x.example/?o=abc".into());
    assert!(resolve(c, &[], None).await.unwrap().workspace_id.is_none());

    let mut c = offline();
    c.host = Some("https://".into());
    assert!(resolve(c, &[], None).await.is_err());
}

#[tokio::test]
async fn host_type_inference() {
    for (host, want) in [
        ("https://accounts.cloud.databricks.com", HostType::Account),
        ("accounts-dod.cloud.databricks.us", HostType::Account),
        ("https://adb-1.azuredatabricks.net", HostType::Workspace),
    ] {
        let mut c = offline();
        c.host = Some(host.into());
        let c = resolve(c, &[], None).await.unwrap();
        assert_eq!(c.host_type(), want, "{host}");
        assert_eq!(c.is_account_client(), want == HostType::Account);
    }
}

#[tokio::test]
async fn host_metadata_backfills_ids_and_discovery_url() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/.well-known/databricks-config"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "oidc_endpoint": "https://login.example/oidc/accounts/{account_id}/",
            "account_id": "acc-42",
            "workspace_id": "777",
            "cloud": "AWS",
            "host_type": "UNIFIED_HOST"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let c = resolve(Config::with_host(server.uri()), &[], None)
        .await
        .unwrap();
    assert_eq!(c.account_id.as_deref(), Some("acc-42"));
    assert_eq!(c.workspace_id.as_deref(), Some("777"));
    assert_eq!(c.cloud.as_deref(), Some("AWS"));
    assert_eq!(c.host_type(), HostType::Unified);
    assert_eq!(
        c.discovery_url.as_deref(),
        Some("https://login.example/oidc/accounts/acc-42/.well-known/oauth-authorization-server")
    );
    assert_eq!(c.source_of("account_id"), Some(&Source::HostMetadata));
}

#[tokio::test]
async fn host_metadata_failure_is_not_fatal() {
    let server = MockServer::start().await; // every path 404s
    let c = resolve(Config::with_host(server.uri()).token("t"), &[], None)
        .await
        .unwrap();
    assert!(c.account_id.is_none());
    assert_eq!(c.host_type(), HostType::Workspace);
}

#[tokio::test]
async fn timeouts_and_attribute_access() {
    let mut c = offline();
    assert_eq!(c.retry_timeout(), Some(std::time::Duration::from_mins(5)));
    c.retry_timeout_seconds = Some(-1);
    assert_eq!(c.retry_timeout(), None);
    c.retry_timeout_seconds = Some(7);
    assert_eq!(c.retry_timeout(), Some(std::time::Duration::from_secs(7)));
    assert_eq!(c.http_timeout(), std::time::Duration::from_mins(1));
    assert_eq!(c.scopes_or_default(), vec!["all-apis".to_owned()]);

    c.set_attribute("warehouse_id", "wh").unwrap();
    c.set_attribute("skip_verify", "true").unwrap();
    c.set_attribute("http_timeout_seconds", "5").unwrap();
    assert_eq!(c.attribute("warehouse_id").as_deref(), Some("wh"));
    assert_eq!(c.attribute("skip_verify").as_deref(), Some("true"));
    assert_eq!(c.http_timeout(), std::time::Duration::from_secs(5));
    assert!(c.set_attribute("nope", "x").is_err());
    let c = Config::with_host("h")
        .client_credentials("id", "sec")
        .account("a");
    assert_eq!(c.account_id.as_deref(), Some("a"));
}
