//! OpenAPI 3.1 emission: one document for account-level and one for
//! workspace-level APIs, generated from the same IR as the Rust code.
//!
//! Databricks-specific behaviour that plain OpenAPI can't express is kept
//! as vendor extensions:
//!
//! * `x-databricks-pagination` — `{kind, items, request_field, response_field}`
//!   with `kind` one of `token`, `offset`, `page`, `single`.
//! * `x-databricks-wait` — the long-running-operation waiter an operation
//!   returns: `{waiter, poll, param, from_response, field, status_path,
//!   message_path, targets, failures, timeout_minutes}`.
//! * `x-databricks-long-running` — the operation returns an `Operation` to
//!   poll: `{poll, cancel, result, metadata}` (`poll`/`cancel` name methods
//!   of the same service, as in Go; `result`/`metadata` are the schemas
//!   decoded from the operation's `response`/`metadata`).
//! * `x-databricks-unsupported` — operations the Rust SDK does not generate
//!   (none at present; binary bodies are generated as `Binary`).
//! * `x-databricks-workspace-header` — send `X-Databricks-Workspace-Id`.
//! * `x-databricks-package` / `x-databricks-service` — Go SDK grouping.
//! * `x-databricks-resource-name` — the operation's path was expanded from a
//!   resource-name parameter (`{name}` → `projects/{project_id}/…`) to keep
//!   OpenAPI paths unique; clients join the parts back into `param`.
//! * `x-databricks-shared-path-operations` (path item) — operations whose
//!   verb and path collide with another and that could not be expanded.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde_json::{Map, Value, json};

use crate::ir::{Field, Ir, Method, Service, TypeDef, TypeRef};
use crate::model::Types;
use crate::names;

pub struct Specs {
    pub account: Value,
    pub workspace: Value,
    /// Operations dropped because another had the same verb + path.
    pub duplicates: Vec<String>,
}

pub fn emit(ir: &Ir, types: &Types<'_>) -> Specs {
    let mut duplicates = Vec::new();
    let account = document(ir, types, "account", &mut duplicates);
    let workspace = document(ir, types, "workspace", &mut duplicates);
    Specs {
        account,
        workspace,
        duplicates,
    }
}

fn schema_name(r: &TypeRef) -> String {
    format!("{}.{}", r.pkg, r.name)
}

fn schema_ref(r: &TypeRef) -> Value {
    json!({ "$ref": format!("#/components/schemas/{}", schema_name(r)) })
}

fn type_schema(r: &TypeRef, reach: &mut BTreeSet<(String, String)>) -> Value {
    match r.kind.as_str() {
        "string" => json!({"type": "string"}),
        "bool" => json!({"type": "boolean"}),
        // Go's `int` doesn't record the spec's width; the Rust models use
        // i64 for it, so the documents promise no narrower range than that.
        "int" | "int64" => json!({"type": "integer", "format": "int64"}),
        "float64" => json!({"type": "number", "format": "double"}),
        "timestamp" => json!({"type": "string", "format": "date-time"}),
        "duration" => json!({"type": "string", "format": "google-duration", "example": "3.5s"}),
        "field_mask" => {
            json!({"type": "string", "format": "google-fieldmask", "example": "name,owner"})
        }
        "binary" => json!({"type": "string", "format": "binary"}),
        "list" => {
            json!({"type": "array", "items": type_schema(r.elem.as_deref().expect("elem"), reach)})
        }
        "map" => {
            json!({"type": "object", "additionalProperties": type_schema(r.elem.as_deref().expect("elem"), reach)})
        }
        "ref" => {
            reach.insert((r.pkg.clone(), r.name.clone()));
            schema_ref(r)
        }
        _ => json!({}),
    }
}

fn with_desc(mut v: Value, doc: &str) -> Value {
    if !doc.trim().is_empty() {
        if v.get("$ref").is_some() {
            // 3.1 allows siblings next to $ref.
            v["description"] = Value::String(doc.trim().to_owned());
        } else if let Some(o) = v.as_object_mut() {
            o.insert("description".into(), Value::String(doc.trim().to_owned()));
        }
    }
    v
}

fn component(t: &TypeDef, pkg: &str, reach: &mut BTreeSet<(String, String)>) -> Value {
    match t.kind.as_str() {
        "enum" => {
            let values: Vec<&str> = t
                .values
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|v| v.value.as_str())
                .collect();
            let mut s = json!({"type": "string"});
            if !values.is_empty() {
                s["enum"] = json!(values);
                let descs: Map<String, Value> = t
                    .values
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .filter(|v| !v.doc.is_empty())
                    .map(|v| (v.value.clone(), Value::String(v.doc.clone())))
                    .collect();
                if !descs.is_empty() {
                    s["x-enum-descriptions"] = Value::Object(descs);
                }
            }
            with_desc(s, &t.doc)
        }
        "struct" => {
            let mut props = Map::new();
            let mut required = Vec::new();
            for f in t.fields.as_deref().unwrap_or_default() {
                if f.json.is_empty() {
                    continue;
                }
                props.insert(f.json.clone(), with_desc(type_schema(&f.ty, reach), &f.doc));
                if f.required {
                    required.push(Value::String(f.json.clone()));
                }
            }
            let mut s = json!({"type": "object", "properties": props});
            if !required.is_empty() {
                s["required"] = Value::Array(required);
            }
            with_desc(s, &t.doc)
        }
        _ => {
            let target = t.alias.as_ref().map_or_else(
                || json!({}),
                |a| {
                    let mut a = a.clone();
                    if a.kind == "ref" && a.pkg.is_empty() {
                        pkg.clone_into(&mut a.pkg);
                    }
                    type_schema(&a, reach)
                },
            );
            with_desc(target, &t.doc)
        }
    }
}

fn path_string(m: &Method) -> String {
    m.path
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|p| {
            if p.account_id {
                "{account_id}".to_owned()
            } else if p.field.is_empty() {
                p.lit.clone()
            } else {
                format!("{{{}}}", p.field)
            }
        })
        .collect()
}

fn field<'a>(types: &Types<'a>, r: Option<&TypeRef>, wire: &str) -> Option<&'a Field> {
    types.fields(r?).iter().find(|f| f.name == wire)
}

/// Query parameters; nested objects are flattened to `parent.child` as the
/// SDKs send them.
fn query_params(
    types: &Types<'_>,
    name: &str,
    f: &Field,
    reach: &mut BTreeSet<(String, String)>,
    out: &mut Vec<Value>,
) {
    // Recursive types can't be flattened; stop after a few levels.
    if f.ty.kind == "ref"
        && name.matches('.').count() < 3
        && let Some(t) = types.get(&f.ty)
        && t.kind == "struct"
    {
        for c in t.fields.as_deref().unwrap_or_default() {
            if c.json.is_empty() {
                continue;
            }
            query_params(types, &format!("{name}.{}", c.json), c, reach, out);
        }
        return;
    }
    let mut schema = type_schema(&f.ty, reach);
    let explode = f.ty.kind == "list";
    let mut p = json!({
        "name": name,
        "in": "query",
        "required": f.required,
        "schema": schema.take(),
    });
    if explode {
        p["style"] = json!("form");
        p["explode"] = json!(true);
    }
    if !f.doc.trim().is_empty() {
        p["description"] = Value::String(f.doc.trim().to_owned());
    }
    out.push(p);
}

fn operation(
    ir: &Ir,
    types: &Types<'_>,
    svc: &Service,
    m: &Method,
    reach: &mut BTreeSet<(String, String)>,
) -> Value {
    let op_id = format!(
        "{}.{}",
        names::snake(&svc_path(ir, svc)).replace("__", "."),
        names::method_ident(&m.name)
    );
    let mut op = Map::new();
    op.insert("operationId".into(), json!(op_id));
    op.insert("tags".into(), json!([svc.name]));
    let summary = m.doc.lines().next().unwrap_or_default().trim().to_owned();
    if !summary.is_empty() {
        op.insert("summary".into(), json!(summary));
    }
    if !m.doc.trim().is_empty() {
        op.insert("description".into(), json!(m.doc.trim()));
    }
    op.insert("x-databricks-package".into(), json!(svc.package));
    op.insert("x-databricks-service".into(), json!(svc.name));
    if m.workspace_header {
        op.insert("x-databricks-workspace-header".into(), json!(true));
    }
    let mut params = Vec::new();
    for p in m.path.as_deref().unwrap_or_default() {
        if p.account_id {
            params.push(json!({"name": "account_id", "in": "path", "required": true, "schema": {"type": "string"}, "description": "Databricks account ID."}));
        } else if !p.field.is_empty() {
            let f = field(types, m.request.as_ref(), &p.field);
            let schema = f.map_or_else(|| json!({"type": "string"}), |f| type_schema(&f.ty, reach));
            let mut v = json!({"name": p.field, "in": "path", "required": true, "schema": schema});
            if let Some(f) = f.filter(|f| !f.doc.trim().is_empty()) {
                v["description"] = json!(f.doc.trim());
            }
            if p.multi_segment {
                v["x-databricks-multi-segment"] = json!(true);
            }
            params.push(v);
        }
    }
    let body_verb = matches!(m.verb.as_str(), "POST" | "PUT" | "PATCH");
    if let Some(r) = &m.request {
        for f in types.fields(r).iter().filter(|f| f.location == "header") {
            params.push(with_desc(
                json!({"name": f.name, "in": "header", "required": f.required, "schema": type_schema(&f.ty, reach)}),
                &f.doc,
            ));
        }
    }
    if let Some(r) = &m.request {
        if body_verb {
            for q in m.explicit_query.as_deref().unwrap_or_default() {
                if let Some(f) = field(types, Some(r), &q.field) {
                    query_params(types, &q.name, f, reach, &mut params);
                }
            }
        } else {
            for f in types.fields(r) {
                if !f.query.is_empty() {
                    query_params(types, &f.query, f, reach, &mut params);
                }
            }
        }
        if body_verb {
            let schema = if m.body_field.is_empty() {
                type_schema(r, reach)
            } else {
                field(types, Some(r), &m.body_field)
                    .map_or_else(|| json!({}), |f| type_schema(&f.ty, reach))
            };
            let binary_body = !m.body_field.is_empty()
                && field(types, Some(r), &m.body_field).is_some_and(|f| f.ty.kind == "binary");
            let ct = if binary_body || m.unsupported.contains("content-type") {
                "application/octet-stream"
            } else {
                "application/json"
            };
            op.insert(
                "requestBody".into(),
                json!({"required": true, "content": {ct: {"schema": schema}}}),
            );
        }
    }
    if !params.is_empty() {
        op.insert("parameters".into(), Value::Array(params));
    }
    let ok = match &m.response {
        Some(r) => {
            // The Accept header Go sends: application/octet-stream for file
            // downloads, text/plain for metrics and exports.
            let ct = if !m.accept.is_empty() && m.accept != "application/json" {
                m.accept.as_str()
            } else if m.unsupported.contains("accept") {
                "application/octet-stream"
            } else {
                "application/json"
            };
            let mut ok = json!({"description": "Success."});
            let header_fields: Vec<&Field> = types
                .fields(r)
                .iter()
                .filter(|f| f.location == "header")
                .collect();
            // A binary response's body is the raw bytes of its binary
            // field (other fields come from headers), not a JSON object.
            let binary = types.fields(r).iter().find(|f| f.ty.kind == "binary");
            if let Some(b) = binary {
                ok["content"] =
                    json!({ct: {"schema": with_desc(type_schema(&b.ty, reach), &b.doc)}});
            } else if header_fields.len() < types.fields(r).len() || header_fields.is_empty() {
                // Header-only responses (HEAD metadata) have no body.
                ok["content"] = json!({ct: {"schema": type_schema(r, reach)}});
            }
            if !header_fields.is_empty() {
                let headers: Map<String, Value> = header_fields
                    .iter()
                    .map(|f| {
                        (
                            f.name.clone(),
                            with_desc(json!({"schema": type_schema(&f.ty, reach)}), &f.doc),
                        )
                    })
                    .collect();
                ok["headers"] = Value::Object(headers);
            }
            ok
        }
        None => json!({"description": "Success."}),
    };
    op.insert(
        "responses".into(),
        json!({"200": ok, "default": {"$ref": "#/components/responses/Error"}}),
    );
    if let Some(pg) = &m.pagination {
        reach.insert((pg.item_type.pkg.clone(), pg.item_type.name.clone()));
        op.insert(
            "x-databricks-pagination".into(),
            json!({
                "kind": pg.kind,
                "items": pg.items,
                "request_field": pg.req_field,
                "response_field": pg.resp_field,
            }),
        );
    }
    if let Some(wb) = &m.wait
        && let Some(w) = svc
            .waiters
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|w| w.name == wb.waiter)
    {
        op.insert(
                "x-databricks-wait".into(),
                json!({
                    "waiter": format!("Wait{}", w.name),
                    "poll": format!("{}.{}", names::snake(&svc_path(ir, svc)).replace("__", "."), names::method_ident(&w.poll_method)),
                    "param": w.param,
                    "from_response": wb.from_response,
                    "field": wb.field,
                    "status_path": w.status_path,
                    "message_path": w.message_path,
                    "targets": w.targets,
                    "failures": w.failures,
                    "timeout_minutes": wb.timeout_minutes,
                }),
            );
    }
    if let Some(l) = &m.lro {
        let mut x = serde_json::Map::new();
        x.insert("poll".into(), json!(l.poll));
        if !l.cancel.is_empty() {
            x.insert("cancel".into(), json!(l.cancel));
        }
        if let Some(r) = &l.result {
            x.insert("result".into(), type_schema(r, reach));
        }
        if let Some(md) = &l.metadata {
            x.insert("metadata".into(), type_schema(md, reach));
        }
        op.insert("x-databricks-long-running".into(), Value::Object(x));
    }
    if !m.unsupported.is_empty() && !m.is_binary() {
        op.insert("x-databricks-unsupported".into(), json!(m.unsupported));
    }
    Value::Object(op)
}

/// `settings__default_namespace` for nested services, used in operationIds.
fn svc_path(ir: &Ir, svc: &Service) -> String {
    if svc.parent.is_empty() {
        return svc.accessor.clone();
    }
    let parent = ir
        .services
        .iter()
        .find(|p| p.name == svc.parent && p.package == svc.package && p.client == svc.client)
        .map_or_else(|| svc.parent.clone(), |p| p.accessor.clone());
    format!("{parent}__{}", svc.accessor)
}

/// (resource-name parameter, pattern, path variables).
type Expansion = (String, String, Vec<String>);

/// `/api/2.0/postgres/{name}` + `Format: projects/{project_id}/branches/{branch_id}`
/// → (`/api/2.0/postgres/projects/{project_id}/branches/{branch_id}`,
///    (`name`, pattern, [`project_id`, `branch_id`])).
fn expand_resource_path(types: &Types<'_>, m: &Method) -> Option<(String, Expansion)> {
    let mut out = String::new();
    let mut expanded = None;
    for p in m.path.as_deref().unwrap_or_default() {
        if p.account_id {
            out.push_str("{account_id}");
        } else if p.field.is_empty() {
            out.push_str(&p.lit);
        } else {
            let f = field(types, m.request.as_ref(), &p.field)?;
            match resource_format(&f.doc) {
                Some(pattern) if expanded.is_none() => {
                    let vars: Vec<String> = pattern
                        .split('/')
                        .filter(|s| s.starts_with('{') && s.ends_with('}'))
                        .map(|s| s.trim_matches(['{', '}']).to_owned())
                        .collect();
                    out.push_str(&pattern);
                    expanded = Some((p.field.clone(), pattern, vars));
                }
                _ => {
                    let _ = write!(out, "{{{}}}", p.field);
                }
            }
        }
    }
    expanded.map(|e| (out, e))
}

/// The `Format: a/{b}/c/{d}` pattern in a field's documentation.
fn resource_format(doc: &str) -> Option<String> {
    let rest = &doc[doc.find("Format:")? + "Format:".len()..];
    let token: String = rest
        .trim_start()
        .trim_start_matches(['`', '"'])
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || "/{}_-".contains(*c))
        .collect();
    let token = token.trim_end_matches('/').to_owned();
    (token.contains('{') && !token.starts_with('/')).then_some(token)
}

fn document(ir: &Ir, types: &Types<'_>, client: &str, duplicates: &mut Vec<String>) -> Value {
    let mut paths: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
    let mut reach = BTreeSet::new();
    let mut tags = Vec::new();
    let mut pending: Vec<(&Service, &Method)> = Vec::new();
    for svc in ir.services.iter().filter(|s| s.client == client) {
        let methods = svc.methods.as_deref().unwrap_or_default();
        if methods.is_empty() {
            continue;
        }
        let mut tag = json!({"name": svc.name, "x-databricks-package": svc.package});
        if !svc.doc.trim().is_empty() {
            tag["description"] = json!(svc.doc.trim());
        }
        tags.push(tag);
        for m in methods {
            pending.push((svc, m));
        }
    }
    // Resource-oriented APIs share templates such as `/api/2.0/postgres/{name}`
    // where `name` is `projects/{project_id}/branches/{branch_id}`. When two
    // operations collide, expand the resource name from the field's
    // documented `Format:` so each gets a distinct OpenAPI path.
    let mut counts: BTreeMap<(String, String), usize> = BTreeMap::new();
    for (_, m) in &pending {
        *counts.entry((path_string(m), m.verb.clone())).or_default() += 1;
    }
    for (svc, m) in pending {
        let plain = path_string(m);
        let (p, expanded) = if counts[&(plain.clone(), m.verb.clone())] > 1 {
            expand_resource_path(types, m).map_or((plain, None), |(p, e)| (p, Some(e)))
        } else {
            (plain, None)
        };
        let verb = m.verb.to_lowercase();
        let mut op = operation(ir, types, svc, m, &mut reach);
        let entry = paths.entry(p.clone()).or_default();
        if entry.contains_key(&verb) {
            // OpenAPI allows one operation per verb and path. Keep the
            // others on the path item so nothing is lost.
            duplicates.push(format!(
                "{client}: {} {p} ({}.{})",
                m.verb, svc.name, m.name
            ));
            op["x-databricks-verb"] = json!(m.verb);
            let extra = entry
                .entry("x-databricks-shared-path-operations".to_owned())
                .or_insert_with(|| json!([]));
            if let Some(a) = extra.as_array_mut() {
                a.push(op);
            }
            continue;
        }
        if let Some((param, pattern, vars)) = expanded {
            if let Some(params) = op.get_mut("parameters").and_then(Value::as_array_mut) {
                params.retain(|v| v["name"] != json!(param) || v["in"] != json!("path"));
                for var in vars.iter().rev() {
                    params.insert(0, json!({"name": var, "in": "path", "required": true, "schema": {"type": "string"}}));
                }
            }
            op["x-databricks-resource-name"] = json!({"param": param, "pattern": pattern});
        }
        entry.insert(verb, op);
    }
    // Transitive closure of referenced schemas.
    let mut schemas = Map::new();
    let mut done = BTreeSet::new();
    while let Some(next) = reach.iter().find(|k| !done.contains(*k)).cloned() {
        done.insert(next.clone());
        let r = TypeRef {
            kind: "ref".into(),
            pkg: next.0.clone(),
            name: next.1.clone(),
            elem: None,
        };
        if let Some(t) = types.get(&r) {
            let c = component(t, &next.0, &mut reach);
            schemas.insert(schema_name(&r), c);
        }
    }
    schemas.insert(
        "Error".into(),
        json!({
            "type": "object",
            "description": "Standard Databricks error envelope.",
            "properties": {
                "error_code": {"type": "string", "example": "RESOURCE_DOES_NOT_EXIST"},
                "message": {"type": "string"},
                "details": {"type": "array", "items": {"type": "object", "properties": {"@type": {"type": "string"}}, "additionalProperties": true}}
            }
        }),
    );
    let (title, servers, token_url) = if client == "account" {
        (
            "Databricks Account API",
            json!([
                {"url": "https://accounts.cloud.databricks.com", "description": "AWS"},
                {"url": "https://accounts.azuredatabricks.net", "description": "Azure"},
                {"url": "https://accounts.gcp.databricks.com", "description": "GCP"}
            ]),
            "/oidc/accounts/{account_id}/v1/token",
        )
    } else {
        (
            "Databricks Workspace API",
            json!([{"url": "https://{workspace_host}", "variables": {"workspace_host": {"default": "example.cloud.databricks.com", "description": "Workspace hostname"}}}]),
            "/oidc/v1/token",
        )
    };
    let path_objs: Map<String, Value> = paths
        .into_iter()
        .map(|(k, v)| (k, Value::Object(v)))
        .collect();
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": title,
            "version": ir.source.go_sdk_version.trim_start_matches('v'),
            "description": format!("{title}, generated by databricks-rs (`cargo xtask codegen`) from databricks-sdk-go {} — itself generated from Databricks' OpenAPI spec `{}`. Unofficial; see https://docs.databricks.com/api/{client}/introduction for the reference documentation.", ir.source.go_sdk_version, ir.source.openapi_sha),
            "license": {"name": "Apache-2.0", "identifier": "Apache-2.0"},
            "x-databricks-go-sdk-version": ir.source.go_sdk_version,
            "x-databricks-openapi-sha": ir.source.openapi_sha,
        },
        "servers": servers,
        "security": [{"bearerAuth": []}, {"oauth2": ["all-apis"]}],
        "tags": tags,
        "paths": path_objs,
        "components": {
            "schemas": schemas,
            "responses": {
                "Error": {"description": "Error.", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Error"}}}}
            },
            "securitySchemes": {
                "bearerAuth": {"type": "http", "scheme": "bearer", "description": "Personal access token or OAuth access token."},
                "oauth2": {"type": "oauth2", "flows": {"clientCredentials": {"tokenUrl": token_url, "scopes": {"all-apis": "All APIs"}}}}
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_formats() {
        assert_eq!(
            resource_format("Path. Format: projects/{project_id}/branches/{branch_id}"),
            Some("projects/{project_id}/branches/{branch_id}".into())
        );
        assert_eq!(
            resource_format("Format: `workspaces/{w}/indexes/{i}`"),
            Some("workspaces/{w}/indexes/{i}".into())
        );
        assert_eq!(
            resource_format("Format: \"catalogs/{catalog_id}\"."),
            Some("catalogs/{catalog_id}".into())
        );
        assert_eq!(resource_format("no format"), None);
    }
}
