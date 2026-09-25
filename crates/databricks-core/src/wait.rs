//! Polling for long-running operations (Go: `retries.Poll` plus the
//! generated `Wait…` helpers).

use std::future::Future;
use std::time::Duration;

use tokio::time::Instant;

use crate::error::{Error, Result};
use crate::http::backoff;

/// Outcome of one poll.
#[derive(Debug)]
pub enum PollStatus<T> {
    /// Target state reached.
    Done(T),
    /// Not yet; the string describes the current state.
    Continue(String),
}

/// Call `check` until it returns [`PollStatus::Done`], an error, or
/// `timeout` elapses. Waits `attempt` seconds (max 10s) plus jitter between
/// polls, like Go.
pub async fn poll<T, F, Fut>(timeout: Duration, mut check: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<PollStatus<T>>>,
{
    let deadline = Instant::now() + timeout;
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let last = match check().await? {
            PollStatus::Done(v) => return Ok(v),
            PollStatus::Continue(msg) => msg,
        };
        let wait = backoff(attempt);
        let now = Instant::now();
        if now >= deadline {
            return Err(Error::Timeout {
                after: timeout,
                last,
            });
        }
        tracing::debug!(attempt, %last, "waiting");
        tokio::time::sleep(wait.min(deadline - now)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn returns_when_done() {
        let mut n = 0;
        let v = poll(Duration::from_mins(1), || {
            n += 1;
            let n = n;
            async move {
                Ok(if n < 3 {
                    PollStatus::Continue(format!("n={n}"))
                } else {
                    PollStatus::Done(n)
                })
            }
        })
        .await
        .unwrap();
        assert_eq!(v, 3);
    }

    #[tokio::test(start_paused = true)]
    async fn times_out_with_last_status() {
        let e = poll::<(), _, _>(Duration::from_secs(5), || async {
            Ok(PollStatus::Continue("PENDING".into()))
        })
        .await
        .unwrap_err();
        assert!(
            matches!(e, Error::Timeout { ref last, .. } if last == "PENDING"),
            "{e}"
        );
    }

    #[tokio::test]
    async fn errors_halt() {
        let e = poll::<(), _, _>(Duration::from_secs(5), || async {
            Err(Error::OperationFailed("INTERNAL_ERROR".into()))
        })
        .await
        .unwrap_err();
        assert!(matches!(e, Error::OperationFailed(_)));
    }
}
