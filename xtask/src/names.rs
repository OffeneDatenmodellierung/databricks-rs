//! Identifier conversion.

const KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for",
    "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where",
    "while", "async", "await", "dyn", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "typeof", "unsized", "virtual", "yield", "try", "gen",
];

/// `fooBar`, `FooBar`, `foo-bar`, `$ref`, `HTTPServer` → `foo_bar`, …, `http_server`.
pub fn snake(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_ascii_alphanumeric() {
            if !out.is_empty() && !out.ends_with('_') {
                out.push('_');
            }
            continue;
        }
        if c.is_ascii_uppercase() && i > 0 {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(char::is_ascii_lowercase);
            if ((prev.is_ascii_lowercase() || prev.is_ascii_digit())
                || (prev.is_ascii_uppercase() && next_lower))
                && !out.ends_with('_')
                && !out.is_empty()
            {
                out.push('_');
            }
        }
        out.push(c.to_ascii_lowercase());
    }
    let out = out.trim_matches('_').to_owned();
    if out.is_empty() {
        return "value".to_owned();
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        return format!("n_{out}");
    }
    out
}

/// A field or method identifier (raw identifier for keywords).
pub fn ident(snake_name: &str) -> String {
    match snake_name {
        // Not allowed as raw identifiers.
        "self" | "super" | "crate" | "Self" => format!("{snake_name}_"),
        k if KEYWORDS.contains(&k) => format!("r#{k}"),
        other => other.to_owned(),
    }
}

/// Method names use a trailing underscore instead of raw identifiers so
/// callers don't have to write `r#move`.
pub fn method_ident(go_name: &str) -> String {
    let s = snake(go_name);
    // Also avoid shadowing trait methods every service derives or uses.
    if KEYWORDS.contains(&s.as_str()) || matches!(s.as_str(), "self" | "clone" | "fmt" | "new") {
        format!("{s}_")
    } else {
        s
    }
}

/// `SOME_VALUE`, `some-value`, `Standard_DS3_v2`, `1.0` → `SomeValue`, …
pub fn pascal(s: &str) -> String {
    let mut out = String::new();
    let mut upper = true;
    let all_upper = !s.chars().any(|c| c.is_ascii_lowercase());
    for c in s.chars() {
        if !c.is_ascii_alphanumeric() {
            upper = true;
            continue;
        }
        if upper {
            out.push(c.to_ascii_uppercase());
            upper = false;
        } else if all_upper {
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    if out.is_empty() {
        return "Empty".to_owned();
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, 'V');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions() {
        assert_eq!(snake("RunNow"), "run_now");
        assert_eq!(snake("startIndex"), "start_index");
        assert_eq!(snake("HTTPServer"), "http_server");
        assert_eq!(snake("content-length"), "content_length");
        assert_eq!(snake("$ref"), "ref");
        assert_eq!(snake("Resources"), "resources");
        assert_eq!(snake("GetOpenApi"), "get_open_api");
        assert_eq!(snake("ListS3Buckets"), "list_s3_buckets");
        assert_eq!(ident("type"), "r#type");
        assert_eq!(ident("self"), "self_");
        assert_eq!(method_ident("Move"), "move_");
        assert_eq!(method_ident("Clone"), "clone_");
        assert_eq!(pascal("DATA_SECURITY_MODE_AUTO"), "DataSecurityModeAuto");
        assert_eq!(pascal("Standard_DS3_v2"), "StandardDS3V2");
        assert_eq!(pascal("1.0"), "V10");
        assert_eq!(pascal("text/plain"), "TextPlain");
        assert_eq!(pascal(""), "Empty");
    }
}
