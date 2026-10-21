# Generated surface

Produced by `cargo xtask codegen` from `spec/ir.json`. DO NOT EDIT.

Source: databricks-sdk-go v0.188.0 (OpenAPI `bb616cfe22844c61a00b33e9f3ea8deb3dd54c2c`).

- 40 packages, 193 services, 1313 operations (1313 generated), 3656 types.

## Not generated (binary or streaming payloads)


## OpenAPI path collisions (moved to `x-databricks-shared-path-operations`)

- account: DELETE /api/networking/v1/{name} (PrivateNetworkGateways.DeletePrivateNetworkGateway)
- account: GET /api/networking/v1/{name} (PrivateNetworkGateways.GetPrivateNetworkGateway)
- account: GET /api/networking/v1/{name} (PrivateNetworkGateways.GetPrivateNetworkGatewayOperation)
- workspace: DELETE /api/2.0/agents/{name} (AgentKit.DeleteMemoryStore)
- workspace: DELETE /api/2.0/agents/{name} (AgentKit.DeleteSession)
- workspace: DELETE /api/2.0/agents/{name} (AgentKit.DeleteSessionStore)
- workspace: GET /api/2.0/agents/{name} (AgentKit.GetMemoryStore)
- workspace: GET /api/2.0/agents/{name} (AgentKit.GetSession)
- workspace: GET /api/2.0/agents/{name} (AgentKit.GetSessionStore)
- workspace: PATCH /api/2.0/agents/{name} (AgentKit.UpdateMemoryStore)
- workspace: PATCH /api/2.0/agents/{name} (AgentKit.UpdateSession)
- workspace: PATCH /api/2.0/agents/{name} (AgentKit.UpdateSessionStore)
- workspace: GET /api/environments/v1/{name} (Environments.GetOperation)
- workspace: PATCH /api/environments/v1/{name} (Environments.UpdateWorkspaceBaseEnvironment)
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
