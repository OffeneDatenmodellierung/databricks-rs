# databricks-rs

An unofficial Rust SDK for the Databricks **Account** and **Workspace** REST APIs. It behaves like the official SDKs and uses the same configuration.

> **Status: milestone 2.** The whole Account and Workspace surface is generated: 39 packages, 191 services, 1,260 operations and 3,550 types. Nothing has been run against a live workspace yet. See [docs/milestone-2.md](docs/milestone-2.md).

Behaviour tracks **databricks-sdk-go v0.182.0**, released 2026-09-21. The same env vars and `~/.databrickscfg` profiles, the same auth precedence, the same retry rules, the same error codes and the same user-agent format apply in both SDKs.

## Crates

| Crate | What |
|---|---|
| `databricks-core` | Runtime: config resolution, auth (PAT, OAuth M2M), HTTP client (retries, rate limit, user agent), typed errors, pagination streams, LRO waiters. |
| `databricks-sdk` | `WorkspaceClient` / `AccountClient`. Each service package is a feature that re-exports its crate as `databricks_sdk::service::<package>`. `default` is `catalog`, `compute`, `jobs` and `provisioning`; `full` is everything. |
| `databricks-sdk-<package>` ×39 | Generated models and services, one crate per Go SDK package. `compute`, `jobs` and `catalog` are examples. Split into crates so that you only compile what you enable. |
| `xtask` | `cargo xtask codegen`: `spec/ir.json` → the generated crates and `spec/openapi/*.json`. |

## OpenAPI specs

[`spec/openapi/account.json`](spec/openapi/account.json) and [`spec/openapi/workspace.json`](spec/openapi/workspace.json) are OpenAPI 3.1 documents for the whole Account and Workspace APIs. They come from the same IR as the Rust code. `info.version` is the Go SDK release and `info.x-databricks-openapi-sha` is the upstream spec SHA. Pagination and long-running-operation waiters are kept as `x-databricks-*` extensions.

A weekly workflow regenerates everything when a new upstream spec ships. See [spec/README.md](spec/README.md) and [spec/DOCS-CHECK.md](spec/DOCS-CHECK.md).

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
    let req = ListClustersRequest::default()
        .with_filter_by(ListClustersFilterBy::default().with_cluster_states([State::Running]));
    let mut clusters = w.clusters().list(req);
    while let Some(c) = clusters.try_next().await? {
        println!("{:?} {:?}", c.cluster_id, c.state);
    }

    // Long-running operation with a waiter (default timeout 20 min, as in Go).
    let run = w.jobs().run_now(RunNow::new(123)).await?
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
for ws in a.workspaces().list().await? { println!("{:?} {:?}", ws.workspace_id, ws.workspace_name); }
```

Runnable examples are in `crates/databricks-sdk/examples/` (`list_clusters`, `run_job`, `list_workspaces`).

Request types are built with `Default` plus `with_<field>` setters. Types with one or two required fields also get `new(..)`. Every struct is `#[non_exhaustive]`, so new API fields are not breaking changes, and every enum has an `Unknown(String)` variant, so new server values are not breaking either.

## Configuration and auth

| Source | Notes |
|---|---|
| Code | `Config::with_host(..).token(..)`, `.client_credentials(..)`, `.account(..)`, or `set_attribute(name, value)` |
| Env | `DATABRICKS_HOST`, `DATABRICKS_TOKEN`, `DATABRICKS_CLIENT_ID`, `DATABRICKS_CLIENT_SECRET`, `DATABRICKS_ACCOUNT_ID`, `DATABRICKS_WORKSPACE_ID`, `DATABRICKS_GROUP_ID`, `DATABRICKS_CONFIG_PROFILE`, `DATABRICKS_CONFIG_FILE`, `DATABRICKS_AUTH_TYPE`, … (same names as Go) |
| `~/.databrickscfg` | Uses the requested profile, else `[__settings__] default_profile`, else `DEFAULT`. The file is skipped when a host or credentials are already set and no profile was requested. |
| Host metadata | Best-effort `/.well-known/databricks-config`. Fills in `account_id`, `workspace_id`, `cloud`, the host type (workspace, account or unified) and the OIDC discovery URL. |

Custom headers for every request go in `Config::header(name, value)`; they never override the headers the SDK sets itself.

Auth types implemented: `pat`, `oauth-m2m`. Setting `group_id` makes `oauth-m2m` assume that group's role, and makes `pat` fail rather than give normal access. OAuth tokens are cached and refreshed in the background once they enter their refresh window, `min(TTL/2, 20 min)` before expiry, as in Go.

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
cargo xtask codegen            # regenerate after changing spec/ir.json, codegen/overrides.json or xtask
cargo test --workspace --all-features
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
