//! Errors, and the Databricks error envelope mapped to typed errors.
//!
//! Mirrors `databricks-sdk-go/apierr`: the standard JSON envelope
//! (`error_code`, `message`, `details`), the legacy API 1.2 `error` field,
//! SCIM errors, `CODE: message` plain-text errors, and HTML `<pre>` errors are
//! all parsed into an [`ApiError`] whose [`ErrorKind`] follows the same
//! two-level hierarchy as Go's `apierr.Err*` sentinels.

use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use serde::Deserialize;
use serde_json::Value;

/// Result alias used throughout the SDK.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Every error the SDK can return.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The API returned an error response.
    #[error(transparent)]
    Api(Box<ApiError>),

    /// The configuration is invalid or incomplete.
    #[error("config: {0}")]
    Config(String),

    /// No credential strategy could authenticate, or the selected one failed.
    #[error("{auth_type} auth: {message}")]
    Auth {
        /// Name of the strategy (for example `pat` or `oauth-m2m`).
        auth_type: String,
        /// What went wrong.
        message: String,
    },

    /// The HTTP request could not be sent or the response could not be read.
    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),

    /// A streamed request or response body failed part-way.
    #[error("body stream: {0}")]
    Body(Box<dyn std::error::Error + Send + Sync>),

    /// A request or response body could not be (de)serialised.
    #[error("json ({context}): {source}")]
    Json {
        /// What was being (de)serialised.
        context: String,
        /// Underlying serde error.
        #[source]
        source: serde_json::Error,
    },

    /// Retries or polling ran out of time.
    #[error("timed out after {after:?}: {last}")]
    Timeout {
        /// Configured timeout.
        after: Duration,
        /// Last observed status or error.
        last: String,
    },

    /// A long-running operation reached a failure state.
    #[error("operation failed: {0}")]
    OperationFailed(String),

    /// Local I/O failed (for example reading `~/.databrickscfg`).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl Error {
    /// The [`ApiError`], if this is an API error.
    #[must_use]
    pub fn as_api(&self) -> Option<&ApiError> {
        match self {
            Self::Api(e) => Some(e),
            _ => None,
        }
    }

    /// True if this is an API error of `kind` (or a child of it).
    #[must_use]
    pub fn is(&self, kind: ErrorKind) -> bool {
        self.as_api().is_some_and(|e| e.is(kind))
    }

    /// Equivalent of Go's `apierr.IsMissing`.
    #[must_use]
    pub fn is_missing(&self) -> bool {
        self.is(ErrorKind::NotFound)
    }

    pub(crate) fn json(context: impl Into<String>, source: serde_json::Error) -> Self {
        Self::Json {
            context: context.into(),
            source,
        }
    }
}

impl From<ApiError> for Error {
    fn from(e: ApiError) -> Self {
        Self::Api(Box::new(e))
    }
}

/// Error categories, matching `databricks-sdk-go/apierr` sentinels.
///
/// The first group maps from HTTP status codes; the second from
/// `error_code` values and each has a parent in the first group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// HTTP 400.
    BadRequest,
    /// HTTP 401.
    Unauthenticated,
    /// HTTP 403.
    PermissionDenied,
    /// HTTP 404.
    NotFound,
    /// HTTP 409.
    ResourceConflict,
    /// HTTP 429.
    TooManyRequests,
    /// HTTP 499.
    Cancelled,
    /// HTTP 500.
    InternalError,
    /// HTTP 501.
    NotImplemented,
    /// HTTP 503.
    TemporarilyUnavailable,
    /// HTTP 504.
    DeadlineExceeded,
    /// `INVALID_STATE` (a [`BadRequest`](Self::BadRequest)).
    InvalidState,
    /// `INVALID_PARAMETER_VALUE` (a [`BadRequest`](Self::BadRequest)).
    InvalidParameterValue,
    /// `RESOURCE_DOES_NOT_EXIST` (a [`NotFound`](Self::NotFound)).
    ResourceDoesNotExist,
    /// `ABORTED` (a [`ResourceConflict`](Self::ResourceConflict)).
    Aborted,
    /// `ALREADY_EXISTS` (a [`ResourceConflict`](Self::ResourceConflict)).
    AlreadyExists,
    /// `RESOURCE_ALREADY_EXISTS` (a [`ResourceConflict`](Self::ResourceConflict)).
    ResourceAlreadyExists,
    /// `RESOURCE_EXHAUSTED` (a [`TooManyRequests`](Self::TooManyRequests)).
    ResourceExhausted,
    /// `REQUEST_LIMIT_EXCEEDED` (a [`TooManyRequests`](Self::TooManyRequests)).
    RequestLimitExceeded,
    /// `UNKNOWN` (an [`InternalError`](Self::InternalError)).
    Unknown,
    /// `DATA_LOSS` (an [`InternalError`](Self::InternalError)).
    DataLoss,
    /// Neither the status code nor the error code is mapped.
    Other,
}

impl ErrorKind {
    /// The parent category, for error-code kinds.
    #[must_use]
    pub fn parent(self) -> Option<Self> {
        use ErrorKind as K;
        match self {
            K::InvalidState | K::InvalidParameterValue => Some(K::BadRequest),
            K::ResourceDoesNotExist => Some(K::NotFound),
            K::Aborted | K::AlreadyExists | K::ResourceAlreadyExists => Some(K::ResourceConflict),
            K::ResourceExhausted | K::RequestLimitExceeded => Some(K::TooManyRequests),
            K::Unknown | K::DataLoss => Some(K::InternalError),
            _ => None,
        }
    }

    fn from_error_code(code: &str) -> Option<Self> {
        use ErrorKind as K;
        Some(match code {
            "INVALID_STATE" => K::InvalidState,
            "INVALID_PARAMETER_VALUE" => K::InvalidParameterValue,
            "RESOURCE_DOES_NOT_EXIST" => K::ResourceDoesNotExist,
            "ABORTED" => K::Aborted,
            "ALREADY_EXISTS" => K::AlreadyExists,
            "RESOURCE_ALREADY_EXISTS" => K::ResourceAlreadyExists,
            "RESOURCE_EXHAUSTED" => K::ResourceExhausted,
            "REQUEST_LIMIT_EXCEEDED" => K::RequestLimitExceeded,
            "UNKNOWN" => K::Unknown,
            "DATA_LOSS" => K::DataLoss,
            _ => return None,
        })
    }

    fn from_status(status: u16) -> Option<Self> {
        use ErrorKind as K;
        Some(match status {
            400 => K::BadRequest,
            401 => K::Unauthenticated,
            403 => K::PermissionDenied,
            404 => K::NotFound,
            409 => K::ResourceConflict,
            429 => K::TooManyRequests,
            499 => K::Cancelled,
            500 => K::InternalError,
            501 => K::NotImplemented,
            503 => K::TemporarilyUnavailable,
            504 => K::DeadlineExceeded,
            _ => return None,
        })
    }

    /// Error code first, then status code — the same precedence as Go.
    pub(crate) fn classify(status: u16, error_code: &str) -> Self {
        Self::from_error_code(error_code)
            .or_else(|| Self::from_status(status))
            .unwrap_or(Self::Other)
    }
}

/// A Databricks API error response.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
#[non_exhaustive]
pub struct ApiError {
    /// HTTP status code.
    pub status_code: u16,
    /// Databricks `error_code` (for example `RESOURCE_DOES_NOT_EXIST`).
    pub error_code: String,
    /// Human-readable message.
    pub message: String,
    /// Structured `details`, when present.
    pub details: ErrorDetails,
    /// Category derived from the error code and status code.
    pub kind: ErrorKind,
}

impl ApiError {
    /// Build an error by hand (mainly for tests and mocks).
    #[must_use]
    pub fn new(status_code: u16, error_code: &str, message: &str) -> Self {
        Self {
            status_code,
            error_code: error_code.to_owned(),
            message: message.to_owned(),
            details: ErrorDetails::default(),
            kind: ErrorKind::classify(status_code, error_code),
        }
    }

    /// True if this error is `kind` or a child of `kind`.
    #[must_use]
    pub fn is(&self, kind: ErrorKind) -> bool {
        self.kind == kind || self.kind.parent() == Some(kind)
    }

    /// Whether the request should be retried. Mirrors `APIError.IsRetriable`:
    /// 429 (and its children), 503, and a set of known transient messages.
    #[must_use]
    pub fn is_retriable(&self) -> bool {
        if self.is(ErrorKind::TooManyRequests) || self.status_code == 503 {
            return true;
        }
        TRANSIENT.iter().any(|r| r.is_match(&self.message))
    }

    /// Parse an error response. `method` and `path` drive the per-endpoint
    /// overrides (for example clusters/get 400 → `RESOURCE_DOES_NOT_EXIST`).
    #[must_use]
    pub fn from_response(status: u16, method: &str, path: &str, body: &[u8]) -> Self {
        let mut err = parse_body(status, body);
        apply_overrides(&mut err, method, path);
        err
    }
}

static TRANSIENT: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"com\.databricks\.backend\.manager\.util\.UnknownWorkerEnvironmentException",
        r"does not have any associated worker environments",
        r"There is no worker environment with id",
        r"Unknown worker environment",
        r"ClusterNotReadyException",
        r"worker env .* not found",
        r"Timed out after ",
        r"deadline exceeded",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("static regex"))
    .collect()
});

fn parse_body(status: u16, body: &[u8]) -> ApiError {
    if body.is_empty() {
        let text = reqwest::StatusCode::from_u16(status)
            .ok()
            .and_then(|s| s.canonical_reason())
            .unwrap_or("")
            .to_owned();
        return ApiError::new(status, "", &text);
    }
    if let Some(e) = parse_standard(status, body) {
        return e;
    }
    let text = String::from_utf8_lossy(body);
    if let Some(e) = parse_string(status, &text) {
        return e;
    }
    if let Some(e) = parse_html(status, &text) {
        return e;
    }
    ApiError::new(status, "UNKNOWN", &text)
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(default)]
    error_code: Option<Value>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    details: Vec<Value>,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default, rename = "scimType")]
    scim_type: Option<String>,
}

fn parse_standard(status: u16, body: &[u8]) -> Option<ApiError> {
    let env: Envelope = serde_json::from_slice(body).ok()?;
    let mut message = env.message.unwrap_or_default();
    let mut code = match env.error_code {
        Some(Value::String(s)) => s,
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    };
    // API 1.2 used {"error": "..."}.
    if let Some(e) = env.error.filter(|e| !e.is_empty()) {
        message = e;
    }
    // SCIM (RFC 7644 §3.7.3).
    if message.is_empty()
        && let Some(detail) = env.detail.filter(|d| !d.is_empty())
    {
        let detail = if detail == "null" {
            "SCIM API Internal Error".to_owned()
        } else {
            detail
        };
        let scim = format!("{} {}", env.scim_type.unwrap_or_default(), detail);
        scim.trim().clone_into(&mut message);
        code = format!("SCIM_{}", env.status.unwrap_or_default());
    }
    let mut err = ApiError::new(status, &code, &message);
    err.details = ErrorDetails::from_raw(env.details);
    Some(err)
}

static STRING_ERR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Z_]+): (.*)$").expect("static regex"));
static HTML_ERR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<pre>(.*)</pre>").expect("static regex"));

fn parse_string(status: u16, text: &str) -> Option<ApiError> {
    let c = STRING_ERR.captures(text)?;
    Some(ApiError::new(status, &c[1], &c[2]))
}

fn parse_html(status: u16, text: &str) -> Option<ApiError> {
    let c = HTML_ERR.captures(text)?;
    let reason = reqwest::StatusCode::from_u16(status)
        .ok()
        .and_then(|s| s.canonical_reason())
        .map_or_else(
            || "UNKNOWN".to_owned(),
            |r| r.trim_matches([' ', '.']).to_uppercase().replace(' ', "_"),
        );
    Some(ApiError::new(
        status,
        &reason,
        c[1].trim_matches([' ', '.']),
    ))
}

struct Override {
    path: Regex,
    method: &'static str,
    status: u16,
    code: &'static str,
    message: Regex,
    kind: ErrorKind,
}

static OVERRIDES: LazyLock<Vec<Override>> = LazyLock::new(|| {
    let mk = |path: &str, message: &str| Override {
        path: Regex::new(path).expect("static regex"),
        method: "GET",
        status: 400,
        code: "INVALID_PARAMETER_VALUE",
        message: Regex::new(message).expect("static regex"),
        kind: ErrorKind::ResourceDoesNotExist,
    };
    vec![
        mk(r"^/api/2\.\d/clusters/get", r"Cluster .* does not exist"),
        mk(r"^/api/2\.\d/jobs/get", r"Job .* does not exist"),
        mk(
            r"^/api/2\.\d/jobs/runs/get",
            r"(Run .* does not exist|Run: .* in job: .* doesn't exist)",
        ),
    ]
});

fn apply_overrides(err: &mut ApiError, method: &str, path: &str) {
    for o in OVERRIDES.iter() {
        if o.method.eq_ignore_ascii_case(method)
            && o.status == err.status_code
            && o.code == err.error_code
            && o.path.is_match(path)
            && o.message.is_match(&err.message)
        {
            err.kind = o.kind;
            return;
        }
    }
}

/// Structured error details (`google.rpc` types).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ErrorDetails {
    /// `google.rpc.ErrorInfo` entries.
    pub error_info: Vec<ErrorInfo>,
    /// `google.rpc.RequestInfo`, if present.
    pub request_info: Option<RequestInfo>,
    /// `google.rpc.RetryInfo` delay, if present.
    pub retry_delay: Option<Duration>,
    /// `google.rpc.Help` links.
    pub help_links: Vec<HelpLink>,
    /// Every detail as received, including types not modelled above.
    pub raw: Vec<Value>,
}

/// `google.rpc.ErrorInfo`.
#[derive(Debug, Clone, Default, Deserialize)]
#[non_exhaustive]
pub struct ErrorInfo {
    /// Reason code.
    #[serde(default)]
    pub reason: String,
    /// Logical grouping.
    #[serde(default)]
    pub domain: String,
    /// Additional metadata.
    #[serde(default)]
    pub metadata: std::collections::HashMap<String, String>,
}

/// `google.rpc.RequestInfo`.
#[derive(Debug, Clone, Default, Deserialize)]
#[non_exhaustive]
pub struct RequestInfo {
    /// Server request ID, useful for support tickets.
    #[serde(default)]
    pub request_id: String,
    /// Opaque serving data.
    #[serde(default)]
    pub serving_data: String,
}

/// `google.rpc.Help.Link`.
#[derive(Debug, Clone, Default, Deserialize)]
#[non_exhaustive]
pub struct HelpLink {
    /// What the link offers.
    #[serde(default)]
    pub description: String,
    /// The URL.
    #[serde(default)]
    pub url: String,
}

impl ErrorDetails {
    fn from_raw(raw: Vec<Value>) -> Self {
        let mut d = Self::default();
        for v in &raw {
            let ty = v.get("@type").and_then(Value::as_str).unwrap_or_default();
            match ty {
                "type.googleapis.com/google.rpc.ErrorInfo" => {
                    if let Ok(e) = serde_json::from_value(v.clone()) {
                        d.error_info.push(e);
                    }
                }
                "type.googleapis.com/google.rpc.RequestInfo" => {
                    d.request_info = serde_json::from_value(v.clone()).ok();
                }
                "type.googleapis.com/google.rpc.RetryInfo" => {
                    d.retry_delay = v
                        .get("retry_delay")
                        .and_then(Value::as_str)
                        .and_then(parse_proto_duration);
                }
                "type.googleapis.com/google.rpc.Help" => {
                    if let Some(links) = v.get("links") {
                        d.help_links = serde_json::from_value(links.clone()).unwrap_or_default();
                    }
                }
                _ => {}
            }
        }
        d.raw = raw;
        d
    }
}

/// Parse a protobuf JSON duration such as `"1.5s"`.
fn parse_proto_duration(s: &str) -> Option<Duration> {
    let secs: f64 = s.strip_suffix('s')?.parse().ok()?;
    (secs.is_finite() && secs >= 0.0).then(|| Duration::from_secs_f64(secs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn standard_envelope_with_details() {
        let body = br#"{
            "error_code": "RESOURCE_DOES_NOT_EXIST",
            "message": "Cluster abc does not exist",
            "details": [
              {"@type": "type.googleapis.com/google.rpc.ErrorInfo", "reason": "R", "domain": "D", "metadata": {"k": "v"}},
              {"@type": "type.googleapis.com/google.rpc.RequestInfo", "request_id": "req-1", "serving_data": ""},
              {"@type": "type.googleapis.com/google.rpc.RetryInfo", "retry_delay": "1.5s"},
              {"@type": "type.googleapis.com/google.rpc.Help", "links": [{"description": "docs", "url": "https://x"}]},
              {"@type": "type.googleapis.com/google.rpc.QuotaFailure"}
            ]}"#;
        let e = ApiError::from_response(404, "GET", "/api/2.1/clusters/get", body);
        assert_eq!(e.kind, ErrorKind::ResourceDoesNotExist);
        assert!(e.is(ErrorKind::NotFound));
        assert!(!e.is(ErrorKind::BadRequest));
        assert_eq!(e.details.error_info[0].reason, "R");
        assert_eq!(e.details.error_info[0].metadata["k"], "v");
        assert_eq!(e.details.request_info.as_ref().unwrap().request_id, "req-1");
        assert_eq!(e.details.retry_delay, Some(Duration::from_millis(1500)));
        assert_eq!(e.details.help_links[0].url, "https://x");
        assert_eq!(e.details.raw.len(), 5);
        assert!(!e.is_retriable());
    }

    #[test]
    fn numeric_error_code_and_status_fallback() {
        let e =
            ApiError::from_response(403, "GET", "/x", br#"{"error_code": 42, "message": "no"}"#);
        assert_eq!(e.error_code, "42");
        assert_eq!(e.kind, ErrorKind::PermissionDenied);
    }

    #[test]
    fn api_12_error_field() {
        let e = ApiError::from_response(400, "POST", "/x", br#"{"error": "old style"}"#);
        assert_eq!(e.message, "old style");
        assert_eq!(e.kind, ErrorKind::BadRequest);
    }

    #[test]
    fn scim_errors() {
        let e = ApiError::from_response(
            409,
            "POST",
            "/api/2.0/preview/scim/v2/Users",
            br#"{"detail": "User exists", "status": "409", "scimType": "uniqueness"}"#,
        );
        assert_eq!(e.message, "uniqueness User exists");
        assert_eq!(e.error_code, "SCIM_409");
        assert_eq!(e.kind, ErrorKind::ResourceConflict);
        let e = ApiError::from_response(
            500,
            "GET",
            "/scim",
            br#"{"detail": "null", "status": "500"}"#,
        );
        assert_eq!(e.message, "SCIM API Internal Error");
    }

    #[test]
    fn string_html_empty_and_unknown_bodies() {
        let e = ApiError::from_response(400, "GET", "/x", b"INVALID_STATE: nope");
        assert_eq!(
            (e.error_code.as_str(), e.message.as_str()),
            ("INVALID_STATE", "nope")
        );
        assert_eq!(e.kind, ErrorKind::InvalidState);

        let e = ApiError::from_response(404, "GET", "/x", b"<html><pre>Not here</pre></html>");
        assert_eq!(
            (e.error_code.as_str(), e.message.as_str()),
            ("NOT_FOUND", "Not here")
        );

        let e = ApiError::from_response(502, "GET", "/x", b"");
        assert_eq!(e.message, "Bad Gateway");
        assert_eq!(e.kind, ErrorKind::Other);

        let e = ApiError::from_response(502, "GET", "/x", b"gateway exploded");
        assert_eq!(e.error_code, "UNKNOWN");
        assert!(e.is(ErrorKind::InternalError));
    }

    #[test]
    fn overrides_apply_only_when_everything_matches() {
        let body =
            br#"{"error_code":"INVALID_PARAMETER_VALUE","message":"Cluster x does not exist"}"#;
        let hit = ApiError::from_response(400, "GET", "/api/2.1/clusters/get", body);
        assert!(hit.is_missing_kind());
        let wrong_verb = ApiError::from_response(400, "POST", "/api/2.1/clusters/get", body);
        assert_eq!(wrong_verb.kind, ErrorKind::InvalidParameterValue);
        let run = br#"{"error_code":"INVALID_PARAMETER_VALUE","message":"Run: 1 in job: 2 doesn't exist"}"#;
        assert!(
            ApiError::from_response(400, "GET", "/api/2.2/jobs/runs/get", run).is_missing_kind()
        );
    }

    #[test]
    fn retriable_rules() {
        assert!(ApiError::new(429, "", "").is_retriable());
        assert!(ApiError::new(400, "REQUEST_LIMIT_EXCEEDED", "").is_retriable());
        assert!(ApiError::new(503, "", "").is_retriable());
        assert!(ApiError::new(400, "", "ClusterNotReadyException: soon").is_retriable());
        assert!(!ApiError::new(500, "", "boom").is_retriable());
    }

    #[test]
    fn error_wrapper_helpers() {
        let e: Error = ApiError::new(404, "", "gone").into();
        assert!(e.is_missing());
        assert_eq!(e.to_string(), "gone");
        let c = Error::Config("x".into());
        assert!(c.as_api().is_none());
        assert!(!c.is_missing());
        assert!(parse_proto_duration("abc").is_none());
    }

    impl ApiError {
        fn is_missing_kind(&self) -> bool {
            self.kind == ErrorKind::ResourceDoesNotExist
        }
    }
}
