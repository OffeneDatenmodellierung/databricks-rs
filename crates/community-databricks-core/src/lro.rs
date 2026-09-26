//! Long-running operations (Go: the generated `XOperationInterface`
//! wrappers in `postgres`, `apps`, `environments` and `ml`).
//!
//! An operation-returning call gives back a [`LongRunning`] handle. The
//! service's `Operation` message says whether the work is `done`, and
//! carries the result in `response` (or an `error`) and progress in
//! `metadata`. [`LongRunning::wait`] polls the service's `GetOperation`
//! until the operation is done, and decodes the result.
//!
//! Polling follows Go's `api.BackoffPolicy` defaults: a random delay in
//! `[0, d]` where `d` starts at 1 second and doubles up to 60 seconds. Go
//! waits indefinitely unless given a timeout; so does this, unless
//! [`LongRunning::with_timeout`] is set.

use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::time::Instant;

use crate::error::{ApiError, Error, Result};

/// The fields of a service's `Operation` message that polling needs.
/// Generated for each package's `Operation` type.
pub trait OperationState: Send + Sync + 'static {
    /// Server-assigned name, used to poll the operation.
    fn name(&self) -> &str;
    /// Whether the operation has finished (successfully or not).
    fn is_done(&self) -> bool;
    /// `(error_code, message)` when the operation failed.
    fn failure(&self) -> Option<(String, String)>;
    /// The result, once done.
    fn response(&self) -> Option<&Value>;
    /// Progress information.
    fn metadata(&self) -> Option<&Value>;
}

/// Fetch the current state of an operation by name.
pub type PollFn<O> = Arc<dyn Fn(String) -> BoxFuture<'static, Result<O>> + Send + Sync>;

/// Ask the server to cancel an operation by name.
pub type CancelFn = Arc<dyn Fn(String) -> BoxFuture<'static, Result<()>> + Send + Sync>;

/// A long-running operation: poll it with [`wait`](Self::wait) for its
/// result `T` (`()` for deletes), or inspect its progress `M`.
pub struct LongRunning<O, T, M> {
    operation: O,
    poll: PollFn<O>,
    cancel: Option<CancelFn>,
    timeout: Option<Duration>,
    initial: Duration,
    maximum: Duration,
    _types: PhantomData<fn() -> (T, M)>,
}

impl<O: OperationState, T, M> LongRunning<O, T, M> {
    /// Wrap an operation returned by the service. Generated code calls
    /// this; `poll` is the service's `GetOperation`.
    #[must_use]
    pub fn new(operation: O, poll: PollFn<O>, cancel: Option<CancelFn>) -> Self {
        Self {
            operation,
            poll,
            cancel,
            timeout: None,
            initial: Duration::from_secs(1),
            maximum: Duration::from_mins(1),
            _types: PhantomData,
        }
    }

    /// Stop [`wait`](Self::wait) with [`Error::Timeout`] after `timeout`.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Change the polling backoff (default: from 1s, doubling, up to 60s).
    #[must_use]
    pub fn with_poll_interval(mut self, initial: Duration, maximum: Duration) -> Self {
        self.initial = initial;
        self.maximum = maximum.max(initial);
        self
    }

    /// The operation's server-assigned name.
    #[must_use]
    pub fn name(&self) -> &str {
        self.operation.name()
    }

    /// The operation as last seen.
    #[must_use]
    pub fn operation(&self) -> &O {
        &self.operation
    }

    /// The operation as last seen, dropping the handle.
    #[must_use]
    pub fn into_operation(self) -> O {
        self.operation
    }

    /// Progress information from the operation as last seen, or `None`
    /// when the service sent none.
    pub fn metadata(&self) -> Result<Option<M>>
    where
        M: DeserializeOwned,
    {
        self.operation
            .metadata()
            .filter(|v| !v.is_null())
            .map(|v| {
                serde_json::from_value(v.clone()).map_err(|e| Error::json("operation metadata", e))
            })
            .transpose()
    }

    /// Refresh the operation and report whether it has finished.
    pub async fn done(&mut self) -> Result<bool> {
        self.refresh().await?;
        Ok(self.operation.is_done())
    }

    /// Ask the server to cancel the operation (best effort, as in Go). Only
    /// some operations can be cancelled; others return a config error.
    pub async fn cancel(&self) -> Result<()> {
        match &self.cancel {
            Some(cancel) => cancel(self.name().to_owned()).await,
            None => Err(Error::Config(format!(
                "operation {} cannot be cancelled",
                self.name()
            ))),
        }
    }

    /// Poll until the operation finishes, then return its result.
    ///
    /// A failed operation is an [`Error::Api`] carrying the operation's
    /// error code and message (the HTTP status is 0: the poll itself
    /// succeeded).
    pub async fn wait(mut self) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let deadline = self.timeout.map(|t| Instant::now() + t);
        let mut current = self.initial;
        loop {
            if self.operation.is_done() {
                return self.result();
            }
            let delay = jitter(current);
            current = (current * 2).min(self.maximum);
            if let Some(d) = deadline {
                let now = Instant::now();
                if now >= d {
                    return Err(Error::Timeout {
                        after: self.timeout.unwrap_or_default(),
                        last: format!("operation {} still in progress", self.name()),
                    });
                }
                tokio::time::sleep(delay.min(d - now)).await;
            } else {
                tokio::time::sleep(delay).await;
            }
            tracing::debug!(operation = self.name(), "polling long-running operation");
            self.refresh().await?;
        }
    }

    async fn refresh(&mut self) -> Result<()> {
        self.operation = (self.poll)(self.name().to_owned()).await?;
        Ok(())
    }

    fn result(&self) -> Result<T>
    where
        T: DeserializeOwned,
    {
        if let Some((code, message)) = self.operation.failure() {
            let message = if message.is_empty() {
                "unknown error".to_owned()
            } else {
                message
            };
            let text = if code.is_empty() {
                format!("operation {} failed: {message}", self.name())
            } else {
                format!("operation {} failed: [{code}] {message}", self.name())
            };
            return Err(ApiError::new(0, &code, &text).into());
        }
        let value = self.operation.response().cloned().unwrap_or(Value::Null);
        // `()` (deletes) accepts null; a result type needs a response.
        serde_json::from_value(value).map_err(|e| Error::json("operation response", e))
    }
}

/// A random duration in `[0, max]` ("full jitter").
fn jitter(max: Duration) -> Duration {
    let ms = u64::try_from(max.as_millis()).unwrap_or(u64::MAX);
    Duration::from_millis(fastrand::u64(0..=ms))
}

impl<O: OperationState + fmt::Debug, T, M> fmt::Debug for LongRunning<O, T, M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LongRunning")
            .field("operation", &self.operation)
            .field("timeout", &self.timeout)
            .field("cancellable", &self.cancel.is_some())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug, Clone, Default)]
    struct Op {
        name: String,
        done: bool,
        error: Option<(String, String)>,
        response: Option<Value>,
        metadata: Option<Value>,
    }

    impl OperationState for Op {
        fn name(&self) -> &str {
            &self.name
        }
        fn is_done(&self) -> bool {
            self.done
        }
        fn failure(&self) -> Option<(String, String)> {
            self.error.clone()
        }
        fn response(&self) -> Option<&Value> {
            self.response.as_ref()
        }
        fn metadata(&self) -> Option<&Value> {
            self.metadata.as_ref()
        }
    }

    fn pending() -> Op {
        Op {
            name: "operations/1".into(),
            metadata: Some(serde_json::json!({"progress": 10})),
            ..Op::default()
        }
    }

    /// Done after `after` polls, with `last` as the final state.
    fn poller(after: usize, last: Op, calls: Arc<AtomicUsize>) -> PollFn<Op> {
        Arc::new(move |name| {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            let last = last.clone();
            Box::pin(async move {
                assert_eq!(name, "operations/1");
                Ok(if n >= after { last } else { pending() })
            })
        })
    }

    fn fast<T, M>(lro: LongRunning<Op, T, M>) -> LongRunning<Op, T, M> {
        lro.with_poll_interval(Duration::from_millis(1), Duration::from_millis(2))
    }

    #[tokio::test]
    async fn waits_until_done_and_decodes_the_response() {
        let calls = Arc::new(AtomicUsize::new(0));
        let done = Op {
            name: "operations/1".into(),
            done: true,
            response: Some(serde_json::json!({"id": 7})),
            ..Op::default()
        };
        let lro: LongRunning<Op, serde_json::Map<String, Value>, Value> = fast(LongRunning::new(
            pending(),
            poller(3, done, calls.clone()),
            None,
        ));
        assert_eq!(lro.name(), "operations/1");
        assert_eq!(lro.metadata().unwrap().unwrap()["progress"], 10);
        assert!(format!("{lro:?}").contains("cancellable: false"));
        let out = lro.wait().await.unwrap();
        assert_eq!(out["id"], 7);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_finished_operation_is_not_polled() {
        let calls = Arc::new(AtomicUsize::new(0));
        let done = Op {
            name: "operations/1".into(),
            done: true,
            ..Op::default()
        };
        let lro: LongRunning<Op, (), Value> =
            LongRunning::new(done.clone(), poller(1, done, calls.clone()), None);
        lro.wait().await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn failures_become_api_errors() {
        let failed = Op {
            name: "operations/1".into(),
            done: true,
            error: Some(("RESOURCE_EXHAUSTED".into(), "quota".into())),
            ..Op::default()
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let lro: LongRunning<Op, (), Value> =
            fast(LongRunning::new(pending(), poller(1, failed, calls), None));
        let e = lro.wait().await.unwrap_err();
        assert!(e.is(crate::ErrorKind::ResourceExhausted), "{e}");
        assert!(
            e.to_string()
                .contains("operation operations/1 failed: [RESOURCE_EXHAUSTED] quota"),
            "{e}"
        );

        let bare = Op {
            name: "operations/1".into(),
            done: true,
            error: Some((String::new(), String::new())),
            ..Op::default()
        };
        let lro: LongRunning<Op, (), Value> =
            LongRunning::new(bare.clone(), poller(1, bare, Arc::default()), None);
        let e = lro.wait().await.unwrap_err();
        assert!(e.to_string().contains("failed: unknown error"), "{e}");
    }

    #[tokio::test]
    async fn a_result_type_needs_a_response() {
        let done = Op {
            name: "operations/1".into(),
            done: true,
            ..Op::default()
        };
        let lro: LongRunning<Op, serde_json::Map<String, Value>, Value> =
            LongRunning::new(done.clone(), poller(1, done, Arc::default()), None);
        assert!(matches!(lro.wait().await, Err(Error::Json { .. })));
    }

    #[tokio::test]
    async fn timeout_stops_the_wait() {
        let lro: LongRunning<Op, (), Value> = fast(LongRunning::new(
            pending(),
            poller(usize::MAX, pending(), Arc::default()),
            None,
        ))
        .with_timeout(Duration::from_millis(30));
        let e = lro.wait().await.unwrap_err();
        assert!(matches!(e, Error::Timeout { .. }), "{e}");
        assert!(e.to_string().contains("still in progress"));
    }

    #[tokio::test]
    async fn done_refreshes_and_cancel_uses_the_service() {
        let calls = Arc::new(AtomicUsize::new(0));
        let done = Op {
            name: "operations/1".into(),
            done: true,
            ..Op::default()
        };
        let cancelled = Arc::new(AtomicUsize::new(0));
        let c = cancelled.clone();
        let cancel: CancelFn = Arc::new(move |name| {
            assert_eq!(name, "operations/1");
            c.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        });
        let mut lro: LongRunning<Op, (), Value> =
            LongRunning::new(pending(), poller(2, done, calls), Some(cancel));
        assert!(!lro.done().await.unwrap());
        assert!(lro.done().await.unwrap());
        lro.cancel().await.unwrap();
        assert_eq!(cancelled.load(Ordering::SeqCst), 1);
        assert!(lro.operation().is_done());
        assert!(lro.into_operation().done);

        let plain: LongRunning<Op, (), Value> =
            LongRunning::new(pending(), poller(1, pending(), Arc::default()), None);
        assert!(plain.cancel().await.is_err());
        let none = Op {
            name: "operations/1".into(),
            metadata: Some(Value::Null),
            ..Op::default()
        };
        let lro: LongRunning<Op, (), Value> =
            LongRunning::new(none, poller(1, pending(), Arc::default()), None);
        assert!(lro.metadata().unwrap().is_none());
    }

    #[test]
    fn jitter_stays_within_the_bound() {
        for _ in 0..100 {
            assert!(jitter(Duration::from_millis(5)) <= Duration::from_millis(5));
        }
        assert_eq!(jitter(Duration::ZERO), Duration::ZERO);
    }
}
