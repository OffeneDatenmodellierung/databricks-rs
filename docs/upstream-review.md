# Upstream review: databricks-sdk-go

Reviewed on 2026-09-25 against the version we pin: **v0.182.0** (released 2026-09-21, OpenAPI `4648d66f`). It covers:
- open issues and PRs;
- PRs closed since that release;
- fixes already in that release that this crate hadn't matched yet.

The aim is to find changes this crate should follow or can do better on.

**State of upstream:** `main` is the v0.182.0 release commit, so nothing has merged since the release. The notable open PR is **#1851**, which moves to spec `38892892`. The weekly `upstream.yml` workflow picks it up once it ships in a release more than 48 hours old; nothing needs doing by hand.

## Adopted in this PR

### Fixes from open upstream PRs and issues (ahead of Go)

| Upstream | Problem | What we do now |
|---|---|---|
| [#1811](https://github.com/databricks/databricks-sdk-go/pull/1811) (open) / [#1765](https://github.com/databricks/databricks-sdk-go/issues/1765) | Path parameters go into the URL unescaped, so a `/` in a name (`Inferences/Second`) adds a path segment and the call 404s | Every path parameter is escaped. A **single-segment** parameter escapes `/` as `%2F`. A **multi-segment** parameter (a resource name like `catalogs/x/schemas/y`, or a file path) keeps `/` and escapes each segment. The extractor classifies the parameters (rule below) and agrees with #1811 on 766 of 769. We keep `/` in the other 3: two default-base-environment `name`s and `apps.GetSpaceOperation`'s `name`. |
| [#1498](https://github.com/databricks/databricks-sdk-go/issues/1498) (open) | MLflow returns metric values as the string `"NaN"`, which a strict float decoder rejects | Float fields accept `"NaN"`, `"Infinity"`, `"-Infinity"` and numeric strings (`databricks_core::serde_num`). Response decoding also falls back to treating bare `NaN`/`Infinity` tokens as `null`. |
| [#1846](https://github.com/databricks/databricks-sdk-go/pull/1846) (open) / [#1340](https://github.com/databricks/databricks-sdk-go/pull/1340) (open) | No way to send extra headers (tracing, proxies) | `Config::headers` / `Config::header(name, value)`. They are sent on every request but never override `Authorization`, `User-Agent`, `Content-Type`, `Accept`, `X-Databricks-Workspace-Id` or a header set by the credentials provider. You can set them in code only, as in #1846. #1340's env and config-file variant and its URL path prefix are not adopted. |
| [#1796](https://github.com/databricks/databricks-sdk-go/issues/1796) (open) | A 200 response that the client can't decode gives "failed to unmarshal response body" with no detail | The decode error includes the first 200 characters of the body, so a type mismatch can be diagnosed from the error alone. |

### Parity with the pinned release

These shipped in Go before v0.182.0, but the Rust port didn't have them:

| Upstream | Go release | What we do now |
|---|---|---|
| [#1808](https://github.com/databricks/databricks-sdk-go/pull/1808) | v0.174.0 | Integer fields accept a number, a numeric string (`"9007199254740993"`) or an integral float. `null` is treated as absent. Serialisation stays numeric. The generator adds `deserialize_with` to 838 integer fields and 44 float fields. |
| [#1812](https://github.com/databricks/databricks-sdk-go/pull/1812), [#1817](https://github.com/databricks/databricks-sdk-go/pull/1817) | v0.176.0 | `Config::group_id` / `DATABRICKS_GROUP_ID` requests group role assumption. `oauth-m2m` sends it to the token endpoint as `assume_group`. `pat` refuses to authenticate while it is set, rather than silently giving normal access. |

### Multi-segment classification

A path parameter is multi-segment if Go already wraps it in `EncodeMultiSegmentPathParameter`, or if it is a string field and either:

1. its doc gives a resource pattern (`catalogs/{catalog}/schemas/{schema}`, matched by `[a-z][a-z_-]*/\{[a-z_]+\}`); or
2. it is `Name` or `Parent`, and the literal before it ends in a version segment (`/v1/`, `/2.0/`) or the method is a long-running `…Operation` call.

The rule lives in `codegen/extract-go/main.go` (`isResourceName`). It marks 167 parameters as multi-segment, and the OpenAPI documents record them as `x-databricks-multi-segment`.

## Already better

- [#1448](https://github.com/databricks/databricks-sdk-go/issues/1448): Go's `ExecuteAndWait` ignores `WaitTimeout` and uses a fixed 20 minutes. Our generated waiters take `.timeout(..)`. When the hand-written SQL `execute_and_wait` helper is ported (milestone-2 open item 3), it should honour `wait_timeout`.
- [#1438](https://github.com/databricks/databricks-sdk-go/pull/1438): a panic from an unchecked type assertion in `shouldRetry`. Our retry decision matches on a typed `Failure` enum, so there is no assertion to panic.
- [#1363](https://github.com/databricks/databricks-sdk-go/pull/1363): if resetting the request body for a retry failed, that error replaced the real one. We keep the body as bytes and re-send it on each attempt, so there is no reset step.

## Backlog (auth strategies not yet ported)

The Rust crate has PAT and OAuth M2M so far. Follow these when the other strategies are ported:

- [#1832](https://github.com/databricks/databricks-sdk-go/pull/1832) (open): the interactive U2M and token-cache APIs are deprecated, and that responsibility moves to the Databricks CLI. Port U2M as "read the CLI's token", not as the deprecated flow.
- [#1815](https://github.com/databricks/databricks-sdk-go/pull/1815) (open): the `oauth-m2m-gcp` auth type, which adds a Google access token to Databricks M2M for GCP account-level provisioning.
- [#1813](https://github.com/databricks/databricks-sdk-go/pull/1813) (open): Azure MSI picks its endpoint from the host environment (Function Apps), falling back to IMDS.
- [#1790](https://github.com/databricks/databricks-sdk-go/issues/1790) (open): pass an OIDC token in memory for workload identity federation, instead of through a file or environment variable.
- Group role assumption for WIF (#1817) and U2M (#1812): add `assume_group` when those strategies are ported.

## Not applicable

The remaining open issues are about service behaviour or questions about the Databricks platform, not SDK bugs. If the APIs change, the regular spec updates will cover them.
