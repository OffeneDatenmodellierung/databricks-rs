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

/// Progress callback passed to waiters.
pub type Progress<T> = Box<dyn FnMut(&T) + Send>;

/// The target/failure states of a generated waiter.
#[derive(Debug, Clone, Copy)]
pub struct States<'a> {
    /// JSON path to the status field (e.g. `["state", "life_cycle_state"]`).
    pub status_path: &'a [&'a str],
    /// JSON path to the status message, if any.
    pub message_path: &'a [&'a str],
    /// States that end the wait successfully.
    pub targets: &'a [&'a str],
    /// States that end the wait with [`Error::OperationFailed`].
    pub failures: &'a [&'a str],
}

fn walk<'v>(v: &'v serde_json::Value, path: &[&str]) -> Option<&'v serde_json::Value> {
    path.iter().try_fold(v, |acc, k| acc.get(*k))
}

/// Classify a polled value against `states` (the body of every Go
/// `Wait…` function).
pub fn check_state<T: serde::Serialize>(value: T, states: States<'_>) -> Result<PollStatus<T>> {
    let v = serde_json::to_value(&value).map_err(|e| Error::json("waiter status", e))?;
    let current = walk(&v, states.status_path)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let message = (!states.message_path.is_empty())
        .then(|| walk(&v, states.message_path).and_then(serde_json::Value::as_str))
        .flatten()
        .map_or_else(|| format!("current status: {current}"), str::to_owned);
    if states.targets.contains(&current.as_str()) {
        return Ok(PollStatus::Done(value));
    }
    if states.failures.contains(&current.as_str()) {
        return Err(Error::OperationFailed(format!(
            "failed to reach {}, got {current}: {message}",
            states.targets.join(" or ")
        )));
    }
    Ok(PollStatus::Continue(message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_state_follows_paths() {
        let s = States {
            status_path: &["state", "life"],
            message_path: &["state", "msg"],
            targets: &["DONE"],
            failures: &["BAD"],
        };
        let v = |life: &str| serde_json::json!({"state": {"life": life, "msg": "m"}});
        assert!(matches!(check_state(v("DONE"), s), Ok(PollStatus::Done(_))));
        assert!(matches!(check_state(v("RUN"), s), Ok(PollStatus::Continue(ref m)) if m == "m"));
        let e = check_state(v("BAD"), s).unwrap_err();
        assert!(
            e.to_string().contains("failed to reach DONE, got BAD: m"),
            "{e}"
        );
        let no_msg = States {
            message_path: &[],
            ..s
        };
        assert!(
            matches!(check_state(v("RUN"), no_msg), Ok(PollStatus::Continue(ref m)) if m == "current status: RUN")
        );
    }

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
