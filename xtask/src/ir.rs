//! The intermediate representation written by `codegen/extract-go`.
//!
//! Every field is deserialised so the schema is documented in one place,
//! even where the Rust emitter doesn't need it.

#![allow(dead_code)]

use std::collections::BTreeMap;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Ir {
    pub source: Source,
    pub services: Vec<Service>,
    #[serde(default)]
    pub skipped: Vec<String>,
    pub packages: BTreeMap<String, Package>,
}

#[derive(Debug, Deserialize)]
pub struct Source {
    pub go_sdk_version: String,
    pub openapi_sha: String,
}

/// `codegen/ir_patches.json`: corrections for upstream spec defects.
#[derive(Debug, Deserialize)]
pub struct Patches {
    /// `package.Type.field` → replacement type.
    #[serde(default)]
    pub field_types: BTreeMap<String, FieldTypePatch>,
}

#[derive(Debug, Deserialize)]
pub struct FieldTypePatch {
    #[serde(rename = "type")]
    pub ty: TypeRef,
    /// Why the patch exists (required, so every patch is explained).
    pub why: String,
}

impl Ir {
    /// Apply `patches`; a patch that no longer matches is an error, so a
    /// fixed upstream spec prompts removing it.
    pub fn apply(&mut self, patches: &Patches) -> Result<(), String> {
        for (key, patch) in &patches.field_types {
            if patch.why.trim().is_empty() {
                return Err(format!("ir_patches: {key} has no `why`"));
            }
            let mut parts = key.splitn(3, '.');
            let (Some(pkg), Some(ty), Some(field)) = (parts.next(), parts.next(), parts.next())
            else {
                return Err(format!("ir_patches: {key} is not package.Type.field"));
            };
            let f = self
                .packages
                .get_mut(pkg)
                .and_then(|p| p.types.iter_mut().find(|t| t.name == ty))
                .and_then(|t| t.fields.as_mut())
                .and_then(|fs| fs.iter_mut().find(|f| f.name == field))
                .ok_or_else(|| format!("ir_patches: {key} not found in spec/ir.json"))?;
            f.ty = patch.ty.clone();
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
pub struct Package {
    pub name: String,
    #[serde(default)]
    pub types: Vec<TypeDef>,
}

#[derive(Debug, Deserialize)]
pub struct TypeDef {
    pub name: String,
    #[serde(default)]
    pub doc: String,
    pub kind: String,
    #[serde(default)]
    pub fields: Option<Vec<Field>>,
    #[serde(default)]
    pub values: Option<Vec<EnumValue>>,
    #[serde(default)]
    pub alias: Option<TypeRef>,
}

#[derive(Debug, Deserialize)]
pub struct Field {
    pub name: String,
    #[serde(default)]
    pub doc: String,
    #[serde(rename = "type")]
    pub ty: TypeRef,
    pub required: bool,
    pub location: String,
    #[serde(default)]
    pub json: String,
    #[serde(default)]
    pub query: String,
}

#[derive(Debug, Deserialize)]
pub struct EnumValue {
    pub value: String,
    #[serde(default)]
    pub doc: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct TypeRef {
    pub kind: String,
    #[serde(default)]
    pub pkg: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub elem: Option<Box<TypeRef>>,
}

#[derive(Debug, Deserialize)]
pub struct Service {
    pub client: String,
    #[serde(default)]
    pub parent: String,
    pub accessor: String,
    pub package: String,
    pub name: String,
    #[serde(default)]
    pub doc: String,
    #[serde(default)]
    pub methods: Option<Vec<Method>>,
    #[serde(default)]
    pub waiters: Option<Vec<Waiter>>,
}

#[derive(Debug, Deserialize)]
pub struct PathPart {
    #[serde(default)]
    pub lit: String,
    #[serde(default)]
    pub field: String,
    #[serde(default)]
    pub account_id: bool,
    #[serde(default)]
    pub multi_segment: bool,
}

#[derive(Debug, Deserialize)]
pub struct QueryParam {
    pub name: String,
    pub field: String,
}

#[derive(Debug, Deserialize)]
pub struct Method {
    pub name: String,
    #[serde(default)]
    pub doc: String,
    pub verb: String,
    #[serde(default)]
    pub path: Option<Vec<PathPart>>,
    #[serde(default)]
    pub request: Option<TypeRef>,
    #[serde(default)]
    pub response: Option<TypeRef>,
    #[serde(default)]
    pub body_field: String,
    #[serde(default)]
    pub explicit_query: Option<Vec<QueryParam>>,
    pub workspace_header: bool,
    /// `Accept` header Go sends (empty for none).
    #[serde(default)]
    pub accept: String,
    #[serde(default)]
    pub pagination: Option<Pagination>,
    #[serde(default)]
    pub wait: Option<WaitBinding>,
    /// Request fields the Go SDK fills in before the call.
    #[serde(default)]
    pub request_init: Option<Vec<FieldInit>>,
    #[serde(default)]
    pub unsupported: String,
}

impl Method {
    /// A binary request or response body, sent or returned as
    /// `community_databricks_core::http::Binary`. The extractor flags these
    /// as `unsupported` because Go uses `io.ReadCloser` for them.
    pub fn is_binary(&self) -> bool {
        self.unsupported.starts_with("binary")
    }
}

/// One request field set before a call (see `codegen/extract-go`).
#[derive(Debug, Deserialize)]
pub struct FieldInit {
    /// Wire name.
    pub field: String,
    /// `always`, or `unset` (only when the caller left it empty).
    pub when: String,
    /// Go literal (an integer for every current use).
    #[serde(default)]
    pub value: String,
    /// Fill with a random UUID v4.
    #[serde(default)]
    pub uuid: bool,
}

#[derive(Debug, Deserialize)]
pub struct Pagination {
    pub kind: String,
    pub items: String,
    pub item_type: TypeRef,
    #[serde(default)]
    pub resp_field: String,
    #[serde(default)]
    pub req_field: String,
    #[serde(default)]
    pub stop_on_empty: bool,
}

#[derive(Debug, Deserialize)]
pub struct WaitBinding {
    pub waiter: String,
    pub from_response: bool,
    pub field: String,
    pub timeout_minutes: u64,
}

#[derive(Debug, Deserialize)]
pub struct Waiter {
    pub name: String,
    pub poll_method: String,
    pub param: String,
    pub param_type: TypeRef,
    pub result: TypeRef,
    pub status_path: Vec<String>,
    #[serde(default)]
    pub message_path: Option<Vec<String>>,
    pub targets: Vec<String>,
    #[serde(default)]
    pub failures: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ir() -> Ir {
        serde_json::from_value(serde_json::json!({
            "source": {"go_sdk_version": "v0", "openapi_sha": "x"},
            "services": [],
            "packages": {"sql": {"name": "sql", "types": [{
                "name": "Req", "kind": "struct",
                "fields": [{"name": "object_id", "type": {"kind": "ref", "pkg": "sql", "name": "Obj"},
                            "required": true, "location": "path"}]
            }]}}
        }))
        .unwrap()
    }

    fn patches(key: &str, why: &str) -> Patches {
        serde_json::from_value(serde_json::json!({
            "field_types": {key: {"type": {"kind": "string"}, "why": why}}
        }))
        .unwrap()
    }

    #[test]
    fn patches_replace_the_field_type() {
        let mut ir = ir();
        ir.apply(&patches("sql.Req.object_id", "upstream bug"))
            .unwrap();
        let f = &ir.packages["sql"].types[0].fields.as_ref().unwrap()[0];
        assert_eq!(f.ty.kind, "string");
    }

    #[test]
    fn stale_or_unexplained_patches_are_errors() {
        let e = ir().apply(&patches("sql.Req.gone", "x")).unwrap_err();
        assert!(e.contains("not found"), "{e}");
        let e = ir().apply(&patches("sql.Req", "x")).unwrap_err();
        assert!(e.contains("package.Type.field"), "{e}");
        let e = ir().apply(&patches("sql.Req.object_id", " ")).unwrap_err();
        assert!(e.contains("no `why`"), "{e}");
    }
}
