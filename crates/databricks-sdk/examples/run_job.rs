//! Trigger a job and wait for it to finish.
//!
//! ```sh
//! cargo run --example run_job -- <job_id>
//! ```

use std::time::Duration;

use databricks_sdk::WorkspaceClient;
use databricks_sdk::service::jobs::RunNow;

#[tokio::main]
async fn main() -> databricks_sdk::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let job_id: i64 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .expect("usage: run_job <job_id>");

    let w = WorkspaceClient::from_env().await?;
    let waiter = w.jobs().run_now(RunNow::new(job_id)).await?;
    println!("started run {}", waiter.run_id);

    let run = waiter
        .timeout(Duration::from_mins(30))
        .on_progress(|r| {
            let s = r.state.as_ref();
            println!(
                "  {:?}: {}",
                s.and_then(|s| s.life_cycle_state.clone()),
                s.and_then(|s| s.state_message.clone()).unwrap_or_default()
            );
        })
        .wait()
        .await?;

    println!(
        "finished: {:?} {}",
        run.state.and_then(|s| s.result_state),
        run.run_page_url.unwrap_or_default()
    );
    Ok(())
}
