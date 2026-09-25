//! Azure managed identity (`azure-msi`).
//!
//! Go: `AzureMsiCredentials`, including the endpoint selection from
//! databricks-sdk-go#1813. As in Azure Identity's
//! `ManagedIdentityCredential`, the first matching host environment wins:
//! Service Fabric, App Service / Functions, Azure Arc, Azure ML, Cloud
//! Shell, AKS workload identity, and finally IMDS.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use reqwest::header::{HeaderName, HeaderValue, WWW_AUTHENTICATE};
use secrecy::SecretString;
use serde::Deserialize;
use serde_json::Value;

use super::common::{
    EarlyExpiry, TokenHeaders, instant_after, instant_from_unix, parse_azure_ml_timestamp,
    send_token_request,
};
use super::token::{CachedTokenSource, Token, TokenSource};
use super::{CredentialsProvider, CredentialsStrategy, reject_group_role};
use crate::config::Config;
use crate::error::{ApiError, Error, Result};

const NAME: &str = "azure-msi";
const IMDS_AUTHORITY: &str = "http://169.254.169.254";
const IMDS_TOKEN_PATH: &str = "/metadata/identity/oauth2/token";
const ARC_LINUX_TOKEN_DIR: &str = "/var/opt/azcmagent/tokens";
const ARC_MAX_SECRET_SIZE: u64 = 4096;
/// Managed identity endpoints are local; fail fast when not on Azure.
const MSI_TIMEOUT: Duration = Duration::from_secs(10);
/// Azure Databricks rejects tokens with 30s or less left.
const AZURE_EARLY_EXPIRY: Duration = Duration::from_secs(40);
const SP_MANAGEMENT_TOKEN: &str = "x-databricks-azure-sp-management-token";
const WORKSPACE_RESOURCE_ID: &str = "x-databricks-azure-workspace-resource-id";

/// Azure managed identity (`azure_use_msi = true`).
#[derive(Debug, Clone, Copy, Default)]
pub struct AzureMsiCredentials;

fn auth(message: impl Into<String>) -> Error {
    Error::Auth {
        auth_type: NAME.into(),
        message: message.into(),
    }
}

fn wanted(cfg: &Config) -> bool {
    cfg.is_azure()
        && cfg.attr_bool("azure_use_msi")
        && (cfg.attr("azure_workspace_resource_id").is_some()
            || cfg.host.as_deref().is_some_and(|h| !h.is_empty()))
}

impl CredentialsStrategy for AzureMsiCredentials {
    fn name(&self) -> &'static str {
        NAME
    }

    fn configure<'a>(
        &'a self,
        cfg: &'a Config,
        http: &'a reqwest::Client,
    ) -> BoxFuture<'a, Result<Option<Arc<dyn CredentialsProvider>>>> {
        Box::pin(async move {
            if !wanted(cfg) {
                return Ok(None);
            }
            reject_group_role(cfg, NAME)?;
            if cfg.host.as_deref().is_none_or(str::is_empty) {
                return Err(auth(
                    "resolve host: create the client with ApiClient::new so the workspace URL can be resolved from azure_workspace_resource_id",
                ));
            }
            tracing::debug!("generating AAD token via Azure MSI");
            let env = cfg.environment();
            let source = |resource: &str| -> Result<CachedTokenSource> {
                let s = MsiSource::from_config(cfg, http, resource)?;
                Ok(CachedTokenSource::new(
                    EarlyExpiry {
                        inner: s,
                        by: AZURE_EARLY_EXPIRY,
                    },
                    true,
                ))
            };
            let primary = source(env.azure_application_id)?;
            let management = source(env.azure_service_management_endpoint())?;
            let mut fixed = Vec::new();
            if let Some(id) = cfg.attr("azure_workspace_resource_id") {
                let v = HeaderValue::from_str(&id)
                    .map_err(|_| auth("azure_workspace_resource_id is not a valid header value"))?;
                fixed.push((HeaderName::from_static(WORKSPACE_RESOURCE_ID), v));
            }
            Ok(Some(Arc::new(TokenHeaders {
                primary,
                secondary: Some((HeaderName::from_static(SP_MANAGEMENT_TOKEN), management)),
                fixed,
            }) as Arc<dyn CredentialsProvider>))
        })
    }
}

/// When only `azure_workspace_resource_id` is configured, look the
/// workspace URL up in Azure Resource Manager with a managed-identity
/// token and set `host`. Go: `azureEnsureWorkspaceUrl`.
pub(crate) async fn ensure_workspace_host(cfg: &mut Config, http: &reqwest::Client) -> Result<()> {
    let Some(resource_id) = cfg.attr("azure_workspace_resource_id") else {
        return Ok(());
    };
    let msi_allowed = cfg
        .auth_type
        .as_deref()
        .is_none_or(|t| t.is_empty() || t == NAME);
    if cfg.host.as_deref().is_some_and(|h| !h.is_empty()) || !wanted(cfg) || !msi_allowed {
        return Ok(());
    }
    let arm = cfg.environment().azure_resource_manager_endpoint();
    resolve_host(cfg, http, &resource_id, arm).await
}

async fn resolve_host(
    cfg: &mut Config,
    http: &reqwest::Client,
    resource_id: &str,
    arm: &str,
) -> Result<()> {
    let token = MsiSource::from_config(cfg, http, arm)?.token().await?;
    let url = format!(
        "{}{resource_id}?api-version=2018-04-01",
        arm.trim_end_matches('/')
    );
    let body = send_token_request(&url, || http.get(&url).bearer_auth(token.secret()))
        .await
        .map_err(|e| auth(format!("resolve workspace: {e}")))?;
    let v: Value = serde_json::from_slice(&body).map_err(|e| Error::json("workspace", e))?;
    let host = v
        .pointer("/properties/workspaceUrl")
        .and_then(Value::as_str)
        .filter(|h| !h.is_empty())
        .ok_or_else(|| auth("resolve workspace: response has no properties.workspaceUrl"))?;
    tracing::debug!(host, "discovered workspace url");
    cfg.host = Some(format!("https://{host}"));
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    ServiceFabric,
    AppService,
    Arc,
    Ml,
    CloudShell,
    Workload,
    Imds,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::ServiceFabric => "Service Fabric",
            Self::AppService => "App Service",
            Self::Arc => "Azure Arc",
            Self::Ml => "Azure ML",
            Self::CloudShell => "Cloud Shell",
            Self::Workload => "workload identity",
            Self::Imds => "IMDS",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Endpoint {
    kind: Kind,
    url: String,
    secret: Option<String>,
    tenant_id: Option<String>,
    token_file: Option<String>,
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("kind", &self.kind)
            .field("url", &self.url)
            .field("secret", &self.secret.as_ref().map(|_| "***"))
            .field("tenant_id", &self.tenant_id)
            .field("token_file", &self.token_file)
            .finish()
    }
}

impl Endpoint {
    fn new(kind: Kind, url: String) -> Self {
        Self {
            kind,
            url,
            secret: None,
            tenant_id: None,
            token_file: None,
        }
    }

    fn secret(mut self, s: String) -> Self {
        self.secret = Some(s);
        self
    }
}

/// Pick the managed-identity endpoint from the environment. Returns the
/// endpoint and the client ID to request (workload identity falls back to
/// `AZURE_CLIENT_ID`).
fn select_endpoint(
    getenv: &dyn Fn(&str) -> Option<String>,
    client_id: Option<String>,
) -> Result<(Endpoint, Option<String>)> {
    let no_client_id = |e: Endpoint, client_id: Option<String>| {
        if client_id.is_some() {
            Err(auth(format!(
                "azure_client_id is not supported by {:?} managed identity",
                e.kind.label()
            )))
        } else {
            Ok((e, None))
        }
    };
    if let Some(url) = getenv("IDENTITY_ENDPOINT") {
        if let Some(header) = getenv("IDENTITY_HEADER") {
            if getenv("IDENTITY_SERVER_THUMBPRINT").is_some() {
                return no_client_id(
                    Endpoint::new(Kind::ServiceFabric, url).secret(header),
                    client_id,
                );
            }
            return Ok((
                Endpoint::new(Kind::AppService, url).secret(header),
                client_id,
            ));
        }
        if getenv("IMDS_ENDPOINT").is_some() {
            return no_client_id(Endpoint::new(Kind::Arc, url), client_id);
        }
        return Err(auth(
            "no managed identity endpoint found: IDENTITY_ENDPOINT requires IDENTITY_HEADER or IMDS_ENDPOINT",
        ));
    }
    if let Some(url) = getenv("MSI_ENDPOINT") {
        if let Some(secret) = getenv("MSI_SECRET") {
            return Ok((Endpoint::new(Kind::Ml, url).secret(secret), client_id));
        }
        return no_client_id(Endpoint::new(Kind::CloudShell, url), client_id);
    }
    if let (Some(authority), Some(tenant), Some(file)) = (
        getenv("AZURE_AUTHORITY_HOST"),
        getenv("AZURE_TENANT_ID"),
        getenv("AZURE_FEDERATED_TOKEN_FILE"),
    ) {
        let client_id = client_id.or_else(|| getenv("AZURE_CLIENT_ID"));
        if client_id.is_none() {
            return Err(auth("azure_client_id is required for workload identity"));
        }
        let mut e = Endpoint::new(Kind::Workload, authority);
        e.tenant_id = Some(tenant);
        e.token_file = Some(file);
        return Ok((e, client_id));
    }
    let authority =
        getenv("AZURE_POD_IDENTITY_AUTHORITY_HOST").unwrap_or_else(|| IMDS_AUTHORITY.to_owned());
    Ok((
        Endpoint::new(
            Kind::Imds,
            format!("{}{IMDS_TOKEN_PATH}", authority.trim_end_matches('/')),
        ),
        client_id,
    ))
}

/// Where the Arc agent keeps challenge secrets.
fn arc_token_dir(getenv: &dyn Fn(&str) -> Option<String>) -> Result<PathBuf> {
    if cfg!(target_os = "linux") {
        Ok(PathBuf::from(ARC_LINUX_TOKEN_DIR))
    } else if cfg!(windows) {
        let pd = getenv("PROGRAMDATA").ok_or_else(|| {
            auth("PROGRAMDATA is required for Azure Arc managed identity on Windows")
        })?;
        Ok(Path::new(&pd)
            .join("AzureConnectedMachineAgent")
            .join("Tokens"))
    } else {
        Err(auth(format!(
            "Azure Arc managed identity is not supported on {:?}",
            std::env::consts::OS
        )))
    }
}

/// Read the Arc challenge secret, checking it is a small `.key` file in
/// the agent's token directory (the path comes from a server header).
fn read_arc_secret(key_file: &Path, expected_dir: &Path) -> Result<String> {
    let meta = std::fs::metadata(key_file)
        .map_err(|e| auth(format!("read Azure Arc secret {}: {e}", key_file.display())))?;
    if key_file.parent() != Some(expected_dir) {
        return Err(auth(format!(
            "unexpected Azure Arc managed identity secret directory {}",
            key_file.parent().unwrap_or(Path::new("")).display()
        )));
    }
    if key_file.extension().is_none_or(|e| e != "key") {
        return Err(auth(format!(
            "Azure Arc managed identity secret file {} must have a .key extension",
            key_file.display()
        )));
    }
    if meta.len() > ARC_MAX_SECRET_SIZE {
        return Err(auth(format!(
            "Azure Arc managed identity secret file {} exceeds {ARC_MAX_SECRET_SIZE} bytes",
            key_file.display()
        )));
    }
    std::fs::read_to_string(key_file)
        .map_err(|e| auth(format!("read Azure Arc secret {}: {e}", key_file.display())))
}

/// `Basic realm=/var/opt/azcmagent/tokens/x.key` → the key file.
fn arc_challenge_file(header: Option<&HeaderValue>) -> Result<PathBuf> {
    let h = header.and_then(|v| v.to_str().ok()).ok_or_else(|| {
        auth("Azure Arc managed identity response has no WWW-Authenticate header")
    })?;
    let file = h
        .split_once('=')
        .map(|(_, f)| f.trim().trim_matches('"'))
        .filter(|f| !f.is_empty())
        .ok_or_else(|| auth(format!("invalid Azure Arc WWW-Authenticate header: {h:?}")))?;
    Ok(PathBuf::from(file))
}

type Pairs = Vec<(&'static str, String)>;

/// One managed-identity resource (the Databricks app, or service
/// management, or ARM).
struct MsiSource {
    http: reqwest::Client,
    resource: String,
    client_id: Option<String>,
    endpoint: Endpoint,
    arc_dir: Result<PathBuf>,
}

impl MsiSource {
    fn from_config(cfg: &Config, http: &reqwest::Client, resource: &str) -> Result<Self> {
        let getenv = |k: &str| cfg.getenv(k);
        let (endpoint, client_id) = select_endpoint(&getenv, cfg.attr("azure_client_id"))?;
        Ok(Self {
            http: http.clone(),
            resource: resource.to_owned(),
            client_id,
            endpoint,
            arc_dir: arc_token_dir(&getenv),
        })
    }

    /// Query or form parameters and headers for the endpoint.
    fn params(&self) -> (Pairs, Pairs) {
        let mut data = vec![("resource", self.resource.clone())];
        let mut headers = Vec::new();
        let secret = self.endpoint.secret.clone().unwrap_or_default();
        let client_id = self.client_id.clone();
        match self.endpoint.kind {
            Kind::ServiceFabric => {
                headers.push(("secret", secret));
                data.push(("api-version", "2019-07-01-preview".into()));
            }
            Kind::AppService => {
                headers.push(("x-identity-header", secret));
                data.push(("api-version", "2019-08-01".into()));
                data.extend(client_id.map(|c| ("client_id", c)));
            }
            Kind::Arc => {
                headers.push(("metadata", "true".into()));
                data.push(("api-version", "2020-06-01".into()));
            }
            Kind::Ml => {
                headers.push(("secret", secret));
                data.push(("api-version", "2017-09-01".into()));
                data.extend(client_id.map(|c| ("clientid", c)));
            }
            Kind::CloudShell => headers.push(("metadata", "true".into())),
            Kind::Imds => {
                headers.push(("metadata", "true".into()));
                data.push(("api-version", "2018-02-01".into()));
                data.extend(client_id.map(|c| ("client_id", c)));
            }
            Kind::Workload => {}
        }
        (data, headers)
    }

    fn request(&self, basic: Option<&str>) -> reqwest::RequestBuilder {
        let (data, headers) = self.params();
        let mut req = if self.endpoint.kind == Kind::CloudShell {
            self.http.post(&self.endpoint.url).form(&data)
        } else {
            match reqwest::Url::parse(&self.endpoint.url) {
                Ok(mut url) => {
                    url.query_pairs_mut().extend_pairs(&data);
                    self.http.get(url)
                }
                // Let reqwest report the bad URL on send.
                Err(_) => self.http.get(&self.endpoint.url),
            }
        };
        for (k, v) in headers {
            req = req.header(k, v);
        }
        if let Some(secret) = basic {
            req = req.header("authorization", format!("Basic {secret}"));
        }
        req.timeout(MSI_TIMEOUT)
    }

    async fn fetch(&self) -> Result<Token> {
        let ml = self.endpoint.kind == Kind::Ml;
        match self.endpoint.kind {
            Kind::Workload => self.workload().await,
            Kind::Arc => self.arc().await,
            _ => {
                let body = send_token_request(&self.endpoint.url, || self.request(None))
                    .await
                    .map_err(|e| {
                        auth(format!(
                            "request managed identity token from {:?}: {e}",
                            self.endpoint.url
                        ))
                    })?;
                parse_msi_token(&body, ml)
            }
        }
    }

    async fn workload(&self) -> Result<Token> {
        let file = self.endpoint.token_file.clone().unwrap_or_default();
        let assertion = tokio::fs::read_to_string(&file)
            .await
            .map_err(|e| auth(format!("read federated token file {file:?}: {e}")))?;
        let tenant = self.endpoint.tenant_id.clone().unwrap_or_default();
        let url = format!(
            "{}/{}/oauth2/v2.0/token",
            self.endpoint.url.trim_end_matches('/'),
            url::form_urlencoded::byte_serialize(tenant.as_bytes()).collect::<String>()
        );
        let scope = if self.resource.ends_with("/.default") {
            self.resource.clone()
        } else {
            format!("{}/.default", self.resource.trim_end_matches('/'))
        };
        let form = [
            ("client_assertion", assertion.trim().to_owned()),
            (
                "client_assertion_type",
                "urn:ietf:params:oauth:client-assertion-type:jwt-bearer".into(),
            ),
            ("client_id", self.client_id.clone().unwrap_or_default()),
            ("grant_type", "client_credentials".into()),
            ("scope", scope),
        ];
        let body = send_token_request(&url, || {
            self.http.post(&url).form(&form).timeout(MSI_TIMEOUT)
        })
        .await
        .map_err(|e| auth(format!("request managed identity token from {url:?}: {e}")))?;
        parse_msi_token(&body, false)
    }

    /// Arc answers the first request with 401 and a challenge naming a
    /// local key file; the key's contents go back as Basic credentials.
    async fn arc(&self) -> Result<Token> {
        let url = &self.endpoint.url;
        let resp = self.request(None).send().await?;
        let status = resp.status().as_u16();
        let challenge = resp.headers().get(WWW_AUTHENTICATE).cloned();
        let body = resp.bytes().await?;
        if (200..300).contains(&status) {
            return parse_msi_token(&body, false);
        }
        if status != 401 {
            return Err(auth(format!(
                "request managed identity token from {url:?}: {}",
                ApiError::from_response(status, "GET", IMDS_TOKEN_PATH, &body)
            )));
        }
        let key_file = arc_challenge_file(challenge.as_ref())
            .map_err(|e| auth(format!("handle Azure Arc challenge from {url:?}: {e}")))?;
        let dir = self
            .arc_dir
            .as_ref()
            .map_err(|e| auth(e.to_string()))?
            .clone();
        let secret = tokio::task::spawn_blocking(move || read_arc_secret(&key_file, &dir))
            .await
            .map_err(|e| auth(format!("read Azure Arc secret: {e}")))??;
        let body = send_token_request(url, || self.request(Some(secret.trim())))
            .await
            .map_err(|e| auth(format!("request managed identity token from {url:?}: {e}")))?;
        parse_msi_token(&body, false)
    }
}

impl TokenSource for MsiSource {
    fn token(&self) -> BoxFuture<'_, Result<Token>> {
        Box::pin(async move {
            tokio::time::timeout(MSI_TIMEOUT, self.fetch())
                .await
                .map_err(|_| {
                    auth(format!(
                        "managed identity endpoint {:?} did not respond within {MSI_TIMEOUT:?}",
                        self.endpoint.url
                    ))
                })?
        })
    }
}

/// A number or a string holding one (or, on Azure ML, a date).
#[derive(Deserialize)]
#[serde(untagged)]
enum Expiry {
    Num(i64),
    Str(String),
}

#[derive(Deserialize)]
struct MsiToken {
    #[serde(default)]
    token_type: String,
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    expires_on: Option<Expiry>,
    #[serde(default)]
    expires_in: Option<Expiry>,
}

fn parse_msi_token(body: &[u8], azure_ml: bool) -> Result<Token> {
    let t: MsiToken =
        serde_json::from_slice(body).map_err(|e| Error::json("managed identity token", e))?;
    if t.access_token.is_empty() {
        return Err(auth("token parse: invalid token"));
    }
    let invalid = |why: String| auth(format!("invalid token expiry: {why}"));
    let expiry = match (t.expires_on, t.expires_in) {
        (Some(Expiry::Num(n)), _) => instant_from_unix(n),
        (Some(Expiry::Str(s)), _) if !s.is_empty() => match s.trim().parse::<i64>() {
            Ok(n) => instant_from_unix(n),
            Err(_) if azure_ml => {
                instant_from_unix(parse_azure_ml_timestamp(&s).ok_or_else(|| invalid(s.clone()))?)
            }
            Err(_) => return Err(invalid(s)),
        },
        (_, Some(Expiry::Num(n))) => instant_after(u64::try_from(n).unwrap_or(0)),
        (_, Some(Expiry::Str(s))) => {
            let n: u64 = s.trim().parse().map_err(|_| invalid(s.clone()))?;
            instant_after(n)
        }
        _ => return Err(invalid("expires_on is missing".into())),
    };
    Ok(Token {
        access_token: SecretString::from(t.access_token),
        token_type: if t.token_type.is_empty() {
            "Bearer".into()
        } else {
            t.token_type
        },
        expiry: Some(expiry),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use wiremock::matchers::{body_string_contains, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k| m.get(k).cloned()
    }

    fn pick(pairs: &[(&str, &str)], client: Option<&str>) -> Result<(Endpoint, Option<String>)> {
        select_endpoint(&env(pairs), client.map(str::to_owned))
    }

    #[test]
    fn endpoint_selection_follows_azure_identity_order() {
        let (e, c) = pick(&[], Some("cid")).unwrap();
        assert_eq!(
            (e.kind, e.url.as_str()),
            (
                Kind::Imds,
                "http://169.254.169.254/metadata/identity/oauth2/token"
            )
        );
        assert_eq!(c.as_deref(), Some("cid"));
        let (e, _) = pick(
            &[("AZURE_POD_IDENTITY_AUTHORITY_HOST", "http://pod/")],
            None,
        )
        .unwrap();
        assert_eq!(e.url, "http://pod/metadata/identity/oauth2/token");

        let app = [
            ("IDENTITY_ENDPOINT", "http://app"),
            ("IDENTITY_HEADER", "h"),
        ];
        let (e, c) = pick(&app, Some("cid")).unwrap();
        assert_eq!(
            (e.kind, e.secret.as_deref(), c.as_deref()),
            (Kind::AppService, Some("h"), Some("cid"))
        );

        let sf = [app[0], app[1], ("IDENTITY_SERVER_THUMBPRINT", "t")];
        assert_eq!(pick(&sf, None).unwrap().0.kind, Kind::ServiceFabric);
        assert!(
            pick(&sf, Some("cid"))
                .unwrap_err()
                .to_string()
                .contains("not supported by \"Service Fabric\"")
        );

        let arc = [
            ("IDENTITY_ENDPOINT", "http://arc"),
            ("IMDS_ENDPOINT", "http://imds"),
        ];
        assert_eq!(pick(&arc, None).unwrap().0.kind, Kind::Arc);
        assert!(pick(&arc, Some("c")).is_err());
        assert!(
            pick(&[("IDENTITY_ENDPOINT", "x")], None)
                .unwrap_err()
                .to_string()
                .contains("requires IDENTITY_HEADER")
        );

        let ml = [("MSI_ENDPOINT", "http://ml"), ("MSI_SECRET", "s")];
        assert_eq!(pick(&ml, Some("c")).unwrap().0.kind, Kind::Ml);
        assert_eq!(
            pick(&[("MSI_ENDPOINT", "http://cs")], None).unwrap().0.kind,
            Kind::CloudShell
        );
        assert!(pick(&[("MSI_ENDPOINT", "http://cs")], Some("c")).is_err());

        let wi = [
            ("AZURE_AUTHORITY_HOST", "https://login"),
            ("AZURE_TENANT_ID", "t"),
            ("AZURE_FEDERATED_TOKEN_FILE", "/f"),
        ];
        assert!(
            pick(&wi, None)
                .unwrap_err()
                .to_string()
                .contains("required for workload identity")
        );
        let mut with_id = wi.to_vec();
        with_id.push(("AZURE_CLIENT_ID", "env-cid"));
        let (e, c) = pick(&with_id, None).unwrap();
        assert_eq!((e.kind, c.as_deref()), (Kind::Workload, Some("env-cid")));
        assert_eq!(
            pick(&with_id, Some("cfg")).unwrap().1.as_deref(),
            Some("cfg")
        );
    }

    fn source(server: &MockServer, e: Endpoint, client: Option<&str>) -> MsiSource {
        let _ = server;
        MsiSource {
            http: reqwest::Client::new(),
            resource: "res".into(),
            client_id: client.map(str::to_owned),
            endpoint: e,
            arc_dir: Err(auth("no arc dir")),
        }
    }

    #[tokio::test]
    async fn get_endpoints_send_their_headers_and_api_versions() {
        let server = MockServer::start().await;
        for (p, hname, hval, api, id_param) in [
            ("/imds", "metadata", "true", "2018-02-01", Some("client_id")),
            (
                "/app",
                "x-identity-header",
                "sec",
                "2019-08-01",
                Some("client_id"),
            ),
            ("/sf", "secret", "sec", "2019-07-01-preview", None),
            ("/ml", "secret", "sec", "2017-09-01", Some("clientid")),
        ] {
            let mut m = Mock::given(method("GET"))
                .and(path(p))
                .and(header(hname, hval))
                .and(query_param("resource", "res"))
                .and(query_param("api-version", api));
            if let Some(k) = id_param {
                m = m.and(query_param(k, "cid"));
            }
            m.respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"access_token": format!("tok{p}"), "expires_on": "4102444800"}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        }
        for (p, kind, client) in [
            ("/imds", Kind::Imds, Some("cid")),
            ("/app", Kind::AppService, Some("cid")),
            ("/sf", Kind::ServiceFabric, None),
            ("/ml", Kind::Ml, Some("cid")),
        ] {
            let e = Endpoint::new(kind, format!("{}{p}", server.uri())).secret("sec".into());
            let t = source(&server, e, client).token().await.unwrap();
            assert_eq!(t.secret(), format!("tok{p}"));
        }
    }

    #[tokio::test]
    async fn cloud_shell_posts_a_form_and_workload_identity_exchanges_the_file() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/cs"))
            .and(header("metadata", "true"))
            .and(body_string_contains("resource=res"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"access_token": "cs", "expires_in": "3600", "token_type": "Bearer"}),
            ))
            .mount(&server)
            .await;
        let e = Endpoint::new(Kind::CloudShell, format!("{}/cs", server.uri()));
        assert_eq!(
            source(&server, e, None).token().await.unwrap().secret(),
            "cs"
        );

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("token");
        std::fs::write(&file, "federated-jwt\n").unwrap();
        Mock::given(method("POST"))
            .and(path("/tenant-1/oauth2/v2.0/token"))
            .and(body_string_contains("client_assertion=federated-jwt&"))
            .and(body_string_contains("client_id=cid"))
            .and(body_string_contains("scope=res%2F.default"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"access_token": "wi", "expires_in": 3600})),
            )
            .mount(&server)
            .await;
        let mut e = Endpoint::new(Kind::Workload, format!("{}/", server.uri()));
        e.tenant_id = Some("tenant-1".into());
        e.token_file = Some(file.to_string_lossy().into_owned());
        assert_eq!(
            source(&server, e.clone(), Some("cid"))
                .token()
                .await
                .unwrap()
                .secret(),
            "wi"
        );
        e.token_file = Some("/definitely/missing".into());
        assert!(
            source(&server, e, Some("cid"))
                .token()
                .await
                .unwrap_err()
                .to_string()
                .contains("federated token file")
        );
    }

    #[tokio::test]
    async fn arc_answers_the_challenge_with_the_key_file() {
        let server = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("x.key");
        std::fs::write(&key, "arc-secret").unwrap();
        Mock::given(method("GET"))
            .and(path("/arc"))
            .and(header("authorization", "Basic arc-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"access_token": "arc", "expires_on": 4_102_444_800_i64}),
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/arc"))
            .and(header("metadata", "true"))
            .respond_with(ResponseTemplate::new(401).insert_header(
                "www-authenticate",
                format!("Basic realm={}", key.display()).as_str(),
            ))
            .mount(&server)
            .await;
        let mut s = source(
            &server,
            Endpoint::new(Kind::Arc, format!("{}/arc", server.uri())),
            None,
        );
        s.arc_dir = Ok(dir.path().to_path_buf());
        assert_eq!(s.token().await.unwrap().secret(), "arc");

        // A challenge pointing outside the agent directory is refused.
        s.arc_dir = Ok(PathBuf::from("/elsewhere"));
        assert!(
            s.token()
                .await
                .unwrap_err()
                .to_string()
                .contains("unexpected Azure Arc")
        );
    }

    #[tokio::test]
    async fn workspace_host_is_resolved_from_the_resource_id() {
        let server = MockServer::start().await;
        let arm = format!("{}/arm/", server.uri());
        Mock::given(method("GET"))
            .and(path("/metadata/identity/oauth2/token"))
            .and(query_param("resource", arm.as_str()))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({"access_token": "arm-token", "expires_in": 60}),
                ),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/arm/subscriptions/s/workspaces/w"))
            .and(query_param("api-version", "2018-04-01"))
            .and(header("authorization", "Bearer arm-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"properties": {"workspaceUrl": "adb-1.2.azuredatabricks.net"}}),
            ))
            .mount(&server)
            .await;
        let uri = server.uri();
        let mut cfg = Config::default();
        cfg.set_attribute(
            "azure_workspace_resource_id",
            "/subscriptions/s/workspaces/w",
        )
        .unwrap();
        cfg.set_attribute("azure_use_msi", "true").unwrap();
        let mut cfg = cfg
            .resolve_with(
                move |k| (k == "AZURE_POD_IDENTITY_AUTHORITY_HOST").then(|| uri.clone()),
                None,
            )
            .await
            .unwrap();
        assert!(cfg.is_azure());
        let http = reqwest::Client::new();
        resolve_host(&mut cfg, &http, "/subscriptions/s/workspaces/w", &arm)
            .await
            .unwrap();
        assert_eq!(
            cfg.host.as_deref(),
            Some("https://adb-1.2.azuredatabricks.net")
        );
        // Already has a host: nothing to do.
        ensure_workspace_host(&mut cfg, &http).await.unwrap();
    }

    #[test]
    fn arc_secret_checks() {
        let dir = tempfile::tempdir().unwrap();
        let ok = dir.path().join("a.key");
        std::fs::write(&ok, "s").unwrap();
        assert_eq!(read_arc_secret(&ok, dir.path()).unwrap(), "s");
        let txt = dir.path().join("a.txt");
        std::fs::write(&txt, "s").unwrap();
        assert!(
            read_arc_secret(&txt, dir.path())
                .unwrap_err()
                .to_string()
                .contains(".key")
        );
        let big = dir.path().join("b.key");
        std::fs::write(&big, vec![b'x'; 5000]).unwrap();
        assert!(
            read_arc_secret(&big, dir.path())
                .unwrap_err()
                .to_string()
                .contains("exceeds")
        );
        assert!(read_arc_secret(&dir.path().join("none.key"), dir.path()).is_err());
        assert!(arc_challenge_file(None).is_err());
        let bad = HeaderValue::from_static("Basic");
        assert!(arc_challenge_file(Some(&bad)).is_err());
        let e = Endpoint::new(Kind::Ml, "u".into()).secret("hush".into());
        assert!(!format!("{e:?}").contains("hush"));
        let good = HeaderValue::from_static("Basic realm=\"/t/x.key\"");
        assert_eq!(
            arc_challenge_file(Some(&good)).unwrap(),
            PathBuf::from("/t/x.key")
        );
        let windows = arc_token_dir(&env(&[("PROGRAMDATA", "C:\\ProgramData")]));
        assert!(windows.is_ok() || !cfg!(any(target_os = "linux", windows)));
    }

    #[test]
    fn token_expiry_formats() {
        let t =
            parse_msi_token(br#"{"access_token":"a","expires_on":"4102444800"}"#, false).unwrap();
        assert_eq!(t.token_type, "Bearer");
        assert!(parse_msi_token(br#"{"access_token":"a","expires_in":60}"#, false).is_ok());
        assert!(parse_msi_token(br#"{"access_token":"a","expires_in":"x"}"#, false).is_err());
        assert!(
            parse_msi_token(br#"{"access_token":"a"}"#, false)
                .unwrap_err()
                .to_string()
                .contains("expires_on is missing")
        );
        assert!(parse_msi_token(br#"{"expires_in":60}"#, false).is_err());
        let ml = br#"{"access_token":"a","expires_on":"12/31/2099 11:00:00 PM +00:00"}"#;
        assert!(parse_msi_token(ml, true).is_ok());
        assert!(parse_msi_token(ml, false).is_err());
        assert!(parse_msi_token(br#"{"access_token":"a","expires_on":"soon"}"#, true).is_err());
    }
}
