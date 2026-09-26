//! Compute helpers from Go's `service/compute/ext_*.go`: node type and
//! Spark version selection, keeping a cluster running, library installs,
//! and command execution.
//!
//! Go serialises `EnsureClusterIsRunning` and `GetOrCreateRunningCluster`
//! with process-wide mutexes. These don't: a concurrent start shows up as
//! an `INVALID_STATE` error, which `ensure_cluster_is_running` retries as
//! Go does.

use std::cmp::Ordering;
use std::fmt;
use std::time::Duration;

use community_databricks_core::wait::{self, PollStatus};
use community_databricks_core::{ApiClient, Error, Result};

pub use community_databricks_core::text::trim_leading_whitespace;

use crate::{
    CloudProviderNodeStatus, ClusterDetails, ClusterLibraryStatuses, ClusterStatus, ClustersApi,
    Command, CommandExecutionApi, CreateCluster, CreateContext, DestroyContext, GetClusterRequest,
    GetSparkVersionsResponse, InstallLibraries, Language, LibrariesApi, Library,
    LibraryInstallStatus, ListClustersRequest, ListNodeTypesResponse, NodeType, ResultType,
    Results, StartCluster, State, UninstallLibraries,
};

/// How long [`ClustersApi::ensure_cluster_is_running`] keeps trying (Go:
/// 20 minutes).
pub const ENSURE_RUNNING_TIMEOUT: Duration = Duration::from_mins(20);

/// How long [`LibrariesApi::wait`] polls by default (Go: 30 minutes).
pub const LIBRARIES_WAIT_TIMEOUT: Duration = Duration::from_mins(30);

// ---------------------------------------------------------------- node types

/// What [`ListNodeTypesResponse::smallest`] looks for (Go:
/// `compute.NodeTypeRequest`). Zero and `false` mean "no requirement",
/// except `graviton` and `fleet`, which must match exactly, and GPUs:
/// with `min_gpus` 0 only GPU-less nodes qualify.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeTypeRequest {
    /// Unused by the selection (kept for parity with Go).
    pub id: String,
    /// Minimum memory in GB.
    pub min_memory_gb: i64,
    /// Minimum GB of memory per core.
    pub gb_per_core: i64,
    /// Minimum cores.
    pub min_cores: i64,
    /// Minimum GPUs; 0 excludes GPU nodes.
    pub min_gpus: i64,
    /// Require a local disk.
    pub local_disk: bool,
    /// Minimum local disk size in GB (implies `local_disk`).
    pub local_disk_min_size: i64,
    /// Node category, compared case-insensitively.
    pub category: String,
    /// Require Photon worker support.
    pub photon_worker_capable: bool,
    /// Require Photon driver support.
    pub photon_driver_capable: bool,
    /// Graviton (ARM) nodes only, or none.
    pub graviton: bool,
    /// Require the IO cache.
    pub is_io_cache_enabled: bool,
    /// Require port forwarding.
    pub support_port_forwarding: bool,
    /// Fleet node types (`-fleet.` in the ID) only, or none.
    pub fleet: bool,
}

fn node_sort_key(nt: &NodeType) -> impl Ord + '_ {
    let i = nt.node_instance_type.as_ref();
    (
        nt.is_deprecated.unwrap_or(false),
        // f64 cores: compare as integers, as Go does.
        cores(nt),
        nt.memory_mb,
        i.and_then(|i| i.local_disks).unwrap_or(0),
        i.and_then(|i| i.local_disk_size_gb).unwrap_or(0),
        i.and_then(|i| i.local_nvme_disks).unwrap_or(0),
        i.and_then(|i| i.local_nvme_disk_size_gb).unwrap_or(0),
        nt.num_gpus.unwrap_or(0),
        nt.instance_type_id.as_str(),
    )
}

#[allow(clippy::cast_possible_truncation)]
fn cores(nt: &NodeType) -> i64 {
    nt.num_cores as i64
}

fn should_be_skipped(nt: &NodeType) -> bool {
    nt.node_info.as_ref().is_some_and(|info| {
        info.status.iter().any(|s| {
            matches!(
                s,
                CloudProviderNodeStatus::NotAvailableInRegion
                    | CloudProviderNodeStatus::NotEnabledOnSubscription
            )
        })
    })
}

fn node_matches(nt: &NodeType, r: &NodeTypeRequest) -> bool {
    let gbs = nt.memory_mb / 1024;
    let gpus = nt.num_gpus.unwrap_or(0);
    let inst = nt.node_instance_type.as_ref();
    let disks = inst.map(|i| i.local_disks.unwrap_or(0) + i.local_nvme_disks.unwrap_or(0));
    let disk_gb =
        inst.map(|i| i.local_disk_size_gb.unwrap_or(0) + i.local_nvme_disk_size_gb.unwrap_or(0));
    !(should_be_skipped(nt)
        || r.fleet != nt.node_type_id.contains("-fleet.")
        || (r.min_memory_gb > 0 && gbs < r.min_memory_gb)
        // Go divides by the core count; a node reporting 0 cores can't
        // meet a per-core requirement.
        || (r.gb_per_core > 0 && (cores(nt) == 0 || gbs / cores(nt) < r.gb_per_core))
        || (r.min_cores > 0 && cores(nt) < r.min_cores)
        || (r.min_gpus > 0 && gpus < r.min_gpus)
        || (r.min_gpus == 0 && gpus > 0)
        || ((r.local_disk || r.local_disk_min_size > 0) && disks.is_some_and(|d| d < 1))
        || (r.local_disk_min_size > 0 && disk_gb.is_some_and(|g| g < r.local_disk_min_size))
        || (!r.category.is_empty() && !nt.category.eq_ignore_ascii_case(&r.category))
        || (r.is_io_cache_enabled && !nt.is_io_cache_enabled.unwrap_or(false))
        || (r.support_port_forwarding && !nt.support_port_forwarding.unwrap_or(false))
        || (r.photon_driver_capable && !nt.photon_driver_capable.unwrap_or(false))
        || (r.photon_worker_capable && !nt.photon_worker_capable.unwrap_or(false))
        || nt.is_graviton.unwrap_or(false) != r.graviton)
}

impl ListNodeTypesResponse {
    /// The smallest node type meeting `r` (Go:
    /// `ListNodeTypesResponse.Smallest`): non-deprecated first, then fewest
    /// cores, least memory, fewest and smallest local disks, fewest GPUs,
    /// and instance type name.
    pub fn smallest(&self, r: &NodeTypeRequest) -> Result<String> {
        if self.node_types.is_empty() {
            return Err(Error::OperationFailed(
                "cannot determine smallest node type with empty response".into(),
            ));
        }
        let mut sorted: Vec<&NodeType> = self.node_types.iter().collect();
        sorted.sort_by(|a, b| node_sort_key(a).cmp(&node_sort_key(b)));
        sorted
            .into_iter()
            .find(|nt| node_matches(nt, r))
            .map(|nt| nt.node_type_id.clone())
            .ok_or_else(|| Error::OperationFailed("cannot determine smallest node type".into()))
    }
}

// ------------------------------------------------------------ spark versions

/// What [`GetSparkVersionsResponse::select`] looks for (Go:
/// `compute.SparkVersionRequest`). The boolean flags must match the
/// runtime's key exactly (an ML runtime is only chosen with `ml`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SparkVersionRequest {
    /// Unused by the selection (kept for parity with Go).
    pub id: String,
    /// Long-term support (`LTS` in the name, or an `-esr-` key).
    pub long_term_support: bool,
    /// Beta runtimes.
    pub beta: bool,
    /// With several matches, take the newest instead of failing.
    pub latest: bool,
    /// Machine-learning runtimes.
    pub ml: bool,
    /// Genomics (`-hls-`) runtimes.
    pub genomics: bool,
    /// GPU runtimes.
    pub gpu: bool,
    /// Scala version in the key (`-scala2.12`); empty matches any.
    pub scala: String,
    /// Apache Spark version in the name, e.g. `3.5`.
    pub spark_version: String,
    /// Photon runtimes.
    pub photon: bool,
    /// Graviton (`-aarch64-`) runtimes.
    pub graviton: bool,
}

/// `13.3` from `13.3.x-scala2.12` as `(13, 3)`; `None` if the key isn't a
/// DBR version (Go compares those as equal and lowest).
fn dbr_version(key: &str) -> Option<(u64, u64)> {
    let (ver, _) = key.split_once(".x-")?;
    let (major, minor) = ver.split_once('.').unwrap_or((ver, "0"));
    Some((major.parse().ok()?, minor.parse().ok()?))
}

fn strip_scala(v: &str) -> String {
    v.replace("-scala2.12", "").replace("-scala2.13", "")
}

impl GetSparkVersionsResponse {
    /// The runtime key matching `req` (Go: `GetSparkVersionsResponse.Select`).
    /// No match is an error; several are too, unless `latest` is set (the
    /// newest DBR version wins) or they differ only in Scala version (2.12
    /// wins).
    pub fn select(&self, req: &SparkVersionRequest) -> Result<String> {
        let mut versions: Vec<String> = self
            .versions
            .iter()
            .filter_map(|v| {
                let key = v.key.as_deref().unwrap_or_default();
                let name = v.name.as_deref().unwrap_or_default();
                let mut ok = key.contains(&format!("-scala{}", req.scala))
                    && !key.contains("apache-spark-")
                    && key.contains("-ml-") == req.ml
                    && key.contains("-hls-") == req.genomics
                    && key.contains("-gpu-") == req.gpu
                    && key.contains("-photon-") == req.photon
                    && key.contains("-aarch64-") == req.graviton
                    && name.contains("Beta") == req.beta;
                if ok && req.long_term_support {
                    ok = name.contains("LTS") || key.contains("-esr-");
                }
                if ok && !req.spark_version.is_empty() {
                    ok = name.contains(&format!("Apache Spark {}", req.spark_version));
                }
                ok.then(|| key.to_owned())
            })
            .collect();
        match versions.len() {
            0 => Err(Error::OperationFailed(
                "spark versions query returned no results. Please change your search criteria and try again".into(),
            )),
            1 => Ok(versions.remove(0)),
            n => {
                if n == 2 && strip_scala(&versions[0]) == strip_scala(&versions[1]) {
                    versions.sort();
                    return Ok(versions.remove(0));
                }
                if !req.latest {
                    return Err(Error::OperationFailed(format!(
                        "spark versions query returned multiple results {versions:?}. Please change your search criteria and try again"
                    )));
                }
                // Newest DBR version first. Go's sort is unstable; ties are
                // broken by key here, so Scala 2.12 wins as in the
                // two-version rule above.
                versions.sort_by(|a, b| {
                    match (dbr_version(a), dbr_version(b)) {
                        (Some(x), Some(y)) => y.cmp(&x),
                        (Some(_), None) => Ordering::Less,
                        (None, Some(_)) => Ordering::Greater,
                        (None, None) => Ordering::Equal,
                    }
                    .then_with(|| a.cmp(b))
                });
                Ok(versions.remove(0))
            }
        }
    }
}

// ------------------------------------------------------------------ clusters

impl ClusterDetails {
    /// Whether the cluster is `RUNNING` or `RESIZING` (Go:
    /// `ClusterDetails.IsRunningOrResizing`).
    #[must_use]
    pub fn is_running_or_resizing(&self) -> bool {
        matches!(self.state, Some(State::Running | State::Resizing))
    }
}

fn retriable_start_error(e: &Error) -> bool {
    match e {
        Error::Api(api) => api.error_code == "INVALID_STATE",
        Error::Transport(t) => t.is_connect() || t.is_timeout(),
        _ => false,
    }
}

impl ClustersApi {
    /// The smallest node type meeting `r` (Go: `ClustersAPI.SelectNodeType`).
    pub async fn select_node_type(&self, r: &NodeTypeRequest) -> Result<String> {
        self.list_node_types().await?.smallest(r)
    }

    /// The runtime key matching `r` (Go: `ClustersAPI.SelectSparkVersion`).
    pub async fn select_spark_version(&self, r: &SparkVersionRequest) -> Result<String> {
        self.spark_versions().await?.select(r)
    }

    /// Start the cluster if needed and wait until it runs (Go:
    /// `ClustersAPI.EnsureClusterIsRunning`): a terminating cluster is
    /// waited out and restarted, a pending, resizing or restarting one is
    /// waited on. `INVALID_STATE` (another process started it) and
    /// connection failures are retried for up to 20 minutes.
    pub async fn ensure_cluster_is_running(&self, cluster_id: &str) -> Result<()> {
        wait::poll(ENSURE_RUNNING_TIMEOUT, || async move {
            match self
                .start_cluster_if_needed(cluster_id, ENSURE_RUNNING_TIMEOUT)
                .await
            {
                Ok(()) => Ok(PollStatus::Done(())),
                Err(e) if retriable_start_error(&e) => Ok(PollStatus::Continue(e.to_string())),
                Err(e) => Err(e),
            }
        })
        .await
    }

    async fn start_cluster_if_needed(&self, cluster_id: &str, timeout: Duration) -> Result<()> {
        let info = self.get(GetClusterRequest::new(cluster_id)).await?;
        match info.state {
            Some(State::Running) => Ok(()),
            Some(State::Terminating) => {
                self.wait_get_cluster_terminated(cluster_id, timeout, None)
                    .await?;
                self.start(StartCluster::new(cluster_id))
                    .await?
                    .wait()
                    .await?;
                Ok(())
            }
            Some(State::Terminated) => {
                self.start(StartCluster::new(cluster_id))
                    .await?
                    .wait()
                    .await?;
                Ok(())
            }
            Some(State::Pending | State::Resizing | State::Restarting) => {
                self.wait_get_cluster_running(cluster_id, timeout, None)
                    .await?;
                Ok(())
            }
            other => Err(Error::OperationFailed(format!(
                "cluster {} is in {} state: {}",
                info.cluster_name.as_deref().unwrap_or_default(),
                other.map_or_else(|| "UNKNOWN".to_owned(), |s| s.to_string()),
                info.state_message.as_deref().unwrap_or_default()
            ))),
        }
    }

    /// A running cluster named `name` (Go:
    /// `ClustersAPI.GetOrCreateRunningCluster`): an existing one, started if
    /// needed, or else a new one from `custom`, or by default a 1-worker
    /// cluster on the smallest local-disk node type with the latest LTS
    /// runtime, terminating after 10 idle minutes.
    pub async fn get_or_create_running_cluster(
        &self,
        name: &str,
        custom: Option<CreateCluster>,
    ) -> Result<ClusterDetails> {
        let clusters = self.list_all(ListClustersRequest::default()).await?;
        if let Some(cl) = clusters
            .into_iter()
            .find(|c| c.cluster_name.as_deref() == Some(name))
        {
            if cl.is_running_or_resizing() {
                return Ok(cl);
            }
            let id = cl.cluster_id.clone().unwrap_or_default();
            // As in Go, a cluster that can't be started is replaced.
            if let Ok(started) = Box::pin(self.start(StartCluster::new(id))).await
                && let Ok(details) = Box::pin(started.wait()).await
            {
                return Ok(details);
            }
        }
        let request = match custom {
            Some(r) => r,
            None => {
                let node_type = self.list_node_types().await?.smallest(&NodeTypeRequest {
                    local_disk: true,
                    ..NodeTypeRequest::default()
                })?;
                let version = self.spark_versions().await?.select(&SparkVersionRequest {
                    latest: true,
                    long_term_support: true,
                    ..SparkVersionRequest::default()
                })?;
                CreateCluster::new(version)
                    .with_num_workers(1)
                    .with_cluster_name(name)
                    .with_node_type_id(node_type)
                    .with_autotermination_minutes(10)
            }
        };
        // `CreateCluster` is large; boxing keeps this future small.
        let waiter = Box::pin(self.create(request)).await?;
        Box::pin(waiter.wait()).await
    }
}

// ----------------------------------------------------------------- libraries

fn nonempty(s: &Option<String>) -> Option<&str> {
    s.as_deref().filter(|s| !s.is_empty())
}

impl fmt::Display for Library {
    /// Go's `Library.String`: `whl:…`, `jar:…`, `pypi:<repo><package>`,
    /// `mvn:<repo><coordinates><exclusions>`, `egg:…`, `cran:<repo><package>`
    /// or `unknown`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(w) = nonempty(&self.whl) {
            return write!(f, "whl:{w}");
        }
        if let Some(j) = nonempty(&self.jar) {
            return write!(f, "jar:{j}");
        }
        if let Some(p) = self.pypi.as_ref().filter(|p| !p.package.is_empty()) {
            return write!(
                f,
                "pypi:{}{}",
                p.repo.as_deref().unwrap_or_default(),
                p.package
            );
        }
        if let Some(m) = self.maven.as_ref().filter(|m| !m.coordinates.is_empty()) {
            return write!(
                f,
                "mvn:{}{}{}",
                m.repo.as_deref().unwrap_or_default(),
                m.coordinates,
                m.exclusions.concat()
            );
        }
        if let Some(e) = nonempty(&self.egg) {
            return write!(f, "egg:{e}");
        }
        if let Some(c) = self.cran.as_ref().filter(|c| !c.package.is_empty()) {
            return write!(
                f,
                "cran:{}{}",
                c.repo.as_deref().unwrap_or_default(),
                c.package
            );
        }
        f.write_str("unknown")
    }
}

impl InstallLibraries {
    /// Sort the libraries by their string form (Go: `InstallLibraries.Sort`).
    pub fn sort(&mut self) {
        self.libraries.sort_by_cached_key(ToString::to_string);
    }
}

/// What [`LibrariesApi::wait`] waits for (Go: `compute.Wait`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LibraryWait {
    /// The cluster.
    pub cluster_id: String,
    /// Only these libraries count; empty means all.
    pub libraries: Vec<Library>,
    /// The cluster is running, so installs can finish; otherwise the
    /// statuses are returned as they are.
    pub is_running: bool,
    /// Ignore failed libraries (refreshing state rather than installing).
    pub is_refresh: bool,
}

impl LibraryWait {
    /// Whether `lib` is outside the libraries being waited for (Go:
    /// `Wait.IsNotInScope`).
    #[must_use]
    pub fn is_not_in_scope(&self, lib: &Library) -> bool {
        if self.libraries.is_empty() {
            return false;
        }
        let key = lib.to_string();
        !self.libraries.iter().any(|l| l.to_string() == key)
    }
}

impl ClusterLibraryStatuses {
    /// The cluster's libraries as an install request, sorted (Go:
    /// `ClusterLibraryStatuses.ToLibraryList`).
    #[must_use]
    pub fn to_library_list(&self) -> InstallLibraries {
        let mut out = InstallLibraries::new(
            self.cluster_id.clone().unwrap_or_default(),
            self.library_statuses
                .iter()
                .filter_map(|s| s.library.clone())
                .collect::<Vec<_>>(),
        );
        out.sort();
        out
    }

    /// Whether to keep waiting (Go: `ClusterLibraryStatuses.IsRetryNeeded`):
    /// `Ok(Some(progress))` while libraries in scope are pending,
    /// `Ok(None)` when all are ready, and an error listing failed ones
    /// (unless `w.is_refresh`).
    pub fn is_retry_needed(&self, w: &LibraryWait) -> Result<Option<String>> {
        let (mut pending, mut ready) = (0, 0);
        let mut errors = Vec::new();
        for s in &self.library_statuses {
            if s.is_library_for_all_clusters.unwrap_or(false) {
                continue;
            }
            let Some(lib) = &s.library else { continue };
            if w.is_not_in_scope(lib) {
                continue;
            }
            match s.status {
                Some(
                    LibraryInstallStatus::Pending
                    | LibraryInstallStatus::Resolving
                    | LibraryInstallStatus::Installing,
                ) => pending += 1,
                Some(
                    LibraryInstallStatus::Installed
                    | LibraryInstallStatus::Skipped
                    | LibraryInstallStatus::UninstallOnRestart,
                ) => ready += 1,
                Some(LibraryInstallStatus::Failed) if !w.is_refresh => {
                    errors.push(format!("{lib} failed: {}", s.messages.join(", ")));
                }
                _ => {}
            }
        }
        if pending > 0 {
            return Ok(Some(format!(
                "{ready} libraries are ready, but there are still {pending} pending"
            )));
        }
        if !errors.is_empty() {
            return Err(Error::OperationFailed(errors.join("\n")));
        }
        Ok(None)
    }
}

/// Libraries to add and remove on a cluster (Go: `compute.Update`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LibraryUpdate {
    /// The cluster.
    pub cluster_id: String,
    /// Libraries to install.
    pub install: Vec<Library>,
    /// Libraries to uninstall.
    pub uninstall: Vec<Library>,
}

impl LibrariesApi {
    /// Uninstall, then install, then wait for the changed libraries (Go:
    /// `LibrariesAPI.UpdateAndWait`).
    pub async fn update_and_wait(&self, update: LibraryUpdate, timeout: Duration) -> Result<()> {
        if !update.uninstall.is_empty() {
            self.uninstall(UninstallLibraries::new(
                update.cluster_id.clone(),
                update.uninstall.clone(),
            ))
            .await
            .map_err(|e| Error::OperationFailed(format!("uninstall: {e}")))?;
        }
        if !update.install.is_empty() {
            self.install(InstallLibraries::new(
                update.cluster_id.clone(),
                update.install.clone(),
            ))
            .await
            .map_err(|e| Error::OperationFailed(format!("install: {e}")))?;
        }
        let libraries = update.install.into_iter().chain(update.uninstall).collect();
        self.wait(
            &LibraryWait {
                cluster_id: update.cluster_id,
                libraries,
                is_running: true,
                is_refresh: false,
            },
            timeout,
        )
        .await
        .map(|_| ())
    }

    /// Poll the cluster's library statuses until the libraries in scope
    /// are installed (Go: `LibrariesAPI.Wait`). On a running cluster,
    /// libraries that failed are uninstalled afterwards and left out of the
    /// result.
    pub async fn wait(&self, w: &LibraryWait, timeout: Duration) -> Result<ClusterLibraryStatuses> {
        let mut result = wait::poll(timeout, || async move {
            let status = match self
                .cluster_status_page(ClusterStatus::new(w.cluster_id.clone()))
                .await
            {
                Ok(s) => s,
                Err(e) if e.is_missing() => return Ok(PollStatus::Continue(e.to_string())),
                Err(e) => return Err(e),
            };
            if !w.is_running {
                return Ok(PollStatus::Done(status));
            }
            Ok(match status.is_retry_needed(w)? {
                Some(progress) => PollStatus::Continue(progress),
                None => PollStatus::Done(status),
            })
        })
        .await?;
        if w.is_running {
            let (failed, installed): (Vec<_>, Vec<_>) =
                std::mem::take(&mut result.library_statuses)
                    .into_iter()
                    .partition(|s| s.status == Some(LibraryInstallStatus::Failed));
            result.library_statuses = installed;
            let cleanup: Vec<Library> = failed.into_iter().filter_map(|s| s.library).collect();
            if !cleanup.is_empty() {
                self.uninstall(UninstallLibraries::new(w.cluster_id.clone(), cleanup))
                    .await
                    .map_err(|e| {
                        Error::OperationFailed(format!("cannot cleanup libraries: {e}"))
                    })?;
            }
        }
        Ok(result)
    }
}

// ------------------------------------------------------------------ commands

impl Results {
    /// Whether the command failed (Go: `Results.Failed`).
    #[must_use]
    pub fn failed(&self) -> bool {
        self.result_type == Some(ResultType::Error)
    }

    /// Text output without `Out[n]:` prompts; empty unless the result is
    /// text (Go: `Results.Text`).
    #[must_use]
    pub fn text(&self) -> String {
        if self.result_type != Some(ResultType::Text) {
            return String::new();
        }
        community_databricks_core::text::strip_out_prompts(
            self.data
                .as_ref()
                .and_then(|d| d.as_str())
                .unwrap_or_default(),
        )
    }

    /// The failure as an error, if the command failed (Go: `Results.Err`).
    pub fn err(&self) -> Result<()> {
        if self.failed() {
            return Err(Error::OperationFailed(self.error()));
        }
        Ok(())
    }

    /// The readable error message; empty unless the command failed (Go:
    /// `Results.Error`).
    #[must_use]
    pub fn error(&self) -> String {
        if !self.failed() {
            return String::new();
        }
        community_databricks_core::text::command_error(
            self.summary.as_deref().unwrap_or_default(),
            self.cause.as_deref().unwrap_or_default(),
        )
    }

    /// The next row of a table result, advancing `pos` (Go: `Results.Scan`,
    /// which reads into typed destinations; here each cell is a JSON value).
    pub fn scan(&mut self) -> Option<Vec<serde_json::Value>> {
        if self.result_type != Some(ResultType::Table) {
            return None;
        }
        let pos = self.pos.unwrap_or(0);
        let row = self
            .data
            .as_ref()?
            .as_array()?
            .get(usize::try_from(pos).ok()?)?
            .as_array()?
            .clone();
        self.pos = Some(pos + 1);
        Some(row)
    }
}

/// A command context on one cluster (Go: `compute.CommandExecutorV2`),
/// from [`CommandExecutionApi::start`]. Call [`destroy`](Self::destroy)
/// when done.
#[derive(Debug, Clone)]
pub struct CommandExecutor {
    execution: CommandExecutionApi,
    language: Language,
    cluster_id: String,
    context_id: String,
}

impl CommandExecutionApi {
    /// Make sure the cluster is running and open a `language` context on it
    /// (Go: `CommandExecutionAPI.Start`).
    pub async fn start(&self, cluster_id: &str, language: Language) -> Result<CommandExecutor> {
        ClustersApi::new(self.api.clone())
            .ensure_cluster_is_running(cluster_id)
            .await?;
        let context = self
            .create(
                CreateContext::default()
                    .with_cluster_id(cluster_id)
                    .with_language(language.clone()),
            )
            .await?
            .wait()
            .await?;
        Ok(CommandExecutor {
            execution: self.clone(),
            language,
            cluster_id: cluster_id.to_owned(),
            context_id: context.id.unwrap_or_default(),
        })
    }
}

impl CommandExecutor {
    /// The context's ID.
    #[must_use]
    pub fn context_id(&self) -> &str {
        &self.context_id
    }

    /// Run `command` (common indentation removed) and wait for its results
    /// (Go: `CommandExecutorV2.Execute`).
    pub async fn execute(&self, command: &str) -> Result<Option<Results>> {
        let status = self
            .execution
            .execute(
                Command::default()
                    .with_command(trim_leading_whitespace(command))
                    .with_cluster_id(self.cluster_id.clone())
                    .with_context_id(self.context_id.clone())
                    .with_language(self.language.clone()),
            )
            .await?
            .wait()
            .await?;
        Ok(status.results)
    }

    /// Close the context (Go: `CommandExecutorV2.Destroy`).
    pub async fn destroy(&self) -> Result<()> {
        self.execution
            .destroy(DestroyContext::new(
                self.cluster_id.clone(),
                self.context_id.clone(),
            ))
            .await
    }
}

/// One-shot command execution on a running cluster (Go:
/// `compute.CommandsHighLevelAPI`); failures come back as error
/// [`Results`] rather than `Err`.
#[derive(Debug, Clone)]
pub struct CommandsHighLevelApi {
    clusters: ClustersApi,
    execution: CommandExecutionApi,
}

/// A [`CommandsHighLevelApi`] over `api` (Go: `NewCommandExecutor`).
#[must_use]
pub fn new_command_executor(api: ApiClient) -> CommandsHighLevelApi {
    CommandsHighLevelApi {
        clusters: ClustersApi::new(api.clone()),
        execution: CommandExecutionApi::new(api),
    }
}

fn error_results(summary: impl Into<String>) -> Results {
    Results::default()
        .with_result_type(ResultType::Error)
        .with_summary(summary)
}

impl CommandsHighLevelApi {
    /// Run `command` in a fresh context on a running (or resizing) cluster,
    /// then destroy the context (Go: `CommandsHighLevelAPI.Execute`).
    pub async fn execute(&self, cluster_id: &str, language: Language, command: &str) -> Results {
        let cluster = match self.clusters.get(GetClusterRequest::new(cluster_id)).await {
            Ok(c) => c,
            Err(e) => return error_results(e.to_string()),
        };
        if !cluster.is_running_or_resizing() {
            return error_results(format!(
                "Cluster {cluster_id} has to be running or resizing, but is {}",
                cluster
                    .state
                    .map_or_else(|| "UNKNOWN".to_owned(), |s| s.to_string())
            ));
        }
        let context = match self
            .execution
            .create(
                CreateContext::default()
                    .with_cluster_id(cluster_id)
                    .with_language(language.clone()),
            )
            .await
        {
            Ok(w) => match w.wait().await {
                Ok(c) => c,
                Err(e) => return error_results(e.to_string()),
            },
            Err(e) => return error_results(e.to_string()),
        };
        let context_id = context.id.unwrap_or_default();
        let outcome = match self
            .execution
            .execute(
                Command::default()
                    .with_cluster_id(cluster_id)
                    .with_context_id(context_id.clone())
                    .with_language(language)
                    .with_command(trim_leading_whitespace(command)),
            )
            .await
        {
            Ok(w) => w.wait().await,
            Err(e) => Err(e),
        };
        // Go defers the destroy and ignores its error.
        let _ = self
            .execution
            .destroy(DestroyContext::new(cluster_id, context_id))
            .await;
        match outcome {
            Ok(status) => status
                .results
                .unwrap_or_else(|| error_results("Command has no results")),
            Err(e) => error_results(e.to_string()),
        }
    }
}
