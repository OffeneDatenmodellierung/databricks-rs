//! The HTTP client every service call goes through.
//!
//! Behaviour follows `databricks-sdk-go` `httpclient.ApiClient` +
//! `config.HTTPClientConfigFromConfig`:
//!
//! * Lazily configures credentials on the first request.
//! * Client-side rate limit (default 15 requests/second).
//! * Retries until the retry budget (default 5 minutes) is spent, with
//!   backoff of `attempt` seconds capped at 10s plus 50–750ms jitter, on:
//!   429 and its children (`RESOURCE_EXHAUSTED`, `REQUEST_LIMIT_EXCEEDED`),
//!   503, 504, known transient messages, and connection/timeout errors.
//! * **Difference from Go:** `Retry-After` is honoured on API calls (Go only
//!   does so for token requests). The wait is `max(backoff, Retry-After)`.
//! * `X-Databricks-Workspace-Id` is sent when `workspace_id` is configured
//!   (Go adds it per operation; every workspace-level operation does so).

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
pub use reqwest::Method;
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue, RETRY_AFTER, USER_AGENT};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::{Mutex, OnceCell};
use tokio::time::Instant;
use url::Url;

use crate::auth::{CredentialsProvider, DefaultCredentials};
use crate::config::{Config, DEFAULT_RATE_LIMIT};
use crate::error::{ApiError, Error, ErrorKind, Result};
use crate::{query, useragent};

const MAX_BACKOFF: Duration = Duration::from_secs(10);

/// Go's `retries.backoff`: `attempt` seconds, capped at 10s, plus 50–750ms
/// of jitter.
#[must_use]
pub fn backoff(attempt: u32) -> Duration {
    let base = Duration::from_secs(u64::from(attempt)).min(MAX_BACKOFF);
    base + Duration::from_millis(fastrand::u64(50..=750))
}

/// Parse `Retry-After` as seconds or an HTTP date.
#[must_use]
pub fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let v = headers.get(RETRY_AFTER)?.to_str().ok()?.trim();
    if let Ok(secs) = v.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let when = httpdate::parse_http_date(v).ok()?;
    when.duration_since(std::time::SystemTime::now()).ok()
}

struct RateLimiter {
    interval: Duration,
    next: Mutex<Instant>,
}

impl RateLimiter {
    fn new(per_second: u32) -> Self {
        Self {
            interval: Duration::from_secs(1) / per_second.max(1),
            next: Mutex::new(Instant::now()),
        }
    }

    async fn acquire(&self) {
        let slot = {
            let mut next = self.next.lock().await;
            let slot = (*next).max(Instant::now());
            *next = slot + self.interval;
            slot
        };
        tokio::time::sleep_until(slot).await;
    }
}

struct Inner {
    cfg: Config,
    base: Url,
    http: reqwest::Client,
    credentials: DefaultCredentials,
    auth: OnceCell<(&'static str, Arc<dyn CredentialsProvider>)>,
    limiter: RateLimiter,
}

/// Authenticated, retrying, rate-limited client for one host.
///
/// Cheap to clone; clones share the connection pool, token cache and rate
/// limiter.
#[derive(Clone)]
pub struct ApiClient {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for ApiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiClient")
            .field("host", &self.inner.base.as_str())
            .field("auth_type", &self.auth_type())
            .finish_non_exhaustive()
    }
}

impl ApiClient {
    /// Resolve `cfg` (if not already resolved) and build a client using the
    /// default credential chain.
    pub async fn new(cfg: Config) -> Result<Self> {
        Self::with_credentials(cfg, DefaultCredentials::default()).await
    }

    /// Like [`new`](Self::new) with a custom credential chain.
    pub async fn with_credentials(cfg: Config, credentials: DefaultCredentials) -> Result<Self> {
        let cfg = cfg.resolve().await?;
        Self::from_resolved(cfg, credentials)
    }

    /// Build from an already-resolved config (see [`Config::resolve_with`]).
    pub fn from_resolved(cfg: Config, credentials: DefaultCredentials) -> Result<Self> {
        let host = cfg
            .host
            .clone()
            .filter(|h| !h.is_empty())
            .ok_or_else(|| cfg.wrap(Error::Config("no host configured".into())))?;
        let base = Url::parse(&host).map_err(|e| Error::Config(format!("invalid host: {e}")))?;
        let http = reqwest::Client::builder()
            .timeout(cfg.http_timeout())
            .danger_accept_invalid_certs(cfg.skip_verify)
            .build()?;
        let limiter = RateLimiter::new(
            cfg.rate_limit
                .filter(|r| *r > 0)
                .unwrap_or(DEFAULT_RATE_LIMIT),
        );
        Ok(Self {
            inner: Arc::new(Inner {
                cfg,
                base,
                http,
                credentials,
                auth: OnceCell::new(),
                limiter,
            }),
        })
    }

    /// The resolved configuration.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.inner.cfg
    }

    /// The auth type in use, once the first request has configured it.
    #[must_use]
    pub fn auth_type(&self) -> Option<&'static str> {
        self.inner.auth.get().map(|(n, _)| *n)
    }

    /// Configure credentials now rather than on the first request.
    pub async fn authenticate(&self) -> Result<&'static str> {
        Ok(self.provider().await?.0)
    }

    async fn provider(&self) -> Result<&(&'static str, Arc<dyn CredentialsProvider>)> {
        self.inner
            .auth
            .get_or_try_init(|| {
                self.inner
                    .credentials
                    .configure(&self.inner.cfg, &self.inner.http)
            })
            .await
    }

    /// `GET`/`DELETE`/`HEAD` with `request` encoded as the query string.
    pub async fn query<Q, R>(&self, method: Method, path: &str, request: &Q) -> Result<R>
    where
        Q: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let q = query::to_pairs(request)?;
        self.execute(method, path, &q, None).await
    }

    /// `POST`/`PUT`/`PATCH` with `request` as the JSON body.
    pub async fn json<B, R>(&self, method: Method, path: &str, request: &B) -> Result<R>
    where
        B: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let body = serde_json::to_vec(request).map_err(|e| Error::json("request body", e))?;
        self.execute(method, path, &[], Some(body)).await
    }

    /// Send a request and decode the JSON response, retrying as described
    /// in the module docs.
    pub async fn execute<R: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<Vec<u8>>,
    ) -> Result<R> {
        let bytes = self.execute_raw(method, path, query, body).await?;
        decode(&bytes)
    }

    /// As [`execute`](Self::execute), returning the raw body.
    pub async fn execute_raw(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<Vec<u8>>,
    ) -> Result<Bytes> {
        Ok(self.run(method, path, query, body, true).await?.0)
    }

    /// Send a [`Call`] built by generated service code and decode the
    /// JSON response.
    pub async fn send<R: DeserializeOwned>(&self, call: Call) -> Result<R> {
        Ok(self.send_with_headers(call).await?.0)
    }

    /// As [`send`](Self::send), also returning the response headers (for
    /// operations whose result is partly or wholly in headers, e.g. the
    /// Files API `HEAD` metadata calls).
    pub async fn send_with_headers<R: DeserializeOwned>(
        &self,
        call: Call,
    ) -> Result<(R, HeaderMap)> {
        let (bytes, headers) = self
            .run(
                call.method,
                &call.path,
                &call.query,
                call.body,
                call.workspace_header,
            )
            .await?;
        Ok((decode(&bytes)?, headers))
    }

    /// The configured account ID, required by account-level paths.
    pub fn account_id(&self) -> Result<&str> {
        self.inner
            .cfg
            .account_id
            .as_deref()
            .filter(|a| !a.is_empty())
            .ok_or_else(|| Error::Config("account_id is required for account-level APIs".into()))
    }

    async fn run(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<Vec<u8>>,
        workspace_header: bool,
    ) -> Result<(Bytes, HeaderMap)> {
        let mut url = self
            .inner
            .base
            .join(path)
            .map_err(|e| Error::Config(format!("invalid path {path:?}: {e}")))?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        let (auth_type, provider) = self.provider().await?;
        let user_agent = useragent::build(Some(auth_type));
        let deadline = self.inner.cfg.retry_timeout().map(|d| Instant::now() + d);
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            self.inner.limiter.acquire().await;
            let outcome = self
                .attempt(
                    &method,
                    &url,
                    body.as_deref(),
                    provider.as_ref(),
                    &user_agent,
                    workspace_header,
                )
                .await;
            let (err, hint) = match outcome {
                Ok(done) => return Ok(done),
                Err(Failure::Fatal(e)) => return Err(e),
                Err(Failure::Retriable(e, hint)) => (e, hint),
            };
            let wait = backoff(attempt).max(hint.unwrap_or_default());
            if deadline.is_some_and(|d| Instant::now() + wait > d) {
                tracing::warn!(%method, path, attempt, "retry budget exhausted: {err}");
                return Err(err);
            }
            tracing::debug!(%method, path, attempt, ?wait, "retrying: {err}");
            tokio::time::sleep(wait).await;
        }
    }

    async fn attempt(
        &self,
        method: &Method,
        url: &Url,
        body: Option<&[u8]>,
        provider: &dyn CredentialsProvider,
        user_agent: &str,
        workspace_header: bool,
    ) -> std::result::Result<(Bytes, HeaderMap), Failure> {
        let mut req = self
            .inner
            .http
            .request(method.clone(), url.clone())
            .header(ACCEPT, HeaderValue::from_static("application/json"))
            .header(USER_AGENT, user_agent);
        if let Some(b) = body {
            req = req
                .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
                .body(b.to_vec());
        }
        if let Some(ws) = self.workspace_header().filter(|_| workspace_header) {
            req = req.header("X-Databricks-Workspace-Id", ws);
        }
        for (k, v) in provider.headers().await.map_err(Failure::Fatal)? {
            req = req.header(k, v);
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) if e.is_connect() || e.is_timeout() => {
                return Err(Failure::Retriable(e.into(), None));
            }
            Err(e) => return Err(Failure::Fatal(e.into())),
        };
        let status = resp.status();
        let headers = resp.headers().clone();
        let hint = retry_after(&headers);
        if self.inner.cfg.debug_headers {
            tracing::trace!(%method, %url, %status, headers = ?resp.headers(), "response");
        } else {
            tracing::debug!(%method, path = url.path(), %status, "response");
        }
        let bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(e) if e.is_timeout() => return Err(Failure::Retriable(e.into(), None)),
            Err(e) => return Err(Failure::Fatal(e.into())),
        };
        if status.is_success() || status.is_redirection() {
            return Ok((bytes, headers));
        }
        let api = ApiError::from_response(status.as_u16(), method.as_str(), url.path(), &bytes);
        let retriable = api.is_retriable()
            || status == StatusCode::GATEWAY_TIMEOUT
            || api.is(ErrorKind::RequestLimitExceeded)
            || api.message.contains("REQUEST_LIMIT_EXCEEDED");
        let hint = hint.or(api.details.retry_delay);
        if retriable {
            Err(Failure::Retriable(api.into(), hint))
        } else {
            Err(Failure::Fatal(api.into()))
        }
    }

    fn workspace_header(&self) -> Option<&str> {
        let cfg = &self.inner.cfg;
        if cfg.is_account_client() {
            return None;
        }
        cfg.workspace_id.as_deref().filter(|w| !w.is_empty())
    }
}

/// One API call, as built by generated service code.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Call {
    /// HTTP verb.
    pub method: Method,
    /// Path including any encoded path parameters.
    pub path: String,
    /// Query-string pairs.
    pub query: Vec<(String, String)>,
    /// JSON body, if any.
    pub body: Option<Vec<u8>>,
    /// Send `X-Databricks-Workspace-Id` when configured (workspace-level
    /// operations only, as in Go).
    pub workspace_header: bool,
}

impl Call {
    /// A call with no query or body.
    #[must_use]
    pub fn new(method: Method, path: String) -> Self {
        Self {
            method,
            path,
            query: Vec::new(),
            body: None,
            workspace_header: false,
        }
    }

    /// Send the workspace header.
    #[must_use]
    pub fn workspace(mut self) -> Self {
        self.workspace_header = true;
        self
    }

    /// Append query pairs.
    #[must_use]
    pub fn query(mut self, pairs: Vec<(String, String)>) -> Self {
        self.query.extend(pairs);
        self
    }

    /// Set the JSON body.
    pub fn json<B: Serialize + ?Sized>(mut self, body: &B) -> Result<Self> {
        self.body = Some(serde_json::to_vec(body).map_err(|e| Error::json("request body", e))?);
        Ok(self)
    }
}

/// A response header parsed as `T` (`None` when absent or unparsable).
#[must_use]
pub fn header<T: std::str::FromStr>(headers: &HeaderMap, name: &str) -> Option<T> {
    headers.get(name)?.to_str().ok()?.trim().parse().ok()
}

/// Encode a path parameter.
///
/// Go inserts most values with `%v` (no escaping, so `/` in a resource name
/// such as `catalogs/a/schemas/b` stays a separator) and escapes each
/// segment of multi-segment parameters with `url.PathEscape`. Here both
/// keep `/` and escape everything that would change the URL's meaning
/// (`?`, `#`, `%`, spaces, non-ASCII) within each segment.
#[must_use]
pub fn path_param(value: &str, _multi_segment: bool) -> String {
    const KEEP: &[u8] = b"-._~!$&'()*+,;=:@";
    let escape = |seg: &str| {
        let mut out = String::with_capacity(seg.len());
        for b in seg.bytes() {
            if b.is_ascii_alphanumeric() || KEEP.contains(&b) {
                out.push(char::from(b));
            } else {
                let _ = write!(out, "%{b:02X}");
            }
        }
        out
    };
    value.split('/').map(escape).collect::<Vec<_>>().join("/")
}

enum Failure {
    Fatal(Error),
    Retriable(Error, Option<Duration>),
}

fn decode<R: DeserializeOwned>(bytes: &[u8]) -> Result<R> {
    let trimmed = bytes.iter().all(u8::is_ascii_whitespace);
    if trimmed {
        // Empty bodies decode as `{}` (structs with defaults) or `null` (`()`).
        return serde_json::from_slice(b"{}")
            .or_else(|_| serde_json::from_slice(b"null"))
            .map_err(|e| Error::json("empty response", e));
    }
    serde_json::from_slice(bytes).map_err(|e| Error::json("response body", e))
}
