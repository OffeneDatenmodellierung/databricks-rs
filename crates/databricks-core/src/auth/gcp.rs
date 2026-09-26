//! Google Cloud auth: `oauth-m2m-gcp`, `google-credentials` and
//! `google-id`.
//!
//! Google tokens come from Google's `google-cloud-auth` crate: service
//! account keys, `gcloud` user credentials, impersonation, external
//! accounts (workload identity federation from files, URLs, executables or
//! AWS) and the metadata server. This module only adds the Databricks
//! parts (Go: `auth_gcp_*.go`):
//!
//! * `Authorization: Bearer <Google ID token>` with the workspace host as
//!   the audience (`google-credentials`, `google-id`), or a Databricks M2M
//!   token (`oauth-m2m-gcp`, databricks-sdk-go#1815);
//! * a Google access token with the `cloud-platform` scope in
//!   `X-Databricks-GCP-SA-Access-Token`, which Databricks uses to call
//!   Google Cloud APIs for the caller.
//!
//! For service-account keys `google-cloud-auth` mints self-signed JWT
//! access tokens (AIP-4111) where Go exchanges them at Google's token
//! endpoint. Both are accepted by Google Cloud APIs, which is all
//! Databricks does with the token.

use std::sync::Arc;

use futures_util::future::BoxFuture;
use google_cloud_auth::credentials::idtoken::{self, IDTokenCredentials};
use google_cloud_auth::credentials::{
    AccessTokenCredentials, Builder as AdcBuilder, Credentials, external_account, impersonated,
    service_account, user_account,
};
use reqwest::header::HeaderName;
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;

use super::common::{Secondary, TokenHeaders};
use super::m2m::ClientCredentials;
use super::token::{CachedTokenSource, Token, TokenSource};
use super::{CredentialsProvider, CredentialsStrategy, reject_group_role};
use crate::config::Config;
use crate::error::{Error, Result};

const GCP_SA_ACCESS_TOKEN: &str = "x-databricks-gcp-sa-access-token";
const SCOPES: [&str; 2] = [
    "https://www.googleapis.com/auth/cloud-platform",
    "https://www.googleapis.com/auth/compute",
];

fn auth(name: &str, message: impl Into<String>) -> Error {
    Error::Auth {
        auth_type: name.into(),
        message: message.into(),
    }
}

fn google_err(name: &str, e: impl std::fmt::Display) -> Error {
    auth(name, e.to_string())
}

/// A path to a JSON file, or the JSON itself (Go: `readCredentials`).
fn read_credentials(name: &str, value: &str) -> Result<Value> {
    let text = std::fs::read_to_string(value).unwrap_or_else(|_| value.to_owned());
    serde_json::from_str(&text).map_err(|e| {
        auth(
            name,
            format!("could not read GoogleCredentials. Make sure the file exists, or the JSON content is valid: {e}"),
        )
    })
}

fn kind(json: &Value) -> &str {
    json.get("type").and_then(Value::as_str).unwrap_or_default()
}

/// Access tokens (cloud-platform + compute scopes) from a credentials file.
fn access_from_json(name: &str, json: Value) -> Result<AccessTokenCredentials> {
    let built = match kind(&json) {
        "service_account" => service_account::Builder::new(json)
            .with_access_specifier(service_account::AccessSpecifier::from_scopes(SCOPES))
            .build_access_token_credentials(),
        "authorized_user" => user_account::Builder::new(json)
            .with_scopes(SCOPES)
            .build_access_token_credentials(),
        "external_account" => external_account::Builder::new(json)
            .with_scopes(SCOPES)
            .build_access_token_credentials(),
        "impersonated_service_account" => impersonated::Builder::new(json)
            .with_scopes(SCOPES)
            .build_access_token_credentials(),
        other => {
            return Err(auth(
                name,
                format!("unsupported Google credentials type {other:?}"),
            ));
        }
    };
    built.map_err(|e| {
        google_err(
            name,
            format!("could not obtain OAuth2 token from JSON: {e}"),
        )
    })
}

/// Application Default Credentials (`GOOGLE_APPLICATION_CREDENTIALS`,
/// gcloud's file, then the metadata server).
fn adc(name: &str) -> Result<Credentials> {
    AdcBuilder::default()
        .with_scopes(SCOPES)
        .build()
        .map_err(|e| google_err(name, format!("{e}. {ADC_HINT}")))
}

/// Access tokens for `service_account`, impersonated from ADC.
fn impersonated_access(name: &str, service_account: &str) -> Result<AccessTokenCredentials> {
    impersonated::Builder::from_source_credentials(adc(name)?)
        .with_target_principal(service_account)
        .with_scopes(SCOPES)
        .build_access_token_credentials()
        .map_err(|e| {
            google_err(
                name,
                format!("could not create GCP SA access token source: {e}"),
            )
        })
}

/// `…/serviceAccounts/{email}:generateAccessToken` → `email`.
fn impersonated_email(url: &str) -> Option<&str> {
    let (_, rest) = url.split_once("/serviceAccounts/")?;
    rest.split_once(':')
        .map(|(email, _)| email)
        .filter(|e| !e.is_empty())
}

/// ID tokens for `audience` from a credentials file. Go's `idtoken`
/// supports service accounts, impersonated service accounts and external
/// accounts that impersonate a service account; user credentials have no
/// ID token for an arbitrary audience.
fn id_tokens_from_json(name: &str, audience: &str, mut json: Value) -> Result<IDTokenCredentials> {
    let built = match kind(&json) {
        "service_account" => idtoken::service_account::Builder::new(audience, json).build(),
        "impersonated_service_account" => idtoken::impersonated::Builder::new(audience, json)
            .with_include_email()
            .build(),
        "external_account" => {
            let url = json
                .get("service_account_impersonation_url")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let email = url.as_deref().and_then(impersonated_email).map(str::to_owned).ok_or_else(|| {
                auth(
                    name,
                    "ID tokens from external_account credentials need service_account_impersonation_url",
                )
            })?;
            // The federated identity itself asks IAM for the SA's ID token.
            if let Some(o) = json.as_object_mut() {
                o.remove("service_account_impersonation_url");
            }
            let source = external_account::Builder::new(json)
                .with_scopes(SCOPES)
                .build()
                .map_err(|e| google_err(name, e))?;
            idtoken::impersonated::Builder::from_source_credentials(audience, email, source)
                .with_include_email()
                .build()
        }
        other => {
            return Err(auth(
                name,
                format!("Google credentials of type {other:?} cannot mint ID tokens"),
            ));
        }
    };
    built.map_err(|e| google_err(name, format!("could not obtain OIDC token from JSON: {e}")))
}

/// Google access tokens (cached and refreshed by google-cloud-auth).
struct GoogleAccess {
    name: &'static str,
    creds: AccessTokenCredentials,
}

impl TokenSource for GoogleAccess {
    fn token(&self) -> BoxFuture<'_, Result<Token>> {
        Box::pin(async move {
            let t = self
                .creds
                .access_token()
                .await
                .map_err(|e| google_err(self.name, e))?;
            Ok(Token {
                access_token: SecretString::from(t.token),
                token_type: "Bearer".into(),
                expiry: None,
            })
        })
    }
}

/// Google ID tokens (cached and refreshed by google-cloud-auth).
struct GoogleId {
    name: &'static str,
    creds: IDTokenCredentials,
}

impl TokenSource for GoogleId {
    fn token(&self) -> BoxFuture<'_, Result<Token>> {
        Box::pin(async move {
            let t = self
                .creds
                .id_token()
                .await
                .map_err(|e| google_err(self.name, e))?;
            Ok(Token {
                access_token: SecretString::from(t),
                token_type: "Bearer".into(),
                expiry: None,
            })
        })
    }
}

fn gcp_header(source: impl TokenSource + 'static, optional: bool) -> Secondary {
    Secondary {
        header: HeaderName::from_static(GCP_SA_ACCESS_TOKEN),
        source: Arc::new(source),
        optional,
    }
}

fn provider(
    primary: impl TokenSource + 'static,
    secondary: Option<Secondary>,
) -> Arc<dyn CredentialsProvider> {
    Arc::new(TokenHeaders {
        primary: Arc::new(primary),
        secondary,
        fixed: Vec::new(),
    })
}

const ADC_HINT: &str = "Running 'gcloud auth application-default login' may help";

// ------------------------------------------------------------ oauth-m2m-gcp

const M2M_GCP: &str = "oauth-m2m-gcp";

/// Databricks M2M identity plus Google access-token passthrough. Select it
/// with `auth_type = "oauth-m2m-gcp"`: it combines the `oauth` and
/// `google` attribute groups, which config validation otherwise rejects.
#[derive(Debug, Clone, Copy, Default)]
pub struct GcpM2mCredentials;

impl CredentialsStrategy for GcpM2mCredentials {
    fn name(&self) -> &'static str {
        M2M_GCP
    }

    fn configure<'a>(
        &'a self,
        cfg: &'a Config,
        http: &'a reqwest::Client,
    ) -> BoxFuture<'a, Result<Option<Arc<dyn CredentialsProvider>>>> {
        Box::pin(async move {
            let creds = cfg.attr("google_credentials");
            let sa = cfg.attr("google_service_account");
            let has_secret = cfg
                .client_secret
                .as_ref()
                .is_some_and(|s| !s.expose_secret().is_empty());
            if !cfg.is_gcp()
                || (creds.is_none() && sa.is_none())
                || cfg.client_id.as_deref().is_none_or(str::is_empty)
                || !has_secret
            {
                return Ok(None);
            }
            // Local checks first, so bad Google credentials fail before any
            // network call. The Google header is required: this mode exists
            // to send it.
            let google = match creds {
                Some(c) => access_from_json(M2M_GCP, read_credentials(M2M_GCP, &c)?)?,
                None => impersonated_access(M2M_GCP, &sa.unwrap_or_default())?,
            };
            let primary = ClientCredentials::from_config(cfg, http, M2M_GCP).await?;
            let primary = primary.map(|p| CachedTokenSource::new(p, false));
            tracing::info!("using Databricks OAuth (M2M) with GCP SA access token passthrough");
            let google = GoogleAccess {
                name: M2M_GCP,
                creds: google,
            };
            Ok(primary.map(|p| provider(p, Some(gcp_header(google, false)))))
        })
    }
}

// ------------------------------------------------------- google-credentials

const CREDENTIALS: &str = "google-credentials";

/// A Google credentials file (`google_credentials`, a path or inline
/// JSON): an ID token for the workspace plus an access token.
#[derive(Debug, Clone, Copy, Default)]
pub struct GoogleCredentials;

impl CredentialsStrategy for GoogleCredentials {
    fn name(&self) -> &'static str {
        CREDENTIALS
    }

    fn configure<'a>(
        &'a self,
        cfg: &'a Config,
        _http: &'a reqwest::Client,
    ) -> BoxFuture<'a, Result<Option<Arc<dyn CredentialsProvider>>>> {
        Box::pin(async move {
            reject_group_role(cfg, CREDENTIALS)?;
            let Some(value) = cfg.attr("google_credentials").filter(|_| cfg.is_gcp()) else {
                return Ok(None);
            };
            let host = cfg.host.clone().unwrap_or_default();
            let json = read_credentials(CREDENTIALS, &value)?;
            let id = id_tokens_from_json(CREDENTIALS, &host, json.clone())?;
            let access = access_from_json(CREDENTIALS, json)?;
            tracing::info!("using Google credentials");
            let id = GoogleId {
                name: CREDENTIALS,
                creds: id,
            };
            let access = GoogleAccess {
                name: CREDENTIALS,
                creds: access,
            };
            Ok(Some(provider(id, Some(gcp_header(access, true)))))
        })
    }
}

// ---------------------------------------------------------------- google-id

const GOOGLE_ID: &str = "google-id";

/// Impersonate `google_service_account` with Application Default
/// Credentials: its ID token for the workspace, plus its access token.
#[derive(Debug, Clone, Copy, Default)]
pub struct GoogleIdCredentials;

impl CredentialsStrategy for GoogleIdCredentials {
    fn name(&self) -> &'static str {
        GOOGLE_ID
    }

    fn configure<'a>(
        &'a self,
        cfg: &'a Config,
        _http: &'a reqwest::Client,
    ) -> BoxFuture<'a, Result<Option<Arc<dyn CredentialsProvider>>>> {
        Box::pin(async move {
            reject_group_role(cfg, GOOGLE_ID)?;
            let Some(sa) = cfg.attr("google_service_account").filter(|_| cfg.is_gcp()) else {
                return Ok(None);
            };
            let host = cfg.host.clone().unwrap_or_default();
            let id = idtoken::impersonated::Builder::from_source_credentials(
                &host,
                &sa,
                adc(GOOGLE_ID)?,
            )
            .with_include_email()
            .build()
            .map_err(|e| {
                google_err(
                    GOOGLE_ID,
                    format!("could not obtain OIDC token. {e} {ADC_HINT}"),
                )
            })?;
            // Go continues without the access-token header if it can't be
            // set up.
            let secondary = impersonated_access(GOOGLE_ID, &sa)
                .inspect_err(|e| tracing::warn!("{e}; proceeding without SA token"))
                .ok()
                .map(|creds| {
                    gcp_header(
                        GoogleAccess {
                            name: GOOGLE_ID,
                            creds,
                        },
                        true,
                    )
                });
            tracing::info!("using Google Default Application Credentials");
            Ok(Some(provider(
                GoogleId {
                    name: GOOGLE_ID,
                    creds: id,
                },
                secondary,
            )))
        })
    }
}

#[cfg(test)]
mod tests {
    use aws_lc_rs::encoding::{AsDer, Pkcs8V1Der};
    use aws_lc_rs::rsa::{KeyPair, KeySize};
    use aws_lc_rs::signature::{KeyPair as _, RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};
    use base64::Engine as _;
    use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
    use serde_json::json;
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    /// A service-account key generated per run (no key material in git).
    fn service_account_key() -> (Value, KeyPair) {
        let kp = KeyPair::generate(KeySize::Rsa2048).unwrap();
        let der = AsDer::<Pkcs8V1Der>::as_der(&kp).unwrap();
        let pem = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            STANDARD.encode(der.as_ref())
        );
        let key = json!({
            "type": "service_account",
            "client_email": "sa@p.iam.gserviceaccount.com",
            "private_key_id": "kid-1",
            "private_key": pem,
            "project_id": "p",
        });
        (key, kp)
    }

    #[tokio::test]
    async fn service_account_access_tokens_are_signed_scoped_jwts() {
        let (key, kp) = service_account_key();
        let creds = access_from_json("t", key).unwrap();
        let jwt = GoogleAccess { name: "t", creds }.token().await.unwrap();
        let jwt = jwt.secret();
        let (input, sig) = jwt.rsplit_once('.').unwrap();
        UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, kp.public_key().as_ref())
            .verify(input.as_bytes(), &URL_SAFE_NO_PAD.decode(sig).unwrap())
            .unwrap();
        let claims: Value = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(input.split('.').nth(1).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(claims["iss"], "sa@p.iam.gserviceaccount.com");
        assert!(claims["scope"].as_str().unwrap().contains("cloud-platform"));

        // The same key yields an ID-token source for the workspace.
        let (key, _) = service_account_key();
        assert!(id_tokens_from_json("t", "https://x.gcp.databricks.com", key).is_ok());
    }

    fn external_account(token_url: &str, source: &Value) -> Value {
        json!({
            "type": "external_account",
            "audience": "//iam.googleapis.com/projects/1/locations/global/workloadIdentityPools/p/providers/pr",
            "subject_token_type": "urn:ietf:params:oauth:token-type:jwt",
            "token_url": token_url,
            "credential_source": source,
        })
    }

    async fn mount_sts(server: &MockServer, subject: &str) {
        Mock::given(method("POST"))
            .and(path("/sts"))
            .and(body_string_contains(subject))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "federated",
                "issued_token_type": "urn:ietf:params:oauth:token-type:access_token",
                "token_type": "Bearer",
                "expires_in": 3600,
            })))
            .expect(1)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn external_account_from_a_file() {
        let server = MockServer::start().await;
        mount_sts(&server, "file-jwt").await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("oidc");
        std::fs::write(&file, "file-jwt").unwrap();
        let json = external_account(
            &format!("{}/sts", server.uri()),
            &json!({"file": file.to_string_lossy()}),
        );
        let creds = access_from_json("t", json).unwrap();
        let t = GoogleAccess { name: "t", creds }.token().await.unwrap();
        assert_eq!(t.secret(), "federated");
    }

    #[tokio::test]
    async fn external_account_from_a_url_with_json_format() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/subject"))
            .and(header("x-flavor", "test"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"jwt": "url-jwt"})))
            .mount(&server)
            .await;
        mount_sts(&server, "url-jwt").await;
        let json = external_account(
            &format!("{}/sts", server.uri()),
            &json!({
                "url": format!("{}/subject", server.uri()),
                "headers": {"x-flavor": "test"},
                "format": {"type": "json", "subject_token_field_name": "jwt"},
            }),
        );
        let creds = access_from_json("t", json).unwrap();
        let t = GoogleAccess { name: "t", creds }.token().await.unwrap();
        assert_eq!(t.secret(), "federated");
    }

    #[tokio::test]
    async fn id_token_sources_by_credential_type() {
        let aud = "https://x.gcp.databricks.com";
        let ext = external_account(
            "https://sts.googleapis.com/v1/token",
            &json!({"file": "/f"}),
        );
        let e = id_tokens_from_json("t", aud, ext.clone()).err().unwrap();
        assert!(
            e.to_string().contains("service_account_impersonation_url"),
            "{e}"
        );
        let mut with_sa = ext;
        with_sa["service_account_impersonation_url"] = json!(
            "https://iamcredentials.googleapis.com/v1/projects/-/serviceAccounts/sa@p.iam.gserviceaccount.com:generateAccessToken"
        );
        assert!(id_tokens_from_json("t", aud, with_sa).is_ok());
        let e = id_tokens_from_json("t", aud, json!({"type": "authorized_user"}))
            .err()
            .unwrap();
        assert!(e.to_string().contains("cannot mint ID tokens"), "{e}");
        assert_eq!(
            impersonated_email(
                "https://x/v1/projects/-/serviceAccounts/a@b.com:generateAccessToken"
            ),
            Some("a@b.com")
        );
        assert_eq!(impersonated_email("https://x/serviceAccounts/:x"), None);
        assert_eq!(impersonated_email("nothing"), None);
    }

    #[test]
    fn credentials_are_read_from_a_path_or_inline() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("creds.json");
        std::fs::write(&f, r#"{"type":"authorized_user"}"#).unwrap();
        assert_eq!(
            kind(&read_credentials("t", &f.to_string_lossy()).unwrap()),
            "authorized_user"
        );
        assert_eq!(
            kind(&read_credentials("t", r#"{"type":"x"}"#).unwrap()),
            "x"
        );
        assert!(read_credentials("t", "{").is_err());
        let e = access_from_json("t", json!({"type": "service_account"}))
            .err()
            .unwrap();
        assert!(
            e.to_string().contains("could not obtain OAuth2 token"),
            "{e}"
        );
    }

    async fn gcp_config(host: &str, attrs: &[(&str, &str)]) -> Config {
        let mut c = Config::with_host(host);
        c.host_metadata = Some(crate::config::HostMetadata::default());
        c.cloud = Some("GCP".into());
        for (k, v) in attrs {
            c.set_attribute(k, *v).unwrap();
        }
        c.resolve_with(|_| None, None).await.unwrap()
    }

    #[tokio::test]
    async fn google_credentials_and_google_id_build_providers() {
        let http = reqwest::Client::new();
        let (key, _) = service_account_key();
        let cfg = gcp_config(
            "https://x.gcp.databricks.com",
            &[("google_credentials", &key.to_string())],
        )
        .await;
        let p = GoogleCredentials.configure(&cfg, &http).await.unwrap();
        assert!(format!("{p:?}").contains(GCP_SA_ACCESS_TOKEN));

        // Not GCP, or nothing to use: not configured.
        let mut other = cfg.clone();
        other.cloud = Some("AWS".into());
        assert!(
            GoogleCredentials
                .configure(&other, &http)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            GoogleIdCredentials
                .configure(&other, &http)
                .await
                .unwrap()
                .is_none()
        );

        // google-id impersonates through ADC (here the metadata server,
        // which is only contacted when a token is needed).
        let cfg = gcp_config(
            "https://x.gcp.databricks.com",
            &[("google_service_account", "sa@p.iam.gserviceaccount.com")],
        )
        .await;
        let p = GoogleIdCredentials.configure(&cfg, &http).await.unwrap();
        assert!(p.is_some());
    }

    #[tokio::test]
    async fn oauth_m2m_gcp_can_impersonate_a_service_account() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/oidc/.well-known/oauth-authorization-server"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "token_endpoint": format!("{}/oidc/v1/token", server.uri()),
            })))
            .mount(&server)
            .await;
        let mut c = Config::with_host(server.uri()).client_credentials("sp", "secret");
        c.host_metadata = Some(crate::config::HostMetadata::default());
        c.cloud = Some("GCP".into());
        c.auth_type = Some(M2M_GCP.into());
        c.set_attribute("google_service_account", "sa@p.iam.gserviceaccount.com")
            .unwrap();
        let cfg = c.resolve_with(|_| None, None).await.unwrap();
        let p = GcpM2mCredentials
            .configure(&cfg, &reqwest::Client::new())
            .await
            .unwrap();
        assert!(p.is_some());
        // Without a client secret it isn't this auth type.
        let mut no_secret = cfg.clone();
        no_secret.client_secret = None;
        assert!(
            GcpM2mCredentials
                .configure(&no_secret, &reqwest::Client::new())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn id_tokens_come_from_google_cloud_auth() {
        let server = MockServer::start().await;
        let claims = URL_SAFE_NO_PAD
            .encode(json!({"aud": "https://x", "exp": 4_102_444_800_i64}).to_string());
        let jwt = format!(
            "{}.{claims}.sig",
            URL_SAFE_NO_PAD.encode(r#"{"alg":"RS256"}"#)
        );
        Mock::given(method("GET"))
            .and(path(
                "/computeMetadata/v1/instance/service-accounts/default/identity",
            ))
            .and(header("metadata-flavor", "Google"))
            .respond_with(ResponseTemplate::new(200).set_body_string(jwt.clone()))
            .mount(&server)
            .await;
        let creds = idtoken::mds::Builder::new("https://x")
            .with_endpoint(server.uri())
            .build()
            .unwrap();
        let t = GoogleId { name: "t", creds }.token().await.unwrap();
        assert_eq!(t.secret(), jwt);
    }
}
