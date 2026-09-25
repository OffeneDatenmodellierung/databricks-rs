//! Service modules. In milestone 1 these are hand-written; from milestone 2
//! they are generated from the OpenAPI spec, one module (and feature flag)
//! per service, matching the Go SDK's `service/<name>` packages.

#[cfg(feature = "compute")]
pub mod compute;
#[cfg(feature = "jobs")]
pub mod jobs;
#[cfg(feature = "provisioning")]
pub mod provisioning;
