//! Runtime for the Databricks Rust SDK.
//!
//! This crate is the hand-written core that generated service code sits on.
//! Its behaviour tracks `databricks-sdk-go` (currently v0.182.0) so that the
//! same configuration, environment variables and `~/.databrickscfg` profiles
//! work identically across the Go, Python, Java and Rust SDKs.
//!
//! * [`config`]: unified client configuration (env vars, config file,
//!   host metadata discovery).
//! * [`auth`]: credential strategies (PAT and OAuth M2M in milestone 1).
//! * [`http`]: the [`ApiClient`] with retries, rate limiting
//!   and user-agent handling.
//! * [`error`]: the Databricks error envelope mapped to typed errors.
//! * [`paging`]: token-based pagination as a [`Stream`](futures_core::Stream).
//! * [`wait`]: long-running-operation polling.

pub mod auth;
pub mod binary;
pub mod config;
pub mod error;
pub mod http;
pub mod paging;
pub mod query;
pub mod serde_num;
pub mod useragent;
pub mod wait;

#[doc(hidden)]
pub mod open_enum;

pub use config::Config;
pub use error::{ApiError, Error, ErrorKind, Result};
pub use http::ApiClient;

/// Version of this crate, reported in the `User-Agent` header.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[doc(hidden)]
pub mod __private {
    pub use serde;
}
