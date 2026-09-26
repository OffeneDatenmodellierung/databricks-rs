//! `databricks-cli` auth: tokens from `databricks auth token`.
//!
//! Go: `u2mCredentials` + `CliTokenSource`. Interactive U2M login belongs
//! to the Databricks CLI (databricks-sdk-go#1832 deprecates the SDK's own
//! U2M flow), so the SDK only asks the CLI for a token from its cache.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::future::BoxFuture;
use secrecy::SecretString;
use serde::Deserialize;

use super::common::{TokenHeaders, instant_from_unix, parse_timestamp};
use super::token::{CachedTokenSource, Token, TokenSource};
use super::{CredentialsProvider, CredentialsStrategy, reject_group_role};
use crate::config::{Config, HostType, Source};
use crate::error::{Error, Result};

const NAME: &str = "databricks-cli";
/// The Go CLI (≥ 0.100) is a large binary; the legacy Python CLI is a
/// small script. Go tells them apart by size.
const MIN_CLI_SIZE: u64 = 1024 * 1024;
/// `auth token --profile` support (databricks/cli#855).
const VERSION_FOR_PROFILE: (u64, u64, u64) = (0, 207, 1);
/// `auth token --force-refresh` support (databricks/cli#4767).
const VERSION_FOR_FORCE_REFRESH: (u64, u64, u64) = (0, 296, 0);

/// Tokens from the Databricks CLI's cache (`databricks auth login` first).
#[derive(Debug, Clone, Copy, Default)]
pub struct DatabricksCliCredentials;

impl CredentialsStrategy for DatabricksCliCredentials {
    fn name(&self) -> &'static str {
        NAME
    }

    fn configure<'a>(
        &'a self,
        cfg: &'a Config,
        _http: &'a reqwest::Client,
    ) -> BoxFuture<'a, Result<Option<Arc<dyn CredentialsProvider>>>> {
        Box::pin(async move {
            reject_group_role(cfg, NAME)?;
            if cfg.host.as_deref().is_none_or(str::is_empty) {
                return Err(auth("host is required"));
            }
            validate_scopes(cfg)?;
            let cli = find_cli(cfg)?;
            let version = cli_version(&cli).await;
            let cmd = build_command(&cli, cfg, version)?;
            let cache = CachedTokenSource::new(CliTokenSource { cmd }, true);
            // Go fetches a token up front so a missing login fails here.
            cache.token().await?;
            Ok(Some(
                Arc::new(TokenHeaders::bearer(cache)) as Arc<dyn CredentialsProvider>
            ))
        })
    }
}

fn auth(message: impl Into<String>) -> Error {
    Error::Auth {
        auth_type: NAME.into(),
        message: message.into(),
    }
}

/// The CLI's token cache is keyed by profile or host, not scopes, so
/// scopes set in code or the environment would be silently ignored. Scopes
/// from the config file are fine: `databricks auth login` wrote them.
fn validate_scopes(cfg: &Config) -> Result<()> {
    if cfg.scopes.is_empty() || matches!(cfg.source_of("scopes"), Some(Source::File(_))) {
        return Ok(());
    }
    Err(auth(
        "custom scopes are not supported with databricks-cli auth; scopes are determined by what was last used when logging in with `databricks auth login`",
    ))
}

fn find_cli(cfg: &Config) -> Result<PathBuf> {
    let configured = cfg.attr("databricks_cli_path");
    match configured.as_deref() {
        Some(p) if p.contains('/') || p.contains(std::path::MAIN_SEPARATOR) => {
            validate_cli(Path::new(p))
        }
        Some(name) => find_in_path(cfg, name),
        None => find_in_path(cfg, "databricks").or_else(|e| {
            if cfg!(windows) {
                find_in_path(cfg, "databricks.exe")
            } else {
                Err(e)
            }
        }),
    }
}

fn find_in_path(cfg: &Config, name: &str) -> Result<PathBuf> {
    let not_found = || auth("databricks CLI not found");
    let path = cfg.getenv("PATH").ok_or_else(not_found)?;
    let mut last = not_found();
    for dir in std::env::split_paths(&path) {
        match validate_cli(&dir.join(name)) {
            Ok(p) => return Ok(p),
            Err(e) if e.to_string().contains("legacy") => last = e,
            Err(_) => {}
        }
    }
    Err(last)
}

fn validate_cli(path: &Path) -> Result<PathBuf> {
    let meta = std::fs::metadata(path).map_err(|_| auth("databricks CLI not found"))?;
    if meta.is_dir() {
        return Err(auth("databricks CLI not found"));
    }
    if meta.len() < MIN_CLI_SIZE {
        return Err(auth(
            "legacy databricks CLI detected; upgrade to >= 0.100.0",
        ));
    }
    Ok(path.to_path_buf())
}

/// `(major, minor, patch, is_release)` from `Databricks CLI v0.207.1`.
/// Pre-releases (`v0.0.0-dev…`) sort before the release, as in semver.
type Version = (u64, u64, u64, bool);

fn parse_version(out: &str) -> Option<Version> {
    let v = out
        .trim()
        .strip_prefix("Databricks CLI ")?
        .strip_prefix('v')?;
    let v = v.split_once('+').map_or(v, |(core, _)| core);
    let (core, pre) = v.split_once('-').map_or((v, None), |(c, p)| (c, Some(p)));
    let mut it = core.split('.').map(str::parse::<u64>);
    let (a, b, c) = (it.next()?.ok()?, it.next()?.ok()?, it.next()?.ok()?);
    if it.next().is_some() || pre.is_some_and(str::is_empty) {
        return None;
    }
    Some((a, b, c, pre.is_none()))
}

fn at_least(v: Option<Version>, (a, b, c): (u64, u64, u64)) -> bool {
    v.is_some_and(|v| v >= (a, b, c, true))
}

async fn cli_version(cli: &Path) -> Option<Version> {
    let out = tokio::process::Command::new(cli)
        .arg("version")
        .kill_on_drop(true)
        .output()
        .await;
    let v = match out {
        Ok(o) if o.status.success() => parse_version(&String::from_utf8_lossy(&o.stdout)),
        _ => None,
    };
    if v.is_none() {
        tracing::warn!("failed to detect Databricks CLI version; using conservative flags");
    }
    v
}

fn build_command(cli: &Path, cfg: &Config, version: Option<Version>) -> Result<Vec<String>> {
    let cli = cli.to_string_lossy().into_owned();
    let host_cmd = || -> Result<Vec<String>> {
        let host = cfg
            .host
            .clone()
            .filter(|h| !h.is_empty())
            .ok_or_else(|| auth("host is not set"))?;
        let mut cmd = vec![
            cli.clone(),
            "auth".into(),
            "token".into(),
            "--host".into(),
            host,
        ];
        if cfg.host_type() == HostType::Account {
            cmd.push("--account-id".into());
            cmd.push(cfg.account_id.clone().unwrap_or_default());
        }
        Ok(cmd)
    };
    let mut cmd = match cfg.profile.as_deref().filter(|p| !p.is_empty()) {
        None => host_cmd()?,
        Some(p) if at_least(version, VERSION_FOR_PROFILE) => {
            vec![
                cli.clone(),
                "auth".into(),
                "token".into(),
                "--profile".into(),
                p.into(),
            ]
        }
        Some(_) => {
            tracing::warn!(
                "Databricks CLI does not support --profile (requires >= 0.207.1); falling back to --host"
            );
            host_cmd()?
        }
    };
    if at_least(version, VERSION_FOR_FORCE_REFRESH) {
        cmd.push("--force-refresh".into());
    } else {
        tracing::warn!(
            "Databricks CLI does not support --force-refresh (requires >= 0.296.0); its cache may return stale tokens"
        );
    }
    Ok(cmd)
}

struct CliTokenSource {
    cmd: Vec<String>,
}

#[derive(Deserialize)]
struct CliToken {
    access_token: String,
    #[serde(default)]
    token_type: String,
    expiry: String,
}

impl TokenSource for CliTokenSource {
    fn token(&self) -> BoxFuture<'_, Result<Token>> {
        Box::pin(async move {
            let out = tokio::process::Command::new(&self.cmd[0])
                .args(&self.cmd[1..])
                .kill_on_drop(true)
                .output()
                .await
                .map_err(|e| auth(format!("cannot get access token: {e}")))?;
            if !out.status.success() {
                return Err(auth(format!(
                    "cannot get access token: {}: {}",
                    String::from_utf8_lossy(&out.stderr).trim(),
                    out.status
                )));
            }
            let t: CliToken = serde_json::from_slice(&out.stdout)
                .map_err(|e| auth(format!("cannot parse CLI response: {e}")))?;
            let expiry = parse_timestamp(&t.expiry)
                .ok_or_else(|| auth(format!("cannot parse token expiry {:?}", t.expiry)))?;
            Ok(Token {
                access_token: SecretString::from(t.access_token),
                token_type: if t.token_type.is_empty() {
                    "Bearer".into()
                } else {
                    t.token_type
                },
                expiry: Some(instant_from_unix(expiry)),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(
            parse_version("Databricks CLI v0.207.1\n"),
            Some((0, 207, 1, true))
        );
        assert_eq!(
            parse_version("Databricks CLI v0.0.0-dev+abc"),
            Some((0, 0, 0, false))
        );
        assert_eq!(parse_version("Databricks CLI v1.2"), None);
        assert_eq!(parse_version("Databricks CLI v1.2.3-"), None);
        assert_eq!(parse_version("v1.2.3"), None);
        assert!(at_least(Some((0, 207, 1, true)), VERSION_FOR_PROFILE));
        assert!(!at_least(Some((0, 207, 1, false)), VERSION_FOR_PROFILE));
        assert!(!at_least(None, VERSION_FOR_PROFILE));
        assert!(at_least(Some((1, 0, 0, true)), VERSION_FOR_FORCE_REFRESH));
    }

    fn cfg(host: &str) -> Config {
        let mut c = Config::with_host(host);
        c.resolved_host_type = Some(if host.contains("accounts.") {
            HostType::Account
        } else {
            HostType::Workspace
        });
        c
    }

    #[test]
    fn commands() {
        let cli = Path::new("/bin/databricks");
        let new = Some((0, 300, 0, true));
        let c = cfg("https://x.cloud.databricks.com");
        assert_eq!(
            build_command(cli, &c, new).unwrap().join(" "),
            "/bin/databricks auth token --host https://x.cloud.databricks.com --force-refresh"
        );
        let mut c = cfg("https://accounts.cloud.databricks.com");
        c.account_id = Some("acc".into());
        assert_eq!(
            build_command(cli, &c, None).unwrap().join(" "),
            "/bin/databricks auth token --host https://accounts.cloud.databricks.com --account-id acc"
        );
        let mut c = cfg("https://x");
        c.profile = Some("dev".into());
        assert_eq!(
            build_command(cli, &c, new).unwrap().join(" "),
            "/bin/databricks auth token --profile dev --force-refresh"
        );
        assert_eq!(
            build_command(cli, &c, Some((0, 200, 0, true)))
                .unwrap()
                .join(" "),
            "/bin/databricks auth token --host https://x"
        );
        let c = Config {
            profile: Some("dev".into()),
            ..Config::default()
        };
        assert!(
            build_command(cli, &c, None)
                .unwrap_err()
                .to_string()
                .contains("host is not set")
        );
    }
}
