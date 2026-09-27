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
//! * **Difference from Go — idempotency-safe writes (#3):** a failure the
//!   server may already have acted on (a timeout, 502/503/504, a transient
//!   error message) is retried only when the call is idempotent: GET, HEAD,
//!   PUT, DELETE, PATCH, or a POST that carries an idempotency token.
//!   Throttling (429 / `REQUEST_LIMIT_EXCEEDED`) and connection failures
//!   are retried for every method, as the request was never processed. Go
//!   retries every method; set `Config::retry_non_idempotent` for that.
//! * Redirects are followed; a final 3xx, or the private-link login page
//!   (`/login.html?error=private-link-validation-error`), is an error (#5).
//! * Binary bodies ([`Binary`]): an upload may be buffered (replayed on
//!   retry) or streamed (sent once, never retried); a binary response is
//!   returned as a stream once the status is known to be a success.
//! * `X-Databricks-Workspace-Id` is sent when `workspace_id` is configured
//!   (Go adds it per operation; every workspace-level operation does so).

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

pub use reqwest::Method;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue, RETRY_AFTER, USER_AGENT};
use reqwest::{RequestBuilder, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::{Mutex, OnceCell};
use tokio::time::Instant;
use url::Url;

use crate::auth::{CredentialsProvider, DefaultCredentials};
pub use crate::binary::{Binary, ByteStream, Bytes, next_chunk};
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
    credentials: Arc<DefaultCredentials>,
    auth: OnceCell<(&'static str, Arc<dyn CredentialsProvider>)>,
    limiter: Arc<RateLimiter>,
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
        let mut cfg = cfg.resolve().await?;
        if cfg.host.as_deref().is_none_or(str::is_empty) {
            // Go: `azureEnsureWorkspaceUrl` for managed identity.
            let http = reqwest::Client::builder()
                .timeout(cfg.http_timeout())
                .build()?;
            crate::auth::ensure_workspace_host(&mut cfg, &http)
                .await
                .map_err(|e| cfg.wrap(e))?;
        }
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
        if cfg.skip_verify {
            tracing::warn!(
                host,
                "TLS certificate verification is disabled (skip_verify); use this only for testing"
            );
        }
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
                credentials: Arc::new(credentials),
                auth: OnceCell::new(),
                limiter: Arc::new(limiter),
            }),
        })
    }

    /// A client for one workspace, derived from this account-level client
    /// (Go: `AccountClient.GetWorkspaceClient`).
    ///
    /// `host` is the workspace URL, or `None` on a unified host, where the
    /// account host also serves workspace APIs. The derived client shares
    /// this client's connection pool and rate limiter. On a unified host
    /// it also shares the credentials, so nothing is re-authenticated;
    /// on a separate workspace host the same credential settings are used,
    /// but tokens are requested for the workspace, because account tokens
    /// aren't valid there.
    pub fn for_workspace(
        &self,
        host: Option<&str>,
        workspace_id: &str,
        azure_resource_id: Option<&str>,
    ) -> Result<Self> {
        let parent = &self.inner;
        let mut cfg = parent.cfg.clone();
        let same_host = host.is_none_or(|h| Some(h) == parent.cfg.host.as_deref());
        let auth = OnceCell::new();
        let base = if same_host {
            if let Some(shared) = parent.auth.get() {
                auth.set(shared.clone()).ok();
            }
            parent.base.clone()
        } else {
            let host = host.unwrap_or_default();
            cfg.for_workspace_host(host);
            Url::parse(host).map_err(|e| Error::Config(format!("invalid host: {e}")))?
        };
        cfg.workspace_id = Some(workspace_id.to_owned()).filter(|w| !w.is_empty());
        if let Some(id) = azure_resource_id.filter(|id| !id.is_empty()) {
            cfg.set_attribute("azure_workspace_resource_id", id)?;
        }
        Ok(Self {
            inner: Arc::new(Inner {
                cfg,
                base,
                http: parent.http.clone(),
                credentials: Arc::clone(&parent.credentials),
                auth,
                limiter: Arc::clone(&parent.limiter),
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
        let mut call = Call::new(method, path.to_owned()).workspace();
        call.query = query.to_vec();
        call.body = body;
        Ok(self.run(call, false).await?.into_bytes().0)
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
        let (bytes, headers) = self.run(call, false).await?.into_bytes();
        Ok((decode(&bytes)?, headers))
    }

    /// Send a [`Call`] whose response body is binary (file downloads,
    /// exports). Errors are decoded as usual; on success the body is
    /// returned unread, as a stream, with the response headers.
    pub async fn send_binary(&self, call: Call) -> Result<(Binary, HeaderMap)> {
        match self.run(call, true).await? {
            Done::Bytes(bytes, headers) => Ok((Binary::from(bytes), headers)),
            Done::Stream(resp) => {
                let headers = resp.headers().clone();
                Ok((Binary::from_stream(resp.bytes_stream()), headers))
            }
        }
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

    async fn run(&self, call: Call, stream: bool) -> Result<Done> {
        let Call {
            method,
            path,
            query,
            body,
            binary,
            content_type,
            accept,
            workspace_header,
            idempotent,
        } = call;
        let path = path.as_str();
        let replayable = idempotent || self.inner.cfg.retry_non_idempotent;
        // A streamed upload is consumed by the first attempt.
        let resendable = binary.as_ref().is_none_or(|b| !b.is_stream());
        let body = match (body, binary) {
            (_, Some(b)) => Some(Payload::Binary(
                b,
                content_type.unwrap_or_else(|| "application/octet-stream".to_owned()),
            )),
            (Some(json), None) => Some(Payload::Json(Bytes::from(json))),
            (None, None) => None,
        };
        let mut url = self
            .inner
            .base
            .join(path)
            .map_err(|e| Error::Config(format!("invalid path {path:?}: {e}")))?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(&query);
        }
        let (auth_type, provider) = self.provider().await?;
        let user_agent = useragent::build(Some(auth_type));
        let deadline = self.inner.cfg.retry_timeout().map(|d| Instant::now() + d);
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            self.inner.limiter.acquire().await;
            let outcome = self
                .attempt(&Attempt {
                    method: &method,
                    url: &url,
                    body: body.as_ref(),
                    accept,
                    stream,
                    provider: provider.as_ref(),
                    user_agent: &user_agent,
                    workspace_header,
                })
                .await;
            let (err, hint) = match outcome {
                Ok(done) => return Ok(done),
                Err(Failure::Fatal(e)) => return Err(e),
                Err(Failure::Retriable(e, ..)) if !resendable => {
                    tracing::warn!(%method, path, "not retrying a request with a streamed body: {e}");
                    return Err(e);
                }
                Err(Failure::Retriable(e, hint, Replay::Safe)) => (e, hint),
                Err(Failure::Retriable(e, hint, Replay::IfIdempotent)) if replayable => (e, hint),
                Err(Failure::Retriable(e, _, Replay::IfIdempotent)) => {
                    tracing::warn!(
                        %method,
                        path,
                        "not retrying a non-idempotent request that may have been applied: {e}"
                    );
                    return Err(e);
                }
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

    /// Build one attempt's request: headers, body, custom headers, auth.
    async fn request(&self, a: &Attempt<'_>) -> std::result::Result<RequestBuilder, Failure> {
        let mut req = self
            .inner
            .http
            .request(a.method.clone(), a.url.clone())
            .header(
                ACCEPT,
                HeaderValue::from_static(a.accept.unwrap_or("application/json")),
            )
            .header(USER_AGENT, a.user_agent);
        match a.body {
            Some(Payload::Json(b)) => {
                req = req
                    .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
                    .body(b.clone());
            }
            Some(Payload::Binary(b, content_type)) => {
                let ct = HeaderValue::from_str(content_type).map_err(|e| {
                    Failure::Fatal(Error::Config(format!("invalid content type: {e}")))
                })?;
                req = req
                    .header(CONTENT_TYPE, ct)
                    .body(b.to_request_body().map_err(Failure::Fatal)?);
            }
            None => {}
        }
        if let Some(ws) = self.workspace_header().filter(|_| a.workspace_header) {
            req = req.header("X-Databricks-Workspace-Id", ws);
        }
        let auth = a.provider.headers().await.map_err(Failure::Fatal)?;
        for (k, v) in &self.inner.cfg.headers {
            let reserved = [
                "authorization",
                "user-agent",
                "content-type",
                "accept",
                "x-databricks-workspace-id",
            ];
            if reserved.iter().any(|r| k.eq_ignore_ascii_case(r))
                || auth.iter().any(|(h, _)| h.as_str().eq_ignore_ascii_case(k))
            {
                tracing::debug!(header = %k, "ignoring custom header that the SDK manages");
                continue;
            }
            req = req.header(k.as_str(), v.as_str());
        }
        for (k, v) in auth {
            req = req.header(k, v);
        }
        Ok(req)
    }

    async fn attempt(&self, a: &Attempt<'_>) -> std::result::Result<Done, Failure> {
        let (method, url, stream) = (a.method, a.url, a.stream);
        let req = self.request(a).await?;
        let resp = match req.send().await {
            Ok(r) => r,
            // The connection was never made, so nothing was sent.
            Err(e) if e.is_connect() => {
                return Err(Failure::Retriable(e.into(), None, Replay::Safe));
            }
            // Sent, but no answer: it may have been applied.
            Err(e) if e.is_timeout() => {
                return Err(Failure::Retriable(e.into(), None, Replay::IfIdempotent));
            }
            Err(e) => return Err(Failure::Fatal(e.into())),
        };
        if let Some(e) = private_link_error(resp.url()) {
            return Err(Failure::Fatal(e.into()));
        }
        let status = resp.status();
        let headers = resp.headers().clone();
        let hint = retry_after(&headers);
        if self.inner.cfg.debug_headers {
            tracing::trace!(%method, %url, %status, headers = ?resp.headers(), "response");
        } else {
            tracing::debug!(%method, path = url.path(), %status, "response");
        }
        if stream && status.is_success() {
            return Ok(Done::Stream(resp));
        }
        let bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(e) if e.is_timeout() => {
                return Err(Failure::Retriable(e.into(), None, Replay::IfIdempotent));
            }
            Err(e) => return Err(Failure::Fatal(e.into())),
        };
        if status.is_success() {
            return Ok(Done::Bytes(bytes, headers));
        }
        if status.is_redirection() {
            // Redirects are followed, so one that reaches here wasn't
            // (a loop, a missing Location, or a cross-scheme hop); often a
            // proxy or private-link front door sending us somewhere else.
            let to = headers
                .get(reqwest::header::LOCATION)
                .and_then(|l| l.to_str().ok())
                .unwrap_or("(no Location header)");
            let api = ApiError::new(
                status.as_u16(),
                "UNEXPECTED_REDIRECT",
                &format!(
                    "{method} {} was redirected to {to} and not followed; check the host and any proxy or private-link routing",
                    url.path()
                ),
            );
            return Err(Failure::Fatal(api.into()));
        }
        let api = ApiError::from_response(status.as_u16(), method.as_str(), url.path(), &bytes);
        let retriable = api.is_retriable()
            || status == StatusCode::GATEWAY_TIMEOUT
            || api.is(ErrorKind::RequestLimitExceeded)
            || api.message.contains("REQUEST_LIMIT_EXCEEDED");
        let hint = hint.or(api.details.retry_delay);
        // Throttling rejects the request before it is processed.
        let throttled = status == StatusCode::TOO_MANY_REQUESTS
            || api.is(ErrorKind::RequestLimitExceeded)
            || api.message.contains("REQUEST_LIMIT_EXCEEDED");
        if throttled {
            Err(Failure::Retriable(api.into(), hint, Replay::Safe))
        } else if retriable {
            Err(Failure::Retriable(api.into(), hint, Replay::IfIdempotent))
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
    /// Binary body; takes precedence over `body`.
    pub binary: Option<Binary>,
    /// `Content-Type` of the binary body; `application/octet-stream` when
    /// unset.
    pub content_type: Option<String>,
    /// `Accept` header; `application/json` when unset.
    pub accept: Option<&'static str>,
    /// Send `X-Databricks-Workspace-Id` when configured (workspace-level
    /// operations only, as in Go).
    pub workspace_header: bool,
    /// Safe to repeat after a failure the server may have acted on. True
    /// for GET, HEAD, PUT, DELETE and PATCH; POST needs [`Call::idempotent`].
    pub idempotent: bool,
}

impl Call {
    /// A call with no query or body.
    #[must_use]
    pub fn new(method: Method, path: String) -> Self {
        let idempotent = method != Method::POST;
        Self {
            method,
            path,
            query: Vec::new(),
            body: None,
            binary: None,
            content_type: None,
            accept: None,
            workspace_header: false,
            idempotent,
        }
    }

    /// Mark the call safe to retry after a failure the server may already
    /// have acted on: the request carries an idempotency token, or the
    /// operation is idempotent by nature.
    #[must_use]
    pub fn idempotent(mut self) -> Self {
        self.idempotent = true;
        self
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

    /// Set the `Accept` header (for binary or plain-text responses).
    #[must_use]
    pub fn accept(mut self, accept: &'static str) -> Self {
        self.accept = Some(accept);
        self
    }

    /// Set a binary body, sent as `application/octet-stream`.
    #[must_use]
    pub fn binary(mut self, body: Binary) -> Self {
        self.binary = Some(body);
        self
    }

    /// Set a binary body with its own content type (for example
    /// `multipart/form-data; boundary=…`).
    #[must_use]
    pub fn body_with_type(mut self, body: Binary, content_type: impl Into<String>) -> Self {
        self.binary = Some(body);
        self.content_type = Some(content_type.into());
        self
    }

    /// Set the JSON body.
    pub fn json<B: Serialize + ?Sized>(mut self, body: &B) -> Result<Self> {
        self.body = Some(serde_json::to_vec(body).map_err(|e| Error::json("request body", e))?);
        Ok(self)
    }
}

/// A random UUID v4 for idempotency keys. Generated calls fill one in
/// when the caller leaves the request's key empty, so a retry after a
/// timeout can't create a second object (#3).
#[must_use]
pub fn idempotency_token() -> String {
    let mut b: [u8; 16] = fastrand::u128(..).to_le_bytes();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = b.iter().fold(String::with_capacity(32), |mut h, x| {
        let _ = write!(h, "{x:02x}");
        h
    });
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// A response header parsed as `T` (`None` when absent or unparsable).
#[must_use]
pub fn header<T: std::str::FromStr>(headers: &HeaderMap, name: &str) -> Option<T> {
    headers.get(name)?.to_str().ok()?.trim().parse().ok()
}

/// Encode a path parameter.
///
/// * Single-segment values are escaped completely, `/` included, so a
///   column or tag name containing `/` stays one segment (Go's
///   `EncodeSingleSegmentPathParameter` from databricks-sdk-go#1811, which
///   fixes #1765; the released Go SDK still inserts these raw).
/// * Multi-segment values (hierarchical resource names such as
///   `projects/{p}/branches/{b}`, and file paths) keep `/` as a separator
///   and escape each segment (`EncodeMultiSegmentPathParameter`).
#[must_use]
pub fn path_param(value: &str, multi_segment: bool) -> String {
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
    if multi_segment {
        value.split('/').map(escape).collect::<Vec<_>>().join("/")
    } else {
        escape(value)
    }
}

/// The inputs to one request attempt.
struct Attempt<'a> {
    method: &'a Method,
    url: &'a Url,
    body: Option<&'a Payload>,
    accept: Option<&'static str>,
    stream: bool,
    provider: &'a dyn CredentialsProvider,
    user_agent: &'a str,
    workspace_header: bool,
}

enum Payload {
    Json(Bytes),
    Binary(Binary, String),
}

/// A successful response: read into memory, or (for binary responses)
/// left unread.
enum Done {
    Bytes(Bytes, HeaderMap),
    Stream(reqwest::Response),
}

impl Done {
    fn into_bytes(self) -> (Bytes, HeaderMap) {
        match self {
            Self::Bytes(b, h) => (b, h),
            // Only `send_binary` asks for a stream.
            Self::Stream(_) => unreachable!("stream requested by send_binary only"),
        }
    }
}

enum Failure {
    Fatal(Error),
    Retriable(Error, Option<Duration>, Replay),
}

/// Whether a retriable failure can be replayed for any call.
enum Replay {
    /// The server didn't process the request (throttled, not connected).
    Safe,
    /// The server may have processed it; replay only idempotent calls.
    IfIdempotent,
}

/// Go: `apierr.isPrivateLinkRedirect`. A private-link workspace reached
/// from outside the private network redirects to its login page.
fn private_link_error(final_url: &Url) -> Option<ApiError> {
    let query = final_url.query().unwrap_or_default();
    if final_url.path() != "/login.html" || !query.contains("error=private-link-validation-error") {
        return None;
    }
    let host = final_url.host_str().unwrap_or_default();
    let (service, endpoint, docs) = match crate::config::environment_for_hostname(host).cloud {
        crate::config::Cloud::Azure => (
            "Azure Private Link",
            "Azure Private Link endpoint",
            "https://learn.microsoft.com/en-us/azure/databricks/security/network/classic/private-link-standard#authentication-troubleshooting",
        ),
        crate::config::Cloud::Gcp => (
            "Private Service Connect",
            "GCP VPC endpoint",
            "https://docs.gcp.databricks.com/en/security/network/classic/private-service-connect.html",
        ),
        _ => (
            "AWS PrivateLink",
            "AWS VPC endpoint",
            "https://docs.databricks.com/en/security/network/classic/privatelink.html",
        ),
    };
    let mut e = ApiError::new(
        403,
        "PRIVATE_LINK_VALIDATION_ERROR",
        &format!(
            "The requested workspace has {service} enabled and is not accessible from the current network. Ensure that {service} is properly configured and that your device has access to the {endpoint}. For more information, see {docs}."
        ),
    );
    e.kind = ErrorKind::PermissionDenied;
    Some(e)
}

fn decode<R: DeserializeOwned>(bytes: &[u8]) -> Result<R> {
    let trimmed = bytes.iter().all(u8::is_ascii_whitespace);
    if trimmed {
        // Empty bodies decode as `{}` (structs with defaults) or `null` (`()`).
        return serde_json::from_slice(b"{}")
            .or_else(|_| serde_json::from_slice(b"null"))
            .map_err(|e| Error::json("empty response", e));
    }
    match serde_json::from_slice(bytes) {
        Ok(v) => Ok(v),
        Err(first) => {
            // Some services emit bare NaN/Infinity (e.g. MLflow metrics,
            // databricks-sdk-go#1498), which is not JSON. Read them as null.
            if let Some(fixed) = non_finite_to_null(bytes)
                && let Ok(v) = serde_json::from_slice(&fixed)
            {
                return Ok(v);
            }
            // Say what came back: a gateway's HTML or a plain-text error is
            // far easier to diagnose than "expected value at line 1"
            // (databricks-sdk-go#1796).
            Err(Error::json(
                format!("response body {}", snippet(bytes)),
                first,
            ))
        }
    }
}

/// Replace bare `NaN`, `Infinity` and `-Infinity` tokens outside strings
/// with `null`. `None` if there were none.
fn non_finite_to_null(bytes: &[u8]) -> Option<Vec<u8>> {
    const TOKENS: [&[u8]; 3] = [b"-Infinity", b"Infinity", b"NaN"];
    let mut out = Vec::with_capacity(bytes.len());
    let (mut in_str, mut escaped, mut changed) = (false, false, false);
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if in_str {
            out.push(b);
            match (escaped, b) {
                (true, _) => escaped = false,
                (false, b'\\') => escaped = true,
                (false, b'"') => in_str = false,
                _ => {}
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            in_str = true;
            out.push(b);
            i += 1;
            continue;
        }
        if let Some(t) = TOKENS.iter().find(|t| bytes[i..].starts_with(t)) {
            out.extend_from_slice(b"null");
            i += t.len();
            changed = true;
            continue;
        }
        out.push(b);
        i += 1;
    }
    changed.then_some(out)
}

/// First 200 characters of a body, for error messages.
fn snippet(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut s: String = text.chars().take(200).collect();
    if text.chars().count() > 200 {
        s.push('…');
    }
    format!("{s:?}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_finite_numbers_become_null() {
        #[derive(serde::Deserialize, Debug)]
        struct M {
            value: Option<f64>,
            key: String,
        }
        let m: Vec<M> = decode(br#"[{"key":"NaN","value":NaN},{"key":"a\"Infinity","value":-Infinity},{"key":"x","value":1.5}]"#).unwrap();
        assert_eq!(m[0].key, "NaN");
        assert!(m[0].value.is_none() && m[1].value.is_none());
        assert_eq!(m[1].key, "a\"Infinity");
        assert_eq!(m[2].value, Some(1.5));
        assert!(non_finite_to_null(br#"{"a":1}"#).is_none());
    }

    #[test]
    fn decode_errors_show_the_body() {
        let e = decode::<serde_json::Value>(b"rate limited, retry later").unwrap_err();
        assert!(e.to_string().contains("rate limited, retry later"), "{e}");
        let long = "x".repeat(500);
        let e = decode::<serde_json::Value>(long.as_bytes()).unwrap_err();
        assert!(e.to_string().contains('…'), "{e}");
    }
}
