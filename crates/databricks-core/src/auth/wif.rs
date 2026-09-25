//! Workload identity federation: exchange an OIDC ID token from the
//! environment for a Databricks token (RFC 8693).
//!
//! Go: `oidcStrategy` + `databricksOIDCTokenSource`. The ID token comes
//! from GitHub Actions (`github-oidc`), an environment variable
//! (`env-oidc`), a file (`file-oidc`), or, new in Rust, an in-memory
//! [`IdTokenSource`] set on the config (`mem-oidc`, databricks-sdk-go#1790).

use std::fmt;
use std::future::Future;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;

use super::common::{TokenHeaders, parse_oauth_token, send_token_request};
use super::oidc::OAuthEndpoints;
use super::token::{CachedTokenSource, Token, TokenSource};
use super::{CredentialsProvider, CredentialsStrategy};
use crate::config::{Config, HostType};
use crate::error::{Error, Result};

/// An OIDC ID token (a JWT).
#[derive(Clone)]
pub struct IdToken {
    /// The raw JWT.
    pub value: SecretString,
}

impl IdToken {
    /// Wrap a raw JWT.
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: SecretString::from(value.into()),
        }
    }
}

impl fmt::Debug for IdToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("IdToken(***)")
    }
}

/// Supplies OIDC ID tokens for the exchange. Go: `oidc.IDTokenSource`.
///
/// `audience` is the configured `audience`, else the account ID on
/// account hosts, else the Databricks token endpoint. Implement this, or
/// use [`IdToken`] (a fixed token) or [`IdTokenFn`] (a closure).
pub trait IdTokenSource: Send + Sync + fmt::Debug {
    /// An ID token for `audience`.
    fn id_token<'a>(&'a self, audience: &'a str) -> BoxFuture<'a, Result<IdToken>>;
}

/// A fixed token. Fine for short-lived clients; for long-lived ones use a
/// source that mints a fresh token each time.
impl IdTokenSource for IdToken {
    fn id_token<'a>(&'a self, _audience: &'a str) -> BoxFuture<'a, Result<IdToken>> {
        Box::pin(async move { Ok(self.clone()) })
    }
}

/// An [`IdTokenSource`] from an async closure taking the audience.
///
/// ```
/// use databricks_core::auth::{IdToken, IdTokenFn};
/// let source = IdTokenFn(|audience: String| async move {
///     Ok(IdToken::new(format!("jwt-for-{audience}")))
/// });
/// let cfg = databricks_core::Config::with_host("https://x").id_tokens(source);
/// # let _ = cfg;
/// ```
pub struct IdTokenFn<F>(pub F);

impl<F> fmt::Debug for IdTokenFn<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("IdTokenFn")
    }
}

impl<F, Fut> IdTokenSource for IdTokenFn<F>
where
    F: Fn(String) -> Fut + Send + Sync,
    Fut: Future<Output = Result<IdToken>> + Send + 'static,
{
    fn id_token<'a>(&'a self, audience: &'a str) -> BoxFuture<'a, Result<IdToken>> {
        Box::pin((self.0)(audience.to_owned()))
    }
}

fn auth(name: &str, message: impl Into<String>) -> Error {
    Error::Auth {
        auth_type: name.into(),
        message: message.into(),
    }
}

/// Build the provider and fetch the first token, as Go's
/// `tokenSourceStrategy` does.
async fn configure_wif(
    cfg: &Config,
    http: &reqwest::Client,
    name: &'static str,
    id_tokens: Arc<dyn IdTokenSource>,
) -> Result<Option<Arc<dyn CredentialsProvider>>> {
    if cfg.host.as_deref().is_none_or(str::is_empty) {
        return Err(auth(name, "missing Host"));
    }
    let source = Exchange {
        cfg: cfg.clone(),
        http: http.clone(),
        name,
        id_tokens,
    };
    let cache = CachedTokenSource::new(source, true);
    cache.token().await?;
    Ok(Some(
        Arc::new(TokenHeaders::bearer(cache)) as Arc<dyn CredentialsProvider>
    ))
}

/// The token exchange. Without `client_id` this is account-wide token
/// federation; with it, workload identity federation for that service
/// principal.
struct Exchange {
    cfg: Config,
    http: reqwest::Client,
    name: &'static str,
    id_tokens: Arc<dyn IdTokenSource>,
}

impl Exchange {
    fn audience(&self, endpoints: &OAuthEndpoints) -> String {
        if let Some(a) = self.cfg.attr("audience") {
            return a;
        }
        if self.cfg.host_type() != HostType::Workspace
            && let Some(acc) = self.cfg.account_id.clone().filter(|a| !a.is_empty())
        {
            return acc;
        }
        endpoints.token_endpoint.clone()
    }
}

impl TokenSource for Exchange {
    fn token(&self) -> BoxFuture<'_, Result<Token>> {
        Box::pin(async move {
            let endpoints = OAuthEndpoints::discover(&self.cfg, &self.http).await?;
            let audience = self.audience(&endpoints);
            let id = self.id_tokens.id_token(&audience).await?;
            let scopes = self.cfg.scopes_or_default().join(" ");
            let client_id = self.cfg.client_id.clone().filter(|c| !c.is_empty());
            let group = self.cfg.group_id.clone().filter(|g| !g.is_empty());
            let federation = if client_id.is_some() {
                "workload identity"
            } else {
                "account-wide"
            };
            tracing::debug!(auth = self.name, federation, "exchanging OIDC token");
            let url = endpoints.token_endpoint;
            let body = send_token_request(&url, || {
                self.http.post(&url).form(&[
                    (
                        "grant_type",
                        Some("urn:ietf:params:oauth:grant-type:token-exchange"),
                    ),
                    (
                        "subject_token_type",
                        Some("urn:ietf:params:oauth:token-type:jwt"),
                    ),
                    ("subject_token", Some(id.value.expose_secret())),
                    ("scope", Some(scopes.as_str())),
                    ("client_id", client_id.as_deref()),
                    ("assume_group", group.as_deref()),
                ])
            })
            .await?;
            parse_oauth_token(&body)
        })
    }
}

macro_rules! wif_strategy {
    ($(#[$doc:meta])* $ty:ident, $name:literal, |$cfg:ident, $http:ident| $source:expr) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, Default)]
        pub struct $ty;

        impl CredentialsStrategy for $ty {
            fn name(&self) -> &'static str {
                $name
            }

            fn configure<'a>(
                &'a self,
                $cfg: &'a Config,
                $http: &'a reqwest::Client,
            ) -> BoxFuture<'a, Result<Option<Arc<dyn CredentialsProvider>>>> {
                Box::pin(async move {
                    let source: Option<Arc<dyn IdTokenSource>> = $source?;
                    match source {
                        Some(s) => configure_wif($cfg, $http, $name, s).await,
                        None => Ok(None),
                    }
                })
            }
        }
    };
}

wif_strategy!(
    /// ID tokens from GitHub Actions (`id-token: write` permission).
    GithubOidcCredentials,
    "github-oidc",
    |cfg, http| github_source(cfg, http)
);

wif_strategy!(
    /// An ID token from the variable named by `oidc_token_env`
    /// (default `DATABRICKS_OIDC_TOKEN`), read on every exchange.
    EnvOidcCredentials,
    "env-oidc",
    |cfg, http| env_source(cfg)
);

wif_strategy!(
    /// An ID token from the file at `databricks_id_token_filepath`, read on
    /// every exchange so a rotated file is picked up.
    FileOidcCredentials,
    "file-oidc",
    |cfg, http| file_source(cfg)
);

wif_strategy!(
    /// An ID token from [`Config::id_token_source`] (never touches a file
    /// or the environment).
    MemOidcCredentials,
    "mem-oidc",
    |cfg, http| Ok::<_, Error>(cfg.id_token_source.clone())
);

fn github_source(cfg: &Config, http: &reqwest::Client) -> Result<Option<Arc<dyn IdTokenSource>>> {
    let name = "github-oidc";
    let url = cfg
        .attr("actions_id_token_request_url")
        .ok_or_else(|| auth(name, "missing ActionsIDTokenRequestURL"))?;
    let token = cfg
        .attr("actions_id_token_request_token")
        .ok_or_else(|| auth(name, "missing ActionsIDTokenRequestToken"))?;
    Ok(Some(Arc::new(GithubIdTokens {
        http: http.clone(),
        url,
        token: SecretString::from(token),
    })))
}

fn env_source(cfg: &Config) -> Result<Option<Arc<dyn IdTokenSource>>> {
    let var = cfg
        .attr("oidc_token_env")
        .unwrap_or_else(|| "DATABRICKS_OIDC_TOKEN".to_owned());
    if cfg.getenv(&var).is_none() {
        return Err(auth("env-oidc", format!("missing env var {var:?}")));
    }
    Ok(Some(Arc::new(EnvIdTokens {
        cfg: cfg.clone(),
        var,
    })))
}

fn file_source(cfg: &Config) -> Result<Option<Arc<dyn IdTokenSource>>> {
    let path = cfg
        .attr("databricks_id_token_filepath")
        .ok_or_else(|| auth("file-oidc", "missing path"))?;
    Ok(Some(Arc::new(FileIdTokens { path })))
}

struct GithubIdTokens {
    http: reqwest::Client,
    url: String,
    token: SecretString,
}

impl fmt::Debug for GithubIdTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GithubIdTokens")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct GithubIdTokenResponse {
    value: String,
}

impl IdTokenSource for GithubIdTokens {
    fn id_token<'a>(&'a self, audience: &'a str) -> BoxFuture<'a, Result<IdToken>> {
        Box::pin(async move {
            let mut url = reqwest::Url::parse(&self.url).map_err(|e| {
                auth(
                    "github-oidc",
                    format!("invalid ActionsIDTokenRequestURL: {e}"),
                )
            })?;
            if !audience.is_empty() {
                url.query_pairs_mut().append_pair("audience", audience);
            }
            let body = send_token_request(url.as_str(), || {
                self.http
                    .get(url.clone())
                    .bearer_auth(self.token.expose_secret())
            })
            .await
            .map_err(|e| {
                auth(
                    "github-oidc",
                    format!("failed to request ID token from {}: {e}", self.url),
                )
            })?;
            let r: GithubIdTokenResponse =
                serde_json::from_slice(&body).map_err(|e| Error::json("GitHub ID token", e))?;
            Ok(IdToken::new(r.value))
        })
    }
}

struct EnvIdTokens {
    cfg: Config,
    var: String,
}

impl fmt::Debug for EnvIdTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnvIdTokens")
            .field("var", &self.var)
            .finish_non_exhaustive()
    }
}

impl IdTokenSource for EnvIdTokens {
    fn id_token<'a>(&'a self, _audience: &'a str) -> BoxFuture<'a, Result<IdToken>> {
        Box::pin(async move {
            self.cfg
                .getenv(&self.var)
                .map(IdToken::new)
                .ok_or_else(|| auth("env-oidc", format!("missing env var {:?}", self.var)))
        })
    }
}

#[derive(Debug)]
struct FileIdTokens {
    path: String,
}

impl IdTokenSource for FileIdTokens {
    fn id_token<'a>(&'a self, _audience: &'a str) -> BoxFuture<'a, Result<IdToken>> {
        Box::pin(async move {
            let t = tokio::fs::read_to_string(&self.path).await.map_err(|e| {
                auth(
                    "file-oidc",
                    if e.kind() == std::io::ErrorKind::NotFound {
                        format!("file {:?} does not exist", self.path)
                    } else {
                        format!("read {:?}: {e}", self.path)
                    },
                )
            })?;
            if t.is_empty() {
                return Err(auth("file-oidc", format!("file {:?} is empty", self.path)));
            }
            Ok(IdToken::new(t))
        })
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn exchange(cfg: Config) -> Exchange {
        Exchange {
            cfg,
            http: reqwest::Client::new(),
            name: "mem-oidc",
            id_tokens: Arc::new(IdToken::new("t")),
        }
    }

    #[tokio::test]
    async fn audience_rules_and_static_tokens() {
        let endpoints = OAuthEndpoints {
            authorization_endpoint: String::new(),
            token_endpoint: "https://h/oidc/v1/token".into(),
        };
        let mut cfg = Config::with_host("https://h");
        assert_eq!(
            exchange(cfg.clone()).audience(&endpoints),
            "https://h/oidc/v1/token"
        );
        cfg.account_id = Some("acc".into());
        cfg.resolved_host_type = Some(HostType::Unified);
        assert_eq!(exchange(cfg.clone()).audience(&endpoints), "acc");
        cfg.set_attribute("audience", "aud").unwrap();
        assert_eq!(exchange(cfg).audience(&endpoints), "aud");

        let t = IdToken::new("jwt").id_token("any").await.unwrap();
        assert_eq!(t.value.expose_secret(), "jwt");
        let e = configure_wif(
            &Config::default(),
            &reqwest::Client::new(),
            "mem-oidc",
            Arc::new(t),
        )
        .await
        .unwrap_err();
        assert!(e.to_string().contains("missing Host"));
    }

    #[tokio::test]
    async fn github_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/fail"))
            .respond_with(ResponseTemplate::new(400).set_body_string("nope"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/garbage"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        let gh = |url: String| GithubIdTokens {
            http: reqwest::Client::new(),
            url,
            token: SecretString::from("t"),
        };
        assert!(format!("{:?}", gh("u".into())).contains("GithubIdTokens"));
        let e = gh("not a url".into()).id_token("a").await.unwrap_err();
        assert!(
            e.to_string().contains("invalid ActionsIDTokenRequestURL"),
            "{e}"
        );
        let e = gh(format!("{}/fail", server.uri()))
            .id_token("")
            .await
            .unwrap_err();
        assert!(e.to_string().contains("failed to request ID token"), "{e}");
        let e = gh(format!("{}/garbage", server.uri()))
            .id_token("a")
            .await
            .unwrap_err();
        assert!(e.to_string().contains("GitHub ID token"), "{e}");
    }

    #[tokio::test]
    async fn env_and_file_edge_cases() {
        let cfg = Config::default()
            .resolve_with(|_| None, None)
            .await
            .unwrap();
        let env = EnvIdTokens {
            cfg,
            var: "V".into(),
        };
        assert!(format!("{env:?}").contains("\"V\""));
        assert!(
            env.id_token("a")
                .await
                .unwrap_err()
                .to_string()
                .contains("missing env var")
        );

        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty");
        std::fs::write(&empty, "").unwrap();
        let file = |p: &std::path::Path| FileIdTokens {
            path: p.to_string_lossy().into_owned(),
        };
        let e = file(&empty).id_token("a").await.unwrap_err();
        assert!(e.to_string().contains("is empty"), "{e}");
        let e = file(dir.path()).id_token("a").await.unwrap_err();
        assert!(e.to_string().contains("read "), "{e}");
    }
}
