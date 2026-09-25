//! Service (API struct, methods, pagination, waiters) emission.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::docs;
use crate::ir::{Method, Service, TypeRef, Waiter};
use crate::model::{FieldInfo, Types, find_field, rust_type};
use crate::names;

/// Per-method overrides (hand-written wrappers in `src/ext`).
pub type Overrides = BTreeMap<String, String>;

pub fn struct_name(s: &Service) -> String {
    format!("{}Api", s.name)
}

pub fn emit(
    types: &Types<'_>,
    svc: &Service,
    children: &[&Service],
    overrides: &Overrides,
    unsupported: &mut Vec<String>,
) -> String {
    let pkg = &svc.package;
    let name = struct_name(svc);
    let mut out = String::new();
    let doc = if svc.doc.is_empty() {
        format!("{} API.", svc.name)
    } else {
        svc.doc.clone()
    };
    out.push_str(&docs::render(&doc, ""));
    let emitted = svc
        .methods
        .as_deref()
        .unwrap_or_default()
        .iter()
        .any(|m| m.unsupported.is_empty())
        || !children.is_empty();
    let allow = if emitted {
        ""
    } else {
        "    #[allow(dead_code)]\n"
    };
    let _ = writeln!(
        out,
        "#[derive(Debug, Clone)]\npub struct {name} {{\n{allow}    api: ApiClient,\n}}\n\nimpl {name} {{"
    );
    let _ = writeln!(
        out,
        "    /// Wrap an [`ApiClient`].\n    #[must_use]\n    pub fn new(api: ApiClient) -> Self {{\n        Self {{ api }}\n    }}\n"
    );
    for child in children {
        let _ = write!(
            out,
            "{}    #[must_use]\n    pub fn {}(&self) -> {} {{\n        {}::new(self.api.clone())\n    }}\n\n",
            docs::render(&docs::summary(&child.doc), "    "),
            names::method_ident(&child.accessor),
            struct_name(child),
            struct_name(child)
        );
    }
    let waiters: BTreeMap<&str, &Waiter> = svc
        .waiters
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|w| (w.name.as_str(), w))
        .collect();
    let mut waiter_structs = String::new();
    for m in svc.methods.as_deref().unwrap_or_default() {
        let struct_path = m.path.as_deref().unwrap_or_default().iter().any(|p| {
            !p.field.is_empty()
                && m.request.as_ref().is_some_and(|r| {
                    types.fields(r).iter().any(|f| {
                        f.name == p.field
                            && f.ty.kind == "ref"
                            && types.get(&f.ty).is_some_and(|t| t.kind == "struct")
                    })
                })
        });
        if struct_path {
            unsupported.push(format!(
                "{}.{}.{}: struct-typed path parameter",
                pkg, svc.name, m.name
            ));
            continue;
        }
        if !m.unsupported.is_empty() {
            unsupported.push(format!(
                "{}.{}.{}: {}",
                pkg, svc.name, m.name, m.unsupported
            ));
            continue;
        }
        let key = format!("{}.{}.{}", pkg, svc.name, m.name);
        let mut fname = names::method_ident(&m.name);
        if let Some(renamed) = overrides.get(&key) {
            fname.clone_from(renamed);
        }
        emit_method(&mut out, types, svc, m, &fname, &waiters);
    }
    for w in waiters.values() {
        emit_waiter_fn(&mut out, types, svc, w);
        emit_waiter_struct(&mut waiter_structs, types, svc, w);
    }
    out.push_str("}\n\n");
    out.push_str(&waiter_structs);
    out
}

fn req_ty(m: &Method, pkg: &str) -> Option<String> {
    m.request.as_ref().map(|r| rust_type(r, pkg))
}

fn resp_ty(m: &Method, pkg: &str) -> String {
    m.response
        .as_ref()
        .map_or_else(|| "()".to_owned(), |r| rust_type(r, pkg))
}

/// Statements building `call` for one operation.
#[allow(clippy::many_single_char_names)]
fn build_call(types: &Types<'_>, svc: &Service, m: &Method) -> String {
    let mut s = String::new();
    // Path.
    let mut fmt = String::new();
    let mut args = Vec::new();
    for part in m.path.as_deref().unwrap_or_default() {
        if part.account_id {
            fmt.push_str("{}");
            args.push("path_param(self.api.account_id()?, false)".to_owned());
        } else if !part.field.is_empty() {
            let f = m
                .request
                .as_ref()
                .and_then(|r| find_field(types, r, &part.field))
                .expect("path field exists on request");
            fmt.push_str("{}");
            args.push(format!(
                "path_param(&{}, {})",
                f.to_string_expr(&format!("request.{}", f.ident)),
                part.multi_segment
            ));
        } else {
            fmt.push_str(&part.lit.replace('{', "{{").replace('}', "}}"));
        }
    }
    if args.is_empty() {
        let _ = writeln!(s, "        let path = String::from({fmt:?});");
    } else {
        let _ = writeln!(
            s,
            "        let path = format!({fmt:?}, {});",
            args.join(", ")
        );
    }
    let verb = m.verb.as_str();
    let _ = write!(s, "        let MUT_call = Call::new(Method::{verb}, path)");
    if m.workspace_header {
        s.push_str(".workspace()");
    }
    s.push_str(";\n");
    let _ = svc;
    let body_verb = matches!(verb, "POST" | "PUT" | "PATCH");
    if let Some(r) = &m.request {
        let t = types.get(r).expect("request type");
        let fields = t.fields.as_deref().unwrap_or_default();
        let infos = crate::model::field_infos(types, &r.pkg, t);
        // Query string.
        let mut q = Vec::new();
        if body_verb {
            for eq in m.explicit_query.as_deref().unwrap_or_default() {
                if let Some(f) = find_field(types, r, &eq.field) {
                    q.push((eq.name.clone(), f));
                }
            }
        } else {
            for (f, info) in fields.iter().zip(&infos) {
                if !f.query.is_empty() {
                    q.push((f.query.clone(), info.clone()));
                }
            }
        }
        for (name, f) in &q {
            let _ = writeln!(
                s,
                "        call = call.query(query::field({name:?}, &request.{})?);",
                f.ident
            );
        }
        if body_verb {
            if m.body_field.is_empty() {
                s.push_str("        call = call.json(&request)?;\n");
            } else {
                let f = find_field(types, r, &m.body_field).expect("body field");
                let _ = writeln!(s, "        call = call.json(&request.{})?;", f.ident);
            }
        }
    }
    let mutated = s.contains("        call = call.");
    s.replace("MUT_call", if mutated { "mut call" } else { "call" })
}

fn send_expr(m: &Method, pkg: &str) -> String {
    if m.response.is_none() {
        "self.api.send::<::serde::de::IgnoredAny>(call).await.map(|_| ())".to_owned()
    } else {
        format!("self.api.send::<{}>(call).await", resp_ty(m, pkg))
    }
}

fn emit_method(
    out: &mut String,
    types: &Types<'_>,
    svc: &Service,
    m: &Method,
    fname: &str,
    waiters: &BTreeMap<&str, &Waiter>,
) {
    let pkg = &svc.package;
    let doc = docs::render_or(&m.doc, &format!("`{}`.", m.name), "    ");
    let path_doc = format!(
        "    ///\n    /// `{} {}`\n",
        m.verb,
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
            .collect::<String>()
    );
    let req_param = req_ty(m, pkg).map_or_else(String::new, |t| format!(", request: {t}"));
    let body_uses_request = build_call(types, svc, m).contains("request");
    let req_unused = if m.request.is_some()
        && !body_uses_request
        && m.wait.as_ref().is_none_or(|w| w.from_response)
    {
        "        let _ = request;\n"
    } else {
        ""
    };

    if let Some(pg) = &m.pagination {
        let item = rust_type(&pg.item_type, pkg);
        let req_t = req_ty(m, pkg).unwrap_or_else(|| "()".into());
        let resp_t = resp_ty(m, pkg);
        // One page.
        let page_fn = format!("{fname}_page");
        let _ = write!(
            out,
            "    /// One page of [`{fname}`](Self::{fname}).\n{path_doc}    pub async fn {page_fn}(&self{req_param}) -> ::databricks_core::Result<{resp_t}> {{\n{req_unused}{}        {}\n    }}\n\n",
            build_call(types, svc, m),
            send_expr(m, pkg)
        );
        // Stream.
        let step = pagination_step(types, m);
        let (req_in, req_param_all, fetch) = if m.request.is_some() {
            (
                "request",
                format!(", request: {req_t}"),
                format!(
                    "move |req: &{req_t}| {{\n                let this = Clone::clone(&this);\n                let req = req.clone();\n                async move {{ this.{page_fn}(req).await }}\n            }}"
                ),
            )
        } else {
            (
                "()",
                String::new(),
                format!(
                    "move |_req: &()| {{\n                let this = Clone::clone(&this);\n                async move {{ this.{page_fn}().await }}\n            }}"
                ),
            )
        };
        let _ = write!(
            out,
            "{doc}{path_doc}    ///\n    /// Returns a lazily paginated stream.\n    #[must_use]\n    pub fn {fname}(&self{req_param_all}) -> Paged<'static, {item}> {{\n        let this = Clone::clone(self);\n        paging::paginate(\n            {req_in},\n            {fetch},\n            {step},\n        )\n    }}\n\n"
        );
        let req_in_all = req_param_all;
        // list_all
        let call_all = if m.request.is_some() {
            format!("self.{fname}(request)")
        } else {
            format!("self.{fname}()")
        };
        let _ = write!(
            out,
            "    /// Every page of [`{fname}`](Self::{fname}), collected.\n    pub async fn {fname}_all(&self{req_in_all}) -> ::databricks_core::Result<Vec<{item}>> {{\n        paging::collect({call_all}).await\n    }}\n\n"
        );
        return;
    }

    let mut ret = if m.response.is_some() {
        resp_ty(m, pkg)
    } else {
        "()".to_owned()
    };
    let mut body = build_call(types, svc, m);
    let mut tail = send_expr(m, pkg);
    if let Some(wb) = &m.wait
        && let Some(w) = waiters.get(wb.waiter.as_str())
    {
        let wname = format!("Wait{}", w.name);
        let param_expr = binding_expr(types, m, wb);
        if !wb.from_response {
            let _ = writeln!(body, "        let param = {param_expr};");
        }
        ret = format!("{wname}<{ret}>");
        let param = if wb.from_response {
            param_expr
        } else {
            "param".to_owned()
        };
        tail = format!(
            "let response = {tail}?;\n        let param = {param};\n        Ok({wname} {{\n            api: Clone::clone(self),\n            {pf}: param,\n            response,\n            timeout: ::std::time::Duration::from_secs({secs}),\n            on_progress: None,\n        }})",
            pf = names::ident(&names::snake(&w.param)),
            secs = wb.timeout_minutes * 60
        );
    }
    let _ = write!(
        out,
        "{doc}{path_doc}    pub async fn {fname}(&self{req_param}) -> ::databricks_core::Result<{ret}> {{\n{req_unused}{body}        {tail}\n    }}\n\n"
    );
}

/// Expression for the waiter parameter from the request or response.
fn binding_expr(types: &Types<'_>, m: &Method, wb: &crate::ir::WaitBinding) -> String {
    let (src, owner) = if wb.from_response {
        ("response", m.response.as_ref())
    } else {
        ("request", m.request.as_ref())
    };
    let f = owner
        .and_then(|r| find_field(types, r, &wb.field))
        .expect("wait binding field");
    let access = format!("{src}.{}", f.ident);
    if f.optional && !f.collection {
        format!("{access}.clone().unwrap_or_default()")
    } else {
        format!("{access}.clone()")
    }
}

fn pagination_step(types: &Types<'_>, m: &Method) -> String {
    let pg = m.pagination.as_ref().expect("pagination");
    let resp = m.response.as_ref().expect("paginated response");
    let items = find_field(types, resp, &pg.items).expect("items field");
    let req_t = m.request.as_ref();
    let get_req = |wire: &str| -> FieldInfo {
        find_field(types, req_t.expect("request"), wire).expect("request paging field")
    };
    let get_resp =
        |wire: &str| -> FieldInfo { find_field(types, resp, wire).expect("resp paging field") };
    let items_expr = if items.collection {
        format!("resp.{}", items.ident)
    } else {
        format!("resp.{}.unwrap_or_default()", items.ident)
    };
    match pg.kind.as_str() {
        "token" => {
            let rf = get_resp(&pg.resp_field);
            let qf = get_req(&pg.req_field);
            let token = if rf.optional {
                format!("resp.{}", rf.ident)
            } else {
                format!("Some(resp.{})", rf.ident)
            };
            let set = if qf.optional { "Some(t)" } else { "t" };
            format!(
                "|req, resp| {{\n                let items = {items_expr};\n                let more = paging::next_token({token}, |t| req.{} = {set});\n                (items, more)\n            }}",
                qf.ident
            )
        }
        "offset" | "page" => {
            let rf = get_resp(&pg.resp_field);
            let qf = get_req(&pg.req_field);
            let cur = if rf.optional {
                format!("resp.{}.unwrap_or_default()", rf.ident)
            } else {
                format!("resp.{}", rf.ident)
            };
            let next = if pg.kind == "page" {
                format!("{cur} + 1")
            } else {
                format!("{cur} + i64::try_from(items.len()).unwrap_or(i64::MAX)")
            };
            let set = if qf.optional {
                format!("Some({next})")
            } else {
                next
            };
            format!(
                "|req, resp| {{\n                let items = {items_expr};\n                let more = !items.is_empty();\n                req.{} = {set};\n                (items, more)\n            }}",
                qf.ident
            )
        }
        _ => format!("|_req, resp| ({items_expr}, false)"),
    }
}

fn waiter_param_type(t: &TypeRef) -> (&'static str, &'static str) {
    match t.kind.as_str() {
        "int" | "int64" => ("i64", "i64"),
        _ => ("impl Into<String>", "String"),
    }
}

fn states_const(w: &Waiter) -> String {
    let arr = |v: &[String]| {
        let items: Vec<String> = v.iter().map(|s| format!("{s:?}")).collect();
        format!("&[{}]", items.join(", "))
    };
    format!(
        "wait::States {{ status_path: {}, message_path: {}, targets: {}, failures: {} }}",
        arr(&w.status_path),
        arr(w.message_path.as_deref().unwrap_or_default()),
        arr(&w.targets),
        arr(w.failures.as_deref().unwrap_or_default())
    )
}

fn emit_waiter_fn(out: &mut String, types: &Types<'_>, svc: &Service, w: &Waiter) {
    let pkg = &svc.package;
    let result = rust_type(&w.result, pkg);
    let (ptype, _) = waiter_param_type(&w.param_type);
    let pname = names::ident(&names::snake(&w.param));
    let poll = names::method_ident(&w.poll_method);
    let poll_m = svc
        .methods
        .as_deref()
        .unwrap_or_default()
        .iter()
        .find(|m| m.name == w.poll_method)
        .expect("poll method");
    let req = poll_m.request.as_ref().expect("poll request");
    let req_t = rust_type(req, pkg);
    let pf = find_field(types, req, &w.param).expect("poll param field");
    let conv = if ptype == "i64" { "" } else { ".into()" };
    let targets = w.targets.join(" or ");
    let _ = write!(
        out,
        "    /// Repeatedly calls [`{poll}`](Self::{poll}) until the result reaches {targets}.\n    pub async fn wait_{wname}(\n        &self,\n        {pname}: {ptype},\n        timeout: ::std::time::Duration,\n        on_progress: Option<wait::Progress<{result}>>,\n    ) -> ::databricks_core::Result<{result}> {{\n        let param: {pt} = {pname}{conv};\n        let callback = ::std::sync::Mutex::new(on_progress);\n        let callback = &callback;\n        wait::poll(timeout, || {{\n            let fut = self.{poll}({req_t}::default().{setter}(param.clone()));\n            async move {{\n                let value = fut.await?;\n                if let Some(cb) = callback\n                    .lock()\n                    .unwrap_or_else(::std::sync::PoisonError::into_inner)\n                    .as_mut()\n                {{\n                    cb(&value);\n                }}\n                wait::check_state(value, {states})\n            }}\n        }})\n        .await\n    }}\n\n",
        wname = names::snake(&w.name),
        pt = waiter_param_type(&w.param_type).1,
        setter = pf.setter,
        states = states_const(w),
    );
}

fn emit_waiter_struct(out: &mut String, _types: &Types<'_>, svc: &Service, w: &Waiter) {
    let pkg = &svc.package;
    let result = rust_type(&w.result, pkg);
    let name = format!("Wait{}", w.name);
    let api = struct_name(svc);
    let pname = names::ident(&names::snake(&w.param));
    let (_, pt) = waiter_param_type(&w.param_type);
    let targets = w.targets.join(" or ");
    let _ = write!(
        out,
        "/// Returned by operations that start a long-running change; waits until\n/// the result reaches {targets}.\npub struct {name}<R> {{\n    api: {api},\n    /// The ID being waited on.\n    pub {pname}: {pt},\n    /// The operation's immediate response.\n    pub response: R,\n    timeout: ::std::time::Duration,\n    on_progress: Option<wait::Progress<{result}>>,\n}}\n\nimpl<R: ::std::fmt::Debug> ::std::fmt::Debug for {name}<R> {{\n    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {{\n        f.debug_struct({name:?})\n            .field({pn:?}, &self.{pname})\n            .field(\"response\", &self.response)\n            .field(\"timeout\", &self.timeout)\n            .finish_non_exhaustive()\n    }}\n}}\n\nimpl<R> {name}<R> {{\n    /// Override the default timeout.\n    #[must_use]\n    pub fn timeout(mut self, timeout: ::std::time::Duration) -> Self {{\n        self.timeout = timeout;\n        self\n    }}\n\n    /// Called with the polled value on every poll.\n    #[must_use]\n    pub fn on_progress(mut self, f: impl FnMut(&{result}) + Send + 'static) -> Self {{\n        self.on_progress = Some(Box::new(f));\n        self\n    }}\n\n    /// Wait until the result reaches {targets}.\n    pub async fn wait(self) -> ::databricks_core::Result<{result}> {{\n        self.api\n            .wait_{wname}(self.{pname}, self.timeout, self.on_progress)\n            .await\n    }}\n}}\n\n",
        pn = pname.trim_start_matches("r#"),
        wname = names::snake(&w.name),
    );
}
