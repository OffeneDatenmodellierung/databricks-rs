# Milestone 2: generated SDK and OpenAPI specs

**Goal:** generate the whole Account and Workspace API surface from a pinned spec, keeping the milestone-1 runtime and the spike's behaviour. Also publish OpenAPI specs that stay current as the REST APIs change.

## Spec source

Databricks does not publish its OpenAPI document; each SDK release only pins its SHA in `.codegen/_openapi_sha`. The docs site (docs.databricks.com/api) is a JS app with no downloadable spec. The community scrape (openapi-community/databricks-openapi) was last updated in December 2024.

The spec of record is therefore **databricks-sdk-go**. It is generated from the spec, so it carries everything the spec does:

| Go source | What the extractor reads |
|---|---|
| struct tags in `model.go` | Field wire names, whether a field is required, and whether it goes in the body, query, path or a header |
| `impl.go` | Verbs and paths, including the account-ID and multi-segment path parameters. Also explicit query parameters, the `update_mask` field mask, a sub-field used as the body, the workspace header, and pagination (token, offset, page or single) |
| `api.go` | Docs, waiters (poll method, status path, target and failure states), and which operations return a waiter and how they bind to it |
| `workspace_client.go`, `account_client.go` | Which client each service belongs to, including nested services such as `settings().default_namespace()` |

Pinned: **databricks-sdk-go v0.182.0**, OpenAPI `4648d66f`. The databricks CLI v1.18.0 pins the same SHA. `scripts/check_upstream.py` compares the SDKs:

| SDK | Latest release | Spec SHA | Outside the 48h window |
|---|---|---|---|
| databricks-sdk-go | v0.182.0 (2026-09-21) | `4648d66f` | yes |
| cli | v1.18.0 (2026-09-24) | `4648d66f` | yes |
| databricks-sdk-java | v0.156.0 (2026-09-17) | `463ed2cc` | yes (older spec) |
| databricks-sdk-py | v0.142.0 (2026-09-25) | `bb6433a6` | **no**: released today, so the next weekly run picks it up once Go ships that spec |

## Pipeline

```
databricks-sdk-go ──(codegen/extract-go, Go)──▶ spec/ir.json ──(cargo xtask codegen)──▶ crates/community-databricks-sdk-<package>/src/lib.rs ×39
                                                                                     ├─▶ crates/community-databricks-sdk/src/{service/mod.rs, accessors.rs} + Cargo.toml features
                                                                                     ├─▶ spec/openapi/{account,workspace}.json
                                                                                     └─▶ spec/GENERATED.md
```

- `cargo xtask codegen --check` fails CI if any generated file is stale.
- CI also re-extracts the IR from the pinned Go tag and diffs it against the committed `spec/ir.json`.
- `.github/workflows/upstream.yml` runs weekly. It picks up a newer Go SDK release once it is more than 48 hours old, regenerates everything, and opens a PR.

## Decisions

1. **One crate per package.** The first attempt put all 208k generated lines in one crate. A debug build ran for more than 25 minutes and used about 6 GB of RAM on a 2-core machine before I stopped it. As 39 crates, the `compute` crate builds in 9 seconds and the whole workspace with every feature builds in 1 minute 20 seconds.
   - `community-databricks-sdk` becomes an umbrella: each feature enables one crate and re-exports it as `community_databricks_sdk::service::<package>`.
   - Dependencies between packages (for example jobs → compute) are ordinary crate dependencies, and the graph has no cycles.
   - This matches aws-sdk-rust. The cost is publishing 41 crates, which release-plz handles.
2. **Setters, not `bon` builders.** Every struct derives `Default` and gets `with_<field>(impl Into<T>)`. A struct with one or two required fields also gets `new(..)`. Three or more required fields would make `new` an easy-to-misorder positional list, so those go through `Default` plus setters.
   - This avoids running a proc-macro over 3,000 structs.
   - The milestone-1 `builder()` calls in tests and examples became `new(..)` / `with_*` (listed under breaking changes below).
3. **Field representation:**

   | Field | Rust type |
   |---|---|
   | required | `T` with `#[serde(default)]` |
   | optional | `Option<T>` |
   | list or map | `Vec` / `BTreeMap`, omitted when empty |
   | path, query or header only | `#[serde(skip)]`; the generated call builds the query and path |
   | timestamp, duration, field mask | `String` |
   | `any` | `serde_json::Value` |

   - Wire names that aren't snake case (SCIM `startIndex`) are renamed with `#[serde(rename)]`.
   - Types that refer to themselves, directly or through a cycle, are `Box`ed. Tarjan's algorithm finds the cycles.
4. **Enums** use `open_enum!` with `Unknown(String)`. `Default` returns an empty `Unknown`, which a required enum field gets when the server omits it.
5. **Waiters** are generated types, `Wait<Name><R>`, with `.timeout()`, `.on_progress()` and `.wait()`, plus a `wait_<name>()` method on the service. State checking is shared code in `community_databricks_core::wait::check_state`, which follows the JSON status path the same way Go's switch does.
6. **Hand-written overrides** live in `codegen/overrides.json`. There is one: `jobs.Jobs.GetRun` is generated as `get_run_page`, and `crates/community-databricks-sdk-jobs/src/ext.rs` supplies `get_run`, which merges pages as Go does. A package crate includes `src/ext.rs` when that file exists.
7. **Generated code is not linted.** Generated crates `#![allow(clippy::all, clippy::pedantic)]` and the rustdoc HTML and link lints (Go docs contain `<catalog>` and `[Go.Links]`). Go's indented and fenced blocks become ```` ```text ```` so they never run as doctests. Hand-written code stays under the house lint rules.
8. **OpenAPI 3.1.** Pagination, waiters, the workspace header, multi-segment parameters and unsupported operations are kept as `x-databricks-*` extensions. Some operations share a verb and a `{name}` path, e.g. every `GET /api/2.0/postgres/{name}`. Those paths are expanded using the resource-name `Format:` given in the field docs (`projects/{project_id}/branches/{branch_id}`). Four collisions have no documented format; they are kept under `x-databricks-shared-path-operations` so no operation is lost. Both documents pass `openapi-spec-validator`.

## What's generated

- 39 packages, 191 services (38 account-level), 1,268 operations and 3,550 types.
- **1,260 operations are generated in Rust.** The other 8 are listed in `spec/GENERATED.md`:
  - 7 binary or streaming operations, such as the Files API upload and download.
  - 1 legacy DBSQL call whose path parameter is a struct.
- All 1,268 operations are in the OpenAPI documents: 207 account and 1,061 workspace.
- Skipped services are the Go SDK's deprecated hand-written SCIM v1 wrappers, whose V2 services are generated, and the serving data plane.

## Verification

- 134 tests pass under `cargo test --workspace --all-features`.
- The 10 milestone-1 spike tests pass against the generated code. The only changes are the construction syntax (from `builder()` to `new`/`with_*`) and dropping the `other` map assertion.
- `tests/generated_patterns.rs` has one test per generated shape:
  - SCIM offset pagination;
  - page-number pagination;
  - explicit query parameters, a field mask and a sub-field body;
  - multi-segment and single-segment path escaping;
  - numeric strings in integer and float fields;
  - waiters bound from the response and from the request;
  - account paths sending no workspace header.
- `xtask` unit tests cover naming, doc conversion and resource-format parsing.
- `cargo clippy --workspace --all-targets --all-features -D warnings`, `cargo fmt --check`, `cargo xtask codegen --check`, `cargo deny check`, and the 48-hour lockfile check all pass.
- `spec/DOCS-CHECK.md` records 57 of 57 operations sampled from the published reference docs matching the spec. Pages sampled:
  - workspace: clusters, jobs, schemas, catalogs, warehouses;
  - account: workspaces, credentials, storage.

  Single-operation answers from the docs site proved unreliable. The first read of clusters/list reported `/api/2.0`; the verbatim operation list on clusters/get shows `/api/2.1`. So only verbatim per-service lists are recorded.

## Breaking changes from milestone 1

- The `bon` builders are gone; use `new(..)` or `Default` plus `with_*`.
- Response structs no longer have an `other` map. Unknown fields are ignored, and every known field is typed.
- Integer fields are `i64` throughout. Go's `int` is 64-bit, so an `i32` could fail to deserialise large values. They also accept numeric strings, and float fields accept `"NaN"`/`"Infinity"` (`community_databricks_core::serde_num`).
- `AccountClient::workspaces()` and the other accessors are generated. Account IDs are read from the config on each call.

## Upstream review

`docs/upstream-review.md` goes through the open databricks-sdk-go issues and PRs, and the PRs closed since v0.182.0. Adopted from it:
- segment-aware path escaping;
- lenient integer and float decoding;
- custom headers;
- group role assumption;
- clearer decode errors;
- every auth type in Go's chain except `basic` and `metadata-service`, including the open upstream changes to them, with Azure and Google tokens from the vendors' crates (`azure_identity`, `google-cloud-auth`).

## Open items

1. **Ask Databricks for the spec** through the PAB. An OpenAPI → IR front end would replace the Go extractor; the emitters stay the same.
2. **Binary operations**: Files upload and download, and the few octet-stream and text endpoints. These need streaming bodies in `community-databricks-core`.
3. **Port the Go `ext_*` helpers**, such as `clusters.SelectSparkVersion`, `SelectNodeType` and the SCIM v1 dedupe iterators.
4. **Live test**, still carried over from milestone 1: run the examples against a real workspace and account.
5. **Publishing**: run the *Publish new crates* workflow once to create the 41 crates. All the names were free on crates.io on 2026-09-26. release-plz is set up for independent per-crate versions.
