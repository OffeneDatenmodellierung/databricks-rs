# Generated surface

Produced by `cargo xtask codegen` from `spec/ir.json`. DO NOT EDIT.

Source: databricks-sdk-go v0.195.0 (OpenAPI `ced34d63e64d234affc3213758ac687edbf6f3c3`).

- 43 packages, 196 services, 1382 operations (1382 generated), 3903 types.

## Not generated (binary or streaming payloads)


## OpenAPI path collisions (moved to `x-databricks-shared-path-operations`)

- account: PATCH /api/2.0/identity/{name} (AccountIamV2.UpdateServicePrincipal)
- account: PATCH /api/2.0/identity/{name} (AccountIamV2.UpdateUser)
- account: PATCH /api/2.0/identity/{name} (AccountIamV2.UpdateWorkspaceAssignment)
- account: DELETE /api/networking/v1/{name} (PrivateNetworkGateways.DeletePrivateNetworkGateway)
- account: GET /api/networking/v1/{name} (PrivateNetworkGateways.GetPrivateNetworkGateway)
- account: GET /api/networking/v1/{name} (PrivateNetworkGateways.GetPrivateNetworkGatewayOperation)
- workspace: DELETE /api/2.0/agents/{name} (AgentKit.DeleteMemoryStore)
- workspace: DELETE /api/2.0/agents/{name} (AgentKit.DeleteSession)
- workspace: DELETE /api/2.0/agents/{name} (AgentKit.DeleteSessionStore)
- workspace: GET /api/2.0/agents/{name} (AgentKit.GetMemoryStore)
- workspace: GET /api/2.0/agents/{name} (AgentKit.GetSession)
- workspace: GET /api/2.0/agents/{name} (AgentKit.GetSessionStore)
- workspace: GET /api/2.0/agents/{name}/permissions/self (AgentKit.GetSessionStorePermission)
- workspace: POST /api/2.0/agents/{name}/permissions:grant (AgentKit.GrantSessionStorePermission)
- workspace: GET /api/2.0/agents/{parent}/permissions (AgentKit.ListSessionStorePermissions)
- workspace: POST /api/2.0/agents/{name}/permissions:revoke (AgentKit.RevokeSessionStorePermission)
- workspace: PATCH /api/2.0/agents/{name} (AgentKit.UpdateMemoryStore)
- workspace: PATCH /api/2.0/agents/{name} (AgentKit.UpdateSession)
- workspace: PATCH /api/2.0/agents/{name} (AgentKit.UpdateSessionStore)
- workspace: GET /api/environments/v1/{name} (Environments.GetOperation)
- workspace: PATCH /api/environments/v1/{name} (Environments.UpdateWorkspaceBaseEnvironment)
- workspace: GET /api/2.0/lakewatch/{name} (Lakewatch.GetCommonConfig)
- workspace: GET /api/2.0/lakewatch/{name} (Lakewatch.GetDatasource)
- workspace: GET /api/2.0/lakewatch/{name} (Lakewatch.GetDatasourceConfig)
- workspace: GET /api/2.0/lakewatch/{name} (Lakewatch.GetRule)
- workspace: GET /api/2.0/lakewatch/{name} (Lakewatch.GetRuleConfig)
- workspace: PATCH /api/2.0/lakewatch/{name} (Lakewatch.UpdateCommonConfig)
- workspace: PATCH /api/2.0/lakewatch/{name} (Lakewatch.UpdateDatasource)
- workspace: PATCH /api/2.0/lakewatch/{name} (Lakewatch.UpdateDatasourceConfig)
- workspace: PATCH /api/2.0/lakewatch/{name} (Lakewatch.UpdateRule)
- workspace: PATCH /api/2.0/lakewatch/{name} (Lakewatch.UpdateRuleConfig)
- workspace: DELETE /api/2.0/postgres/{name} (Postgres.DeleteSyncedTable)
- workspace: GET /api/2.0/postgres/{name} (Postgres.GetOperation)
- workspace: PATCH /api/2.0/identity/{name} (WorkspaceIamV2.UpdateServicePrincipalProxy)
- workspace: PATCH /api/2.0/identity/{name} (WorkspaceIamV2.UpdateUserProxy)
- workspace: PATCH /api/2.0/identity/{name} (WorkspaceIamV2.UpdateWorkspaceAssignmentProxy)

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
