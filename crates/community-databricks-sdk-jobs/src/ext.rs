//! `JobsAPI.GetRun` from Go's `service/jobs/ext_api.go`.

use crate::{GetRunRequest, JobsApi, Run};

impl JobsApi {
    /// Retrieves the metadata of a run.
    ///
    /// Large runs are paginated. Like the Go SDK, this follows
    /// `next_page_token` and merges `tasks` (or, for a for-each task run,
    /// `iterations`), `job_clusters`, `job_parameters` and `repair_history`
    /// into one [`Run`]. Use [`get_run_page`](Self::get_run_page) for a
    /// single page.
    ///
    /// `GET /api/2.2/jobs/runs/get`
    pub async fn get_run(
        &self,
        mut request: GetRunRequest,
    ) -> community_databricks_core::Result<Run> {
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
}
