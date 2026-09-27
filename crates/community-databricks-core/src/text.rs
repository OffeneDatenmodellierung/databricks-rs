//! Text helpers shared by service crates.

use std::sync::LazyLock;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use regex::Regex;

use crate::error::{Error, Result};

/// Standard base64 (with padding), as the DBFS and Workspace APIs use.
#[must_use]
pub fn base64_encode(data: &[u8]) -> String {
    STANDARD.encode(data)
}

/// Decode standard base64 (with padding).
pub fn base64_decode(data: &str) -> Result<Vec<u8>> {
    STANDARD
        .decode(data)
        .map_err(|e| Error::OperationFailed(format!("invalid base64: {e}")))
}

/// Remove the indentation common to every non-blank line, and drop blank
/// lines, so an indented code string can be sent as a command or notebook
/// (Go: `compute.TrimLeadingWhitespace`). Tabs count as four spaces; each
/// kept line ends with `\n`.
#[must_use]
pub fn trim_leading_whitespace(command: &str) -> String {
    let expanded = command.replace('\t', "    ");
    let lines: Vec<&str> = expanded.split('\n').collect();
    let indent = lines
        .iter()
        .filter_map(|l| l.find(|c: char| c != ' '))
        .min()
        .unwrap_or(0);
    let mut out = String::new();
    for line in lines {
        if line.trim_matches(|c| c == ' ' || c == '\t').is_empty() {
            continue;
        }
        out.push_str(line.get(indent..).unwrap_or(line));
        out.push('\n');
    }
    out
}

static OUT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"Out\[[\d\s]+\]:\s").expect("regex"));
static TAG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]*>").expect("regex"));
static EXCEPTION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r".*Exception:\s+(.*)").expect("regex"));
static EXECUTION_ERROR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"ExecutionError: ([\s\S]*)\n(StatusCode=[0-9]*)\n(StatusDescription=.*)\n")
        .expect("regex")
});
static ERROR_MESSAGE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"ErrorMessage=(.+)\n").expect("regex"));

/// Command output with the REPL's `Out[n]: ` prompts removed (Go:
/// `compute.Results.Text`).
#[must_use]
pub fn strip_out_prompts(text: &str) -> String {
    OUT_RE.replace_all(text, "").into_owned()
}

/// The readable error from a failed command's `summary` and `cause` (Go:
/// `compute.Results.Error`): the exception message from the HTML summary,
/// else an `ExecutionError` or `ErrorMessage=` from the cause, else the
/// summary without markup.
#[must_use]
pub fn command_error(summary: &str, cause: &str) -> String {
    let summary = html_unescape(&TAG_RE.replace_all(summary, ""));
    if let Some(m) = EXCEPTION_RE.captures(&summary) {
        return m[1]
            .replace("; nested exception is:", "")
            .trim_end_matches(' ')
            .to_owned();
    }
    if let Some(m) = EXECUTION_ERROR_RE.captures(cause) {
        return [&m[1], &m[2], &m[3]].join("\n");
    }
    if let Some(m) = ERROR_MESSAGE_RE.captures(cause) {
        return m[1].to_owned();
    }
    summary
}

/// Decode the HTML character references Go's `html.UnescapeString`
/// handles in practice: the five named XML entities and numeric ones.
fn html_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let decoded = rest.find(';').and_then(|end| {
            let entity = &rest[1..end];
            let ch = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" | "#39" => Some('\''),
                _ => entity.strip_prefix('#').and_then(|n| {
                    n.strip_prefix(['x', 'X'])
                        .map_or_else(|| n.parse().ok(), |h| u32::from_str_radix(h, 16).ok())
                        .and_then(char::from_u32)
                }),
            };
            ch.map(|c| (c, end))
        });
        if let Some((c, end)) = decoded {
            out.push(c);
            rest = &rest[end + 1..];
        } else {
            out.push('&');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::{
        base64_decode, base64_encode, command_error, html_unescape, strip_out_prompts,
        trim_leading_whitespace,
    };

    #[test]
    fn strips_out_prompts() {
        assert_eq!(strip_out_prompts("Out[1]: 42"), "42");
        assert_eq!(strip_out_prompts("Out[ 12 ]: x\nOut[3]: y"), "x\ny");
    }

    #[test]
    fn command_errors_like_go() {
        assert_eq!(
            command_error(
                "<div>org.apache.spark.SparkException: Job aborted; nested exception is: boom  </div>",
                ""
            ),
            "Job aborted boom"
        );
        assert_eq!(
            command_error(
                "x",
                "ExecutionError: bad\nthing\nStatusCode=400\nStatusDescription=BadRequest\n"
            ),
            "bad\nthing\nStatusCode=400\nStatusDescription=BadRequest"
        );
        assert_eq!(command_error("x", "ErrorMessage=no table\n"), "no table");
        assert_eq!(
            command_error("<b>a &lt; b &amp;&#65;&#x42;</b>", ""),
            "a < b &AB"
        );
    }

    #[test]
    fn html_unescape_leaves_unknown_entities() {
        assert_eq!(
            html_unescape("&foo; & &quot;q&quot; &apos;"),
            "&foo; & \"q\" '"
        );
        assert_eq!(html_unescape("&#xZZ; &#999999999;"), "&#xZZ; &#999999999;");
    }

    #[test]
    fn base64_round_trips_and_rejects_garbage() {
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert!(base64_decode("not base64!").is_err());
    }

    #[test]
    fn removes_common_indentation_and_blank_lines() {
        let code = "\n    import os\n\n    if True:\n        print(1)\n    ";
        assert_eq!(
            trim_leading_whitespace(code),
            "import os\nif True:\n    print(1)\n"
        );
        assert_eq!(
            trim_leading_whitespace("\tx = 1\n\t\ty = 2"),
            "x = 1\n    y = 2\n"
        );
        assert_eq!(trim_leading_whitespace("a\n  b"), "a\n  b\n");
        assert_eq!(trim_leading_whitespace("   \n\n"), "");
    }
}
