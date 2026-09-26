//! Model (struct / enum / alias) emission.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::docs;
use crate::ir::{Package, TypeDef};
use crate::model::{FieldInfo, Types, field_infos};
use crate::names;

pub fn emit(types: &Types<'_>, pkg: &Package) -> String {
    let mut out = String::new();
    for t in &pkg.types {
        match t.kind.as_str() {
            "struct" => emit_struct(&mut out, types, &pkg.name, t),
            "enum" => emit_enum(&mut out, t),
            _ => emit_alias(&mut out, &pkg.name, t),
        }
        out.push('\n');
    }
    out
}

fn emit_alias(out: &mut String, pkg: &str, t: &TypeDef) {
    let target = t.alias.as_ref().map_or_else(
        || "::serde_json::Value".to_owned(),
        |a| crate::model::rust_type(a, pkg),
    );
    out.push_str(&docs::render_or(&t.doc, &format!("`{}`.", t.name), ""));
    let _ = writeln!(out, "pub type {} = {target};", t.name);
}

fn emit_enum(out: &mut String, t: &TypeDef) {
    let values = t.values.as_deref().unwrap_or_default();
    if values.is_empty() {
        out.push_str(&docs::render_or(
            &t.doc,
            &format!("`{}` values.", t.name),
            "",
        ));
        let _ = writeln!(out, "pub type {} = String;", t.name);
        return;
    }
    out.push_str("::community_databricks_core::open_enum! {\n");
    out.push_str(&docs::render_or(
        &t.doc,
        &format!("`{}` values.", t.name),
        "    ",
    ));
    let _ = writeln!(out, "    pub enum {} {{", t.name);
    let mut used: BTreeMap<String, usize> = BTreeMap::new();
    for v in values {
        let mut variant = names::pascal(&v.value);
        if variant == "Unknown" {
            variant = "UnknownValue".into();
        }
        let n = used.entry(variant.clone()).or_default();
        *n += 1;
        if *n > 1 {
            variant = format!("{variant}{n}");
        }
        out.push_str(&docs::render_or(
            &v.doc,
            &format!("`{}`", v.value),
            "        ",
        ));
        let _ = writeln!(out, "        {variant} => {:?},", v.value);
    }
    out.push_str("    }\n}\n");
}

/// Every struct keeps the fields this SDK version doesn't model, so a
/// value read from the API and sent back (read-modify-write) doesn't drop
/// them, and a caller can send a field before the SDK knows it (#11).
const OTHER_FIELD: &str = "    /// Fields not modelled by this SDK version. Kept when read, so a\n    /// read-modify-write round trip never drops them, and sent with a\n    /// request: in the JSON body, or in the query string for GET/DELETE\n    /// and for requests whose body is a single field (that field's own\n    /// `other` carries unknown body fields).\n    #[serde(flatten, default, skip_serializing_if = \"::std::collections::BTreeMap::is_empty\")]\n    pub other: ::std::collections::BTreeMap<String, ::serde_json::Value>,\n";

const OTHER_SETTER: &str = "    /// Set a field this SDK version doesn't model (see `other`).\n    #[must_use]\n    pub fn with_other(mut self, name: impl Into<String>, value: impl Into<::serde_json::Value>) -> Self {\n        self.other.insert(name.into(), value.into());\n        self\n    }\n\n";

fn emit_struct(out: &mut String, types: &Types<'_>, pkg: &str, t: &TypeDef) {
    let fields = t.fields.as_deref().unwrap_or_default();
    let infos = field_infos(types, pkg, t);
    out.push_str(&docs::render_or(&t.doc, &format!("`{}`.", t.name), ""));
    out.push_str(
        "#[derive(Debug, Clone, Default, PartialEq, ::serde::Serialize, ::serde::Deserialize)]\n",
    );
    out.push_str("#[non_exhaustive]\n");
    let _ = writeln!(out, "pub struct {} {{", t.name);
    for (f, info) in fields.iter().zip(&infos) {
        out.push_str(&docs::render_or(&f.doc, &format!("`{}`", f.name), "    "));
        out.push_str(&serde_attr(info));
        let _ = writeln!(out, "    pub {}: {},", info.ident, info.declared());
    }
    out.push_str(OTHER_FIELD);
    out.push_str("}\n\n");

    // Constructor for 1–2 required fields (more would be an easy-to-misorder
    // positional list), and a setter per field.
    let required: Vec<&FieldInfo> = infos.iter().filter(|i| !i.optional).collect();
    let _ = writeln!(out, "impl {} {{", t.name);
    if (1..=2).contains(&required.len()) {
        let params: Vec<String> = required
            .iter()
            .map(|i| format!("{}: {}", i.ident, param_type(i)))
            .collect();
        let inits: Vec<String> = required
            .iter()
            .map(|i| format!("{}: {}", i.ident, convert(i, &i.ident)))
            .collect();
        let _ = writeln!(
            out,
            "    /// A value with the required fields set.\n    #[must_use]\n    pub fn new({}) -> Self {{\n        Self {{ {}, ..Default::default() }}\n    }}\n",
            params.join(", "),
            inits.join(", ")
        );
    }
    out.push_str(OTHER_SETTER);
    for info in &infos {
        let value = convert(info, "value");
        let assign = if info.optional && !info.collection {
            format!("Some({value})")
        } else {
            value
        };
        let _ = writeln!(
            out,
            "    /// Set `{}`.\n    #[must_use]\n    pub fn {}(mut self, value: {}) -> Self {{\n        self.{} = {assign};\n        self\n    }}\n",
            info.ident.trim_start_matches("r#"),
            info.setter,
            param_type(info),
            info.ident
        );
    }
    out.push_str("}\n");
}

/// Setter / constructor parameter type.
fn param_type(i: &FieldInfo) -> String {
    match i.kind.as_str() {
        "bool" | "int" | "int64" | "float64" => i.inner.clone(),
        _ => format!("impl Into<{}>", i.inner),
    }
}

fn convert(i: &FieldInfo, v: &str) -> String {
    let base = match i.kind.as_str() {
        "bool" | "int" | "int64" | "float64" => v.to_owned(),
        _ => format!("{v}.into()"),
    };
    if i.boxed {
        format!("Box::new({base})")
    } else {
        base
    }
}

fn serde_attr(i: &FieldInfo) -> String {
    if i.json.is_empty() {
        return "    #[serde(skip)]\n".into();
    }
    let mut parts = Vec::new();
    // Integers may arrive as numeric strings (databricks-sdk-go #1808) and
    // floats as "NaN"/"Infinity" (#1498).
    let de = match (i.kind.as_str(), i.elem_kind.as_str()) {
        ("int" | "int64", _) if i.optional => Some("opt_i64"),
        ("int" | "int64", _) => Some("i64"),
        ("list", "int" | "int64") => Some("vec_i64"),
        ("map", "int" | "int64") => Some("map_i64"),
        ("float64", _) if i.optional => Some("opt_f64"),
        ("float64", _) => Some("f64"),
        ("list", "float64") => Some("vec_f64"),
        ("map", "float64") => Some("map_f64"),
        _ => None,
    };
    if let Some(f) = de.filter(|_| !i.boxed) {
        parts.push(format!(
            "deserialize_with = \"::community_databricks_core::serde_num::{f}\""
        ));
    }
    if i.ident.trim_start_matches("r#") != i.json {
        parts.push(format!("rename = {:?}", i.json));
    }
    parts.push("default".into());
    if i.optional {
        if i.collection {
            if i.kind == "list" {
                parts.push("skip_serializing_if = \"Vec::is_empty\"".into());
            } else {
                parts.push(
                    "skip_serializing_if = \"::std::collections::BTreeMap::is_empty\"".into(),
                );
            }
        } else {
            parts.push("skip_serializing_if = \"Option::is_none\"".into());
        }
    }
    format!("    #[serde({})]\n", parts.join(", "))
}
