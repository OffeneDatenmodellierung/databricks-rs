# Generated surface

Produced by `cargo xtask codegen` from `spec/ir.json`. DO NOT EDIT.

Source: databricks-sdk-go v0.184.0 (OpenAPI `fbf1beb6c3c730036b45888219658151047732a8`).

- 40 packages, 192 services, 1303 operations (1303 generated), 3616 types.

## Not generated (binary or streaming payloads)


## OpenAPI path collisions (moved to `x-databricks-shared-path-operations`)

- workspace: GET /api/environments/v1/{name} (Environments.GetOperation)
- workspace: PATCH /api/environments/v1/{name} (Environments.UpdateWorkspaceBaseEnvironment)
- workspace: DELETE /api/2.0/agents/{name} (Mason.DeleteMemoryStore)
- workspace: DELETE /api/2.0/agents/{name} (Mason.DeleteSession)
- workspace: DELETE /api/2.0/agents/{name} (Mason.DeleteSessionStore)
- workspace: GET /api/2.0/agents/{name} (Mason.GetMemoryStore)
- workspace: GET /api/2.0/agents/{name} (Mason.GetSession)
- workspace: GET /api/2.0/agents/{name} (Mason.GetSessionStore)
- workspace: PATCH /api/2.0/agents/{name} (Mason.UpdateMemoryStore)
- workspace: PATCH /api/2.0/agents/{name} (Mason.UpdateSession)
- workspace: PATCH /api/2.0/agents/{name} (Mason.UpdateSessionStore)
- workspace: DELETE /api/2.0/postgres/{name} (Postgres.DeleteSyncedTable)
- workspace: GET /api/2.0/postgres/{name} (Postgres.GetOperation)

## Skipped services

- workspace serving.ServingEndpointsDataPlane: no generated operations (hand-written or deprecated wrapper)
- workspace iam.Groups: no generated operations (hand-written or deprecated wrapper)
- workspace iam.ServicePrincipals: no generated operations (hand-written or deprecated wrapper)
- workspace iam.Users: no generated operations (hand-written or deprecated wrapper)
- account iam.AccountGroups: no generated operations (hand-written or deprecated wrapper)
- account iam.AccountServicePrincipals: no generated operations (hand-written or deprecated wrapper)
- account iam.AccountUsers: no generated operations (hand-written or deprecated wrapper)

## Hand-written overrides

- `jobs.Jobs.Get` is generated as `get_page`; see `src/ext.rs` in the package crate.
- `jobs.Jobs.GetRun` is generated as `get_run_page`; see `src/ext.rs` in the package crate.
- `jobs.Jobs.List` is generated as `page:list`; see `src/ext.rs` in the package crate.
- `jobs.Jobs.ListRuns` is generated as `page:list_runs`; see `src/ext.rs` in the package crate.
