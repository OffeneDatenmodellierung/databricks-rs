//! Name lookups over a listed collection (Go's generated `XNameToIdMap`
//! and list-based `GetByX` helpers).
//!
//! Both load every item into memory first, as in Go. Errors are
//! [`Error::Api`] with HTTP status 0 (nothing failed on the wire):
//! `RESOURCE_DOES_NOT_EXIST` when no item has the name (so
//! [`Error::is_missing`] is true), and `INVALID_STATE` for duplicates.

use std::collections::BTreeMap;

use crate::error::{ApiError, Error, Result};

/// Map each item's key to its value; a key seen twice is an error (Go:
/// `duplicate .<Field>: <key>`).
pub fn unique_map<T, V>(
    items: &[T],
    field: &str,
    key: impl Fn(&T) -> String,
    value: impl Fn(&T) -> V,
) -> Result<BTreeMap<String, V>> {
    let mut out = BTreeMap::new();
    for item in items {
        let k = key(item);
        if out.contains_key(&k) {
            return Err(invalid(&format!("duplicate {field}: {k}")));
        }
        out.insert(k, value(item));
    }
    Ok(out)
}

/// The single item whose key is `name`. `what` names the item type in
/// errors (Go: `<What> named '<name>' does not exist`, `there are <n>
/// instances of <What> named '<name>'`).
pub fn single<T>(items: Vec<T>, what: &str, name: &str, key: impl Fn(&T) -> String) -> Result<T> {
    let mut matches: Vec<T> = items.into_iter().filter(|i| key(i) == name).collect();
    match matches.len() {
        0 => Err(ApiError::new(
            0,
            "RESOURCE_DOES_NOT_EXIST",
            &format!("{what} named '{name}' does not exist"),
        )
        .into()),
        1 => Ok(matches.remove(0)),
        n => Err(invalid(&format!(
            "there are {n} instances of {what} named '{name}'"
        ))),
    }
}

fn invalid(message: &str) -> Error {
    ApiError::new(0, "INVALID_STATE", message).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;

    fn items() -> Vec<(String, i64)> {
        vec![("a".into(), 1), ("b".into(), 2)]
    }

    #[test]
    fn maps_keys_to_values_and_rejects_duplicates() {
        let m = unique_map(&items(), ".Name", |i| i.0.clone(), |i| i.1).unwrap();
        assert_eq!(m["a"], 1);
        assert_eq!(m["b"], 2);
        let mut dup = items();
        dup.push(("a".into(), 3));
        let e = unique_map(&dup, ".Name", |i| i.0.clone(), |i| i.1).unwrap_err();
        assert!(e.to_string().contains("duplicate .Name: a"), "{e}");
        assert!(e.is(ErrorKind::InvalidState), "{e}");
    }

    #[test]
    fn single_finds_one_or_explains_why_not() {
        assert_eq!(single(items(), "Thing", "b", |i| i.0.clone()).unwrap().1, 2);
        let e = single(items(), "Thing", "z", |i| i.0.clone()).unwrap_err();
        assert!(e.is_missing(), "{e}");
        assert!(
            e.to_string().contains("Thing named 'z' does not exist"),
            "{e}"
        );
        let mut dup = items();
        dup.push(("a".into(), 3));
        let e = single(dup, "Thing", "a", |i| i.0.clone()).unwrap_err();
        assert!(
            e.to_string()
                .contains("there are 2 instances of Thing named 'a'"),
            "{e}"
        );
    }
}
