# Generated surface

Produced by `cargo xtask codegen` from `spec/ir.json`. DO NOT EDIT.

Source: databricks-sdk-go v0.199.0 (OpenAPI `e13a7b6206adbdb6c4008e2a58b7cc785e32cb1c`).

- 44 packages, 199 services, 1406 operations (1406 generated), 3988 types.

## Not generated (binary or streaming payloads)


## OpenAPI path collisions (moved to `x-databricks-shared-path-operations`)

- account: DELETE /api/2.0/identity/{name} (AccountIamV2.DeleteGroup)
- account: DELETE /api/2.0/identity/{name} (AccountIamV2.DeleteIdentityVisibilityFilter)
- account: DELETE /api/2.0/identity/{name} (AccountIamV2.DeleteServicePrincipal)
- account: DELETE /api/2.0/identity/{name} (AccountIamV2.DeleteUser)
- account: DELETE /api/2.0/identity/{name} (AccountIamV2.DeleteWorkspaceAssignment)
- account: GET /api/2.0/identity/{name} (AccountIamV2.GetExternalGroup)
- account: GET /api/2.0/identity/{name} (AccountIamV2.GetExternalServicePrincipal)
- account: GET /api/2.0/identity/{name} (AccountIamV2.GetExternalUser)
- account: GET /api/2.0/identity/{name} (AccountIamV2.GetGroup)
- account: GET /api/2.0/identity/{name} (AccountIamV2.GetIdentityVisibilityFilter)
- account: GET /api/2.0/identity/{name} (AccountIamV2.GetServicePrincipal)
- account: GET /api/2.0/identity/{name} (AccountIamV2.GetUser)
- account: GET /api/2.0/identity/{name} (AccountIamV2.GetWorkspaceAssignment)
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
- workspace: DELETE /api/2.0/postgres/{name} (Postgres.DeleteMigration)
- workspace: DELETE /api/2.0/postgres/{name} (Postgres.DeleteSyncedTable)
- workspace: GET /api/2.0/postgres/{name} (Postgres.GetDataApi)
- workspace: GET /api/2.0/postgres/{name} (Postgres.GetMigration)
- workspace: GET /api/2.0/postgres/{name} (Postgres.GetOperation)
- workspace: PATCH /api/2.0/postgres/{name} (Postgres.UpdateMigration)
- workspace: GET /api/2.1/unity-catalog/{name} (SoftwareArtifacts.GetSoftwareArtifactVersion)
- workspace: DELETE /api/2.0/identity/{name} (WorkspaceIamV2.DeleteGroupProxy)
- workspace: DELETE /api/2.0/identity/{name} (WorkspaceIamV2.DeleteServicePrincipalProxy)
- workspace: DELETE /api/2.0/identity/{name} (WorkspaceIamV2.DeleteUserProxy)
- workspace: DELETE /api/2.0/identity/{name} (WorkspaceIamV2.DeleteWorkspaceAssignmentProxy)
- workspace: GET /api/2.0/identity/{name} (WorkspaceIamV2.GetExternalGroupProxy)
- workspace: GET /api/2.0/identity/{name} (WorkspaceIamV2.GetExternalServicePrincipalProxy)
- workspace: GET /api/2.0/identity/{name} (WorkspaceIamV2.GetExternalUserProxy)
- workspace: GET /api/2.0/identity/{name} (WorkspaceIamV2.GetGroupProxy)
- workspace: GET /api/2.0/identity/{name} (WorkspaceIamV2.GetServicePrincipalProxy)
- workspace: GET /api/2.0/identity/{name} (WorkspaceIamV2.GetUserProxy)
- workspace: GET /api/2.0/identity/{name} (WorkspaceIamV2.GetWorkspaceAssignmentProxy)
- workspace: GET /api/2.0/identity/{name} (WorkspaceIamV2.GetWorkspaceIdentityDetail)
- workspace: PATCH /api/2.0/identity/{name} (WorkspaceIamV2.UpdateServicePrincipalProxy)
- workspace: PATCH /api/2.0/identity/{name} (WorkspaceIamV2.UpdateUserProxy)
- workspace: PATCH /api/2.0/identity/{name} (WorkspaceIamV2.UpdateWorkspaceAssignmentProxy)
- workspace: PATCH /api/2.0/identity/{name} (WorkspaceIamV2.UpdateWorkspaceIdentityDetail)

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
