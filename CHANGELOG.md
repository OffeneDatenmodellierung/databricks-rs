# Changelog

All notable changes to this project will be documented in this file. The
format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- Milestone 2: code generation. `codegen/extract-go` extracts `spec/ir.json`
  from databricks-sdk-go v0.182.0; `cargo xtask codegen` emits one
  `databricks-sdk-<package>` crate per package (39) covering 1,260
  operations, plus OpenAPI 3.1 documents `spec/openapi/{account,workspace}.json`.
- Weekly upstream-spec workflow and `scripts/check_upstream.py`.
- Fixes from a review of databricks-sdk-go issues and PRs (`docs/upstream-review.md`):
  - Integer fields accept numeric strings (Go #1808), and float fields accept
    `"NaN"`/`"Infinity"` (Go #1498), via `databricks_core::serde_num`.
  - Path parameters are escaped by segment type: `/` becomes `%2F` in
    single-segment values, and resource names keep `/` (Go #1811, #1765).
  - `Config::headers` / `Config::header` for custom request headers (Go #1846).
  - `Config::group_id` / `DATABRICKS_GROUP_ID` group role assumption for
    OAuth M2M; PAT refuses to authenticate while it is set (Go #1812, #1817).
  - New auth types, ported with the open upstream changes:
    `databricks-cli` (#1832), `github-oidc`/`env-oidc`/`file-oidc` and the
    Rust-only in-memory `mem-oidc` (#1790), `azure-msi` with Azure Identity
    endpoint selection (#1813), and `oauth-m2m-gcp` (#1815).
  - Decode errors include a snippet of the response body, and bare
    `NaN`/`Infinity` tokens in responses decode as `null`.

### Changed
- Request types use `Default` + `with_*` setters (and `new(..)` for one or
  two required fields) instead of `bon` builders.
- `databricks-sdk` service modules are re-exports of the generated crates.

- Milestone 1 runtime (`databricks-core`): unified config (env, `~/.databrickscfg`,
  host metadata), PAT and OAuth M2M auth with async token refresh, retrying
  rate-limited HTTP client, typed API errors, pagination streams, waiters.
- Spike services (`databricks-sdk`): `clusters.list`, `jobs.run_now` / `get_run`
  / wait, account `workspaces.list`.
