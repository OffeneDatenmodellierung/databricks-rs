# Changelog

All notable changes to this project will be documented in this file. The
format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added (milestone 3: Go parity)
- Binary request and response bodies (`core::http::Binary`,
  `ApiClient::send_binary`): Files download/upload, BillableUsage download,
  Genie visualization download, and Serving `export_metrics`,
  `get_open_api` and `http_request` are generated. Every extracted
  operation is now generated (1,268).
- Long-running operations: 37 calls return `core::lro::LongRunning`
  handles (`wait`, `metadata`, `done`, `cancel`), as Go's
  `…OperationInterface` wrappers do.
- Jobs `get`, `list` and `list_runs` follow task pages, and `expand_tasks`
  completes truncated entries, as in Go; the single-page calls are
  `get_page`, `list_page` and `list_runs_page`.
- `Workspace::azure_resource_id`.
- `codegen/ir_patches.json` for upstream spec defects; it fixes the
  `sql` `TransferOwnership` path, which Go builds from a struct.
- `scripts/check_parity.py`, `codegen/parity.toml` and `spec/PARITY.md`:
  a CI check of surface parity with databricks-sdk-go. The remaining
  gaps are tracked in #14–#17.

### Changed
- `postgres`, `apps`, `environments` and `ml` operation-returning calls
  return `LongRunning<Operation, T, M>` instead of `Operation`; use
  `.operation()` or `.into_operation()` for the raw message.
- `jobs().get()` now returns the merged job; the old single-page call is
  `get_page()`.

### Changed
- All crates are renamed with a `community-` prefix (`community-databricks-core`,
  `community-databricks-sdk`, `community-databricks-sdk-<package>`); import
  paths become `community_databricks_sdk::…`. This makes clear that the
  crates are community-maintained, and keeps the `databricks-*` names free
  for Databricks.
- Each crate has its own version and is released independently.

### Added
- Idempotency-safe retries (#3): 5xx and timeouts are retried only for
  idempotent calls; `Config::retry_non_idempotent` restores Go's behaviour.
  Idempotency keys (`idempotency_token`, `request_id`) are generated when
  empty and reused across retries.
- Private-link validation errors and unfollowed redirects become typed API
  errors (#5).
- `AccountClient::get_workspace_client` and `ApiClient::for_workspace` (#4).
- Hygiene (#6): `Config::attribute` masks secrets (`secret_attribute` for
  the raw value); host-metadata lookups share one client and retry; a
  warning is logged when TLS verification is off.
- Unknown-fields catch-all `other` / `with_other` on every generated type
  (#11, [ADR-0001](docs/adr/0001-generate-the-sdk-and-keep-unknown-fields.md)).
- The generator carries Go's request preambles: SCIM lists start at
  `startIndex=1` with `count=10000`, legacy SQL lists start at page 1,
  Unity Catalog and Delta Sharing lists send `max_results=0` so the server
  paginates, and Go's generated `request_id`s are filled in.
- `tests/contracts.rs`: wiremock contract tests for the endpoints `dbk_tool`
  uses (#7–#10, #12).
- `security.yml`: `cargo deny` and `cargo audit` on every change and daily.
- `publish-new-crates.yml` + `scripts/publish_new_crates.py`: the first
  publication of each crate, in dependency order, waiting out crates.io's
  new-crate rate limit.
- CI checks that every crate packages for crates.io.

### Added
- Milestone 2: code generation. `codegen/extract-go` extracts `spec/ir.json`
  from databricks-sdk-go v0.182.0; `cargo xtask codegen` emits one
  `community-databricks-sdk-<package>` crate per package (39) covering 1,260
  operations, plus OpenAPI 3.1 documents `spec/openapi/{account,workspace}.json`.
- Weekly upstream-spec workflow and `scripts/check_upstream.py`.
- Fixes from a review of databricks-sdk-go issues and PRs (`docs/upstream-review.md`):
  - Integer fields accept numeric strings (Go #1808), and float fields accept
    `"NaN"`/`"Infinity"` (Go #1498), via `community_databricks_core::serde_num`.
  - Path parameters are escaped by segment type: `/` becomes `%2F` in
    single-segment values, and resource names keep `/` (Go #1811, #1765).
  - `Config::headers` / `Config::header` for custom request headers (Go #1846).
  - `Config::group_id` / `DATABRICKS_GROUP_ID` group role assumption for
    OAuth M2M; PAT refuses to authenticate while it is set (Go #1812, #1817).
  - New auth types, ported with the open upstream changes:
    `databricks-cli` (#1832), `github-oidc`/`env-oidc`/`file-oidc` and the
    Rust-only in-memory `mem-oidc` (#1790), `azure-msi` with Azure Identity
    endpoint selection (#1813), and `oauth-m2m-gcp` (#1815).
  - The rest of Go's chain except `basic` and `metadata-service`:
    `azure-devops-oidc`, `github-oidc-azure`, `azure-client-secret`,
    `azure-cli`, `google-credentials` and `google-id`. Azure and Google
    tokens come from `azure_identity` and `google-cloud-auth`.
  - Decode errors include a snippet of the response body, and bare
    `NaN`/`Infinity` tokens in responses decode as `null`.

### Changed
- Request types use `Default` + `with_*` setters (and `new(..)` for one or
  two required fields) instead of `bon` builders.
- `community-databricks-sdk` service modules are re-exports of the generated crates.

- Milestone 1 runtime (`community-databricks-core`): unified config (env, `~/.databrickscfg`,
  host metadata), PAT and OAuth M2M auth with async token refresh, retrying
  rate-limited HTTP client, typed API errors, pagination streams, waiters.
- Spike services (`community-databricks-sdk`): `clusters.list`, `jobs.run_now` / `get_run`
  / wait, account `workspaces.list`.
