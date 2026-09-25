//! Jobs (Go: `service/jobs`): `run_now` with its long-running-operation
//! waiter, and `get_run` with Go's multi-page merge.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use databricks_core::http::Method;
use databricks_core::wait::{self, PollStatus};
use databricks_core::{ApiClient, Error, Result, open_enum};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Progress callback for run waiters.
pub type ProgressFn = Box<dyn FnMut(&Run) + Send>;

/// Default timeout for [`RunNowWaiter::wait`] (Go: 20 minutes).
pub const DEFAULT_RUN_TIMEOUT: Duration = Duration::from_mins(20);

/// Jobs API.
#[derive(Debug, Clone)]
pub struct JobsApi {
    api: ApiClient,
}

impl JobsApi {
    pub(crate) fn new(api: ApiClient) -> Self {
        Self { api }
    }

    /// Trigger a run of an existing job. The returned waiter carries the
    /// immediate response; call [`RunNowWaiter::wait`] to block until the
    /// run is `TERMINATED` or `SKIPPED`.
    ///
    /// `POST /api/2.2/jobs/run-now`
    pub async fn run_now(&self, request: RunNow) -> Result<RunNowWaiter> {
        let response: RunNowResponse = self
            .api
            .json(Method::POST, "/api/2.2/jobs/run-now", &request)
            .await?;
        Ok(RunNowWaiter {
            jobs: self.clone(),
            run_id: response.run_id,
            response,
            timeout: DEFAULT_RUN_TIMEOUT,
            on_progress: None,
        })
    }

    /// Metadata for a run. Like Go's `JobsAPI.GetRun`, follows
    /// `next_page_token` and merges tasks (or for-each iterations),
    /// job clusters, job parameters and repair history.
    ///
    /// `GET /api/2.2/jobs/runs/get`
    pub async fn get_run(&self, mut request: GetRunRequest) -> Result<Run> {
        let mut run: Run = self.get_run_page(&request).await?;
        let iterations = !run.iterations.is_empty();
        while let Some(token) = run.next_page_token.take().filter(|t| !t.is_empty()) {
            request.page_token = Some(token);
            let next = self.get_run_page(&request).await?;
            if iterations {
                run.iterations.extend(next.iterations);
            } else {
                run.tasks.extend(next.tasks);
            }
            run.job_clusters.extend(next.job_clusters);
            run.job_parameters.extend(next.job_parameters);
            run.repair_history.extend(next.repair_history);
            run.next_page_token = next.next_page_token;
        }
        Ok(run)
    }

    async fn get_run_page(&self, request: &GetRunRequest) -> Result<Run> {
        self.api
            .query(Method::GET, "/api/2.2/jobs/runs/get", request)
            .await
    }

    /// Poll `runs/get` until the run is `TERMINATED` or `SKIPPED`.
    /// `INTERNAL_ERROR` halts with [`Error::OperationFailed`].
    pub async fn wait_get_run_job_terminated_or_skipped(
        &self,
        run_id: i64,
        timeout: Duration,
        on_progress: Option<ProgressFn>,
    ) -> Result<Run> {
        let callback = Mutex::new(on_progress);
        let callback = &callback;
        wait::poll(timeout, || {
            let fut = self.get_run(GetRunRequest::builder().run_id(run_id).build());
            async move {
                let run = fut.await?;
                if let Some(cb) = callback
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .as_mut()
                {
                    cb(&run);
                }
                let state = run.state.as_ref();
                let life = state.and_then(|s| s.life_cycle_state.clone());
                let message = state
                    .and_then(|s| s.state_message.clone())
                    .unwrap_or_else(|| format!("current status: {}", display(life.as_ref())));
                match life {
                    Some(RunLifeCycleState::Terminated | RunLifeCycleState::Skipped) => {
                        Ok(PollStatus::Done(run))
                    }
                    Some(RunLifeCycleState::InternalError) => Err(Error::OperationFailed(format!(
                        "failed to reach TERMINATED or SKIPPED, got INTERNAL_ERROR: {message}"
                    ))),
                    _ => Ok(PollStatus::Continue(message)),
                }
            }
        })
        .await
    }
}

fn display(v: Option<&RunLifeCycleState>) -> &str {
    v.map_or("", RunLifeCycleState::as_str)
}

/// Returned by [`JobsApi::run_now`]; Go's
/// `WaitGetRunJobTerminatedOrSkipped[RunNowResponse]`.
pub struct RunNowWaiter {
    jobs: JobsApi,
    /// The run that was started.
    pub run_id: i64,
    /// The immediate `run-now` response.
    pub response: RunNowResponse,
    timeout: Duration,
    on_progress: Option<ProgressFn>,
}

impl fmt::Debug for RunNowWaiter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunNowWaiter")
            .field("run_id", &self.run_id)
            .field("response", &self.response)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl RunNowWaiter {
    /// Override the default 20-minute timeout.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Called with the run on every poll.
    #[must_use]
    pub fn on_progress(mut self, f: impl FnMut(&Run) + Send + 'static) -> Self {
        self.on_progress = Some(Box::new(f));
        self
    }

    /// Wait for the run to reach `TERMINATED` or `SKIPPED`.
    pub async fn wait(self) -> Result<Run> {
        self.jobs
            .wait_get_run_job_terminated_or_skipped(self.run_id, self.timeout, self.on_progress)
            .await
    }
}

/// Request body for [`JobsApi::run_now`].
#[derive(Debug, Clone, Serialize, bon::Builder)]
#[builder(on(String, into))]
#[non_exhaustive]
pub struct RunNow {
    /// The job to run.
    pub job_id: i64,
    /// Job-level parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_parameters: Option<BTreeMap<String, String>>,
    /// Guarantees idempotency of the request (max 64 chars).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_token: Option<String>,
    /// Task keys to run (subset of the job).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub only: Option<Vec<String>>,
    /// Serverless performance target.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub performance_target: Option<PerformanceTarget>,
    /// Pipeline-task parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pipeline_params: Option<PipelineParams>,
    /// Queueing behaviour.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue: Option<QueueSettings>,
    /// Deprecated per-task parameters (use `job_parameters`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notebook_params: Option<BTreeMap<String, String>>,
    /// Deprecated per-task parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub python_params: Option<Vec<String>>,
    /// Deprecated per-task parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub python_named_params: Option<BTreeMap<String, String>>,
    /// Deprecated per-task parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jar_params: Option<Vec<String>>,
    /// Deprecated per-task parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spark_submit_params: Option<Vec<String>>,
    /// Deprecated per-task parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sql_params: Option<BTreeMap<String, String>>,
    /// Deprecated per-task parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dbt_commands: Option<Vec<String>>,
}

/// Pipeline-task run parameters.
#[derive(Debug, Clone, Default, Serialize, Deserialize, bon::Builder)]
#[non_exhaustive]
pub struct PipelineParams {
    /// Full refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_refresh: Option<bool>,
}

/// Queue settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct QueueSettings {
    /// Whether to queue the run if it can't start immediately.
    pub enabled: bool,
}

impl QueueSettings {
    /// `{"enabled": enabled}`.
    #[must_use]
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }
}

/// Response from `run-now`.
#[derive(Debug, Clone, Default, Deserialize)]
#[non_exhaustive]
pub struct RunNowResponse {
    /// Globally unique run ID.
    #[serde(default)]
    pub run_id: i64,
    /// Sequence number within the job.
    #[serde(default)]
    pub number_in_job: Option<i64>,
}

/// Query for [`JobsApi::get_run`].
#[derive(Debug, Clone, Serialize, bon::Builder)]
#[builder(on(String, into))]
#[non_exhaustive]
pub struct GetRunRequest {
    /// Run ID.
    pub run_id: i64,
    /// Include repair history.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_history: Option<bool>,
    /// Include resolved parameter values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_resolved_values: Option<bool>,
    /// Page token (managed by [`JobsApi::get_run`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,
}

/// A job run. Nested collections are untyped (`Value`) in the spike; the
/// generator will emit `RunTask`, `JobCluster`, etc.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Run {
    /// Run ID.
    #[serde(default)]
    pub run_id: i64,
    /// Job ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<i64>,
    /// Run name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_name: Option<String>,
    /// Sequence number within the job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number_in_job: Option<i64>,
    /// Creator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator_user_name: Option<String>,
    /// Run state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<RunState>,
    /// Start time (epoch ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<i64>,
    /// End time (epoch ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_time: Option<i64>,
    /// Total duration (ms), for multi-task runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_duration: Option<i64>,
    /// Link to the run in the UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_page_url: Option<String>,
    /// What triggered the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    /// Tasks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<Value>,
    /// For-each iterations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub iterations: Vec<Value>,
    /// Job clusters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub job_clusters: Vec<Value>,
    /// Job parameters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub job_parameters: Vec<Value>,
    /// Repair history.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repair_history: Vec<Value>,
    /// More pages of the above exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_page_token: Option<String>,
    /// Fields not yet modelled.
    #[serde(flatten)]
    pub other: BTreeMap<String, Value>,
}

/// Run state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RunState {
    /// Where the run is in its lifecycle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub life_cycle_state: Option<RunLifeCycleState>,
    /// Outcome, once terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_state: Option<RunResultState>,
    /// Human-readable detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_message: Option<String>,
    /// Why the run is queued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_reason: Option<String>,
    /// Cancelled by a user or timed out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_cancelled_or_timedout: Option<bool>,
}

open_enum! {
    /// Run lifecycle state.
    pub enum RunLifeCycleState {
        /// `BLOCKED`
        Blocked => "BLOCKED",
        /// `INTERNAL_ERROR`
        InternalError => "INTERNAL_ERROR",
        /// `PENDING`
        Pending => "PENDING",
        /// `QUEUED`
        Queued => "QUEUED",
        /// `RUNNING`
        Running => "RUNNING",
        /// `SKIPPED`
        Skipped => "SKIPPED",
        /// `TERMINATED`
        Terminated => "TERMINATED",
        /// `TERMINATING`
        Terminating => "TERMINATING",
        /// `WAITING_FOR_RETRY`
        WaitingForRetry => "WAITING_FOR_RETRY",
    }
}

open_enum! {
    /// Run result.
    pub enum RunResultState {
        /// `CANCELED`
        Canceled => "CANCELED",
        /// `DISABLED`
        Disabled => "DISABLED",
        /// `EXCLUDED`
        Excluded => "EXCLUDED",
        /// `FAILED`
        Failed => "FAILED",
        /// `MAXIMUM_CONCURRENT_RUNS_REACHED`
        MaximumConcurrentRunsReached => "MAXIMUM_CONCURRENT_RUNS_REACHED",
        /// `SUCCESS`
        Success => "SUCCESS",
        /// `SUCCESS_WITH_FAILURES`
        SuccessWithFailures => "SUCCESS_WITH_FAILURES",
        /// `TIMEDOUT`
        Timedout => "TIMEDOUT",
        /// `UPSTREAM_CANCELED`
        UpstreamCanceled => "UPSTREAM_CANCELED",
        /// `UPSTREAM_FAILED`
        UpstreamFailed => "UPSTREAM_FAILED",
    }
}

open_enum! {
    /// Serverless performance target.
    pub enum PerformanceTarget {
        /// `PERFORMANCE_OPTIMIZED`
        PerformanceOptimized => "PERFORMANCE_OPTIMIZED",
        /// `STANDARD`
        Standard => "STANDARD",
    }
}
