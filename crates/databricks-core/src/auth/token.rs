//! OAuth tokens and a cache with proactive asynchronous refresh.
//!
//! Semantics follow Go's `auth.NewCachedTokenSource`:
//!
//! * An expired (or missing) token is refreshed with a blocking call; only
//!   one blocking refresh runs at a time.
//! * Once a token enters its refresh window — `min(TTL / 2, 20 min)` before
//!   expiry — callers get the current token immediately and a single
//!   background refresh is started.
//! * A failed background refresh is retried no sooner than one minute later.
//! * A background result never replaces a newer token.

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::future::BoxFuture;
use secrecy::{ExposeSecret, SecretString};
use tokio::time::Instant;

use crate::error::Result;

const MAX_ASYNC_LEAD: Duration = Duration::from_mins(20);
const ASYNC_RETRY_BACKOFF: Duration = Duration::from_mins(1);
const ASYNC_TIMEOUT: Duration = Duration::from_mins(1);

/// An access token.
#[derive(Clone)]
pub struct Token {
    /// The bearer token.
    pub access_token: SecretString,
    /// Token type (normally `Bearer`).
    pub token_type: String,
    /// When it expires; `None` means it never does.
    pub expiry: Option<Instant>,
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Token")
            .field("access_token", &"***")
            .field("token_type", &self.token_type)
            .field("expiry", &self.expiry)
            .finish()
    }
}

impl Token {
    /// The raw access token (do not log).
    #[must_use]
    pub fn secret(&self) -> &str {
        self.access_token.expose_secret()
    }

    fn expired(&self, now: Instant) -> bool {
        self.expiry.is_some_and(|e| now >= e)
    }
}

/// Something that can mint a fresh token.
pub trait TokenSource: Send + Sync {
    /// Fetch a new token.
    fn token(&self) -> BoxFuture<'_, Result<Token>>;
}

#[derive(Default)]
struct State {
    token: Option<Token>,
    next_async: Option<Instant>,
    refreshing: bool,
}

struct Inner {
    source: Box<dyn TokenSource>,
    state: Mutex<State>,
    blocking: tokio::sync::Mutex<()>,
    async_refresh: bool,
}

/// Caching wrapper around a [`TokenSource`].
#[derive(Clone)]
pub struct CachedTokenSource {
    inner: Arc<Inner>,
}

impl fmt::Debug for CachedTokenSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CachedTokenSource")
            .field("async_refresh", &self.inner.async_refresh)
            .finish_non_exhaustive()
    }
}

fn lead_time(ttl: Duration) -> Duration {
    (ttl / 2).min(MAX_ASYNC_LEAD)
}

impl TokenSource for CachedTokenSource {
    fn token(&self) -> BoxFuture<'_, Result<Token>> {
        Box::pin(CachedTokenSource::token(self))
    }
}

impl CachedTokenSource {
    /// Wrap `source`. `async_refresh` enables proactive background refresh.
    pub fn new(source: impl TokenSource + 'static, async_refresh: bool) -> Self {
        Self {
            inner: Arc::new(Inner {
                source: Box::new(source),
                state: Mutex::new(State::default()),
                blocking: tokio::sync::Mutex::new(()),
                async_refresh,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// A valid token, refreshing as described in the module docs.
    pub async fn token(&self) -> Result<Token> {
        let now = Instant::now();
        let (current, start_async) = {
            let mut s = self.lock();
            match s.token.clone() {
                Some(t) if !t.expired(now) => {
                    let due = self.inner.async_refresh
                        && !s.refreshing
                        && s.next_async.is_some_and(|n| now >= n);
                    if due {
                        s.refreshing = true;
                    }
                    (Some(t), due)
                }
                _ => (None, false),
            }
        };
        if start_async {
            self.spawn_refresh();
        }
        match current {
            Some(t) => Ok(t),
            None => self.blocking_refresh().await,
        }
    }

    async fn blocking_refresh(&self) -> Result<Token> {
        let _guard = self.inner.blocking.lock().await;
        if let Some(t) = self
            .lock()
            .token
            .clone()
            .filter(|t| !t.expired(Instant::now()))
        {
            return Ok(t);
        }
        let t = self.inner.source.token().await?;
        self.store(t.clone());
        Ok(t)
    }

    fn store(&self, t: Token) {
        let now = Instant::now();
        let mut s = self.lock();
        s.next_async = t.expiry.map(|e| {
            e.checked_sub(lead_time(e.saturating_duration_since(now)))
                .unwrap_or(now)
        });
        s.token = Some(t);
    }

    fn spawn_refresh(&self) {
        let this = self.clone();
        tokio::spawn(async move {
            let res = tokio::time::timeout(ASYNC_TIMEOUT, this.inner.source.token()).await;
            let mut s = this.lock();
            s.refreshing = false;
            match res {
                Ok(Ok(t)) => {
                    let newer_cached = matches!(
                        (&s.token, t.expiry),
                        (Some(Token { expiry: Some(cur), .. }), Some(new)) if new < *cur
                    );
                    if newer_cached {
                        return;
                    }
                    drop(s);
                    this.store(t);
                }
                Ok(Err(e)) => {
                    tracing::debug!("async token refresh failed: {e}");
                    s.next_async = Some(Instant::now() + ASYNC_RETRY_BACKOFF);
                }
                Err(_) => {
                    tracing::debug!("async token refresh timed out");
                    s.next_async = Some(Instant::now() + ASYNC_RETRY_BACKOFF);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct Counting {
        calls: Arc<AtomicU32>,
        ttl: Option<Duration>,
        fail_after: Option<u32>,
    }

    impl TokenSource for Counting {
        fn token(&self) -> BoxFuture<'_, Result<Token>> {
            Box::pin(async move {
                let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
                if self.fail_after.is_some_and(|f| n > f) {
                    return Err(crate::Error::OperationFailed("boom".into()));
                }
                Ok(Token {
                    access_token: SecretString::from(format!("t{n}")),
                    token_type: "Bearer".into(),
                    expiry: self.ttl.map(|d| Instant::now() + d),
                })
            })
        }
    }

    fn src(ttl: Option<Duration>, fail_after: Option<u32>) -> (Counting, Arc<AtomicU32>) {
        let calls = Arc::new(AtomicU32::new(0));
        (
            Counting {
                calls: calls.clone(),
                ttl,
                fail_after,
            },
            calls,
        )
    }

    #[test]
    fn lead_time_matches_go() {
        assert_eq!(lead_time(Duration::from_hours(1)), MAX_ASYNC_LEAD);
        assert_eq!(lead_time(Duration::from_mins(10)), Duration::from_mins(5));
        assert_eq!(lead_time(Duration::from_secs(2)), Duration::from_secs(1));
    }

    #[tokio::test(start_paused = true)]
    async fn caches_until_refresh_window_then_refreshes_in_background() {
        let (s, calls) = src(Some(Duration::from_hours(1)), None);
        let c = CachedTokenSource::new(s, true);
        assert_eq!(c.token().await.unwrap().secret(), "t1");
        assert_eq!(c.token().await.unwrap().secret(), "t1");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // 41 minutes in: inside the 20-minute window. Stale token returned,
        // refresh happens in the background.
        tokio::time::advance(Duration::from_mins(41)).await;
        assert_eq!(c.token().await.unwrap().secret(), "t1");
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(c.token().await.unwrap().secret(), "t2");
    }

    #[tokio::test(start_paused = true)]
    async fn expired_token_blocks_and_async_disabled_never_prefetches() {
        let (s, calls) = src(Some(Duration::from_mins(1)), None);
        let c = CachedTokenSource::new(s, false);
        c.token().await.unwrap();
        tokio::time::advance(Duration::from_secs(45)).await;
        assert_eq!(c.token().await.unwrap().secret(), "t1");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_secs(20)).await;
        assert_eq!(c.token().await.unwrap().secret(), "t2");
    }

    #[tokio::test(start_paused = true)]
    async fn failed_async_refresh_backs_off_and_keeps_token() {
        let (s, calls) = src(Some(Duration::from_hours(1)), Some(1));
        let c = CachedTokenSource::new(s, true);
        c.token().await.unwrap();
        tokio::time::advance(Duration::from_mins(41)).await;
        c.token().await.unwrap();
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        // Within the backoff: no new attempt.
        assert_eq!(c.token().await.unwrap().secret(), "t1");
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        // Expired: blocking refresh surfaces the error.
        tokio::time::advance(Duration::from_mins(20)).await;
        assert!(c.token().await.is_err());
    }

    #[tokio::test]
    async fn non_expiring_tokens_are_fetched_once() {
        let (s, calls) = src(None, None);
        let c = CachedTokenSource::new(s, true);
        for _ in 0..3 {
            c.token().await.unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(format!("{:?}", c.token().await.unwrap()).contains("***"));
        assert!(format!("{c:?}").contains("async_refresh"));
    }
}
