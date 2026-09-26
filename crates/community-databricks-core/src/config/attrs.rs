//! The attribute table: one row per configuration attribute, carrying its
//! config-file key, environment variable, auth group and sensitivity.
//!
//! Mirrors the struct tags on Go's `config.Config`
//! (`name:"…" env:"…" auth:"…,sensitive"`). Attributes for auth types this
//! milestone does not implement yet (Azure, GCP, OIDC, basic…) are still
//! recognised, so that conflicting-auth validation and config-file
//! precedence behave exactly like the Go SDK.

use secrecy::{ExposeSecret, SecretString};

use super::Config;

/// Where an attribute's value came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Set in code.
    Code,
    /// Read from an environment variable.
    Env(&'static str),
    /// Read from a config-file profile.
    File(String),
    /// Back-filled from `/.well-known/databricks-config`.
    HostMetadata,
}

pub(crate) struct Attr {
    pub name: &'static str,
    pub env: Option<&'static str>,
    /// Auth group (`pat`, `oauth`, `azure`…); `None` for non-auth attributes.
    pub auth: Option<&'static str>,
    pub sensitive: bool,
    pub get: fn(&Config) -> Option<String>,
    pub set: fn(&mut Config, String) -> Result<(), String>,
}

impl Attr {
    pub fn is_set(&self, cfg: &Config) -> bool {
        (self.get)(cfg).is_some_and(|v| !v.is_empty())
    }
}

fn opt(v: Option<&String>) -> Option<String> {
    v.filter(|s| !s.is_empty()).cloned()
}

fn secret(v: Option<&SecretString>) -> Option<String> {
    v.map(|s| s.expose_secret().to_owned())
        .filter(|s| !s.is_empty())
}

fn parse<T: std::str::FromStr>(name: &str, v: &str) -> Result<T, String> {
    v.trim()
        .parse()
        .map_err(|_| format!("{name}: cannot parse {v:?}"))
}

pub(crate) fn parse_bool(name: &str, v: &str) -> Result<bool, String> {
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "" | "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(format!("{name}: cannot parse {v:?} as bool")),
    }
}

macro_rules! string_attr {
    ($name:literal, $env:expr, $auth:expr, $field:ident) => {
        Attr {
            name: $name,
            env: $env,
            auth: $auth,
            sensitive: false,
            get: |c| opt(c.$field.as_ref()),
            set: |c, v| {
                c.$field = Some(v);
                Ok(())
            },
        }
    };
}

macro_rules! secret_attr {
    ($name:literal, $env:expr, $auth:expr, $field:ident) => {
        Attr {
            name: $name,
            env: $env,
            auth: $auth,
            sensitive: true,
            get: |c| secret(c.$field.as_ref()),
            set: |c, v| {
                c.$field = Some(SecretString::from(v));
                Ok(())
            },
        }
    };
}

/// Attributes stored verbatim in `Config::other`; read with `Config::attribute`.
macro_rules! other_attr {
    ($name:literal, $env:expr, $auth:expr, $sensitive:expr) => {
        Attr {
            name: $name,
            env: $env,
            auth: $auth,
            sensitive: $sensitive,
            get: |c| c.other.get($name).cloned().filter(|s| !s.is_empty()),
            set: |c, v| {
                c.other.insert($name.to_owned(), v);
                Ok(())
            },
        }
    };
}

pub(crate) static ATTRIBUTES: &[Attr] = &[
    string_attr!("host", Some("DATABRICKS_HOST"), None, host),
    string_attr!(
        "cluster_id",
        Some("DATABRICKS_CLUSTER_ID"),
        None,
        cluster_id
    ),
    string_attr!(
        "warehouse_id",
        Some("DATABRICKS_WAREHOUSE_ID"),
        None,
        warehouse_id
    ),
    string_attr!(
        "account_id",
        Some("DATABRICKS_ACCOUNT_ID"),
        None,
        account_id
    ),
    string_attr!(
        "workspace_id",
        Some("DATABRICKS_WORKSPACE_ID"),
        None,
        workspace_id
    ),
    string_attr!("group_id", Some("DATABRICKS_GROUP_ID"), None, group_id),
    secret_attr!("token", Some("DATABRICKS_TOKEN"), Some("pat"), token),
    other_attr!(
        "username",
        Some("DATABRICKS_USERNAME"),
        Some("basic"),
        false
    ),
    other_attr!("password", Some("DATABRICKS_PASSWORD"), Some("basic"), true),
    string_attr!("profile", Some("DATABRICKS_CONFIG_PROFILE"), None, profile),
    string_attr!(
        "config_file",
        Some("DATABRICKS_CONFIG_FILE"),
        None,
        config_file
    ),
    other_attr!(
        "metadata_service_url",
        Some("DATABRICKS_METADATA_SERVICE_URL"),
        Some("metadata-service"),
        true
    ),
    other_attr!(
        "google_service_account",
        Some("DATABRICKS_GOOGLE_SERVICE_ACCOUNT"),
        Some("google"),
        false
    ),
    other_attr!(
        "google_credentials",
        Some("GOOGLE_CREDENTIALS"),
        Some("google"),
        true
    ),
    other_attr!(
        "azure_workspace_resource_id",
        Some("DATABRICKS_AZURE_RESOURCE_ID"),
        Some("azure"),
        false
    ),
    other_attr!("azure_use_msi", Some("ARM_USE_MSI"), Some("azure"), false),
    other_attr!(
        "azure_client_secret",
        Some("ARM_CLIENT_SECRET"),
        Some("azure"),
        true
    ),
    other_attr!(
        "azure_client_id",
        Some("ARM_CLIENT_ID"),
        Some("azure"),
        false
    ),
    other_attr!(
        "azure_tenant_id",
        Some("ARM_TENANT_ID"),
        Some("azure"),
        false
    ),
    other_attr!("azure_environment", Some("ARM_ENVIRONMENT"), None, false),
    other_attr!(
        "azure_login_app_id",
        Some("DATABRICKS_AZURE_LOGIN_APP_ID"),
        Some("azure"),
        false
    ),
    string_attr!(
        "client_id",
        Some("DATABRICKS_CLIENT_ID"),
        Some("oauth"),
        client_id
    ),
    secret_attr!(
        "client_secret",
        Some("DATABRICKS_CLIENT_SECRET"),
        Some("oauth"),
        client_secret
    ),
    other_attr!(
        "databricks_cli_path",
        Some("DATABRICKS_CLI_PATH"),
        None,
        false
    ),
    string_attr!("auth_type", Some("DATABRICKS_AUTH_TYPE"), None, auth_type),
    other_attr!(
        "databricks_id_token_filepath",
        Some("DATABRICKS_OIDC_TOKEN_FILEPATH"),
        Some("file-oidc"),
        false
    ),
    other_attr!(
        "oidc_token_env",
        Some("DATABRICKS_OIDC_TOKEN_ENV"),
        Some("env-oidc"),
        false
    ),
    Attr {
        name: "skip_verify",
        env: None,
        auth: None,
        sensitive: false,
        get: |c| c.skip_verify.then(|| "true".to_owned()),
        set: |c, v| {
            c.skip_verify = parse_bool("skip_verify", &v)?;
            Ok(())
        },
    },
    Attr {
        name: "http_timeout_seconds",
        env: None,
        auth: None,
        sensitive: false,
        get: |c| c.http_timeout_seconds.map(|v| v.to_string()),
        set: |c, v| {
            c.http_timeout_seconds = Some(parse("http_timeout_seconds", &v)?);
            Ok(())
        },
    },
    Attr {
        name: "debug_headers",
        env: Some("DATABRICKS_DEBUG_HEADERS"),
        auth: None,
        sensitive: false,
        get: |c| c.debug_headers.then(|| "true".to_owned()),
        set: |c, v| {
            c.debug_headers = parse_bool("debug_headers", &v)?;
            Ok(())
        },
    },
    Attr {
        name: "rate_limit",
        env: Some("DATABRICKS_RATE_LIMIT"),
        auth: None,
        sensitive: false,
        get: |c| c.rate_limit.map(|v| v.to_string()),
        set: |c, v| {
            c.rate_limit = Some(parse("rate_limit", &v)?);
            Ok(())
        },
    },
    Attr {
        name: "retry_timeout_seconds",
        env: None,
        auth: None,
        sensitive: false,
        get: |c| c.retry_timeout_seconds.map(|v| v.to_string()),
        set: |c, v| {
            c.retry_timeout_seconds = Some(parse("retry_timeout_seconds", &v)?);
            Ok(())
        },
    },
    Attr {
        name: "scopes",
        env: None,
        auth: None,
        sensitive: false,
        get: |c| (!c.scopes.is_empty()).then(|| c.scopes.join(",")),
        set: |c, v| {
            c.scopes = v
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect();
            Ok(())
        },
    },
    string_attr!("cloud", Some("DATABRICKS_CLOUD"), None, cloud),
    other_attr!("audience", Some("DATABRICKS_TOKEN_AUDIENCE"), None, false),
    other_attr!(
        "actions_id_token_request_url",
        Some("ACTIONS_ID_TOKEN_REQUEST_URL"),
        None,
        false
    ),
    other_attr!(
        "actions_id_token_request_token",
        Some("ACTIONS_ID_TOKEN_REQUEST_TOKEN"),
        None,
        true
    ),
    string_attr!(
        "discovery_url",
        Some("DATABRICKS_DISCOVERY_URL"),
        None,
        discovery_url
    ),
];

pub(crate) fn find(name: &str) -> Option<&'static Attr> {
    ATTRIBUTES.iter().find(|a| a.name == name)
}
