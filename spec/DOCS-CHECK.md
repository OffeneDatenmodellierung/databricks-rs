# Spot-check against the published API reference

Spec: databricks-sdk-go v0.182.0, OpenAPI `4648d66fa37e7683468dc92cc043c6aa7a34f301`. Sample: `sample-2026-09-25.json` (2026-09-25).

> WebFetch of docs.databricks.com/api/<client>/<service>/<operation>. The site is a JS single-page app; only some pages expose server-rendered operation lists, which a summarising model extracts verbatim. Single-operation answers proved unreliable (e.g. clusters/list was first reported as /api/2.0/clusters/list; the verbatim list on clusters/get shows /api/2.1), so only verbatim per-service lists are recorded. Pages that returned nothing verbatim: workspace tables/get, jobs/runnow, clusters/list; account budgets/list, metastores/list (404).

**57 matched, 0 missing, 0 weak-evidence mismatches.**

| Client | Docs page | Documented operation | In our spec |
|---|---|---|---|
| workspace | `clusters/get` | `GET /api/2.1/clusters/get` | ✅ `clusters.get` |
| workspace | `clusters/get` | `GET /api/2.1/clusters/list` | ✅ `clusters.list` |
| workspace | `clusters/get` | `POST /api/2.1/clusters/create` | ✅ `clusters.create` |
| workspace | `clusters/get` | `POST /api/2.1/clusters/edit` | ✅ `clusters.edit` |
| workspace | `clusters/get` | `POST /api/2.1/clusters/permanent-delete` | ✅ `clusters.permanent_delete` |
| workspace | `clusters/get` | `POST /api/2.1/clusters/change-owner` | ✅ `clusters.change_owner` |
| workspace | `clusters/get` | `POST /api/2.1/clusters/delete` | ✅ `clusters.delete` |
| workspace | `clusters/get` | `POST /api/2.1/clusters/events` | ✅ `clusters.events` |
| workspace | `clusters/get` | `GET /api/2.1/clusters/list-zones` | ✅ `clusters.list_zones` |
| workspace | `clusters/get` | `GET /api/2.1/clusters/list-node-types` | ✅ `clusters.list_node_types` |
| workspace | `clusters/get` | `GET /api/2.1/clusters/spark-versions` | ✅ `clusters.spark_versions` |
| workspace | `clusters/get` | `POST /api/2.1/clusters/pin` | ✅ `clusters.pin` |
| workspace | `clusters/get` | `POST /api/2.1/clusters/restart` | ✅ `clusters.restart` |
| workspace | `clusters/get` | `POST /api/2.1/clusters/resize` | ✅ `clusters.resize` |
| workspace | `clusters/get` | `POST /api/2.1/clusters/start` | ✅ `clusters.start` |
| workspace | `clusters/get` | `POST /api/2.1/clusters/unpin` | ✅ `clusters.unpin` |
| workspace | `clusters/get` | `POST /api/2.1/clusters/update` | ✅ `clusters.update` |
| workspace | `jobs/list` | `GET /api/2.2/jobs/get` | ✅ `jobs.get` |
| workspace | `jobs/list` | `GET /api/2.2/jobs/list` | ✅ `jobs.list` |
| workspace | `jobs/list` | `POST /api/2.2/jobs/create` | ✅ `jobs.create` |
| workspace | `jobs/list` | `POST /api/2.2/jobs/reset` | ✅ `jobs.reset` |
| workspace | `jobs/list` | `POST /api/2.2/jobs/delete` | ✅ `jobs.delete` |
| workspace | `jobs/list` | `POST /api/2.2/jobs/update` | ✅ `jobs.update` |
| workspace | `jobs/list` | `POST /api/2.2/jobs/run-now` | ✅ `jobs.run_now` |
| workspace | `jobs/getrun` | `GET /api/2.2/jobs/runs/get` | ✅ `jobs.get_run` |
| workspace | `schemas/list` | `GET /api/2.1/unity-catalog/schemas/{full_name_arg}` | ✅ `schemas.get` |
| workspace | `schemas/list` | `GET /api/2.1/unity-catalog/schemas` | ✅ `schemas.list` |
| workspace | `schemas/list` | `POST /api/2.1/unity-catalog/schemas` | ✅ `schemas.create` |
| workspace | `schemas/list` | `PATCH /api/2.1/unity-catalog/schemas/{full_name_arg}` | ✅ `schemas.update` |
| workspace | `schemas/list` | `DELETE /api/2.1/unity-catalog/schemas/{full_name_arg}` | ✅ `schemas.delete` |
| workspace | `catalogs/create` | `GET /api/2.1/unity-catalog/catalogs/{name_arg}` | ✅ `catalogs.get` |
| workspace | `catalogs/create` | `GET /api/2.1/unity-catalog/catalogs` | ✅ `catalogs.list` |
| workspace | `catalogs/create` | `POST /api/2.1/unity-catalog/catalogs` | ✅ `catalogs.create` |
| workspace | `catalogs/create` | `PATCH /api/2.1/unity-catalog/catalogs/{name_arg}` | ✅ `catalogs.update` |
| workspace | `catalogs/create` | `DELETE /api/2.1/unity-catalog/catalogs/{name_arg}` | ✅ `catalogs.delete` |
| workspace | `warehouses/list` | `GET /api/2.0/sql/warehouses/{id}` | ✅ `warehouses.get` |
| workspace | `warehouses/list` | `GET /api/2.0/sql/warehouses` | ✅ `warehouses.list` |
| workspace | `warehouses/list` | `POST /api/2.0/sql/warehouses` | ✅ `warehouses.create` |
| workspace | `warehouses/list` | `POST /api/2.0/sql/warehouses/{id}/edit` | ✅ `warehouses.edit` |
| workspace | `warehouses/list` | `DELETE /api/2.0/sql/warehouses/{id}` | ✅ `warehouses.delete` |
| workspace | `warehouses/list` | `GET /api/2.0/sql/config/warehouses` | ✅ `warehouses.get_workspace_warehouse_config` |
| workspace | `warehouses/list` | `PUT /api/2.0/sql/config/warehouses` | ✅ `warehouses.set_workspace_warehouse_config` |
| workspace | `warehouses/list` | `POST /api/2.0/sql/warehouses/{id}/start` | ✅ `warehouses.start` |
| workspace | `warehouses/list` | `POST /api/2.0/sql/warehouses/{id}/stop` | ✅ `warehouses.stop` |
| account | `workspaces/list` | `GET /api/2.0/accounts/{account_id}/workspaces/{workspace_id}` | ✅ `workspaces.get` |
| account | `workspaces/list` | `GET /api/2.0/accounts/{account_id}/workspaces` | ✅ `workspaces.list` |
| account | `workspaces/list` | `POST /api/2.0/accounts/{account_id}/workspaces` | ✅ `workspaces.create` |
| account | `workspaces/list` | `PATCH /api/2.0/accounts/{account_id}/workspaces/{workspace_id}` | ✅ `workspaces.update` |
| account | `workspaces/list` | `DELETE /api/2.0/accounts/{account_id}/workspaces/{workspace_id}` | ✅ `workspaces.delete` |
| account | `credentials/list` | `GET /api/2.0/accounts/{account_id}/credentials/{credentials_id}` | ✅ `credentials.get` |
| account | `credentials/list` | `GET /api/2.0/accounts/{account_id}/credentials` | ✅ `credentials.list` |
| account | `credentials/list` | `POST /api/2.0/accounts/{account_id}/credentials` | ✅ `credentials.create` |
| account | `credentials/list` | `DELETE /api/2.0/accounts/{account_id}/credentials/{credentials_id}` | ✅ `credentials.delete` |
| account | `storage/list` | `GET /api/2.0/accounts/{account_id}/storage-configurations/{storage_configuration_id}` | ✅ `storage.get` |
| account | `storage/list` | `GET /api/2.0/accounts/{account_id}/storage-configurations` | ✅ `storage.list` |
| account | `storage/list` | `POST /api/2.0/accounts/{account_id}/storage-configurations` | ✅ `storage.create` |
| account | `storage/list` | `DELETE /api/2.0/accounts/{account_id}/storage-configurations/{storage_configuration_id}` | ✅ `storage.delete` |

Regenerate with `python3 spec/docs-check/compare.py` after adding a new `sample-<date>.json`.
