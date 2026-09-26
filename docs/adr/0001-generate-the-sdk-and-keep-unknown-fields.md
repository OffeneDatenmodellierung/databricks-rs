---
Title: Generate the SDK from the Go SDK's spec, and keep unknown fields
type: adr
adr-id: "0001"
status: Accepted
architectural-significance: HIGH
domain: databricks-rs
decision-makers: ["Mark Olliver, maintainer"]
superseded-by:
version: "1.0"
last-modified: 2026-09-26
---

# ADR-0001: Generate the SDK from the Go SDK's spec, and keep unknown fields

| | |
|---|---|
| **State** | Accepted |
| **Architectural Significance** | HIGH |
| **Domain** | databricks-rs |
| **Document version** | 1.0 |

## Reference

[Issue #11](https://github.com/OffeneDatenmodellierung/databricks-rs/issues/11) (strategy for the `dbk_tool` service clients) and [PR #13](https://github.com/OffeneDatenmodellierung/databricks-rs/pull/13) (milestone 2).

## Summary

Generate every Account and Workspace service from the Databricks OpenAPI spec, which we recover from `databricks-sdk-go`, rather than hand-writing the `dbk_tool` slice first. Keep #11's point about data loss: every generated type carries an `other` map for fields this SDK version doesn't model. Unknown fields are kept when read, sent back on write, and can be set by callers. This supersedes decisions 1, 2 and 3 of #11 and adopts decision 4.

## Context

#11 (from an external review at commit f857215) set out four decisions:
1. hand-write the ~40–50 methods `dbk_tool` needs;
2. defer codegen until Databricks supplies its OpenAPI spec;
3. make wiremock contract tests the contract for every client;
4. keep the `#[serde(flatten)] other` catch-all, because dropping it later is a silent-data-loss migration.

Decision 2 rested on the spec being unavailable. Milestone 2 removed that premise. `databricks-sdk-go` is generated from the spec and pins its SHA, so `codegen/extract-go` recovers the whole surface from it: 39 packages, 191 services, 1,268 operations. That covers every `dbk_tool` capability area. The spec CLI v1.18.0 pins is the same one.

The first milestone-2 generator dropped `other`, so unknown fields were silently discarded. A caller doing get → modify → update, for example on a tag policy, would erase any field added to the API after this SDK was generated.

## Recommended option

Option 2: generate everything, with an `other` catch-all on every generated type. Hand-written code is limited to what codegen can't express (`codegen/overrides.json` and `src/ext.rs`). Contract testing is split in two:
- **Generated shapes** (pagination strategies, waiters, path escaping, query and body encoding, unknown fields) are pinned by pattern tests in `tests/generated_patterns.rs`.
- **The endpoints `dbk_tool` depends on** get wiremock contract tests in `tests/contracts.rs`. CI runs both.

## Options considered + consequences

### Option 1: Hand-write the dbk_tool slice, defer codegen (#11 as written)

**Consequences:**
- Pros: small, reviewable surface; the style set by milestone 1.
- Cons:
  - ~40–50 methods to write by hand now, and all of them re-done when codegen arrives anyway.
  - It waits on a spec we already have.
  - Services outside the slice stay unavailable.
  - Hand-written models drift from upstream as the APIs change weekly.

### Option 2: Generate everything, keep an `other` catch-all (chosen)

**Consequences:**
- Pros:
  - The whole API surface, regenerated weekly by `upstream.yml`.
  - One set of pattern tests covers 1,268 operations.
  - `other` makes read-modify-write lossless.
  - Callers can send a field before the SDK models it: it goes in the JSON body, or the query string for GET/DELETE.
- Cons:
  - `#[serde(flatten)]` buffers each object during deserialisation, which is slower than a plain struct.
  - Every struct gains a field and a `with_other` setter.
  - A caller could put a known field's name in `other` and send it twice. `other` is documented as being for unmodelled fields only.

### Option 3: Generate everything, drop unknown fields (milestone-2 first cut)

**Consequences:**
- Pros: the smallest, fastest types.
- Cons: silent data loss on read-modify-write, exactly the migration hazard #11 warned about. Rejected.

## Advice Received

| Date | Advisor | Decision version | Advice |
|------|---------|------------------|--------|
| 2026-09-25 | External SDK review (issue #11) | pre-0.1 | Hand-write now, defer codegen, contract tests as the contract, keep `other`. Dissents from codegen-first. The spec premise no longer holds; the `other` advice is adopted. |
| 2026-09-26 | Mark Olliver, maintainer | 1.0 | Agreed: close #11 as superseded, restore the unknown-fields catch-all. |

## Document version history

| Version | Date | Notes |
|---------|------|-------|
| 1.0 | 2026-09-26 | Accepted. Supersedes #11 decisions 1–3; adopts decision 4. |
