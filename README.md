# databricks-rs

A community-maintained, unofficial Rust SDK for the Databricks **Account** and **Workspace** REST APIs. It behaves like the official SDKs and uses the same configuration.

Every crate is published with a `community-` prefix (`community-databricks-sdk`, `community-databricks-core`, `community-databricks-sdk-<package>`). That makes clear these are not Databricks-published crates, and leaves the `databricks-*` names free, so Databricks could adopt the crates one at a time without a name collision.

> **Status: milestone 3.** The whole Account and Workspace surface is generated: 39 packages, 191 services, all 1,268 operations and 3,550 types, with parity against databricks-sdk-go tracked in [spec/PARITY.md](spec/PARITY.md). Nothing has been run against a live workspace yet. See [docs/milestone-2.md](docs/milestone-2.md).

Behaviour tracks **databricks-sdk-go v0.182.0**, released 2026-09-21. The same env vars and `~/.databrickscfg` profiles, the same auth precedence, the same retry rules, the same error codes and the same user-agent format apply in both SDKs.

## Crates

| Crate | What |
|---|---|
| `community-databricks-core` | Runtime: config resolution, auth (every Go auth type except `basic` and `metadata-service`), HTTP client (retries, rate limit, user agent), typed errors, pagination streams, LRO waiters. |
| `community-databricks-sdk` | `WorkspaceClient` / `AccountClient`. Each service package is a feature that re-exports its crate as `community_databricks_sdk::service::<package>`. `default` is `catalog`, `compute`, `jobs` and `provisioning`; `full` is everything. |
| `community-databricks-sdk-<package>` ×39 | Generated models and services, one crate per Go SDK package. `compute`, `jobs` and `catalog` are examples. Split into crates so that you only compile what you enable. |
| `xtask` | `cargo xtask codegen`: `spec/ir.json` → the generated crates and `spec/openapi/*.json`. |

## OpenAPI specs

[`spec/openapi/account.json`](spec/openapi/account.json) and [`spec/openapi/workspace.json`](spec/openapi/workspace.json) are OpenAPI 3.1 documents for the whole Account and Workspace APIs. They come from the same IR as the Rust code. `info.version` is the Go SDK release and `info.x-databricks-openapi-sha` is the upstream spec SHA. Pagination and long-running-operation waiters are kept as `x-databricks-*` extensions.

A weekly workflow regenerates everything when a new upstream spec ships. See [spec/README.md](spec/README.md) and [spec/DOCS-CHECK.md](spec/DOCS-CHECK.md).

## Quick start

```toml
[dependencies]
community-databricks-sdk = "0.1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
futures-util = "0.3"
```

```rust
use community_databricks_sdk::WorkspaceClient;
use community_databricks_sdk::service::compute::{ListClustersFilterBy, ListClustersRequest, State};
use community_databricks_sdk::service::jobs::RunNow;
use futures_util::TryStreamExt;

#[tokio::main]
async fn main() -> community_databricks_sdk::Result<()> {
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
let a = community_databricks_sdk::AccountClient::from_env().await?; // needs DATABRICKS_ACCOUNT_ID
for ws in a.workspaces().list().await? { println!("{:?} {:?}", ws.workspace_id, ws.workspace_name); }
```

Runnable examples are in `crates/community-databricks-sdk/examples/` (`list_clusters`, `run_job`, `list_workspaces`).

Request types are built with `Default` plus `with_<field>` setters. Types with one or two required fields also get `new(..)`. Every struct is `#[non_exhaustive]`, so new API fields are not breaking changes, and every enum has an `Unknown(String)` variant, so new server values are not breaking either.

## Configuration and auth

| Source | Notes |
|---|---|
| Code | `Config::with_host(..).token(..)`, `.client_credentials(..)`, `.account(..)`, or `set_attribute(name, value)` |
| Env | `DATABRICKS_HOST`, `DATABRICKS_TOKEN`, `DATABRICKS_CLIENT_ID`, `DATABRICKS_CLIENT_SECRET`, `DATABRICKS_ACCOUNT_ID`, `DATABRICKS_WORKSPACE_ID`, `DATABRICKS_GROUP_ID`, `DATABRICKS_CONFIG_PROFILE`, `DATABRICKS_CONFIG_FILE`, `DATABRICKS_AUTH_TYPE`, … (same names as Go) |
| `~/.databrickscfg` | Uses the requested profile, else `[__settings__] default_profile`, else `DEFAULT`. The file is skipped when a host or credentials are already set and no profile was requested. |
| Host metadata | Best-effort `/.well-known/databricks-config`. Fills in `account_id`, `workspace_id`, `cloud`, the host type (workspace, account or unified) and the OIDC discovery URL. |

Custom headers for every request go in `Config::header(name, value)`; they never override the headers the SDK sets itself.

Auth types implemented, in the order the default chain tries them:

| Auth type | Uses |
|---|---|
| `pat` | `token` |
| `oauth-m2m` | `client_id` + `client_secret` |
| `databricks-cli` | `databricks auth token` from the Databricks CLI (run `databricks auth login` first; interactive login lives in the CLI) |
| `github-oidc` | GitHub Actions ID token (`ACTIONS_ID_TOKEN_REQUEST_URL`/`_TOKEN`), exchanged for a Databricks token |
| `azure-devops-oidc` | Azure DevOps pipeline OIDC token (`SYSTEM_ACCESSTOKEN` and the `SYSTEM_*` job variables), exchanged for a Databricks token |
| `env-oidc` | ID token in `DATABRICKS_OIDC_TOKEN` (or the variable named by `oidc_token_env`) |
| `file-oidc` | ID token in the file at `databricks_id_token_filepath` |
| `mem-oidc` | ID token from `Config::id_tokens(..)`, an in-memory `IdTokenSource`; never written to a file or env var |
| `github-oidc-azure` | GitHub Actions ID token federated to an Entra ID app (`azure_client_id`, `azure_tenant_id`) |
| `azure-msi` | Azure managed identity (`azure_use_msi`, optional `azure_client_id`): App Service/Functions, VMs (IMDS) and AKS workload identity |
| `azure-client-secret` | Entra ID service principal: `azure_client_id`, `azure_client_secret`, `azure_tenant_id` |
| `azure-cli` | The Azure CLI login (`az login`, CLI 2.54 or later) |
| `oauth-m2m-gcp` | `oauth-m2m` plus a Google access token (`google_credentials` or `google_service_account`) in `X-Databricks-GCP-SA-Access-Token`; select it with `auth_type` |
| `google-credentials` | A Google credentials file (`google_credentials`, path or inline JSON): service account, impersonated service account, or external account (workload identity federation from a file, URL, executable or AWS) |
| `google-id` | Impersonate `google_service_account` with Application Default Credentials |

Azure tokens come from Microsoft's `azure_identity` crate and Google tokens from Google's `google-cloud-auth` crate; this crate adds only the Databricks parts (token resources and audiences, the `X-Databricks-*` headers, tenant discovery and resolving the host from `azure_workspace_resource_id`). `azure_identity` 1.0 does not yet support managed identity on Azure Arc, Azure ML, Cloud Shell or Service Fabric; it reports those clearly as unsupported.

For the OIDC types, `client_id` selects workload identity federation for that service principal; without it the exchange is account-wide token federation. Setting `group_id` makes the Databricks OAuth types (`oauth-m2m`, the OIDC types, `oauth-m2m-gcp`) assume that group's role; the others refuse to authenticate rather than give normal access. OAuth tokens are cached and refreshed in the background once they enter their refresh window, `min(TTL/2, 20 min)` before expiry, as in Go.

Go's `basic` and `metadata-service` are deliberately not ported (`auth::UNSUPPORTED_AUTH_TYPES`); asking for them gives a clear error.

## Errors

```rust
match w.jobs().get_run(req).await {
    Err(e) if e.is_missing() => { /* 404 / RESOURCE_DOES_NOT_EXIST / Go's overrides */ }
    Err(e) if e.is(community_databricks_sdk::ErrorKind::PermissionDenied) => { /* 403 */ }
    other => { /* ... */ }
}
```

`ErrorKind` has the same two-level hierarchy as Go's `apierr.Err*`. The details carry `ErrorInfo`, `RequestInfo`, `RetryInfo` and `Help`, and the raw list is kept as well.

## Retries, idempotency and unknown fields

- **Retries.** 429 / `REQUEST_LIMIT_EXCEEDED` and connection failures are retried for every call, honouring `Retry-After`. Timeouts and 5xx are retried only when the call is idempotent: `GET`/`PUT`/`PATCH`/`DELETE`/`HEAD`, or a `POST` that carries an idempotency key. `Config::retry_non_idempotent` opts back in to Go's retry-everything behaviour.
- **Idempotency keys.** Jobs `run_now`/`submit` (`idempotency_token`), and every call Go fills a `request_id` for, get a UUID v4 when the caller leaves the key empty. The same key is reused on each retry, so the server can deduplicate. `Call::idempotent()` marks a hand-built call the same way.
- **Unknown fields.** Every generated type has an `other` map ([ADR-0001](docs/adr/0001-generate-the-sdk-and-keep-unknown-fields.md)). Fields this SDK version doesn't model are kept on read and sent back on write, so get → modify → update never drops them. `with_other(name, value)` sends one before the SDK models it: it goes in the JSON body, or in the query string for `GET`/`DELETE` and for requests whose body is a single field (set unknown body fields on that field's own `other`).
- **Private link and redirects.** A redirect to the private-link login page becomes `PermissionDenied` (`PRIVATE_LINK_VALIDATION_ERROR`) with a cloud-specific hint. Any other unfollowed 3xx becomes `UNEXPECTED_REDIRECT` rather than a JSON parse error.
- **Workspace clients from an account.** `AccountClient::get_workspace_client(&workspace)` derives a `WorkspaceClient` sharing the connection pool and rate limiter; on a unified host it shares the credentials too.
- **Logging.** `Config::attribute` masks tokens and secrets as `***`; use `Config::secret_attribute` for the value itself.

## Binary bodies, long-running operations and Jobs

- **Files and exports.** Binary bodies are `core::http::Binary`: buffered bytes, or a stream.
  - File downloads, usage CSVs, metrics and OpenAPI exports come back as a stream once the status is known to be a success. Read them with `.bytes().await`, or chunk by chunk with `.into_stream()`.
  - Uploads take bytes (replayed on retry) or `Binary::from_stream(…)` (sent once and never retried).
- **Long-running operations.** Calls that start one (Lakebase `postgres`, app spaces, workspace base environments, ML backfills and purges) return a `LongRunning` handle, like Go's `…OperationInterface`.
  - `.wait().await` polls until done and returns the typed result. It uses Go's backoff (random, from 1s doubling to 60s) and has no timeout unless you add `.with_timeout(…)`.
  - `.metadata()`, `.done()`, `.name()` and, where the service supports it, `.cancel()` are also available.
  - A failed operation is an `Error::Api` carrying the operation's error code.
- **Jobs.** `jobs().get()` and `get_run()` follow task pages past 100 and merge them. `list()`/`list_runs()` with `expand_tasks` complete truncated entries, as Go does. The single-page calls are `get_page`, `get_run_page`, `list_page` and `list_runs_page`.
- **Parity.** [`spec/PARITY.md`](spec/PARITY.md) lists what the Go SDK offers beyond the generated API and how this SDK covers each part.

## Development

House rules:

- Clippy: `all` is deny and `pedantic` is warn.
- CI runs with `-D warnings`.
- Edition 2024, MSRV 1.94.
- Dual MIT/Apache-2.0 licence.
- Per-file coverage must stay at or above 85%.
- `cargo deny` and `cargo audit` run on every change and daily (`.github/workflows/security.yml`).

### Versions and releases

Each crate has its own version and is released on its own:
- a generated service crate is bumped only when its package changes;
- `community-databricks-sdk` and `community-databricks-core` follow their own changes.

`release-plz` opens a release PR after each merge to `main`. It bumps the changed crates and updates the matching entries in the root `[workspace.dependencies]`, which is how the crates depend on each other. It also writes each crate's changelog and tags it `<crate>-v<version>`. `cargo xtask codegen` keeps whatever version a crate already has, so regenerating never undoes a release.

crates.io rate-limits the creation of new crates. The first publication of each crate (the initial release, or a new service package from an upstream update) therefore goes through the **Publish new crates** workflow. It runs `scripts/publish_new_crates.py`, which:
- publishes in dependency order;
- skips versions already on crates.io;
- when crates.io answers "try again after …", sleeps until that time;
- re-dispatches itself before the six-hour job limit.

The release-plz publish step is skipped, with a warning, until every crate exists.

```sh
python3 scripts/publish_new_crates.py --check     # which crates are not on crates.io yet
python3 scripts/publish_new_crates.py --dry-run   # the publish order
```

```sh
cargo xtask codegen            # regenerate after changing spec/ir.json, codegen/overrides.json or xtask
cargo test --workspace --all-features
cargo clippy --workspace --all-targets
cargo deny check
cargo audit
cargo publish --workspace --dry-run --no-verify
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
