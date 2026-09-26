//! Go doc comments → rustdoc.
//!
//! * Indented blocks (Go's preformatted text) and unlabelled fences become
//!   ```` ```text ```` so rustdoc never tries to compile them as doctests.
//! * `<` and `>` outside code are escaped so `<catalog>.<schema>` renders
//!   instead of being swallowed as HTML.
//! * `[Name]` Go doc links are left as text (the module allows
//!   `rustdoc::broken_intra_doc_links`).

use std::fmt::Write as _;

pub fn render(doc: &str, indent: &str) -> String {
    let mut out = String::new();
    for line in lines(doc) {
        if line.is_empty() {
            let _ = writeln!(out, "{indent}///");
        } else {
            let _ = writeln!(out, "{indent}/// {line}");
        }
    }
    out
}

/// Render with a fallback when the Go doc is empty.
pub fn render_or(doc: &str, fallback: &str, indent: &str) -> String {
    if doc.trim().is_empty() {
        render(fallback, indent)
    } else {
        render(doc, indent)
    }
}

/// First paragraph only (for accessors and summaries).
pub fn summary(doc: &str) -> String {
    doc.split("\n\n")
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned()
}

fn lines(doc: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut in_fence = false;
    let mut in_indent = false;
    for raw in doc.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            if in_indent {
                out.push("```".into());
                in_indent = false;
            }
            if in_fence {
                out.push("```".into());
            } else {
                out.push("```text".into());
            }
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            out.push(line.replace('\t', "    "));
            continue;
        }
        let indented = line.starts_with('\t') || line.starts_with("  ");
        if indented && !trimmed.is_empty() {
            if !in_indent {
                if out.last().is_some_and(|l: &String| !l.is_empty()) {
                    out.push(String::new());
                }
                out.push("```text".into());
                in_indent = true;
            }
            out.push(line.replacen('\t', "", 1).replace('\t', "    "));
            continue;
        }
        if in_indent {
            out.push("```".into());
            in_indent = false;
        }
        out.push(escape(line));
    }
    if in_indent || in_fence {
        out.push("```".into());
    }
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}

/// Escape `<`, `>` and stray backslashes outside inline code spans.
fn escape(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_code = false;
    for c in line.chars() {
        match c {
            '`' => {
                in_code = !in_code;
                out.push(c);
            }
            '<' if !in_code => out.push_str("\\<"),
            '>' if !in_code => out.push_str("\\>"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_go_docs() {
        let go = "Lists tables in <catalog>.<schema>.\n\nExample:\n\n\t{\"a\": 1}\n\tmore\n\nAfter `<x>`.\n```\ncode\n```";
        let r = render(go, "");
        assert!(r.contains("/// Lists tables in \\<catalog\\>.\\<schema\\>."));
        assert!(r.contains("/// ```text\n/// {\"a\": 1}\n/// more\n/// ```"));
        assert!(r.contains("/// After `<x>`."));
        assert!(r.contains("/// ```text\n/// code\n/// ```"));
        assert_eq!(render_or("", "fallback", "    "), "    /// fallback\n");
        assert_eq!(summary("one\ntwo\n\nthree"), "one\ntwo");
    }
}
