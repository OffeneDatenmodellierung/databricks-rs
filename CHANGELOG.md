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

### Changed
- Request types use `Default` + `with_*` setters (and `new(..)` for one or
  two required fields) instead of `bon` builders.
- `databricks-sdk` service modules are re-exports of the generated crates.

- Milestone 1 runtime (`databricks-core`): unified config (env, `~/.databrickscfg`,
  host metadata), PAT and OAuth M2M auth with async token refresh, retrying
  rate-limited HTTP client, typed API errors, pagination streams, waiters.
- Spike services (`databricks-sdk`): `clusters.list`, `jobs.run_now` / `get_run`
  / wait, account `workspaces.list`.
