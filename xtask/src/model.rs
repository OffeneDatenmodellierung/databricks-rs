//! Rust shapes for IR types and fields, shared by the model and service
//! emitters.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::ir::{Field, Ir, TypeDef, TypeRef};
use crate::names;

/// Index over every type in the IR.
pub struct Types<'a> {
    pub by_key: HashMap<(String, String), &'a TypeDef>,
    /// (pkg, struct, field wire name) edges that must be boxed.
    pub boxed: BTreeSet<(String, String, String)>,
}

impl<'a> Types<'a> {
    pub fn new(ir: &'a Ir) -> Self {
        let mut by_key = HashMap::new();
        for (pkg, p) in &ir.packages {
            for t in &p.types {
                by_key.insert((pkg.clone(), t.name.clone()), t);
            }
        }
        let boxed = find_cycles(ir);
        Self { by_key, boxed }
    }

    pub fn get(&self, r: &TypeRef) -> Option<&'a TypeDef> {
        self.by_key.get(&(r.pkg.clone(), r.name.clone())).copied()
    }

    pub fn fields(&self, r: &TypeRef) -> &'a [Field] {
        self.get(r)
            .and_then(|t| t.fields.as_deref())
            .unwrap_or_default()
    }
}

/// Edges `A.f -> B` (non-collection references) inside a strongly connected
/// component need `Box` for the type to have a finite size.
#[allow(clippy::items_after_statements)] // Tarjan's helpers read best inline
fn find_cycles(ir: &Ir) -> BTreeSet<(String, String, String)> {
    type Node = (String, String);
    let mut graph: BTreeMap<Node, Vec<(String, Node)>> = BTreeMap::new();
    for (pkg, p) in &ir.packages {
        for t in &p.types {
            let node = (pkg.clone(), t.name.clone());
            let edges = graph.entry(node).or_default();
            for f in t.fields.as_deref().unwrap_or_default() {
                if f.ty.kind == "ref" {
                    edges.push((f.name.clone(), (f.ty.pkg.clone(), f.ty.name.clone())));
                }
            }
        }
    }
    // Tarjan's SCC.
    struct St<'g> {
        g: &'g BTreeMap<Node, Vec<(String, Node)>>,
        index: HashMap<Node, usize>,
        low: HashMap<Node, usize>,
        on: BTreeSet<Node>,
        stack: Vec<Node>,
        next: usize,
        comp: HashMap<Node, usize>,
        ncomp: usize,
    }
    fn visit(s: &mut St<'_>, v: &Node) {
        s.index.insert(v.clone(), s.next);
        s.low.insert(v.clone(), s.next);
        s.next += 1;
        s.stack.push(v.clone());
        s.on.insert(v.clone());
        for (_, w) in s.g.get(v).cloned().unwrap_or_default() {
            if !s.g.contains_key(&w) {
                continue;
            }
            if !s.index.contains_key(&w) {
                visit(s, &w);
                let lw = s.low[&w];
                let lv = s.low.get_mut(v).expect("visited");
                *lv = (*lv).min(lw);
            } else if s.on.contains(&w) {
                let iw = s.index[&w];
                let lv = s.low.get_mut(v).expect("visited");
                *lv = (*lv).min(iw);
            }
        }
        if s.low[v] == s.index[v] {
            loop {
                let w = s.stack.pop().expect("non-empty");
                s.on.remove(&w);
                s.comp.insert(w.clone(), s.ncomp);
                if &w == v {
                    break;
                }
            }
            s.ncomp += 1;
        }
    }
    let mut st = St {
        g: &graph,
        index: HashMap::new(),
        low: HashMap::new(),
        on: BTreeSet::new(),
        stack: Vec::new(),
        next: 0,
        comp: HashMap::new(),
        ncomp: 0,
    };
    for v in graph.keys() {
        if !st.index.contains_key(v) {
            visit(&mut st, v);
        }
    }
    let mut out = BTreeSet::new();
    for (v, edges) in &graph {
        for (field, w) in edges {
            if st.comp.contains_key(v) && st.comp.get(v) == st.comp.get(w) {
                out.insert((v.0.clone(), v.1.clone(), field.clone()));
            }
        }
    }
    out
}

/// Crate name for a package (`community-databricks-sdk-compute`).
pub fn crate_name(pkg: &str) -> String {
    format!("community-databricks-sdk-{pkg}")
}

/// Crate identifier for a package (`community_databricks_sdk_compute`).
pub fn crate_ident(pkg: &str) -> String {
    format!("community_databricks_sdk_{pkg}")
}

/// Rust type for a reference, relative to `from_pkg`.
pub fn rust_type(r: &TypeRef, from_pkg: &str) -> String {
    match r.kind.as_str() {
        "string" | "timestamp" | "duration" | "field_mask" => "String".into(),
        "bool" => "bool".into(),
        "int" | "int64" => "i64".into(),
        "float64" => "f64".into(),
        "list" => format!("Vec<{}>", rust_type(elem(r), from_pkg)),
        "map" => format!(
            "::std::collections::BTreeMap<String, {}>",
            rust_type(elem(r), from_pkg)
        ),
        "ref" if r.pkg == from_pkg => r.name.clone(),
        "ref" => format!("::{}::{}", crate_ident(&r.pkg), r.name),
        "binary" => "::community_databricks_core::http::Binary".into(),
        // any
        _ => "::serde_json::Value".into(),
    }
}

fn elem(r: &TypeRef) -> &TypeRef {
    r.elem.as_deref().expect("list/map has elem")
}

/// How a field is represented in Rust.
#[derive(Debug, Clone)]
pub struct FieldInfo {
    pub ident: String,
    pub setter: String,
    /// The value type (without Option/Box).
    pub inner: String,
    /// `Vec`/`BTreeMap` (never wrapped in Option).
    pub collection: bool,
    pub optional: bool,
    pub boxed: bool,
    pub kind: String,
    /// Element kind for lists and maps.
    pub elem_kind: String,
    pub json: String,
}

impl FieldInfo {
    /// The declared field type.
    pub fn declared(&self) -> String {
        let t = if self.boxed {
            format!("Box<{}>", self.inner)
        } else {
            self.inner.clone()
        };
        if self.optional && !self.collection {
            format!("Option<{t}>")
        } else {
            t
        }
    }

    /// Expression converting `expr` (this field, by reference) into an owned
    /// `String` for a path or waiter parameter.
    pub fn to_string_expr(&self, expr: &str) -> String {
        if self.optional && !self.collection {
            format!("{expr}.as_ref().map(ToString::to_string).unwrap_or_default()")
        } else {
            format!("{expr}.to_string()")
        }
    }
}

/// Per-struct field infos with de-duplicated identifiers.
pub fn field_infos(types: &Types<'_>, pkg: &str, t: &TypeDef) -> Vec<FieldInfo> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    t.fields
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|f| {
            let mut base = names::snake(&f.name);
            let n = seen.entry(base.clone()).or_default();
            *n += 1;
            if *n > 1 {
                base = format!("{base}_{n}");
            }
            let collection = matches!(f.ty.kind.as_str(), "list" | "map");
            FieldInfo {
                ident: names::ident(&base),
                setter: format!("with_{base}"),
                inner: rust_type(&f.ty, pkg),
                collection,
                // A binary body defaults to empty rather than being optional.
                optional: !f.required && f.ty.kind != "binary",
                boxed: types
                    .boxed
                    .contains(&(pkg.to_owned(), t.name.clone(), f.name.clone())),
                kind: f.ty.kind.clone(),
                elem_kind: f
                    .ty
                    .elem
                    .as_ref()
                    .map(|e| e.kind.clone())
                    .unwrap_or_default(),
                json: f.json.clone(),
            }
        })
        .collect()
}

/// Field info for the field with wire name `wire` on type `r`.
pub fn find_field(types: &Types<'_>, r: &TypeRef, wire: &str) -> Option<FieldInfo> {
    let t = types.get(r)?;
    let fields = t.fields.as_deref().unwrap_or_default();
    let idx = fields.iter().position(|f| f.name == wire)?;
    field_infos(types, &r.pkg, t).into_iter().nth(idx)
}
