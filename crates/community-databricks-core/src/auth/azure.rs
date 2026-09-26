//! Azure auth: `azure-msi`, `azure-client-secret`, `azure-cli` and
//! `github-oidc-azure`.
//!
//! Tokens come from Microsoft's `azure_identity` crate. This module only
//! adds what is specific to Databricks (Go: `auth_azure_*.go`):
//!
//! * the Databricks application ID as the token resource, and the Azure
//!   cloud matching the workspace (public, US Government, China);
//! * `X-Databricks-Azure-SP-Management-Token` and
//!   `X-Databricks-Azure-Workspace-Resource-Id`;
//! * tenant discovery from `{host}/aad/auth` for the Azure CLI;
//! * resolving `host` from `azure_workspace_resource_id` through ARM.
//!
//! `azure_identity` 1.0 supports managed identity on App Service /
//! Functions, VMs (IMDS) and AKS (workload identity). Azure Arc, Azure ML,
//! Cloud Shell and Service Fabric are detected but return an "isn't
//! supported" error from the crate (databricks-sdk-go#1813 handles them;
//! they arrive here when `azure_identity` adds them).

// `..Default::default()` on azure_identity's option structs keeps this
// compiling when a minor release adds fields.
#![allow(clippy::needless_update)]

use std::sync::Arc;

use azure_core::cloud::CloudConfiguration;
use azure_core::credentials::{Secret, TokenCredential};
use azure_core::http::{ClientMethodOptions, ClientOptions};
use azure_identity::{
    AzureCliCredential, AzureCliCredentialOptions, ClientAssertion, ClientAssertionCredential,
    ClientAssertionCredentialOptions, ClientSecretCredential, ClientSecretCredentialOptions,
    Executor, ManagedIdentityCredential, ManagedIdentityCredentialOptions, UserAssignedId,
    WorkloadIdentityCredential, WorkloadIdentityCredentialOptions,
};
use futures_util::future::BoxFuture;
use reqwest::header::{HeaderName, HeaderValue, LOCATION};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;

use super::common::{Secondary, TokenHeaders, instant_from_unix, send_token_request};
use super::token::{Token, TokenSource};
use super::wif::GithubIdTokens;
use super::{CredentialsProvider, CredentialsStrategy, reject_group_role};
use crate::config::Config;
use crate::error::{Error, Result};

const SP_MANAGEMENT_TOKEN: &str = "x-databricks-azure-sp-management-token";
const WORKSPACE_RESOURCE_ID: &str = "x-databricks-azure-workspace-resource-id";
/// The audience Entra ID expects for federated GitHub tokens.
const AZURE_AD_TOKEN_EXCHANGE: &str = "api://AzureADTokenExchange";

fn auth(name: &str, message: impl Into<String>) -> Error {
    Error::Auth {
        auth_type: name.into(),
        message: message.into(),
    }
}

/// `azure_identity` takes v2 scopes; `resource/.default` asks for the same
/// token as Go's v1 `resource=` parameter.
fn scope(resource: &str) -> String {
    format!("{resource}/.default")
}

/// The Azure cloud for this workspace, for the Entra ID authority.
fn client_options(cfg: &Config) -> ClientOptions {
    let cloud = match cfg.environment().azure.map(|a| a.name) {
        Some("USGOVERNMENT") => CloudConfiguration::AzureGovernment,
        Some("CHINA") => CloudConfiguration::AzureChina,
        _ => CloudConfiguration::AzurePublic,
    };
    ClientOptions {
        cloud: Some(Arc::new(cloud)),
        ..ClientOptions::default()
    }
}

/// One resource's tokens from an `azure_identity` credential, which caches
/// and refreshes them itself (5 minutes before expiry, well inside the
/// 30 seconds Azure Databricks requires).
struct AzureTokens {
    name: &'static str,
    cred: Arc<dyn TokenCredential>,
    scope: String,
}

impl TokenSource for AzureTokens {
    fn token(&self) -> BoxFuture<'_, Result<Token>> {
        Box::pin(async move {
            let t = self
                .cred
                .get_token(&[self.scope.as_str()], None)
                .await
                .map_err(|e| auth(self.name, e.to_string()))?;
            Ok(Token {
                access_token: SecretString::from(t.token.secret().to_owned()),
                token_type: "Bearer".into(),
                expiry: Some(instant_from_unix(t.expires_on.unix_timestamp())),
            })
        })
    }
}

/// Databricks token, plus the service-management token and resource-ID
/// headers. Go: `azureVisitor(serviceToServiceVisitor(..))`.
fn databricks_provider(
    cfg: &Config,
    name: &'static str,
    cred: &Arc<dyn TokenCredential>,
    management: bool,
) -> Result<Arc<dyn CredentialsProvider>> {
    let env = cfg.environment();
    let tokens = |resource: &str| AzureTokens {
        name,
        cred: Arc::clone(cred),
        scope: scope(resource),
    };
    let mut fixed = Vec::new();
    if let Some(id) = cfg.attr("azure_workspace_resource_id") {
        let v = HeaderValue::from_str(&id).map_err(|_| {
            auth(
                name,
                "azure_workspace_resource_id is not a valid header value",
            )
        })?;
        fixed.push((HeaderName::from_static(WORKSPACE_RESOURCE_ID), v));
    }
    Ok(Arc::new(TokenHeaders {
        primary: Arc::new(tokens(env.azure_application_id)),
        secondary: management.then(|| Secondary {
            header: HeaderName::from_static(SP_MANAGEMENT_TOKEN),
            source: Arc::new(tokens(env.azure_service_management_endpoint())),
            optional: false,
        }),
        fixed,
    }))
}

fn require_host(cfg: &Config, name: &str) -> Result<()> {
    if cfg.host.as_deref().is_some_and(|h| !h.is_empty()) {
        return Ok(());
    }
    Err(auth(
        name,
        "resolve host: create the client with ApiClient::new so the workspace URL can be resolved from azure_workspace_resource_id",
    ))
}

// ---------------------------------------------------------------- azure-msi

const MSI: &str = "azure-msi";

/// Azure managed identity (`azure_use_msi = true`); `azure_client_id`
/// selects a user-assigned identity. On AKS with workload identity
/// (`AZURE_FEDERATED_TOKEN_FILE`) the federated token is used instead.
#[derive(Debug, Clone, Copy, Default)]
pub struct AzureMsiCredentials;

fn msi_wanted(cfg: &Config) -> bool {
    cfg.is_azure()
        && cfg.attr_bool("azure_use_msi")
        && (cfg.attr("azure_workspace_resource_id").is_some()
            || cfg.host.as_deref().is_some_and(|h| !h.is_empty()))
}

fn msi_credential(cfg: &Config, options: ClientOptions) -> Result<Arc<dyn TokenCredential>> {
    let client_id = cfg.attr("azure_client_id");
    // Go (#1813): AKS workload identity when its three variables are set.
    if let (Some(_), Some(tenant), Some(file)) = (
        cfg.getenv("AZURE_AUTHORITY_HOST"),
        cfg.getenv("AZURE_TENANT_ID"),
        cfg.getenv("AZURE_FEDERATED_TOKEN_FILE"),
    ) {
        let client_id = client_id.or_else(|| cfg.getenv("AZURE_CLIENT_ID"));
        let cred = WorkloadIdentityCredential::new(Some(WorkloadIdentityCredentialOptions {
            credential_options: ClientAssertionCredentialOptions {
                client_options: options,
                ..Default::default()
            },
            client_id,
            tenant_id: Some(tenant),
            token_file_path: Some(file.into()),
            ..Default::default()
        }))
        .map_err(|e| auth(MSI, e.to_string()))?;
        return Ok(cred);
    }
    let cred = ManagedIdentityCredential::new(Some(ManagedIdentityCredentialOptions {
        user_assigned_id: client_id.map(UserAssignedId::ClientId),
        client_options: options,
        ..Default::default()
    }))
    .map_err(|e| auth(MSI, e.to_string()))?;
    Ok(cred)
}

fn configure_msi(
    cfg: &Config,
    options: ClientOptions,
) -> Result<Option<Arc<dyn CredentialsProvider>>> {
    if !msi_wanted(cfg) {
        return Ok(None);
    }
    reject_group_role(cfg, MSI)?;
    require_host(cfg, MSI)?;
    let cred = msi_credential(cfg, options)?;
    Ok(Some(databricks_provider(cfg, MSI, &cred, true)?))
}

impl CredentialsStrategy for AzureMsiCredentials {
    fn name(&self) -> &'static str {
        MSI
    }

    fn configure<'a>(
        &'a self,
        cfg: &'a Config,
        _http: &'a reqwest::Client,
    ) -> BoxFuture<'a, Result<Option<Arc<dyn CredentialsProvider>>>> {
        Box::pin(async move { configure_msi(cfg, client_options(cfg)) })
    }
}

// ------------------------------------------------------ azure-client-secret

const SECRET: &str = "azure-client-secret";

/// An Entra ID service principal: `azure_client_id`,
/// `azure_client_secret`, `azure_tenant_id`.
#[derive(Debug, Clone, Copy, Default)]
pub struct AzureClientSecretCredentials;

fn secret_credential(
    cfg: &Config,
    options: ClientOptions,
) -> Result<Option<Arc<dyn TokenCredential>>> {
    let (Some(id), Some(secret), Some(tenant)) = (
        cfg.attr("azure_client_id"),
        cfg.attr("azure_client_secret"),
        cfg.attr("azure_tenant_id"),
    ) else {
        return Ok(None);
    };
    let cred = ClientSecretCredential::new(
        &tenant,
        id,
        Secret::new(secret),
        Some(ClientSecretCredentialOptions {
            client_options: options,
        }),
    )
    .map_err(|e| auth(SECRET, e.to_string()))?;
    Ok(Some(cred))
}

fn configure_secret(
    cfg: &Config,
    options: ClientOptions,
) -> Result<Option<Arc<dyn CredentialsProvider>>> {
    reject_group_role(cfg, SECRET)?;
    if !cfg.is_azure() {
        return Ok(None);
    }
    let Some(cred) = secret_credential(cfg, options)? else {
        return Ok(None);
    };
    require_host(cfg, SECRET)?;
    tracing::info!("generating AAD token for service principal");
    Ok(Some(databricks_provider(cfg, SECRET, &cred, true)?))
}

impl CredentialsStrategy for AzureClientSecretCredentials {
    fn name(&self) -> &'static str {
        SECRET
    }

    fn configure<'a>(
        &'a self,
        cfg: &'a Config,
        _http: &'a reqwest::Client,
    ) -> BoxFuture<'a, Result<Option<Arc<dyn CredentialsProvider>>>> {
        Box::pin(async move { configure_secret(cfg, client_options(cfg)) })
    }
}

// ---------------------------------------------------------------- azure-cli

const CLI: &str = "azure-cli";

/// The identity logged in to the Azure CLI (`az login`; CLI ≥ 2.54).
#[derive(Debug, Clone, Copy, Default)]
pub struct AzureCliCredentials;

/// Go: `loadAzureTenantId`. `{host}/aad/auth` redirects to
/// `https://login.microsoftonline.com/{tenant}/…`.
async fn discover_tenant(cfg: &Config) -> Result<Option<String>> {
    if let Some(t) = cfg.attr("azure_tenant_id") {
        return Ok(Some(t));
    }
    let Some(host) = cfg.host.as_deref().filter(|h| !h.is_empty()) else {
        return Ok(None);
    };
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(cfg.http_timeout())
        .build()?;
    let resp = client.get(format!("{host}/aad/auth")).send().await?;
    let tenant = resp
        .headers()
        .get(LOCATION)
        .and_then(|l| l.to_str().ok())
        .and_then(|l| reqwest::Url::parse(l).ok())
        .and_then(|u| u.path_segments()?.next().map(str::to_owned))
        .filter(|t| !t.is_empty());
    Ok(tenant)
}

fn cli_credential(
    cfg: &Config,
    tenant: Option<String>,
    executor: Option<Arc<dyn Executor>>,
) -> Result<Arc<dyn TokenCredential>> {
    // Go only scopes to the workspace's subscription when no tenant is known.
    let subscription = tenant
        .is_none()
        .then(|| cfg.attr("azure_workspace_resource_id"))
        .flatten()
        .and_then(|id| id.split('/').nth(2).map(str::to_owned))
        .filter(|s| !s.is_empty());
    let cred = AzureCliCredential::new(Some(AzureCliCredentialOptions {
        subscription,
        tenant_id: tenant,
        executor,
        ..Default::default()
    }))
    .map_err(|e| auth(CLI, e.to_string()))?;
    Ok(cred)
}

/// Go returns "not configured" when `az` is missing or logged out.
fn cli_not_configured(e: &Error) -> bool {
    let m = e.to_string();
    m.contains("not found on PATH") || m.contains("No subscription found") || m.contains("az login")
}

async fn configure_cli(
    cfg: &Config,
    executor: Option<Arc<dyn Executor>>,
) -> Result<Option<Arc<dyn CredentialsProvider>>> {
    reject_group_role(cfg, CLI)?;
    if !cfg.is_azure() {
        return Ok(None);
    }
    let tenant = discover_tenant(cfg)
        .await
        .map_err(|e| auth(CLI, format!("load tenant id: {e}")))?;
    let cred = cli_credential(cfg, tenant, executor)?;
    let env = cfg.environment();
    let databricks = AzureTokens {
        name: CLI,
        cred: Arc::clone(&cred),
        scope: scope(env.azure_application_id),
    };
    match databricks.token().await {
        Ok(_) => {}
        Err(e) if cli_not_configured(&e) => {
            tracing::debug!("Azure CLI not usable: {e}");
            return Ok(None);
        }
        Err(e) => return Err(e),
    }
    require_host(cfg, CLI)?;
    // Go sends the management token only if the CLI can issue one.
    let management = AzureTokens {
        name: CLI,
        cred: Arc::clone(&cred),
        scope: scope(env.azure_service_management_endpoint()),
    };
    let with_management = management.token().await.is_ok();
    if !with_management {
        tracing::debug!("not including the service management token in headers");
    }
    tracing::info!("using Azure CLI authentication with AAD tokens");
    Ok(Some(databricks_provider(cfg, CLI, &cred, with_management)?))
}

impl CredentialsStrategy for AzureCliCredentials {
    fn name(&self) -> &'static str {
        CLI
    }

    fn configure<'a>(
        &'a self,
        cfg: &'a Config,
        _http: &'a reqwest::Client,
    ) -> BoxFuture<'a, Result<Option<Arc<dyn CredentialsProvider>>>> {
        Box::pin(configure_cli(cfg, None))
    }
}

// ------------------------------------------------------- github-oidc-azure

const GITHUB: &str = "github-oidc-azure";

/// A GitHub Actions ID token federated to an Entra ID app registration
/// (`azure_client_id`, `azure_tenant_id`).
#[derive(Debug, Clone, Copy, Default)]
pub struct AzureGithubOidcCredentials;

/// Supplies a fresh GitHub ID token as the client assertion. (Go fetches
/// one at configure time and reuses it after it expires.)
#[derive(Debug)]
struct GithubAssertion(GithubIdTokens);

#[async_trait::async_trait]
impl ClientAssertion for GithubAssertion {
    async fn secret(&self, _: Option<ClientMethodOptions<'_>>) -> azure_core::Result<String> {
        use crate::auth::IdTokenSource as _;
        self.0
            .id_token(AZURE_AD_TOKEN_EXCHANGE)
            .await
            .map(|t| t.value.expose_secret().to_owned())
            .map_err(|e| {
                azure_core::Error::with_message(
                    azure_core::error::ErrorKind::Credential,
                    e.to_string(),
                )
            })
    }
}

fn github_credential(
    cfg: &Config,
    http: &reqwest::Client,
    options: ClientOptions,
) -> Result<Option<Arc<dyn TokenCredential>>> {
    let (Some(client), Some(tenant), Some(url), Some(token)) = (
        cfg.attr("azure_client_id"),
        cfg.attr("azure_tenant_id"),
        cfg.attr("actions_id_token_request_url"),
        cfg.attr("actions_id_token_request_token"),
    ) else {
        return Ok(None);
    };
    let assertion = GithubAssertion(GithubIdTokens::new(http, url, token));
    let cred = ClientAssertionCredential::new(
        tenant,
        client,
        assertion,
        Some(ClientAssertionCredentialOptions {
            client_options: options,
            ..Default::default()
        }),
    )
    .map_err(|e| auth(GITHUB, e.to_string()))?;
    Ok(Some(cred))
}

async fn configure_github(
    cfg: &Config,
    http: &reqwest::Client,
    options: ClientOptions,
) -> Result<Option<Arc<dyn CredentialsProvider>>> {
    reject_group_role(cfg, GITHUB)?;
    if !cfg.is_azure() || cfg.host.as_deref().is_none_or(str::is_empty) {
        return Ok(None);
    }
    let Some(cred) = github_credential(cfg, http, options)? else {
        return Ok(None);
    };
    let tokens = AzureTokens {
        name: GITHUB,
        cred,
        scope: scope(cfg.environment().azure_application_id),
    };
    // Go gets the first token here, so a bad setup fails early.
    tokens.token().await?;
    Ok(Some(
        Arc::new(TokenHeaders::bearer(tokens)) as Arc<dyn CredentialsProvider>
    ))
}

impl CredentialsStrategy for AzureGithubOidcCredentials {
    fn name(&self) -> &'static str {
        GITHUB
    }

    fn configure<'a>(
        &'a self,
        cfg: &'a Config,
        http: &'a reqwest::Client,
    ) -> BoxFuture<'a, Result<Option<Arc<dyn CredentialsProvider>>>> {
        Box::pin(configure_github(cfg, http, client_options(cfg)))
    }
}

// ------------------------------------------------------ host from resource ID

/// When only `azure_workspace_resource_id` is configured, look the
/// workspace URL up in Azure Resource Manager and set `host`, using
/// whichever of MSI, client secret or the Azure CLI applies (explicitly
/// via `auth_type`, else in chain order). Go: `azureEnsureWorkspaceUrl`.
pub(crate) async fn ensure_workspace_host(cfg: &mut Config, http: &reqwest::Client) -> Result<()> {
    let Some(resource_id) = cfg.attr("azure_workspace_resource_id") else {
        return Ok(());
    };
    if cfg.host.as_deref().is_some_and(|h| !h.is_empty()) {
        return Ok(());
    }
    let Some(cred) = arm_credential(cfg)? else {
        return Ok(());
    };
    let arm = cfg.environment().azure_resource_manager_endpoint();
    resolve_host(cfg, http, &resource_id, arm, cred).await
}

fn arm_credential(cfg: &Config) -> Result<Option<Arc<dyn TokenCredential>>> {
    let explicit = cfg.auth_type.as_deref().filter(|t| !t.is_empty());
    let allowed = |name: &str| explicit.is_none_or(|t| t == name);
    let options = client_options(cfg);
    if allowed(MSI) && msi_wanted(cfg) {
        return msi_credential(cfg, options).map(Some);
    }
    if allowed(SECRET)
        && let Some(c) = secret_credential(cfg, options.clone())?
    {
        return Ok(Some(c));
    }
    if allowed(CLI) {
        let tenant = cfg.attr("azure_tenant_id");
        return cli_credential(cfg, tenant, None).map(Some);
    }
    Ok(None)
}

async fn resolve_host(
    cfg: &mut Config,
    http: &reqwest::Client,
    resource_id: &str,
    arm: &str,
    cred: Arc<dyn TokenCredential>,
) -> Result<()> {
    let token = AzureTokens {
        name: "azure",
        cred,
        scope: scope(arm.trim_end_matches('/')),
    }
    .token()
    .await?;
    let url = format!(
        "{}{resource_id}?api-version=2018-04-01",
        arm.trim_end_matches('/')
    );
    let body = send_token_request(&url, || http.get(&url).bearer_auth(token.secret()))
        .await
        .map_err(|e| auth("azure", format!("resolve workspace: {e}")))?;
    let v: Value = serde_json::from_slice(&body).map_err(|e| Error::json("workspace", e))?;
    let host = v
        .pointer("/properties/workspaceUrl")
        .and_then(Value::as_str)
        .filter(|h| !h.is_empty())
        .ok_or_else(|| {
            auth(
                "azure",
                "resolve workspace: response has no properties.workspaceUrl",
            )
        })?;
    tracing::debug!(host, "discovered workspace url");
    cfg.host = Some(format!("https://{host}"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::sync::Mutex;

    use azure_core::credentials::{AccessToken, TokenRequestOptions};
    use azure_core::http::headers::Headers;
    use azure_core::http::{AsyncRawResponse, Body, HttpClient, Request, StatusCode, Transport};
    use azure_core::time::OffsetDateTime;
    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    /// Answers `azure_identity`'s requests from a table of URL fragments and
    /// records every request, so no test touches Entra ID or IMDS.
    #[derive(Debug, Default)]
    struct MockTransport {
        routes: Vec<(&'static str, Value)>,
        seen: Mutex<Vec<(String, String)>>,
    }

    impl MockTransport {
        fn seen(&self) -> Vec<(String, String)> {
            self.seen.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl HttpClient for MockTransport {
        async fn execute_request(&self, request: &Request) -> azure_core::Result<AsyncRawResponse> {
            let url = request.url().to_string();
            let body = match request.body() {
                Body::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
                Body::SeekableStream(_) => String::new(),
            };
            self.seen.lock().unwrap().push((url.clone(), body));
            let (status, reply) = self
                .routes
                .iter()
                .find(|(frag, _)| url.contains(frag))
                .map_or(
                    (StatusCode::NotFound, json!({"error": "no route"})),
                    |(_, v)| (StatusCode::Ok, v.clone()),
                );
            Ok(AsyncRawResponse::from_bytes(
                status,
                Headers::new(),
                reply.to_string().into_bytes(),
            ))
        }
    }

    fn options(cfg: &Config, t: &Arc<MockTransport>) -> ClientOptions {
        let mut o = client_options(cfg);
        o.transport = Some(Transport::new(Arc::clone(t) as Arc<dyn HttpClient>));
        o
    }

    async fn resolved(host: &str, attrs: &[(&str, &str)], env: &[(&str, &str)]) -> Config {
        let mut c = Config::with_host(host);
        c.host_metadata = Some(crate::config::HostMetadata::default());
        for (k, v) in attrs {
            c.set_attribute(k, *v).unwrap();
        }
        let env: std::collections::HashMap<String, String> = env
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        c.resolve_with(move |k| env.get(k).cloned(), None)
            .await
            .unwrap()
    }

    fn header_value(h: &crate::auth::Headers, name: &str) -> Option<String> {
        h.iter()
            .find(|(k, _)| k.as_str() == name)
            .map(|(_, v)| v.to_str().unwrap().to_owned())
    }

    const HOST: &str = "https://adb-1.2.azuredatabricks.net";
    const APP: &str = "2ff814a6-3304-4ab8-85cb-cd0e6f879c1d";

    #[tokio::test]
    async fn msi_uses_imds_with_the_databricks_and_management_resources() {
        let t = Arc::new(MockTransport {
            routes: vec![(
                "169.254.169.254",
                json!({"access_token": "msi", "expires_on": "4102444800", "expires_in": "3600",
                       "resource": "r", "token_type": "Bearer"}),
            )],
            ..Default::default()
        });
        let cfg = resolved(
            HOST,
            &[
                ("azure_use_msi", "true"),
                ("azure_client_id", "uami"),
                ("azure_workspace_resource_id", "/subscriptions/s/ws"),
            ],
            &[],
        )
        .await;
        let p = configure_msi(&cfg, options(&cfg, &t)).unwrap().unwrap();
        let h = p.headers().await.unwrap();
        assert_eq!(
            header_value(&h, "authorization").as_deref(),
            Some("Bearer msi")
        );
        assert_eq!(
            header_value(&h, SP_MANAGEMENT_TOKEN).as_deref(),
            Some("msi")
        );
        assert_eq!(
            header_value(&h, WORKSPACE_RESOURCE_ID).as_deref(),
            Some("/subscriptions/s/ws")
        );
        let urls: Vec<String> = t.seen().into_iter().map(|(u, _)| u).collect();
        assert!(
            urls.iter().any(|u| u.contains(&format!("resource={APP}"))),
            "{urls:?}"
        );
        assert!(
            urls.iter()
                .any(|u| u.contains("resource=https%3A%2F%2Fmanagement.core.windows.net%2F")),
            "{urls:?}"
        );
        assert!(
            urls.iter().all(|u| u.contains("client_id=uami")),
            "{urls:?}"
        );
    }

    #[tokio::test]
    async fn msi_preconditions() {
        let t = Arc::new(MockTransport::default());
        // Not asked for, or not Azure: not configured.
        let cfg = resolved(HOST, &[], &[]).await;
        assert!(configure_msi(&cfg, options(&cfg, &t)).unwrap().is_none());
        let cfg = resolved(
            "https://x.cloud.databricks.com",
            &[("azure_use_msi", "true")],
            &[],
        )
        .await;
        assert!(configure_msi(&cfg, options(&cfg, &t)).unwrap().is_none());
        // Group roles need Databricks OAuth.
        let mut cfg = resolved(HOST, &[("azure_use_msi", "true")], &[]).await;
        cfg.group_id = Some("g".into());
        let e = configure_msi(&cfg, options(&cfg, &t)).err().unwrap();
        assert!(e.to_string().contains("group role"), "{e}");
        // Resource ID only: the host must be resolved first.
        let mut cfg = resolved(
            HOST,
            &[
                ("azure_use_msi", "true"),
                ("azure_workspace_resource_id", "/s"),
            ],
            &[],
        )
        .await;
        cfg.host = None;
        let e = configure_msi(&cfg, options(&cfg, &t)).err().unwrap();
        assert!(e.to_string().contains("resolve host"), "{e}");
    }

    #[tokio::test]
    async fn aks_workload_identity_exchanges_the_federated_token() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("token");
        std::fs::write(&file, "k8s-jwt").unwrap();
        let t = Arc::new(MockTransport {
            routes: vec![(
                "/tenant-1/oauth2/v2.0/token",
                json!({"access_token": "wi", "expires_in": 3600, "ext_expires_in": 3600, "token_type": "Bearer"}),
            )],
            ..Default::default()
        });
        let cfg = resolved(
            HOST,
            &[("azure_use_msi", "true")],
            &[
                ("AZURE_AUTHORITY_HOST", "https://login.microsoftonline.com/"),
                ("AZURE_TENANT_ID", "tenant-1"),
                ("AZURE_FEDERATED_TOKEN_FILE", &file.to_string_lossy()),
                ("AZURE_CLIENT_ID", "wi-client"),
            ],
        )
        .await;
        let p = configure_msi(&cfg, options(&cfg, &t)).unwrap().unwrap();
        let h = p.headers().await.unwrap();
        assert_eq!(
            header_value(&h, "authorization").as_deref(),
            Some("Bearer wi")
        );
        let (_, body) = &t.seen()[0];
        assert!(body.contains("client_assertion=k8s-jwt"), "{body}");
        assert!(body.contains("client_id=wi-client"), "{body}");
    }

    #[tokio::test]
    async fn client_secret_uses_the_workspace_cloud() {
        let t = Arc::new(MockTransport {
            routes: vec![(
                "/oauth2/v2.0/token",
                json!({"access_token": "sp", "expires_in": 3600, "ext_expires_in": 3600, "token_type": "Bearer"}),
            )],
            ..Default::default()
        });
        let attrs = [
            ("azure_client_id", "app"),
            ("azure_client_secret", "shh"),
            ("azure_tenant_id", "tenant-2"),
        ];
        let cfg = resolved("https://adb-1.2.databricks.azure.cn", &attrs, &[]).await;
        let p = configure_secret(&cfg, options(&cfg, &t)).unwrap().unwrap();
        let h = p.headers().await.unwrap();
        assert_eq!(
            header_value(&h, "authorization").as_deref(),
            Some("Bearer sp")
        );
        assert_eq!(header_value(&h, SP_MANAGEMENT_TOKEN).as_deref(), Some("sp"));
        let seen = t.seen();
        assert!(
            seen.iter()
                .all(|(u, _)| u.starts_with("https://login.chinacloudapi.cn/tenant-2/")),
            "{seen:?}"
        );
        assert!(seen.iter().any(|(_, b)| b.contains("client_secret=shh")));
        assert!(
            seen.iter()
                .any(|(_, b)| b.contains(&format!("scope={APP}%2F.default"))),
            "{seen:?}"
        );

        // Incomplete or not Azure: not configured.
        let cfg = resolved(HOST, &attrs[..2], &[]).await;
        assert!(configure_secret(&cfg, options(&cfg, &t)).unwrap().is_none());
        let cfg = resolved("https://x.gcp.databricks.com", &attrs, &[]).await;
        assert!(configure_secret(&cfg, options(&cfg, &t)).unwrap().is_none());
    }

    /// A fake `az`: answers `get-access-token` and records the command.
    #[derive(Debug, Default)]
    struct FakeAz {
        fail: Option<&'static str>,
        seen: Mutex<Vec<String>>,
    }

    fn exit(code: i32) -> std::process::ExitStatus {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(code << 8)
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(code.cast_unsigned())
        }
    }

    #[async_trait::async_trait]
    impl Executor for FakeAz {
        async fn run(
            &self,
            _program: &OsStr,
            args: &[&OsStr],
        ) -> std::io::Result<std::process::Output> {
            let cmd = args.last().unwrap().to_string_lossy().into_owned();
            self.seen.lock().unwrap().push(cmd);
            if let Some(stderr) = self.fail {
                return Ok(std::process::Output {
                    status: exit(1),
                    stdout: Vec::new(),
                    stderr: stderr.as_bytes().to_vec(),
                });
            }
            let out = json!({"accessToken": "cli", "expires_on": 4_102_444_800_i64, "tokenType": "Bearer"});
            Ok(std::process::Output {
                status: exit(0),
                stdout: out.to_string().into_bytes(),
                stderr: Vec::new(),
            })
        }
    }

    #[tokio::test]
    async fn azure_cli_tokens_scoped_by_tenant_or_subscription() {
        let az = Arc::new(FakeAz::default());
        let cfg = resolved(HOST, &[("azure_tenant_id", "t-9")], &[]).await;
        let p = configure_cli(&cfg, Some(Arc::clone(&az) as Arc<dyn Executor>))
            .await
            .unwrap()
            .unwrap();
        let h = p.headers().await.unwrap();
        assert_eq!(
            header_value(&h, "authorization").as_deref(),
            Some("Bearer cli")
        );
        assert_eq!(
            header_value(&h, SP_MANAGEMENT_TOKEN).as_deref(),
            Some("cli")
        );
        let seen = az.seen.lock().unwrap().clone();
        assert!(
            seen[0].contains(&format!("--scope {APP}/.default")),
            "{seen:?}"
        );
        assert!(seen.iter().all(|c| c.contains("--tenant t-9")), "{seen:?}");

        // No tenant known: the workspace's subscription scopes the call.
        let az = Arc::new(FakeAz::default());
        let cfg = resolved(
            "",
            &[
                ("azure_workspace_resource_id", "/subscriptions/sub-1/rg/x"),
                ("azure_tenant_id", ""),
            ],
            &[],
        )
        .await;
        let e = configure_cli(&cfg, Some(Arc::clone(&az) as Arc<dyn Executor>))
            .await
            .err()
            .unwrap();
        assert!(e.to_string().contains("resolve host"), "{e}");
        assert!(az.seen.lock().unwrap()[0].contains("--subscription \"sub-1\""));
    }

    #[tokio::test]
    async fn azure_cli_missing_or_logged_out_is_not_configured() {
        let cfg = resolved(HOST, &[("azure_tenant_id", "t")], &[]).await;
        for stderr in [
            "az not found on PATH",
            "ERROR: Please run 'az login' to setup account.",
        ] {
            let az = Arc::new(FakeAz {
                fail: Some(stderr),
                ..Default::default()
            });
            let r = configure_cli(&cfg, Some(az as Arc<dyn Executor>))
                .await
                .unwrap();
            assert!(r.is_none(), "{stderr}");
        }
        let az = Arc::new(FakeAz {
            fail: Some("ERROR: something else"),
            ..Default::default()
        });
        assert!(
            configure_cli(&cfg, Some(az as Arc<dyn Executor>))
                .await
                .is_err()
        );
        let cfg = resolved("https://x.cloud.databricks.com", &[], &[]).await;
        assert!(configure_cli(&cfg, None).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn tenant_is_discovered_from_the_workspace() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/aad/auth"))
            .respond_with(ResponseTemplate::new(302).insert_header(
                "location",
                "https://login.microsoftonline.com/tenant-from-host/oauth2/authorize?x=1",
            ))
            .mount(&server)
            .await;
        let cfg = resolved(&server.uri(), &[], &[]).await;
        assert_eq!(
            discover_tenant(&cfg).await.unwrap().as_deref(),
            Some("tenant-from-host")
        );
        let cfg = resolved(&server.uri(), &[("azure_tenant_id", "set")], &[]).await;
        assert_eq!(discover_tenant(&cfg).await.unwrap().as_deref(), Some("set"));
    }

    #[tokio::test]
    async fn github_oidc_azure_federates_the_github_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/gh"))
            .and(query_param("audience", AZURE_AD_TOKEN_EXCHANGE))
            .and(header("authorization", "Bearer req-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": "gh-jwt"})))
            .mount(&server)
            .await;
        let t = Arc::new(MockTransport {
            routes: vec![(
                "/tenant-3/oauth2/v2.0/token",
                json!({"access_token": "gh-aad", "expires_in": 3600, "ext_expires_in": 3600, "token_type": "Bearer"}),
            )],
            ..Default::default()
        });
        let url = format!("{}/gh", server.uri());
        let cfg = resolved(
            HOST,
            &[
                ("azure_client_id", "app-3"),
                ("azure_tenant_id", "tenant-3"),
                ("actions_id_token_request_url", &url),
                ("actions_id_token_request_token", "req-token"),
            ],
            &[],
        )
        .await;
        let http = reqwest::Client::new();
        let p = configure_github(&cfg, &http, options(&cfg, &t))
            .await
            .unwrap()
            .unwrap();
        let h = p.headers().await.unwrap();
        assert_eq!(
            header_value(&h, "authorization").as_deref(),
            Some("Bearer gh-aad")
        );
        assert!(header_value(&h, SP_MANAGEMENT_TOKEN).is_none());
        let (_, body) = &t.seen()[0];
        assert!(body.contains("client_assertion=gh-jwt"), "{body}");

        // Missing GitHub variables: not configured.
        let cfg = resolved(
            HOST,
            &[("azure_client_id", "a"), ("azure_tenant_id", "t")],
            &[],
        )
        .await;
        assert!(
            configure_github(&cfg, &http, options(&cfg, &t))
                .await
                .unwrap()
                .is_none()
        );
    }

    /// Records the scope it was asked for.
    #[derive(Debug, Default)]
    struct FixedToken(Mutex<Vec<String>>);

    #[async_trait::async_trait]
    impl TokenCredential for FixedToken {
        async fn get_token(
            &self,
            scopes: &[&str],
            _: Option<TokenRequestOptions<'_>>,
        ) -> azure_core::Result<AccessToken> {
            self.0
                .lock()
                .unwrap()
                .extend(scopes.iter().map(|s| (*s).to_owned()));
            Ok(AccessToken::new(
                "arm-token",
                OffsetDateTime::now_utc() + azure_core::time::Duration::hours(1),
            ))
        }
    }

    #[tokio::test]
    async fn workspace_host_is_resolved_through_arm() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/subscriptions/s/workspaces/w"))
            .and(query_param("api-version", "2018-04-01"))
            .and(header("authorization", "Bearer arm-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"properties": {"workspaceUrl": "adb-9.9.azuredatabricks.net"}}),
            ))
            .mount(&server)
            .await;
        // The mock server stands in for ARM.
        let arm = format!("{}/", server.uri());
        let fixed = Arc::new(FixedToken::default());
        let mut cfg = Config::default();
        let http = reqwest::Client::new();
        resolve_host(
            &mut cfg,
            &http,
            "/subscriptions/s/workspaces/w",
            &arm,
            Arc::clone(&fixed) as Arc<dyn TokenCredential>,
        )
        .await
        .unwrap();
        assert_eq!(
            cfg.host.as_deref(),
            Some("https://adb-9.9.azuredatabricks.net")
        );
        assert_eq!(
            *fixed.0.lock().unwrap(),
            [format!("{}/.default", server.uri())]
        );

        // Nothing to do when the host is known or no Azure method applies.
        ensure_workspace_host(&mut cfg, &http).await.unwrap();
        let mut none = Config::default();
        none.set_attribute("azure_workspace_resource_id", "/s")
            .unwrap();
        none.auth_type = Some("pat".into());
        ensure_workspace_host(&mut none, &http).await.unwrap();
        assert!(none.host.is_none());
    }
}
