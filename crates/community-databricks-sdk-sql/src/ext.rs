//! `StatementExecutionAPI.ExecuteAndWait` from Go's
//! `service/sql/ext_utilities.go`.

use std::time::Duration;

use community_databricks_core::wait::{self, PollStatus};
use community_databricks_core::{Error, Result};

use crate::{
    ExecuteStatementRequest, GetStatementRequest, StatementExecutionApi, StatementResponse,
    StatementState, StatementStatus,
};

/// How long [`StatementExecutionApi::execute_and_wait`] polls by default
/// (Go: 20 minutes).
pub const EXECUTE_AND_WAIT_TIMEOUT: Duration = Duration::from_mins(20);

impl StatementExecutionApi {
    /// Execute a statement and poll until it finishes (Go:
    /// `ExecuteAndWait`, 20-minute timeout).
    ///
    /// Returns the response once the statement has `SUCCEEDED`. A statement
    /// that ends `FAILED`, `CANCELED` or `CLOSED` is an
    /// [`Error::OperationFailed`] naming the state and the service error.
    pub async fn execute_and_wait(
        &self,
        request: ExecuteStatementRequest,
    ) -> Result<StatementResponse> {
        self.execute_and_wait_with_timeout(request, EXECUTE_AND_WAIT_TIMEOUT)
            .await
    }

    /// As [`execute_and_wait`](Self::execute_and_wait), with a timeout.
    pub async fn execute_and_wait_with_timeout(
        &self,
        request: ExecuteStatementRequest,
        timeout: Duration,
    ) -> Result<StatementResponse> {
        let first = self.execute_statement(request).await?;
        match outcome(first.status.as_ref())? {
            None => return Ok(first),
            Some(_) => {}
        }
        let id = first.statement_id.clone().unwrap_or_default();
        wait::poll(timeout, || {
            let id = id.clone();
            async move {
                let res = self.get_statement(GetStatementRequest::new(id)).await?;
                Ok(match outcome(res.status.as_ref())? {
                    None => PollStatus::Done(res),
                    Some(state) => PollStatus::Continue(state),
                })
            }
        })
        .await
    }
}

/// `Ok(None)` when succeeded, `Ok(Some(state))` while running, and an error
/// for a terminal failure.
fn outcome(status: Option<&StatementStatus>) -> Result<Option<String>> {
    let state = status.and_then(|s| s.state.clone());
    match state {
        Some(StatementState::Succeeded) => Ok(None),
        Some(s @ (StatementState::Failed | StatementState::Canceled | StatementState::Closed)) => {
            let mut msg = s.to_string();
            if let Some(e) = status.and_then(|s| s.error.as_ref()) {
                msg = format!(
                    "{msg}: {} {}",
                    e.error_code
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default(),
                    e.message.as_deref().unwrap_or_default()
                );
            }
            Err(Error::OperationFailed(msg))
        }
        other => Ok(Some(
            other.map_or_else(|| "UNKNOWN".to_owned(), |s| s.to_string()),
        )),
    }
}
