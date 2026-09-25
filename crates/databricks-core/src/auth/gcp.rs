//! `oauth-m2m-gcp`: Databricks OAuth M2M plus a Google Cloud access token.
//!
//! Go: `GcpM2mCredentials` (databricks-sdk-go#1815). The identity is a
//! Databricks service principal (the `Authorization` header). A Google
//! access token with the `cloud-platform` scope goes in
//! `X-Databricks-GCP-SA-Access-Token`, so Databricks can provision GCP
//! resources for the caller. This is how GCP account-level provisioning
//! APIs are called when SSO is enabled.
//!
//! The Google token comes from `google_credentials` (a service-account
//! key or authorized-user JSON, as a path or inline) or, failing that,
//! from impersonating `google_service_account` with Application Default
//! Credentials.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use futures_util::future::BoxFuture;
use reqwest::header::HeaderName;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::json;

use super::common::{
    TokenHeaders, instant_from_unix, parse_oauth_token, parse_timestamp, send_token_request,
};
use super::m2m::ClientCredentials;
use super::token::{CachedTokenSource, Token, TokenSource};
use super::{CredentialsProvider, CredentialsStrategy};
use crate::config::Config;
use crate::error::{Error, Result};

const NAME: &str = "oauth-m2m-gcp";
const GCP_SA_ACCESS_TOKEN: &str = "x-databricks-gcp-sa-access-token";
const SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/cloud-platform",
    "https://www.googleapis.com/auth/compute",
];
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const IAM_CREDENTIALS: &str = "https://iamcredentials.googleapis.com";

/// Databricks M2M identity plus Google access-token passthrough. Select it
/// with `auth_type = "oauth-m2m-gcp"`: it combines the `oauth` and
/// `google` attribute groups, which config validation otherwise rejects.
#[derive(Debug, Clone, Copy, Default)]
pub struct GcpM2mCredentials;

fn auth(message: impl Into<String>) -> Error {
    Error::Auth {
        auth_type: NAME.into(),
        message: message.into(),
    }
}

impl CredentialsStrategy for GcpM2mCredentials {
    fn name(&self) -> &'static str {
        NAME
    }

    fn configure<'a>(
        &'a self,
        cfg: &'a Config,
        http: &'a reqwest::Client,
    ) -> BoxFuture<'a, Result<Option<Arc<dyn CredentialsProvider>>>> {
        Box::pin(async move {
            if !cfg.is_gcp()
                || (cfg.attr("google_credentials").is_none()
                    && cfg.attr("google_service_account").is_none())
            {
                return Ok(None);
            }
            if cfg.client_id.as_deref().is_none_or(str::is_empty)
                || cfg
                    .client_secret
                    .as_ref()
                    .is_none_or(|s| s.expose_secret().is_empty())
            {
                return Ok(None);
            }
            // Local checks first, so bad Google credentials fail before any
            // network call.
            let google = google_access_tokens(cfg, http)?;
            let Some(primary) = ClientCredentials::from_config(cfg, http, NAME).await? else {
                return Ok(None);
            };
            tracing::info!(
                "using Databricks OAuth (M2M) with GCP service account access token passthrough"
            );
            // Google tokens are fetched on demand, as Go does (no
            // background refresh). The Google header is required: this mode
            // exists to send it.
            Ok(Some(Arc::new(TokenHeaders {
                primary: CachedTokenSource::new(primary, false),
                secondary: Some((
                    HeaderName::from_static(GCP_SA_ACCESS_TOKEN),
                    CachedTokenSource::new(BoxedSource(google), false),
                )),
                fixed: Vec::new(),
            }) as Arc<dyn CredentialsProvider>))
        })
    }
}

struct BoxedSource(Box<dyn TokenSource>);

impl TokenSource for BoxedSource {
    fn token(&self) -> BoxFuture<'_, Result<Token>> {
        self.0.token()
    }
}

/// Go: `googleAccessTokenSource`. Prefers `google_credentials`.
fn google_access_tokens(cfg: &Config, http: &reqwest::Client) -> Result<Box<dyn TokenSource>> {
    if let Some(creds) = cfg.attr("google_credentials") {
        let json = read_credentials(&creds);
        return from_json(&json, http).map_err(|e| {
            auth(format!(
                "could not read GoogleCredentials. Make sure the file exists, or the JSON content is valid: {e}"
            ))
        });
    }
    let target = cfg.attr("google_service_account").ok_or_else(|| {
        auth("oauth-m2m-gcp requires google_credentials or google_service_account to be set")
    })?;
    let base = application_default(cfg, http)
        .map_err(|e| auth(format!("could not create GCP SA access token source: {e}")))?;
    Ok(Box::new(Impersonated {
        http: http.clone(),
        base,
        target,
        endpoint: IAM_CREDENTIALS.to_owned(),
    }))
}

/// A path to a JSON file, or the JSON itself (Go: `readCredentials`).
fn read_credentials(value: &str) -> String {
    std::fs::read_to_string(value).unwrap_or_else(|_| value.to_owned())
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CredentialsFile {
    ServiceAccount {
        client_email: String,
        private_key: String,
        #[serde(default)]
        private_key_id: String,
        #[serde(default)]
        token_uri: Option<String>,
    },
    AuthorizedUser {
        client_id: String,
        client_secret: String,
        refresh_token: String,
        #[serde(default)]
        token_uri: Option<String>,
    },
}

fn from_json(json: &str, http: &reqwest::Client) -> Result<Box<dyn TokenSource>> {
    let kind = serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_owned))
        .unwrap_or_default();
    let file: CredentialsFile = serde_json::from_str(json).map_err(|e| {
        if matches!(kind.as_str(), "service_account" | "authorized_user") {
            Error::json("Google credentials", e)
        } else {
            Error::Config(format!(
                "Google credentials of type {kind:?} are not supported; use a service_account or authorized_user file"
            ))
        }
    })?;
    Ok(match file {
        CredentialsFile::ServiceAccount {
            client_email,
            private_key,
            private_key_id,
            token_uri,
        } => Box::new(ServiceAccountKey {
            http: http.clone(),
            email: client_email,
            key: parse_pem_key(&private_key)?,
            key_id: private_key_id,
            token_uri: token_uri.unwrap_or_else(|| GOOGLE_TOKEN_URL.to_owned()),
        }),
        CredentialsFile::AuthorizedUser {
            client_id,
            client_secret,
            refresh_token,
            token_uri,
        } => Box::new(AuthorizedUser {
            http: http.clone(),
            client_id,
            client_secret: SecretString::from(client_secret),
            refresh_token: SecretString::from(refresh_token),
            token_uri: token_uri.unwrap_or_else(|| GOOGLE_TOKEN_URL.to_owned()),
        }),
    })
}

fn parse_pem_key(pem: &str) -> Result<RsaKeyPair> {
    let b64: String = pem
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("-----"))
        .collect();
    let der = STANDARD
        .decode(b64)
        .map_err(|e| Error::Config(format!("private_key is not valid PEM: {e}")))?;
    RsaKeyPair::from_pkcs8(&der)
        .map_err(|e| Error::Config(format!("private_key is not a PKCS#8 RSA key: {e}")))
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// A service-account key: a self-signed JWT exchanged for an access token
/// (RFC 7523).
struct ServiceAccountKey {
    http: reqwest::Client,
    email: String,
    key: RsaKeyPair,
    key_id: String,
    token_uri: String,
}

impl ServiceAccountKey {
    fn assertion(&self) -> Result<String> {
        let iat = now_unix();
        let header = json!({"alg": "RS256", "typ": "JWT", "kid": self.key_id});
        let claims = json!({
            "iss": self.email,
            "scope": SCOPES.join(" "),
            "aud": self.token_uri,
            "iat": iat,
            "exp": iat + 3600,
        });
        let signing_input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let mut sig = vec![0; self.key.public_modulus_len()];
        self.key
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                signing_input.as_bytes(),
                &mut sig,
            )
            .map_err(|_| Error::Config("failed to sign the service-account JWT".into()))?;
        Ok(format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(sig)))
    }
}

impl TokenSource for ServiceAccountKey {
    fn token(&self) -> BoxFuture<'_, Result<Token>> {
        Box::pin(async move {
            let assertion = self.assertion()?;
            let body = send_token_request(&self.token_uri, || {
                self.http.post(&self.token_uri).form(&[
                    ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                    ("assertion", assertion.as_str()),
                ])
            })
            .await?;
            parse_oauth_token(&body)
        })
    }
}

/// `gcloud auth application-default login` credentials: a refresh token.
struct AuthorizedUser {
    http: reqwest::Client,
    client_id: String,
    client_secret: SecretString,
    refresh_token: SecretString,
    token_uri: String,
}

impl TokenSource for AuthorizedUser {
    fn token(&self) -> BoxFuture<'_, Result<Token>> {
        Box::pin(async move {
            let body = send_token_request(&self.token_uri, || {
                self.http.post(&self.token_uri).form(&[
                    ("grant_type", "refresh_token"),
                    ("client_id", self.client_id.as_str()),
                    ("client_secret", self.client_secret.expose_secret()),
                    ("refresh_token", self.refresh_token.expose_secret()),
                ])
            })
            .await?;
            parse_oauth_token(&body)
        })
    }
}

/// The GCE / GKE metadata server's default service account.
struct Metadata {
    http: reqwest::Client,
    url: String,
}

/// Off GCE the metadata host doesn't resolve; fail fast rather than
/// retrying for a minute (Go checks `metadata.OnGCE()` first).
const METADATA_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

impl TokenSource for Metadata {
    fn token(&self) -> BoxFuture<'_, Result<Token>> {
        Box::pin(async move {
            let not_gce = |e: &dyn std::fmt::Display| {
                auth(format!(
                    "no Google credentials: GOOGLE_APPLICATION_CREDENTIALS and the gcloud default credentials file are missing, and the GCE metadata server did not answer ({e})"
                ))
            };
            let probe = self
                .http
                .get(&self.url)
                .header("metadata-flavor", "Google")
                .timeout(METADATA_TIMEOUT)
                .send()
                .await;
            if let Err(e) = probe
                && (e.is_connect() || e.is_timeout())
            {
                return Err(not_gce(&e));
            }
            let body = send_token_request(&self.url, || {
                self.http
                    .get(&self.url)
                    .header("metadata-flavor", "Google")
                    .timeout(METADATA_TIMEOUT)
            })
            .await?;
            parse_oauth_token(&body)
        })
    }
}

/// Application Default Credentials: `GOOGLE_APPLICATION_CREDENTIALS`, then
/// gcloud's well-known file, then the metadata server.
fn application_default(cfg: &Config, http: &reqwest::Client) -> Result<Box<dyn TokenSource>> {
    if let Some(path) = cfg.getenv("GOOGLE_APPLICATION_CREDENTIALS") {
        let json = std::fs::read_to_string(&path)
            .map_err(|e| Error::Config(format!("read {path:?}: {e}")))?;
        return from_json(&json, http);
    }
    let dir = cfg.getenv("CLOUDSDK_CONFIG").or_else(|| {
        if cfg!(windows) {
            cfg.getenv("APPDATA").map(|a| format!("{a}\\gcloud"))
        } else {
            cfg.getenv("HOME").map(|h| format!("{h}/.config/gcloud"))
        }
    });
    if let Some(json) = dir
        .map(|d| std::path::Path::new(&d).join("application_default_credentials.json"))
        .and_then(|p| std::fs::read_to_string(p).ok())
    {
        return from_json(&json, http);
    }
    let host = cfg
        .getenv("GCE_METADATA_HOST")
        .unwrap_or_else(|| "metadata.google.internal".to_owned());
    let mut url = reqwest::Url::parse(&format!(
        "http://{host}/computeMetadata/v1/instance/service-accounts/default/token"
    ))
    .map_err(|e| Error::Config(format!("invalid GCE_METADATA_HOST: {e}")))?;
    url.query_pairs_mut()
        .append_pair("scopes", &SCOPES.join(","));
    Ok(Box::new(Metadata {
        http: http.clone(),
        url: url.into(),
    }))
}

/// IAM Credentials `generateAccessToken` for `target`, authenticated as
/// the ADC identity.
struct Impersonated {
    http: reqwest::Client,
    base: Box<dyn TokenSource>,
    target: String,
    endpoint: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeneratedToken {
    access_token: String,
    expire_time: String,
}

impl TokenSource for Impersonated {
    fn token(&self) -> BoxFuture<'_, Result<Token>> {
        Box::pin(async move {
            let base = self.base.token().await?;
            let url = format!(
                "{}/v1/projects/-/serviceAccounts/{}:generateAccessToken",
                self.endpoint,
                url::form_urlencoded::byte_serialize(self.target.as_bytes()).collect::<String>()
            );
            let body = send_token_request(&url, || {
                self.http
                    .post(&url)
                    .bearer_auth(base.secret())
                    .json(&json!({"scope": SCOPES, "lifetime": "3600s"}))
            })
            .await?;
            let t: GeneratedToken =
                serde_json::from_slice(&body).map_err(|e| Error::json("generateAccessToken", e))?;
            let expiry = parse_timestamp(&t.expire_time)
                .ok_or_else(|| auth(format!("cannot parse expireTime {:?}", t.expire_time)))?;
            Ok(Token {
                access_token: SecretString::from(t.access_token),
                token_type: "Bearer".into(),
                expiry: Some(instant_from_unix(expiry)),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use aws_lc_rs::encoding::AsDer;
    use aws_lc_rs::rsa::{KeyPair, KeySize};
    use aws_lc_rs::signature::{KeyPair as _, RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};
    use wiremock::matchers::{body_json, body_string_contains, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    /// A throwaway key generated per test run (no key material in git).
    fn test_key() -> (String, KeyPair) {
        let kp = KeyPair::generate(KeySize::Rsa2048).unwrap();
        let der = AsDer::<aws_lc_rs::encoding::Pkcs8V1Der>::as_der(&kp).unwrap();
        let b64 = STANDARD.encode(der.as_ref());
        let pem = format!("-----BEGIN PRIVATE KEY-----\n{b64}\n-----END PRIVATE KEY-----\n");
        (pem, kp)
    }

    #[tokio::test]
    async fn service_account_key_signs_a_verifiable_jwt() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains(
                "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer",
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"access_token": "ya29", "expires_in": 3599})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let (pem, kp) = test_key();
        let json = json!({
            "type": "service_account",
            "client_email": "sa@p.iam.gserviceaccount.com",
            "private_key": pem,
            "private_key_id": "kid-1",
            "token_uri": format!("{}/token", server.uri()),
        })
        .to_string();
        let t = from_json(&json, &reqwest::Client::new())
            .unwrap()
            .token()
            .await
            .unwrap();
        assert_eq!(t.secret(), "ya29");

        let req = &server.received_requests().await.unwrap()[0];
        let form: std::collections::HashMap<String, String> =
            url::form_urlencoded::parse(&req.body)
                .into_owned()
                .collect();
        let jwt = &form["assertion"];
        let (input, sig) = jwt.rsplit_once('.').unwrap();
        UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, kp.public_key().as_ref())
            .verify(input.as_bytes(), &URL_SAFE_NO_PAD.decode(sig).unwrap())
            .unwrap();
        let claims: serde_json::Value = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(input.split('.').nth(1).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(claims["iss"], "sa@p.iam.gserviceaccount.com");
        assert_eq!(claims["aud"], format!("{}/token", server.uri()));
        assert!(claims["scope"].as_str().unwrap().contains("cloud-platform"));
    }

    #[tokio::test]
    async fn authorized_user_refreshes_and_impersonation_uses_it() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("refresh_token=rt"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"access_token": "user", "expires_in": 3599})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/projects/-/serviceAccounts/target%40p.iam.gserviceaccount.com:generateAccessToken"))
            .and(header("authorization", "Bearer user"))
            .and(body_json(json!({"scope": SCOPES, "lifetime": "3600s"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"accessToken": "impersonated", "expireTime": "2099-01-01T00:00:00Z"}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        let json = json!({
            "type": "authorized_user", "client_id": "c", "client_secret": "s",
            "refresh_token": "rt", "token_uri": format!("{}/token", server.uri()),
        })
        .to_string();
        let http = reqwest::Client::new();
        let imp = Impersonated {
            http: http.clone(),
            base: from_json(&json, &http).unwrap(),
            target: "target@p.iam.gserviceaccount.com".into(),
            endpoint: server.uri(),
        };
        assert_eq!(imp.token().await.unwrap().secret(), "impersonated");
    }

    #[tokio::test]
    async fn metadata_server_is_the_last_resort() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/computeMetadata/v1/instance/service-accounts/default/token",
            ))
            .and(header("metadata-flavor", "Google"))
            .and(query_param("scopes", SCOPES.join(",")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"access_token": "gce", "expires_in": 100})),
            )
            .mount(&server)
            .await;
        let host = server.uri().trim_start_matches("http://").to_owned();
        let cfg = Config::default()
            .resolve_with(
                move |k| (k == "GCE_METADATA_HOST").then(|| host.clone()),
                None,
            )
            .await
            .unwrap();
        let t = application_default(&cfg, &reqwest::Client::new())
            .unwrap()
            .token()
            .await
            .unwrap();
        assert_eq!(t.secret(), "gce");
    }

    #[tokio::test]
    async fn metadata_server_absent_fails_fast() {
        // A port nothing listens on: connection refused, no retries.
        let m = Metadata {
            http: reqwest::Client::new(),
            url: "http://127.0.0.1:9/token".into(),
        };
        let started = std::time::Instant::now();
        let e = m.token().await.unwrap_err();
        assert!(
            e.to_string().contains("GCE metadata server did not answer"),
            "{e}"
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn credentials_errors() {
        let http = reqwest::Client::new();
        let err = |j: &str| from_json(j, &http).err().unwrap().to_string();
        assert!(err(r#"{"type":"external_account"}"#).contains("not supported"));
        assert!(err(r#"{"type":"service_account"}"#).contains("Google credentials"));
        assert!(
            err(r#"{"type":"service_account","client_email":"e","private_key":"!!"}"#)
                .contains("not valid PEM")
        );
        assert!(
            err(r#"{"type":"service_account","client_email":"e","private_key":"AAAA"}"#)
                .contains("PKCS#8")
        );
        assert_eq!(read_credentials("{\"inline\":1}"), "{\"inline\":1}");
    }
}
