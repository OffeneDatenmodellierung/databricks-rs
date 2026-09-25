//! List workspaces in an account (account-level client).
//!
//! ```sh
//! DATABRICKS_HOST=https://accounts.cloud.databricks.com \
//! DATABRICKS_ACCOUNT_ID=... DATABRICKS_CLIENT_ID=... DATABRICKS_CLIENT_SECRET=... \
//!   cargo run --example list_workspaces
//! ```

use databricks_sdk::AccountClient;

#[tokio::main]
async fn main() -> databricks_sdk::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let a = AccountClient::from_env().await?;
    for ws in a.workspaces().list().await? {
        println!(
            "{:<20} {:<14} {}",
            ws.workspace_id,
            ws.workspace_status
                .map(|s| s.to_string())
                .unwrap_or_default(),
            ws.workspace_name.unwrap_or_default()
        );
    }
    Ok(())
}
