# databricks-rs

An unofficial Rust SDK for the Databricks **Account** and **Workspace** REST APIs. It behaves like the official SDKs and uses the same configuration.

> **Status: milestone 1 (spike).** The runtime is complete enough to build on. The service surface is deliberately tiny: `clusters.list`, `jobs.run_now` and `jobs.get_run` on the workspace side, and `workspaces.list` on the account side. These are hand-written so the generated code has a fixed target shape. See [docs/milestone-1.md](docs/milestone-1.md).

Behaviour tracks **databricks-sdk-go v0.182.0**, released 2026-09-21. The same env vars and `~/.databrickscfg` profiles, the same auth precedence, the same retry rules, the same error codes and the same user-agent format apply in both SDKs.

## Crates

| Crate | What |
|---|---|
| `databricks-core` | Runtime: config resolution, auth (PAT, OAuth M2M), HTTP client (retries, rate limit, user agent), typed errors, pagination streams, LRO waiters. |
| `databricks-sdk` | `WorkspaceClient` / `AccountClient` and the service modules. From milestone 2 the service modules are generated, with one feature flag per service. |

## Quick start

```toml
[dependencies]
databricks-sdk = "0.1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
futures-util = "0.3"
```

```rust
use databricks_sdk::WorkspaceClient;
use databricks_sdk::service::compute::{ListClustersFilterBy, ListClustersRequest, State};
use databricks_sdk::service::jobs::RunNow;
use futures_util::TryStreamExt;

#[tokio::main]
async fn main() -> databricks_sdk::Result<()> {
    // DATABRICKS_HOST + DATABRICKS_TOKEN, or DATABRICKS_CLIENT_ID/SECRET,
    // or a ~/.databrickscfg profile (DATABRICKS_CONFIG_PROFILE).
    let w = WorkspaceClient::from_env().await?;

    // Lazily paginated stream.
    let req = ListClustersRequest::builder()
        .filter_by(ListClustersFilterBy::builder().cluster_states(vec![State::Running]).build())
        .build();
    let mut clusters = w.clusters().list(req);
    while let Some(c) = clusters.try_next().await? {
        println!("{:?} {:?}", c.cluster_id, c.state);
    }

    // Long-running operation with a waiter (default timeout 20 min, as in Go).
    let run = w.jobs().run_now(RunNow::builder().job_id(123).build()).await?
        .on_progress(|r| println!("{:?}", r.state))
        .wait()
        .await?;
    println!("{:?}", run.state);
    Ok(())
}
```

For account-level APIs:

```rust
let a = databricks_sdk::AccountClient::from_env().await?; // needs DATABRICKS_ACCOUNT_ID
for ws in a.workspaces().list().await? { println!("{} {:?}", ws.workspace_id, ws.workspace_name); }
```

Runnable examples are in `crates/databricks-sdk/examples/` (`list_clusters`, `run_job`, `list_workspaces`).

## Configuration and auth

| Source | Notes |
|---|---|
| Code | `Config::with_host(..).token(..)`, `.client_credentials(..)`, `.account(..)`, or `set_attribute(name, value)` |
| Env | `DATABRICKS_HOST`, `DATABRICKS_TOKEN`, `DATABRICKS_CLIENT_ID`, `DATABRICKS_CLIENT_SECRET`, `DATABRICKS_ACCOUNT_ID`, `DATABRICKS_WORKSPACE_ID`, `DATABRICKS_CONFIG_PROFILE`, `DATABRICKS_CONFIG_FILE`, `DATABRICKS_AUTH_TYPE`, … (same names as Go) |
| `~/.databrickscfg` | Uses the requested profile, else `[__settings__] default_profile`, else `DEFAULT`. The file is skipped when a host or credentials are already set and no profile was requested. |
| Host metadata | Best-effort `/.well-known/databricks-config`. Fills in `account_id`, `workspace_id`, `cloud`, the host type (workspace, account or unified) and the OIDC discovery URL. |

Auth types implemented: `pat`, `oauth-m2m`. OAuth tokens are cached and refreshed in the background once they enter their refresh window, `min(TTL/2, 20 min)` before expiry, as in Go.

The rest of Go's chain is planned and listed in `auth::PLANNED_AUTH_TYPES`: basic, U2M/CLI, metadata-service, the OIDC/WIF variants, Azure and GCP. Setting `DATABRICKS_AUTH_TYPE` to one of those gives a clear "not implemented in Rust yet" error.

## Errors

```rust
match w.jobs().get_run(req).await {
    Err(e) if e.is_missing() => { /* 404 / RESOURCE_DOES_NOT_EXIST / Go's overrides */ }
    Err(e) if e.is(databricks_sdk::ErrorKind::PermissionDenied) => { /* 403 */ }
    other => { /* ... */ }
}
```

`ErrorKind` has the same two-level hierarchy as Go's `apierr.Err*`. The details carry `ErrorInfo`, `RequestInfo`, `RetryInfo` and `Help`, and the raw list is kept as well.

## Development

House rules:

- Clippy: `all` is deny and `pedantic` is warn.
- CI runs with `-D warnings`.
- Edition 2024, MSRV 1.94.
- Dual MIT/Apache-2.0 licence.
- Per-file coverage must stay at or above 85%.
- Releases are cut by `release-plz`.

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo deny check
cargo tarpaulin --workspace --out Json --output-dir target/coverage \
  --exclude-files 'crates/*/examples/*' --exclude-files 'crates/*/build.rs' --exclude-files 'crates/*/tests/*'
python3 scripts/coverage_gate.py target/coverage/tarpaulin-report.json
```

### Dependency policy

Every dependency uses its **latest release, except releases published in the last 48 hours**. This applies to both direct and transitive crates.

```sh
python3 scripts/check_dep_age.py latest reqwest tokio   # what to put in Cargo.toml
python3 scripts/check_dep_age.py check                  # verify Cargo.lock (CI runs this on PRs)
```

## Licence

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.

This project is not affiliated with Databricks. The API behaviour it replicates comes from the Apache-2.0-licensed [databricks-sdk-go](https://github.com/databricks/databricks-sdk-go).
