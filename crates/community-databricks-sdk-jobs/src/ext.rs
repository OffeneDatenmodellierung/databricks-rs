//! `JobsAPI.Get`, `GetRun`, `List` and `ListRuns` from Go's
//! `service/jobs/ext_api.go`.
//!
//! Jobs and runs with more than 100 tasks (or for-each iterations) come back
//! in pages. These wrappers follow the pages and merge them, as Go does. The
//! generated single-page calls stay available as `get_page`, `get_run_page`,
//! `list_page` and `list_runs_page`.

use community_databricks_core::Result;
use community_databricks_core::paging::{self, Paged};

use crate::{
    BaseJob, BaseRun, GetJobRequest, GetRunRequest, Job, JobsApi, ListJobsRequest, ListRunsRequest,
    Run,
};

impl JobsApi {
    /// Retrieves the details for a single job.
    ///
    /// Large jobs are paginated. Like the Go SDK, this follows
    /// `next_page_token` and merges the settings' `tasks`, `job_clusters`,
    /// `parameters` and `environments` into one [`Job`]. Use
    /// [`get_page`](Self::get_page) for a single page.
    ///
    /// `GET /api/2.2/jobs/get`
    pub async fn get(&self, mut request: GetJobRequest) -> Result<Job> {
        let mut job = self.get_page(request.clone()).await?;
        while let Some(token) = job.next_page_token.take().filter(|t| !t.is_empty()) {
            request.page_token = Some(token);
            let next = self.get_page(request.clone()).await?;
            if let Some(more) = next.settings {
                let settings = job.settings.get_or_insert_with(Default::default);
                settings.tasks.extend(more.tasks);
                settings.job_clusters.extend(more.job_clusters);
                settings.parameters.extend(more.parameters);
                settings.environments.extend(more.environments);
            }
            job.next_page_token = next.next_page_token;
        }
        Ok(job)
    }

    /// Retrieves the metadata of a run.
    ///
    /// Large runs are paginated. Like the Go SDK, this follows
    /// `next_page_token` and merges `tasks` (or, for a for-each task run,
    /// `iterations`), `job_clusters`, `job_parameters` and `repair_history`
    /// into one [`Run`]. Use [`get_run_page`](Self::get_run_page) for a
    /// single page.
    ///
    /// `GET /api/2.2/jobs/runs/get`
    pub async fn get_run(&self, mut request: GetRunRequest) -> Result<Run> {
        let mut run = self.get_run_page(request.clone()).await?;
        let iterations = !run.iterations.is_empty();
        while let Some(token) = run.next_page_token.take().filter(|t| !t.is_empty()) {
            request.page_token = Some(token);
            let next = self.get_run_page(request.clone()).await?;
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

    /// Retrieves a list of jobs.
    ///
    /// With `expand_tasks`, a job whose arrays were truncated (`has_more`)
    /// is completed with [`get`](Self::get): its settings' `tasks`,
    /// `job_clusters`, `parameters` and `environments` are replaced by the
    /// full lists, and `has_more` is cleared, as in Go.
    ///
    /// `GET /api/2.2/jobs/list`
    ///
    /// Returns a lazily paginated stream.
    #[must_use]
    pub fn list(&self, request: ListJobsRequest) -> Paged<'static, BaseJob> {
        let expand = request.expand_tasks.unwrap_or(false);
        let this = Clone::clone(self);
        let jobs = paging::paginate(
            request,
            move |req: &ListJobsRequest| {
                let this = Clone::clone(&this);
                let req = req.clone();
                async move { this.list_page(req).await }
            },
            |req, resp| {
                let more = paging::next_token(resp.next_page_token, |t| req.page_token = Some(t));
                (resp.jobs, more)
            },
        );
        if !expand {
            return jobs;
        }
        let this = Clone::clone(self);
        paging::then_each(jobs, move |job| {
            let this = Clone::clone(&this);
            async move { this.expand_job(job).await }
        })
    }

    /// Every page of [`list`](Self::list), collected.
    pub async fn list_all(&self, request: ListJobsRequest) -> Result<Vec<BaseJob>> {
        paging::collect(self.list(request)).await
    }

    /// List runs in descending order by start time.
    ///
    /// With `expand_tasks`, a run whose arrays were truncated (`has_more`)
    /// is completed with [`get_run`](Self::get_run): its `tasks`,
    /// `job_clusters`, `job_parameters` and `repair_history` are replaced by
    /// the full lists, and `has_more` is cleared, as in Go.
    ///
    /// `GET /api/2.2/jobs/runs/list`
    ///
    /// Returns a lazily paginated stream.
    #[must_use]
    pub fn list_runs(&self, request: ListRunsRequest) -> Paged<'static, BaseRun> {
        let expand = request.expand_tasks.unwrap_or(false);
        let this = Clone::clone(self);
        let runs = paging::paginate(
            request,
            move |req: &ListRunsRequest| {
                let this = Clone::clone(&this);
                let req = req.clone();
                async move { this.list_runs_page(req).await }
            },
            |req, resp| {
                let more = paging::next_token(resp.next_page_token, |t| req.page_token = Some(t));
                (resp.runs, more)
            },
        );
        if !expand {
            return runs;
        }
        let this = Clone::clone(self);
        paging::then_each(runs, move |run| {
            let this = Clone::clone(&this);
            async move { this.expand_run(run).await }
        })
    }

    /// Every page of [`list_runs`](Self::list_runs), collected.
    pub async fn list_runs_all(&self, request: ListRunsRequest) -> Result<Vec<BaseRun>> {
        paging::collect(self.list_runs(request)).await
    }

    async fn expand_job(&self, mut job: BaseJob) -> Result<BaseJob> {
        if !job.has_more.unwrap_or(false) {
            return Ok(job);
        }
        let Some(job_id) = job.job_id else {
            return Ok(job);
        };
        let full = self.get(GetJobRequest::new(job_id)).await?;
        if let Some(src) = full.settings {
            let dst = job.settings.get_or_insert_with(Default::default);
            dst.tasks = src.tasks;
            dst.job_clusters = src.job_clusters;
            dst.parameters = src.parameters;
            dst.environments = src.environments;
        }
        job.has_more = Some(false);
        Ok(job)
    }

    async fn expand_run(&self, mut run: BaseRun) -> Result<BaseRun> {
        if !run.has_more.unwrap_or(false) {
            return Ok(run);
        }
        let Some(run_id) = run.run_id else {
            return Ok(run);
        };
        let full = self.get_run(GetRunRequest::new(run_id)).await?;
        run.tasks = full.tasks;
        run.job_clusters = full.job_clusters;
        run.job_parameters = full.job_parameters;
        run.repair_history = full.repair_history;
        run.has_more = Some(false);
        Ok(run)
    }
}
