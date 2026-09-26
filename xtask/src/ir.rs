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
