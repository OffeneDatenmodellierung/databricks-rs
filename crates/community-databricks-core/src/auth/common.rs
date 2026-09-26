//! Pieces shared by the OAuth-based strategies: a retrying token-endpoint
//! request, the standard token response, a multi-header provider and a
//! small wall-clock time parser.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use futures_util::future::BoxFuture;
use reqwest::header::{HeaderName, HeaderValue};
use secrecy::SecretString;
use serde::Deserialize;
use tokio::time::Instant;

#[cfg(doc)]
use super::token::CachedTokenSource;
use super::token::{Token, TokenSource};
use super::{CredentialsProvider, Headers, bearer};
use crate::error::{ApiError, Error, Result};
use crate::http::{backoff, retry_after};

/// Retry budget for a single token request (Go: 1 minute).
pub(crate) const TOKEN_RETRY_TIMEOUT: Duration = Duration::from_mins(1);
/// Go's `retriableCodes` for token requests.
const RETRIABLE: &[u16] = &[429, 502, 503, 504];

/// Send the request built by `build` until it succeeds, fails with a
/// non-retriable status, or [`TOKEN_RETRY_TIMEOUT`] runs out. Returns the
/// body of the 2xx response.
pub(crate) async fn send_token_request(
    url: &str,
    build: impl Fn() -> reqwest::RequestBuilder,
) -> Result<Bytes> {
    let deadline = Instant::now() + TOKEN_RETRY_TIMEOUT;
    let mut attempt = 0;
    loop {
        attempt += 1;
        let (err, hint) = match attempt_once(url, &build).await {
            Ok(body) => return Ok(body),
            Err((e, None)) => return Err(e),
            Err((e, Some(hint))) => (e, hint),
        };
        let wait = backoff(attempt).max(hint);
        if Instant::now() + wait > deadline {
            return Err(err);
        }
        tracing::debug!(attempt, ?wait, "retrying token request: {err}");
        tokio::time::sleep(wait).await;
    }
}

async fn attempt_once(
    url: &str,
    build: &impl Fn() -> reqwest::RequestBuilder,
) -> std::result::Result<Bytes, (Error, Option<Duration>)> {
    let resp = match build().send().await {
        Ok(r) => r,
        Err(e) if e.is_connect() || e.is_timeout() => {
            return Err((e.into(), Some(Duration::ZERO)));
        }
        Err(e) => return Err((e.into(), None)),
    };
    let status = resp.status().as_u16();
    let wait = retry_after(resp.headers());
    let body = resp.bytes().await.map_err(|e| (e.into(), None))?;
    if (200..300).contains(&status) {
        return Ok(body);
    }
    let path = reqwest::Url::parse(url)
        .map(|u| u.path().to_owned())
        .unwrap_or_default();
    let err: Error = ApiError::from_response(status, "POST", &path, &body).into();
    let retry = RETRIABLE
        .contains(&status)
        .then(|| wait.unwrap_or(Duration::ZERO));
    Err((err, retry))
}

/// The RFC 6749 token response.
#[derive(Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

/// Parse an RFC 6749 token response.
pub(crate) fn parse_oauth_token(body: &[u8]) -> Result<Token> {
    let t: OAuthTokenResponse =
        serde_json::from_slice(body).map_err(|e| Error::json("token response", e))?;
    if t.access_token.is_empty() {
        return Err(Error::Config("token response has no access_token".into()));
    }
    Ok(Token {
        access_token: SecretString::from(t.access_token),
        token_type: t
            .token_type
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "Bearer".into()),
        expiry: t.expires_in.filter(|s| *s > 0).map(instant_after),
    })
}

/// A second token sent in its own header (Go: `serviceToServiceVisitor`).
pub(crate) struct Secondary {
    pub header: HeaderName,
    pub source: Arc<dyn TokenSource>,
    /// Skip the header, rather than fail the request, if the token can't
    /// be obtained (Go's `secondaryOptional`).
    pub optional: bool,
}

/// A provider that sets `Authorization: Bearer` from `primary` and,
/// optionally, a second token header and fixed headers.
///
/// Sources are used as given: wrap hand-written ones in a
/// [`CachedTokenSource`]; vendor credentials (`azure_identity`,
/// `google-cloud-auth`) cache internally.
pub(crate) struct TokenHeaders {
    pub primary: Arc<dyn TokenSource>,
    pub secondary: Option<Secondary>,
    pub fixed: Headers,
}

impl fmt::Debug for TokenHeaders {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenHeaders")
            .field("secondary", &self.secondary.as_ref().map(|s| &s.header))
            .finish_non_exhaustive()
    }
}

impl TokenHeaders {
    pub(crate) fn bearer(primary: impl TokenSource + 'static) -> Self {
        Self {
            primary: Arc::new(primary),
            secondary: None,
            fixed: Vec::new(),
        }
    }
}

impl CredentialsProvider for TokenHeaders {
    fn headers(&self) -> BoxFuture<'_, Result<Headers>> {
        Box::pin(async move {
            let mut h = self.fixed.clone();
            h.extend(bearer(self.primary.token().await?.secret())?);
            if let Some(s) = &self.secondary {
                match s.source.token().await {
                    Ok(t) => {
                        let mut v = HeaderValue::from_str(t.secret()).map_err(|_| {
                            Error::Config("token contains invalid header characters".into())
                        })?;
                        v.set_sensitive(true);
                        h.push((s.header.clone(), v));
                    }
                    Err(e) if s.optional => {
                        tracing::warn!(header = %s.header, "skipping secondary token: {e}");
                    }
                    Err(e) => return Err(e),
                }
            }
            Ok(h)
        })
    }
}

/// `secs` from now, saturating: a nonsensical server value (near `u64::MAX`)
/// must not overflow `Instant` and panic. Capped at ten years.
pub(crate) fn instant_after(secs: u64) -> Instant {
    const MAX: u64 = 10 * 365 * 24 * 3_600;
    Instant::now() + Duration::from_secs(secs.min(MAX))
}

/// Seconds since the Unix epoch → a monotonic deadline (past times map to
/// "now", so the token is refreshed on next use).
pub(crate) fn instant_from_unix(secs: i64) -> Instant {
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
    let left = secs.saturating_sub(now_unix);
    instant_after(u64::try_from(left).unwrap_or(0))
}

/// Days since 1970-01-01 for a proleptic Gregorian date (H. Hinnant).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn num(s: &str) -> Option<i64> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

fn unix(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64) -> Option<i64> {
    let ok = (1..=12).contains(&mo)
        && (1..=31).contains(&d)
        && (0..=23).contains(&h)
        && (0..=59).contains(&mi)
        && (0..=60).contains(&s);
    ok.then(|| days_from_civil(y, mo, d) * 86_400 + h * 3_600 + mi * 60 + s)
}

/// `HH:MM:SS[.fff…]` → (h, m, s); the fraction is dropped.
fn clock(t: &str) -> Option<(i64, i64, i64)> {
    let t = t.split_once('.').map_or(
        t,
        |(whole, frac)| {
            if num(frac).is_some() { whole } else { "\u{0}" }
        },
    );
    let mut it = t.split(':');
    let (h, m, s) = (it.next()?, it.next()?, it.next()?);
    if it.next().is_some() || h.len() != 2 || m.len() != 2 || s.len() != 2 {
        return None;
    }
    Some((num(h)?, num(m)?, num(s)?))
}

/// Parse the timestamps the Databricks CLI and Google APIs return, into
/// seconds since the Unix epoch. Go: `parseExpiry`.
///
/// Accepts RFC 3339 (`2024-03-20T10:30:00.123Z`, `…+01:00`) and
/// `2024-03-20 10:30:00[.123]`, which, as with Go's `time.Parse`, is UTC.
pub(crate) fn parse_timestamp(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, rest) = s.split_at_checked(10)?;
    let sep = rest.chars().next()?;
    if !matches!(sep, 'T' | 't' | ' ') {
        return None;
    }
    let rest = &rest[1..];
    let (time, offset) = if sep == ' ' {
        (rest, 0)
    } else if let Some(t) = rest.strip_suffix(['Z', 'z']) {
        (t, 0)
    } else {
        let at = rest.rfind(['+', '-'])?;
        let (t, off) = rest.split_at(at);
        let sign = if off.starts_with('-') { -1 } else { 1 };
        let (oh, om) = off[1..].split_once(':')?;
        if oh.len() != 2 || om.len() != 2 {
            return None;
        }
        (t, sign * (num(oh)? * 3_600 + num(om)? * 60))
    };
    let mut d = date.split('-');
    let (y, mo, da) = (d.next()?, d.next()?, d.next()?);
    if y.len() != 4 || mo.len() != 2 || da.len() != 2 {
        return None;
    }
    let (h, mi, se) = clock(time)?;
    Some(unix(num(y)?, num(mo)?, num(da)?, h, mi, se)? - offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps() {
        let base = 1_710_930_600; // 2024-03-20T10:30:00Z
        for s in [
            "2024-03-20T10:30:00Z",
            "2024-03-20T10:30:00.123456789Z",
            "2024-03-20T11:30:00+01:00",
            "2024-03-20T09:30:00-01:00",
            "2024-03-20 10:30:00",
            "2024-03-20 10:30:00.123",
        ] {
            assert_eq!(parse_timestamp(s), Some(base), "{s}");
        }
        for s in [
            "",
            "2024-03-20",
            "2024-03-20X10:30:00Z",
            "2024-13-20T10:30:00Z",
            "2024-03-20T10:30Z",
            "2024-03-20T10:30:00+0100",
            "2024-03-20T10:30:00.xZ",
            "24-03-20T10:30:00Z",
        ] {
            assert_eq!(parse_timestamp(s), None, "{s}");
        }
        assert_eq!(parse_timestamp("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_timestamp("2000-02-29T00:00:00Z"), Some(951_782_400));
    }

    #[tokio::test(start_paused = true)]
    async fn unix_to_instant() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let now = i64::try_from(now).unwrap();
        let i = instant_from_unix(now + 100);
        let left = i - Instant::now();
        assert!((99..=100).contains(&left.as_secs()), "{left:?}");
        assert!(instant_from_unix(0) <= Instant::now());
        // Absurd values saturate rather than panic.
        assert!(instant_from_unix(i64::MAX) > Instant::now());
        assert!(instant_after(u64::MAX) > Instant::now());
    }

    #[test]
    fn oauth_token_parsing() {
        let t = parse_oauth_token(br#"{"access_token":"a","expires_in":10}"#).unwrap();
        assert_eq!((t.secret(), t.token_type.as_str()), ("a", "Bearer"));
        assert!(t.expiry.is_some());
        assert!(parse_oauth_token(br#"{"access_token":""}"#).is_err());
        assert!(parse_oauth_token(b"nope").is_err());
    }
}
