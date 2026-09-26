# API specification

Databricks publishes REST API reference documentation at
<https://docs.databricks.com/api/account/> and <https://docs.databricks.com/api/workspace/>.
It does **not** publish the OpenAPI document behind them. The official SDKs are generated from that internal document, and each SDK release pins the spec version it used in `.codegen/_openapi_sha`.

This directory rebuilds the spec from the Go SDK. The Go SDK is generated code, so its struct tags, `impl.go` and `api.go` carry the spec's paths, verbs, parameter placement, schemas, enums, pagination and long-running-operation waiters.

| File | What |
|---|---|
| `ir.json` | The intermediate representation, extracted from databricks-sdk-go by `codegen/extract-go`. It is the single source for everything below. |
| `openapi/workspace.json` | OpenAPI 3.1 document for the **workspace-level** APIs. |
| `openapi/account.json` | OpenAPI 3.1 document for the **account-level** APIs. |
| `GENERATED.md` | Counts, any operations not generated in Rust (none at present), path collisions and skipped services. |
| `PARITY.md` | Parity with the Go SDK's surface beyond the spec: its hand-written helpers, convenience methods and client accessors, each generated, covered by rule, implemented by hand, a tracked gap, or not applicable. Written by `scripts/check_parity.py` from `codegen/parity.toml`; CI fails on anything unaccounted for. |
| `DOCS-CHECK.md` | Spot-check of the specs against the published reference docs. |

`info.version` in each OpenAPI document is the Go SDK version. `info.x-databricks-openapi-sha` is the upstream spec SHA it corresponds to.

## Vendor extensions

Plain OpenAPI can't describe these Databricks behaviours:

| Extension | On | Meaning |
|---|---|---|
| `x-databricks-pagination` | operation | `kind`: `token` \| `offset` \| `page` \| `single`. `items` names the array property in the response. `request_field` and `response_field` name the cursor fields. |
| `x-databricks-wait` | operation | The operation starts a long-running change. Poll `poll` with `param` (taken from the response or the request `field`) until `status_path` reaches one of `targets`. Stop with an error on any of `failures`. Default timeout is `timeout_minutes`. |
| `x-databricks-workspace-header` | operation | Send `X-Databricks-Workspace-Id` when a workspace ID is configured (unified hosts). |
| `x-databricks-resource-name` | operation | The path was expanded from a resource-name parameter so that OpenAPI paths stay unique. For example, `{name}` becomes `projects/{project_id}/branches/{branch_id}` (`pattern`); clients join the parts back into `param`. |
| `x-databricks-shared-path-operations` | path item | Operations whose verb and path collide with another and can't be expanded. Each carries `x-databricks-verb`. |
| `x-databricks-multi-segment` | path parameter | The value is a hierarchical resource name or file path, and may contain `/`. Escape each segment separately and keep the `/` separators. Every other string path parameter is a single segment, so escape `/` as `%2F`. See `docs/upstream-review.md` for how parameters are classified. |
| `x-databricks-long-running` | operation | The operation returns an `Operation` to poll: `poll` (and `cancel`) name the service's methods, as in Go; `result` and `metadata` are the schemas decoded from the operation's `response` and `metadata` once done. |
| `x-databricks-unsupported` | operation | An operation the Rust SDK does not generate (none at present; binary payloads are generated). |
| `x-databricks-package`, `x-databricks-service` | operation, tag | The Go SDK package and service name. |
| `x-enum-descriptions` | enum schema | Description of each value. |

Two conventions to be aware of:
- Query parameters for nested request objects are flattened to `parent.child`, which is how the SDKs send them.
- Schemas are named `<package>.<Type>`, for example `compute.ClusterDetails`.
- `int64` and `double` fields are typed as numbers, but services sometimes send them as strings: `"123"`, or `"NaN"`/`"Infinity"` for floats. Clients should accept both.

## Corrections

`codegen/ir_patches.json` corrects upstream spec defects before generation, for example a path parameter typed as a struct. Each patch must say `why`, and codegen fails if a patch no longer matches `ir.json`, so fixed defects prompt removing the patch.

## Keeping it current

```sh
python3 scripts/check_upstream.py      # which spec SHA each SDK release pins, and whether we're behind
git clone --depth 1 --branch vX.Y.Z https://github.com/databricks/databricks-sdk-go /tmp/go-sdk
(cd codegen/extract-go && go run . -sdk /tmp/go-sdk -out ../../spec/ir.json)
cargo xtask codegen                     # regenerates Rust, both OpenAPI documents and GENERATED.md
```

The `upstream` workflow in `.github/workflows/upstream.yml` runs this weekly. It only picks up a Go SDK release once it is more than 48 hours old, and it opens a pull request with the regenerated output. CI runs `cargo xtask codegen --check`, so the IR, the specs and the Rust code can't drift apart.

When Databricks publishes the OpenAPI document itself, add an OpenAPI → IR front-end alongside `codegen/extract-go`. The Rust and OpenAPI emitters don't change.

## Licence

The extracted data comes from [databricks-sdk-go](https://github.com/databricks/databricks-sdk-go), Apache-2.0, © Databricks, Inc. See `NOTICE`.
