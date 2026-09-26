# Milestone 1: runtime spike

**Goal:** build the hand-written core that generated service code will sit on, and fix the shape of that generated code by hand-writing three operations. The reference is **databricks-sdk-go v0.182.0** (tag of 2026-09-21).

## What's in it

| Rust | Mirrors (Go v0.182.0) | Notes |
|---|---|---|
| `core::config` | `config/config.go`, `config_attributes.go`, `config_file.go`, `host_metadata.go` | Attribute table with the same names, env vars, auth groups and sensitivity. Recognised-but-unported auth attributes (Azure, GCP, OIDC, basic…) are stored, so conflict detection and config-file precedence match Go exactly. |
| `core::auth` | `auth_default.go`, `auth_pat.go`, `auth_m2m.go`, `experimental/auth/cache.go`, `credentials/u2m/endpoint_supplier.go` | Strategy chain with the `auth_type` override and a dry run. PAT and M2M are implemented. M2M uses the client-credentials grant with HTTP Basic auth and the `all-apis` default scope. Tokens refresh asynchronously, starting `min(TTL/2, 20m)` before expiry, retrying after 1m. Token requests retry on 429/502/503/504 within a 1-minute budget. |
| `core::http` | `httpclient/api_client.go`, `config/api_client.go`, `retries/retries.go` | Default 60s timeout, 5-minute retry budget and 15 rps rate limit. Backoff is `attempt` seconds, capped at 10s, plus 50–750ms of jitter. Retries on 429 and its children, 503, 504, the known transient messages and connect/timeout errors. Sends `X-Databricks-Workspace-Id` when `workspace_id` is set. |
| `core::error` | `apierr/errors.go`, `error_mapping.go`, `error_overrides.go`, `details.go` | Parses the standard, API 1.2, SCIM, `CODE: msg`, HTML `<pre>` and unknown formats. Error-code kinds take precedence over status kinds. Includes the clusters/jobs/runs "does not exist" overrides. |
| `core::paging` | `listing/listing.go` | Returns `Paged<'_, T>`, a boxed `Stream`. Pages load lazily and an empty token ends the stream. |
| `core::wait` | `retries.Poll` and the generated `Wait…` types | Uses the same backoff. A timeout returns `Error::Timeout { last }`. |
| `core::query` | `httpclient/request.go` `makeQueryString` | Nested keys become `a.b`, arrays repeat the key, and nulls are dropped. |
| `core::useragent` | `useragent/*` | `product/ver databricks-sdk-rust/ver rust/ver os/… auth/… cicd/… runtime/…` |
| `core::open_enum!` | the generated string enums | Every enum has `Unknown(String)`, so new server values don't break deserialisation. |
| `sdk::service::compute` | `service/compute` `Clusters.List` | Paginated stream plus `list_all`. |
| `sdk::service::jobs` | `service/jobs` `RunNow`, `GetRun` (incl. `ext_api.go` page merge), `WaitGetRunJobTerminatedOrSkipped` | `run_now()` returns a `RunNowWaiter` with `.timeout()`, `.on_progress()` and `.wait()`. |
| `sdk::service::provisioning` | `service/provisioning` `Workspaces.List` | Proves the `AccountClient` path works. |

## Design decisions to carry into codegen

1. **Async-first, tokio + reqwest (rustls).** A blocking façade can come later behind a feature flag.
2. **Services own a cheap `ApiClient` clone**, not a borrow. As a result `list()` returns `Paged<'static, T>` and streams can outlive the client handle.
3. **Request types:**
   - `#[non_exhaustive]` structs.
   - `bon` builders.
   - `Option` fields with `skip_serializing_if`.
   - Required fields (`job_id`, `run_id`) are builder-required.
4. **Response types:**
   - `#[non_exhaustive]` with `#[serde(default)]`.
   - Hand-written spike types keep unmodelled fields in `other: BTreeMap<String, Value>`.
   - The generator will emit full types and can drop `other`, or keep it behind a feature. **Decide in M2.**
5. **Enums:** `open_enum!` everywhere (`Unknown(String)`), plus a `KNOWN` list used in tests.
6. **GET/DELETE requests serialise the request struct to the query string**, and POST/PUT/PATCH send it as the JSON body. This matches Go, where one struct serves both.
7. **Errors:**
   - A single `Error` enum.
   - API failures are `Error::Api(Box<ApiError>)` with `kind()` and `is()`.
   - When the retry budget runs out, the *last* error is returned rather than a wrapper, so `is_missing()` and similar checks still work.
8. **Waiters are types** (`RunNowWaiter`). Each long-running operation in the spec will get one, named after Go's `Wait<Op><State>`.
9. **Config resolution is async**, because host-metadata discovery is a network call. Auth is configured lazily on the first request, as in Go. `ApiClient::authenticate()` forces it early.

## Deliberate differences from Go

- **`Retry-After` is honoured on API calls**, not only on token calls: the wait is `max(backoff, Retry-After)`. So is `RetryInfo.retry_delay`.
- **The PAT strategy returns "not configured" when no token is set**, rather than an error that the chain then swallows. The observable behaviour is the same.
- **A unified host without a discovery URL** uses `{host}/oidc/accounts/{account_id}/.well-known/…`. Go returns "unknown host type" in that case. In practice host metadata always supplies the discovery URL.
- **Not yet ported:**
  - Private-link redirect detection (`apierr/private_link.go`).
  - `GroupID` / `assume_group`.
  - `DisableAsyncTokenRefresh`.
  - Agent and meta-harness user-agent detection.
  - Debug body truncation.

## Verification

- 68 tests in total:
  - 24 unit tests.
  - 42 integration tests against `wiremock`, covering config, the client, the credential chain and the services.
  - 2 doctests.
- `cargo clippy --workspace --all-targets` is clean at pedantic level.
- `cargo doc` builds with `-D warnings`.
- `cargo deny check` passes.
- Coverage with tarpaulin: **95.5% overall**, and every `src` file is at or above 85% (the lowest is `sdk/lib.rs` at 88.6%).
- The lockfile was checked against the 48-hour rule, and 16 transitive crates were pinned back.

**Not verified here:**

- **Toolchain.** Only stable 1.95 was available locally, so the MSRV of 1.94 is set from dependency MSRVs (highest is 1.88) and the std APIs used. The CI matrix covers it.
- **Real workspace.** Nothing has run against one yet; see the exit criteria below.

## Exit criteria for M1

- [ ] `list_clusters`, `run_job` and `list_workspaces` examples work against a real workspace and account, with both PAT and M2M, on at least AWS and Azure.
- [ ] A unified-host workspace works via host metadata.
- [ ] The generated-code shape is agreed (sections 2–8 above), especially: keep or drop `other`, and naming for waiters and enums.

## Milestone 2 inputs

- **Spec source:**
  - Obtain the OpenAPI spec (ask Databricks via the PAB, or use the CLI-embedded spec).
  - Pin it in `spec/` with its SHA.
- **Code generator:**
  - Add a `codegen` crate: spec → IR (services, methods, entities, pagination, waiters, path style) → Rust.
  - Run it through `cargo xtask codegen`.
  - Commit the output.
- **Replace the hand-written services** with generated compute, jobs and Unity Catalog, and check that the tests in `crates/community-databricks-sdk/tests/services.rs` still pass unchanged. They are the contract.
