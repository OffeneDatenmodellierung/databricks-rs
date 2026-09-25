# Generated surface

Produced by `cargo xtask codegen` from `spec/ir.json`. DO NOT EDIT.

Source: databricks-sdk-go v0.182.0 (OpenAPI `4648d66fa37e7683468dc92cc043c6aa7a34f301`).

- 39 packages, 191 services, 1268 operations (1260 generated), 3550 types.

## Not generated (binary or streaming payloads)

- `billing.BillableUsage.Download: binary response`
- `dashboards.Genie.DownloadMessageAttachmentVisualization: binary response`
- `files.Files.Download: binary response`
- `files.Files.Upload: binary/header field contents`
- `serving.ServingEndpoints.ExportMetrics: binary response`
- `serving.ServingEndpoints.GetOpenApi: binary response`
- `serving.ServingEndpoints.HttpRequest: binary response`
- `sql.DbsqlPermissions.TransferOwnership: struct-typed path parameter`

## OpenAPI path collisions (moved to `x-databricks-shared-path-operations`)

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

- `jobs.Jobs.GetRun` is generated as `get_run_page`; see `src/ext.rs` in the package crate.
