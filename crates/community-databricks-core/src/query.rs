//! Query-string encoding for GET/DELETE requests.
//!
//! Go serialises the request struct with `go-querystring` and rewrites
//! nested keys from `a[b]` to `a.b` for the proto-style APIs. We do the same
//! from any `Serialize` value: objects nest with `.`, arrays repeat the key,
//! `null` is skipped and booleans/numbers are rendered plainly.

use serde::Serialize;
use serde_json::Value;

use crate::error::{Error, Result};

/// Flatten `value` into `(key, value)` pairs.
pub fn to_pairs<T: Serialize + ?Sized>(value: &T) -> Result<Vec<(String, String)>> {
    let v = serde_json::to_value(value).map_err(|e| Error::json("query parameters", e))?;
    let mut out = Vec::new();
    match v {
        Value::Object(map) => {
            for (k, v) in map {
                flatten(&k, &v, &mut out);
            }
        }
        Value::Null => {}
        other => {
            return Err(Error::Config(format!(
                "query parameters must serialise to an object, got {other}"
            )));
        }
    }
    Ok(out)
}

/// Encode one named query parameter (Go's url-tag serialisation of a single
/// field). `None`, empty strings for optional values, and empty lists are
/// dropped by the caller choosing `Option`/`Vec`; nested objects become
/// `name.child`.
pub fn field<T: Serialize + ?Sized>(name: &str, value: &T) -> Result<Vec<(String, String)>> {
    let v = serde_json::to_value(value).map_err(|e| Error::json("query parameter", e))?;
    let mut out = Vec::new();
    flatten(name, &v, &mut out);
    Ok(out)
}

fn flatten(key: &str, v: &Value, out: &mut Vec<(String, String)>) {
    match v {
        Value::Null => {}
        Value::Bool(b) => out.push((key.to_owned(), b.to_string())),
        Value::Number(n) => out.push((key.to_owned(), n.to_string())),
        Value::String(s) => out.push((key.to_owned(), s.clone())),
        Value::Array(items) => {
            for item in items {
                flatten(key, item, out);
            }
        }
        Value::Object(map) => {
            for (k, v) in map {
                flatten(&format!("{key}.{k}"), v, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nested_and_repeated() {
        let pairs = to_pairs(&json!({
            "page_size": 10,
            "filter_by": {"cluster_states": ["RUNNING", "PENDING"], "is_pinned": true},
            "skip": null
        }))
        .unwrap();
        assert_eq!(
            pairs,
            vec![
                ("filter_by.cluster_states".into(), "RUNNING".into()),
                ("filter_by.cluster_states".into(), "PENDING".into()),
                ("filter_by.is_pinned".into(), "true".into()),
                ("page_size".into(), "10".into()),
            ]
        );
        assert!(to_pairs(&()).unwrap().is_empty());
        assert_eq!(
            field("sort_by", &json!({"field": "NAME"})).unwrap(),
            vec![("sort_by.field".to_owned(), "NAME".to_owned())]
        );
        assert!(field("x", &None::<u8>).unwrap().is_empty());
        assert!(to_pairs(&1).is_err());
    }
}
