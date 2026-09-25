# Changelog

All notable changes to this project will be documented in this file. The
format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- Milestone 1 runtime (`databricks-core`): unified config (env, `~/.databrickscfg`,
  host metadata), PAT and OAuth M2M auth with async token refresh, retrying
  rate-limited HTTP client, typed API errors, pagination streams, waiters.
- Spike services (`databricks-sdk`): `clusters.list`, `jobs.run_now` / `get_run`
  / wait, account `workspaces.list`.
