//! `~/.databrickscfg` loader, matching Go's `configFileLoader`.
//!
//! * Skipped entirely when no profile is requested and `host` or any auth
//!   attribute is already set.
//! * A missing file is not an error.
//! * The profile is the requested one, else `[__settings__] default_profile`,
//!   else `DEFAULT` (a missing `DEFAULT` section is silently ignored; a
//!   missing explicitly-requested profile is an error).

use std::path::PathBuf;

use ini::{Ini, ParseOption, Properties};

use super::Config;
use super::attrs::{ATTRIBUTES, Source};
use crate::error::{Error, Result};

const SETTINGS: &str = "__settings__";

pub(crate) fn default_path(configured: Option<&str>, home: Option<PathBuf>) -> Option<PathBuf> {
    let raw = configured
        .filter(|p| !p.is_empty())
        .unwrap_or("~/.databrickscfg");
    if let Some(rest) = raw.strip_prefix('~') {
        let home = home?;
        let rest = rest.trim_start_matches(['/', '\\']);
        return Some(if rest.is_empty() {
            home
        } else {
            home.join(rest)
        });
    }
    Some(PathBuf::from(raw))
}

pub(crate) fn load(cfg: &mut Config, home: Option<PathBuf>) -> Result<()> {
    let any_auth = ATTRIBUTES.iter().any(|a| a.auth.is_some() && a.is_set(cfg));
    let host_set = cfg.host.as_deref().is_some_and(|h| !h.is_empty())
        || cfg.other.contains_key("azure_workspace_resource_id");
    if cfg.profile.as_deref().unwrap_or_default().is_empty() && (any_auth || host_set) {
        return Ok(());
    }
    let Some(path) = default_path(cfg.config_file.as_deref(), home) else {
        tracing::debug!("cannot determine home directory; skipping config file");
        return Ok(());
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(path = %path.display(), "config file not found");
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    let opts = ParseOption {
        enabled_escape: false,
        ..ParseOption::default()
    };
    let ini = Ini::load_from_str_opt(&text, opts)
        .map_err(|e| Error::Config(format!("cannot parse config file: {e}")))?;
    let (profile, fallback) = resolve_profile(cfg.profile.as_deref(), &ini)
        .map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;

    let Some(section) = profile_section(&ini, &profile) else {
        if fallback {
            tracing::debug!(path = %path.display(), "no {profile} profile configured");
            return Ok(());
        }
        return Err(Error::Config(format!(
            "{} has no {profile} profile configured",
            path.display()
        )));
    };
    tracing::debug!(path = %path.display(), %profile, "loading profile");
    let source = Source::File(path.display().to_string());
    for attr in ATTRIBUTES {
        if attr.is_set(cfg) {
            continue;
        }
        if let Some(v) = section.get(attr.name) {
            let v = strip_inline_comment(v);
            if v.is_empty() {
                continue;
            }
            (attr.set)(cfg, v.to_owned())
                .map_err(|e| Error::Config(format!("{} {profile} profile: {e}", path.display())))?;
            cfg.sources.insert(attr.name, source.clone());
        }
    }
    cfg.profile = Some(profile);
    Ok(())
}

/// `DEFAULT` also covers keys written before any section header (as in Go's
/// `ini` package).
fn profile_section<'a>(ini: &'a Ini, profile: &str) -> Option<&'a Properties> {
    let named = ini.section(Some(profile)).filter(|s| !s.is_empty());
    if named.is_some() || profile != "DEFAULT" {
        return named;
    }
    ini.section(None::<String>).filter(|s| !s.is_empty())
}

/// Go loads with `SpaceBeforeInlineComment: true`: `value # note` → `value`.
fn strip_inline_comment(v: &str) -> &str {
    let cut = [" #", " ;", "\t#", "\t;"]
        .iter()
        .filter_map(|m| v.find(m))
        .min()
        .unwrap_or(v.len());
    v[..cut].trim()
}

fn resolve_profile(requested: Option<&str>, ini: &Ini) -> Result<(String, bool), String> {
    let reserved =
        || format!("{SETTINGS} is a reserved section name and cannot be used as a profile");
    if let Some(p) = requested.filter(|p| !p.is_empty()) {
        if p == SETTINGS {
            return Err(reserved());
        }
        return Ok((p.to_owned(), false));
    }
    if let Some(v) = ini
        .section(Some(SETTINGS))
        .and_then(|s| s.get("default_profile"))
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        if v == SETTINGS {
            return Err(reserved());
        }
        return Ok((v.to_owned(), false));
    }
    Ok(("DEFAULT".to_owned(), true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_expansion() {
        let home = Some(PathBuf::from("/home/u"));
        assert_eq!(
            default_path(None, home.clone()),
            Some(PathBuf::from("/home/u/.databrickscfg"))
        );
        assert_eq!(
            default_path(Some("~/x/cfg"), home.clone()),
            Some(PathBuf::from("/home/u/x/cfg"))
        );
        assert_eq!(
            default_path(Some("~"), home),
            Some(PathBuf::from("/home/u"))
        );
        assert_eq!(
            default_path(Some("/etc/cfg"), None),
            Some(PathBuf::from("/etc/cfg"))
        );
        assert_eq!(default_path(None, None), None);
    }

    #[test]
    fn inline_comments() {
        assert_eq!(strip_inline_comment("abc # note"), "abc");
        assert_eq!(
            strip_inline_comment("abc#not-a-comment"),
            "abc#not-a-comment"
        );
        assert_eq!(strip_inline_comment("  v  "), "v");
    }
}
