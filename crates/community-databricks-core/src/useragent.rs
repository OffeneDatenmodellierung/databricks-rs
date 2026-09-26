//! `User-Agent` construction, matching the Go SDK's format:
//!
//! `<product>/<version> databricks-sdk-rust/<sdk> rust/<rustc> os/<os> [extra…] auth/<type> [cicd/<p>] [runtime/<v>]`

use std::sync::OnceLock;

/// SDK name reported in the user agent.
pub const SDK_NAME: &str = "databricks-sdk-rust";

static PRODUCT: OnceLock<(String, String)> = OnceLock::new();
static EXTRA: OnceLock<Vec<(String, String)>> = OnceLock::new();

/// Set the calling product's name and version (once per process).
/// Returns `false` if it was already set.
pub fn set_product(name: &str, version: &str) -> bool {
    PRODUCT.set((sanitize(name), sanitize(version))).is_ok()
}

/// Add process-wide `key/value` pairs (once per process), for example a
/// partner integration name.
pub fn set_extra(pairs: &[(&str, &str)]) -> bool {
    EXTRA
        .set(
            pairs
                .iter()
                .map(|(k, v)| (sanitize(k), sanitize(v)))
                .collect(),
        )
        .is_ok()
}

/// Replace characters outside `[A-Za-z0-9_.+-]` with `-`.
#[must_use]
pub fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_.+-".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect()
}

const CICD: &[(&str, &[(&str, &str)])] = &[
    ("github", &[("GITHUB_ACTIONS", "true")]),
    ("gitlab", &[("GITLAB_CI", "true")]),
    ("jenkins", &[("JENKINS_URL", "")]),
    ("azure-devops", &[("TF_BUILD", "True")]),
    ("circle", &[("CIRCLECI", "true")]),
    ("travis", &[("TRAVIS", "true")]),
    ("bitbucket", &[("BITBUCKET_BUILD_NUMBER", "")]),
    (
        "google-cloud-build",
        &[
            ("PROJECT_ID", ""),
            ("BUILD_ID", ""),
            ("PROJECT_NUMBER", ""),
            ("LOCATION", ""),
        ],
    ),
    ("aws-code-build", &[("CODEBUILD_BUILD_ARN", "")]),
    ("tf-cloud", &[("TFC_RUN_ID", "")]),
];

/// The first CI/CD provider whose environment variables are all present.
pub fn detect_cicd(env: impl Fn(&str) -> Option<String>) -> Option<&'static str> {
    CICD.iter()
        .find(|(_, vars)| {
            vars.iter()
                .all(|(k, expected)| env(k).is_some_and(|v| expected.is_empty() || v == *expected))
        })
        .map(|(name, _)| *name)
}

/// Environment-derived parts, computed once.
fn environment_parts() -> &'static [(String, String)] {
    static PARTS: OnceLock<Vec<(String, String)>> = OnceLock::new();
    PARTS.get_or_init(|| {
        let env = |k: &str| std::env::var(k).ok();
        let mut v = Vec::new();
        if let Some(p) = detect_cicd(env) {
            v.push(("cicd".to_owned(), p.to_owned()));
        }
        if let Some(r) = env("DATABRICKS_RUNTIME_VERSION").filter(|r| !r.is_empty()) {
            v.push(("runtime".to_owned(), sanitize(&r)));
        }
        v
    })
}

/// Build the header value.
#[must_use]
pub fn build(auth_type: Option<&str>) -> String {
    let (product, version) = PRODUCT
        .get()
        .map_or(("unknown", "0.0.0"), |(p, v)| (p.as_str(), v.as_str()));
    let mut parts = vec![
        format!("{product}/{version}"),
        format!("{SDK_NAME}/{}", crate::VERSION),
        format!("rust/{}", env!("DATABRICKS_RUSTC_VERSION")),
        format!("os/{}", std::env::consts::OS),
    ];
    for (k, v) in EXTRA.get().into_iter().flatten() {
        parts.push(format!("{k}/{v}"));
    }
    if let Some(a) = auth_type {
        parts.push(format!("auth/{a}"));
    }
    for (k, v) in environment_parts() {
        parts.push(format!("{k}/{v}"));
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn detects_cicd_providers() {
        let env = |m: HashMap<&'static str, &'static str>| {
            move |k: &str| m.get(k).map(|v| (*v).to_owned())
        };
        assert_eq!(
            detect_cicd(env(HashMap::from([("GITHUB_ACTIONS", "true")]))),
            Some("github")
        );
        assert_eq!(
            detect_cicd(env(HashMap::from([("GITHUB_ACTIONS", "false")]))),
            None
        );
        assert_eq!(
            detect_cicd(env(HashMap::from([("JENKINS_URL", "x")]))),
            Some("jenkins")
        );
        assert_eq!(detect_cicd(env(HashMap::from([("PROJECT_ID", "p")]))), None);
        assert_eq!(detect_cicd(env(HashMap::new())), None);
    }

    #[test]
    fn builds_go_compatible_string() {
        assert!(set_product("my app", "1.2"));
        assert!(!set_product("again", "9"));
        assert!(set_extra(&[("partner", "acme")]));
        let ua = build(Some("pat"));
        assert!(ua.starts_with("my-app/1.2 databricks-sdk-rust/"), "{ua}");
        assert!(ua.contains(" rust/"));
        assert!(ua.contains(" os/"));
        assert!(ua.contains(" partner/acme auth/pat"), "{ua}");
        assert_eq!(sanitize("a b/c"), "a-b-c");
    }
}
