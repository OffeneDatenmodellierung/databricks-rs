//! Go's compute helpers: node type and Spark version selection, cluster
//! start-up, libraries and command execution (#15).
//!
//! Run with `--all-features` (CI does).

#![cfg(feature = "compute")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use community_databricks_sdk::core::config::HostMetadata;
use community_databricks_sdk::service::compute::{
    ClusterDetails, ClusterLibraryStatuses, CreateCluster, GetSparkVersionsResponse,
    InstallLibraries, Language, Library, LibraryUpdate, LibraryWait, ListNodeTypesResponse,
    MavenLibrary, NodeTypeRequest, PythonPyPiLibrary, RCranLibrary, ResultType, Results,
    SparkVersionRequest, State, new_command_executor, trim_leading_whitespace,
};
use community_databricks_sdk::{Config, WorkspaceClient};
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, method, path, query_param};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn cfg(server: &MockServer) -> Config {
    let mut c = Config::with_host(server.uri()).token("dapi");
    c.host_metadata = Some(HostMetadata::default());
    c.rate_limit = Some(1000);
    c
}

async fn workspace(server: &MockServer) -> WorkspaceClient {
    WorkspaceClient::new(cfg(server)).await.unwrap()
}

/// Replies from `replies` in order, repeating the last.
struct Sequence(Vec<ResponseTemplate>, Arc<AtomicUsize>);

impl Sequence {
    fn json(bodies: Vec<Value>) -> Self {
        Self(
            bodies
                .into_iter()
                .map(|b| ResponseTemplate::new(200).set_body_json(b))
                .collect(),
            Arc::default(),
        )
    }
}

impl Respond for Sequence {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let i = self.1.fetch_add(1, Ordering::SeqCst).min(self.0.len() - 1);
        self.0[i].clone()
    }
}

fn node(id: &str, cores: f64, memory_mb: i64, extra: &Value) -> Value {
    let mut v = json!({
        "node_type_id": id, "instance_type_id": id, "num_cores": cores,
        "memory_mb": memory_mb, "category": "General Purpose", "description": id,
        "node_instance_type": {"instance_type_id": id, "local_disks": 1, "local_disk_size_gb": 100}
    });
    v.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    v
}

fn node_types() -> ListNodeTypesResponse {
    serde_json::from_value(json!({"node_types": [
        node("big", 16.0, 65536, &json!({"is_io_cache_enabled": true, "photon_worker_capable": true,
                                        "photon_driver_capable": true, "support_port_forwarding": true})),
        node("small", 4.0, 16384, &json!({})),
        node("old-small", 2.0, 8192, &json!({"is_deprecated": true})),
        node("gpu", 8.0, 32768, &json!({"num_gpus": 1})),
        node("arm", 4.0, 16384, &json!({"is_graviton": true})),
        node("m-fleet.small", 4.0, 16384, &json!({})),
        node("nodisk", 4.0, 16384, &json!({"node_instance_type": {"instance_type_id": "nodisk"}})),
        node("memopt", 8.0, 131_072, &json!({"category": "Memory Optimized"})),
        node("unavailable", 1.0, 1024, &json!({"node_info": {"status": ["NotAvailableInRegion"]}})),
        node("zero", 0.0, 4096, &json!({"num_cores": 0.0}))
    ]}))
    .unwrap()
}

#[test]
fn smallest_node_type_follows_go() {
    let nt = node_types();
    let pick = |r: NodeTypeRequest| nt.smallest(&r).unwrap();
    // Deprecated and unavailable nodes lose; 0-core "zero" sorts first but
    // has 4 GB.
    assert_eq!(pick(NodeTypeRequest::default()), "zero");
    assert_eq!(
        pick(NodeTypeRequest {
            min_cores: 4,
            ..Default::default()
        }),
        "nodisk"
    );
    assert_eq!(
        pick(NodeTypeRequest {
            min_cores: 4,
            local_disk: true,
            ..Default::default()
        }),
        "small"
    );
    assert_eq!(
        pick(NodeTypeRequest {
            local_disk_min_size: 50,
            min_cores: 1,
            ..Default::default()
        }),
        "small"
    );
    // Fewest cores first: 8-core "memopt" (128 GB) beats 16-core "big".
    assert_eq!(
        pick(NodeTypeRequest {
            min_memory_gb: 20,
            ..Default::default()
        }),
        "memopt"
    );
    assert_eq!(
        pick(NodeTypeRequest {
            gb_per_core: 16,
            ..Default::default()
        }),
        "memopt"
    );
    assert_eq!(
        pick(NodeTypeRequest {
            min_gpus: 1,
            ..Default::default()
        }),
        "gpu"
    );
    assert_eq!(
        pick(NodeTypeRequest {
            graviton: true,
            ..Default::default()
        }),
        "arm"
    );
    assert_eq!(
        pick(NodeTypeRequest {
            fleet: true,
            ..Default::default()
        }),
        "m-fleet.small"
    );
    assert_eq!(
        pick(NodeTypeRequest {
            category: "memory optimized".into(),
            ..Default::default()
        }),
        "memopt"
    );
}

#[test]
fn smallest_node_type_capabilities_and_errors() {
    let nt = node_types();
    let pick = |r: NodeTypeRequest| nt.smallest(&r).unwrap();
    for r in [
        NodeTypeRequest {
            is_io_cache_enabled: true,
            ..Default::default()
        },
        NodeTypeRequest {
            support_port_forwarding: true,
            ..Default::default()
        },
        NodeTypeRequest {
            photon_driver_capable: true,
            ..Default::default()
        },
        NodeTypeRequest {
            photon_worker_capable: true,
            ..Default::default()
        },
    ] {
        assert_eq!(pick(r), "big");
    }
    let e = nt
        .smallest(&NodeTypeRequest {
            min_cores: 64,
            ..Default::default()
        })
        .unwrap_err();
    assert!(
        e.to_string()
            .contains("cannot determine smallest node type"),
        "{e}"
    );
    let e = ListNodeTypesResponse::default()
        .smallest(&NodeTypeRequest::default())
        .unwrap_err();
    assert!(e.to_string().contains("empty response"), "{e}");
}

fn versions() -> GetSparkVersionsResponse {
    let v = |key: &str, name: &str| json!({"key": key, "name": name});
    serde_json::from_value(json!({"versions": [
        v("13.3.x-scala2.12", "13.3 LTS (includes Apache Spark 3.4.1, Scala 2.12)"),
        v("14.3.x-scala2.12", "14.3 LTS (includes Apache Spark 3.5.0, Scala 2.12)"),
        v("15.0.x-scala2.12", "15.0 (includes Apache Spark 3.5.0, Scala 2.12)"),
        v("16.0.x-scala2.12", "16.0 Beta (includes Apache Spark 3.5.2, Scala 2.12)"),
        v("14.3.x-cpu-ml-scala2.12", "14.3 LTS ML (includes Apache Spark 3.5.0, Scala 2.12)"),
        v("14.3.x-photon-scala2.12", "14.3 LTS Photon (includes Apache Spark 3.5.0, Scala 2.12)"),
        v("12.2.x-esr-scala2.12", "12.2 ESR (includes Apache Spark 3.3.2, Scala 2.12)"),
        v("apache-spark-3.5.x-scala2.12", "Light 3.5"),
        v("17.0.x-scala2.13", "17.0 (includes Apache Spark 4.0.0, Scala 2.13)"),
        v("17.0.x-scala2.12", "17.0 (includes Apache Spark 4.0.0, Scala 2.12)"),
        v("custom-scala2.11", "custom (includes Apache Spark 2.4, Scala 2.11)")
    ]}))
    .unwrap()
}

#[test]
fn spark_version_selection_follows_go() {
    let sv = versions();
    let pick = |r: SparkVersionRequest| sv.select(&r);
    assert_eq!(
        pick(SparkVersionRequest {
            latest: true,
            long_term_support: true,
            ..Default::default()
        })
        .unwrap(),
        "14.3.x-scala2.12"
    );
    assert_eq!(
        pick(SparkVersionRequest {
            latest: true,
            ..Default::default()
        })
        .unwrap(),
        "17.0.x-scala2.12"
    );
    assert_eq!(
        pick(SparkVersionRequest {
            ml: true,
            ..Default::default()
        })
        .unwrap(),
        "14.3.x-cpu-ml-scala2.12"
    );
    assert_eq!(
        pick(SparkVersionRequest {
            photon: true,
            ..Default::default()
        })
        .unwrap(),
        "14.3.x-photon-scala2.12"
    );
    assert_eq!(
        pick(SparkVersionRequest {
            beta: true,
            ..Default::default()
        })
        .unwrap(),
        "16.0.x-scala2.12"
    );
    // Same runtime in two Scala versions: 2.12 wins.
    assert_eq!(
        pick(SparkVersionRequest {
            spark_version: "4.0.0".into(),
            ..Default::default()
        })
        .unwrap(),
        "17.0.x-scala2.12"
    );
    assert_eq!(
        pick(SparkVersionRequest {
            scala: "2.11".into(),
            ..Default::default()
        })
        .unwrap(),
        "custom-scala2.11"
    );
    let e = pick(SparkVersionRequest::default()).unwrap_err();
    assert!(e.to_string().contains("returned multiple results"), "{e}");
    let e = pick(SparkVersionRequest {
        gpu: true,
        ..Default::default()
    })
    .unwrap_err();
    assert!(e.to_string().contains("returned no results"), "{e}");
    // A non-DBR key sorts after DBR versions when taking the latest.
    let mixed: GetSparkVersionsResponse = serde_json::from_value(json!({"versions": [
        {"key": "custom-scala2.12", "name": "custom"},
        {"key": "10.4.x-scala2.12", "name": "10.4"},
        {"key": "odd-scala2.12", "name": "odd"}
    ]}))
    .unwrap();
    assert_eq!(
        mixed
            .select(&SparkVersionRequest {
                latest: true,
                ..Default::default()
            })
            .unwrap(),
        "10.4.x-scala2.12"
    );
}

#[tokio::test]
async fn select_via_the_api() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.1/clusters/list-node-types"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::to_value(node_types()).unwrap()),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.1/clusters/spark-versions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::to_value(versions()).unwrap()),
        )
        .mount(&server)
        .await;
    let c = workspace(&server).await.clusters();
    assert_eq!(
        c.select_node_type(&NodeTypeRequest {
            min_gpus: 1,
            ..Default::default()
        })
        .await
        .unwrap(),
        "gpu"
    );
    assert_eq!(
        c.select_spark_version(&SparkVersionRequest {
            ml: true,
            ..Default::default()
        })
        .await
        .unwrap(),
        "14.3.x-cpu-ml-scala2.12"
    );
}

fn cluster(state: &str) -> Value {
    json!({"cluster_id": "c1", "cluster_name": "etl", "state": state, "state_message": "why"})
}

#[tokio::test]
async fn ensure_cluster_is_running_starts_or_waits() {
    let server = MockServer::start().await;
    // c1: terminated, then running once started.
    Mock::given(method("GET"))
        .and(path("/api/2.1/clusters/get"))
        .and(query_param("cluster_id", "c1"))
        .respond_with(Sequence::json(vec![
            cluster("TERMINATED"),
            cluster("RUNNING"),
        ]))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/2.1/clusters/start"))
        .and(body_partial_json(json!({"cluster_id": "c1"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    // c2: pending, then running.
    Mock::given(method("GET"))
        .and(path("/api/2.1/clusters/get"))
        .and(query_param("cluster_id", "c2"))
        .respond_with(Sequence::json(vec![cluster("PENDING"), cluster("RUNNING")]))
        .mount(&server)
        .await;
    // c3: in ERROR, which can't be fixed.
    Mock::given(method("GET"))
        .and(path("/api/2.1/clusters/get"))
        .and(query_param("cluster_id", "c3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(cluster("ERROR")))
        .mount(&server)
        .await;
    // c4: another process is starting it (INVALID_STATE), then running.
    Mock::given(method("GET"))
        .and(path("/api/2.1/clusters/get"))
        .and(query_param("cluster_id", "c4"))
        .respond_with(Sequence(
            vec![
                ResponseTemplate::new(400).set_body_json(
                    json!({"error_code": "INVALID_STATE", "message": "being started"}),
                ),
                ResponseTemplate::new(200).set_body_json(cluster("RUNNING")),
            ],
            Arc::default(),
        ))
        .mount(&server)
        .await;
    let c = workspace(&server).await.clusters();
    c.ensure_cluster_is_running("c1").await.unwrap();
    c.ensure_cluster_is_running("c2").await.unwrap();
    let e = c.ensure_cluster_is_running("c3").await.unwrap_err();
    assert!(
        e.to_string().contains("cluster etl is in ERROR state: why"),
        "{e}"
    );
    c.ensure_cluster_is_running("c4").await.unwrap();
}

#[tokio::test]
async fn a_terminating_cluster_is_waited_out_and_restarted() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.1/clusters/get"))
        .respond_with(Sequence::json(vec![
            cluster("TERMINATING"),
            cluster("TERMINATED"),
            cluster("RUNNING"),
        ]))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/2.1/clusters/start"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    workspace(&server)
        .await
        .clusters()
        .ensure_cluster_is_running("c1")
        .await
        .unwrap();
}

#[tokio::test]
async fn get_or_create_running_cluster() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.1/clusters/list"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"clusters": [
                {"cluster_id": "c1", "cluster_name": "running", "state": "RUNNING"},
                {"cluster_id": "c2", "cluster_name": "stopped", "state": "TERMINATED"}
            ]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/2.1/clusters/start"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.1/clusters/get"))
        .and(query_param("cluster_id", "c2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"cluster_id": "c2", "cluster_name": "stopped", "state": "RUNNING"}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.1/clusters/list-node-types"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::to_value(node_types()).unwrap()),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.1/clusters/spark-versions"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::to_value(versions()).unwrap()),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/2.1/clusters/create"))
        .and(body_partial_json(json!({
            "cluster_name": "fresh", "num_workers": 1, "autotermination_minutes": 10,
            "spark_version": "14.3.x-scala2.12", "node_type_id": "zero"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"cluster_id": "c3"})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/2.1/clusters/create"))
        .and(body_partial_json(json!({"cluster_name": "custom"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"cluster_id": "c4"})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.1/clusters/get"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"cluster_id": "new", "state": "RUNNING"})),
        )
        .mount(&server)
        .await;
    let c = workspace(&server).await.clusters();
    let running = c
        .get_or_create_running_cluster("running", None)
        .await
        .unwrap();
    assert_eq!(running.cluster_id.as_deref(), Some("c1"));
    let started = c
        .get_or_create_running_cluster("stopped", None)
        .await
        .unwrap();
    assert_eq!(started.cluster_id.as_deref(), Some("c2"));
    let created = c
        .get_or_create_running_cluster("fresh", None)
        .await
        .unwrap();
    assert!(created.is_running_or_resizing());
    let custom = CreateCluster::new("15.0.x-scala2.12").with_cluster_name("custom");
    c.get_or_create_running_cluster("custom", Some(custom))
        .await
        .unwrap();
}

#[test]
fn cluster_state_helpers() {
    let with = |s: State| ClusterDetails::default().with_state(s);
    assert!(with(State::Running).is_running_or_resizing());
    assert!(with(State::Resizing).is_running_or_resizing());
    assert!(!with(State::Pending).is_running_or_resizing());
    assert!(!ClusterDetails::default().is_running_or_resizing());
}

// ----------------------------------------------------------------- libraries

fn pypi(p: &str) -> Library {
    Library::default().with_pypi(PythonPyPiLibrary::new(p))
}

#[test]
fn library_strings_sort_and_scope() {
    let maven = MavenLibrary::new("org:a:1").with_exclusions(vec!["x".to_owned()]);
    assert_eq!(
        Library::default().with_whl("/a.whl").to_string(),
        "whl:/a.whl"
    );
    assert_eq!(
        Library::default().with_jar("/a.jar").to_string(),
        "jar:/a.jar"
    );
    assert_eq!(pypi("numpy").to_string(), "pypi:numpy");
    assert_eq!(
        Library::default()
            .with_pypi(PythonPyPiLibrary::new("x").with_repo("r/"))
            .to_string(),
        "pypi:r/x"
    );
    assert_eq!(
        Library::default().with_maven(maven).to_string(),
        "mvn:org:a:1x"
    );
    assert_eq!(
        Library::default().with_egg("/e.egg").to_string(),
        "egg:/e.egg"
    );
    assert_eq!(
        Library::default()
            .with_cran(RCranLibrary::new("dplyr"))
            .to_string(),
        "cran:dplyr"
    );
    assert_eq!(Library::default().to_string(), "unknown");

    let mut install = InstallLibraries::new("c", vec![pypi("b"), pypi("a")]);
    install.sort();
    assert_eq!(install.libraries[0].to_string(), "pypi:a");

    let statuses: ClusterLibraryStatuses = serde_json::from_value(json!({
        "cluster_id": "c",
        "library_statuses": [
            {"library": {"pypi": {"package": "z"}}, "status": "INSTALLED"},
            {"library": {"pypi": {"package": "y"}}, "status": "PENDING"},
            {"library": {"pypi": {"package": "x"}}, "status": "FAILED", "messages": ["boom", "bang"]},
            {"library": {"jar": "/all.jar"}, "status": "INSTALLED", "is_library_for_all_clusters": true}
        ]
    }))
    .unwrap();
    let list = statuses.to_library_list();
    let names: Vec<_> = list.libraries.iter().map(ToString::to_string).collect();
    assert_eq!(names, ["jar:/all.jar", "pypi:x", "pypi:y", "pypi:z"]);

    // A status without a library (Go panics) is an error.
    let odd: ClusterLibraryStatuses = serde_json::from_value(json!({
        "library_statuses": [{"status": "INSTALLED"}]
    }))
    .unwrap();
    assert!(odd.to_library_list().libraries.is_empty());
    let e = odd.is_retry_needed(&LibraryWait::default()).unwrap_err();
    assert!(
        e.to_string().contains("library status without a library"),
        "{e}"
    );

    let all = LibraryWait::default();
    assert!(!all.is_not_in_scope(&pypi("q")));
    assert_eq!(
        statuses.is_retry_needed(&all).unwrap().as_deref(),
        Some("1 libraries are ready, but there are still 1 pending")
    );
    let only_x = LibraryWait {
        libraries: vec![pypi("x")],
        ..Default::default()
    };
    assert!(only_x.is_not_in_scope(&pypi("z")));
    let e = statuses.is_retry_needed(&only_x).unwrap_err();
    assert!(e.to_string().contains("pypi:x failed: boom, bang"), "{e}");
    let refresh = LibraryWait {
        is_refresh: true,
        ..only_x
    };
    assert_eq!(statuses.is_retry_needed(&refresh).unwrap(), None);
}

#[tokio::test]
async fn update_and_wait_installs_waits_and_cleans_up_failures() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/2.0/libraries/uninstall"))
        .and(body_partial_json(
            json!({"libraries": [{"pypi": {"package": "old"}}]}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/2.0/libraries/install"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/libraries/cluster-status"))
        .and(query_param("cluster_id", "c1"))
        .respond_with(Sequence(
            vec![
                ResponseTemplate::new(404).set_body_json(
                    json!({"error_code": "RESOURCE_DOES_NOT_EXIST", "message": "x"}),
                ),
                ResponseTemplate::new(200).set_body_json(
                    json!({"cluster_id": "c1", "library_statuses": [
                        {"library": {"pypi": {"package": "new"}}, "status": "INSTALLING"}
                    ]}),
                ),
                ResponseTemplate::new(200).set_body_json(
                    json!({"cluster_id": "c1", "library_statuses": [
                        {"library": {"pypi": {"package": "new"}}, "status": "INSTALLED"},
                        {"library": {"pypi": {"package": "flaky"}}, "status": "FAILED"}
                    ]}),
                ),
            ],
            Arc::default(),
        ))
        .mount(&server)
        .await;
    // The failed out-of-scope library is removed afterwards.
    Mock::given(method("POST"))
        .and(path("/api/2.0/libraries/uninstall"))
        .and(body_partial_json(
            json!({"libraries": [{"pypi": {"package": "flaky"}}]}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let libs = workspace(&server).await.libraries();
    libs.update_and_wait(
        LibraryUpdate {
            cluster_id: "c1".into(),
            install: vec![pypi("new")],
            uninstall: vec![pypi("old")],
        },
        Duration::from_secs(30),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn wait_on_a_stopped_cluster_returns_the_statuses() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/libraries/cluster-status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"cluster_id": "c", "library_statuses": [
                {"library": {"pypi": {"package": "p"}}, "status": "PENDING"}
            ]}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/2.0/libraries/install"))
        .respond_with(
            ResponseTemplate::new(500)
                .set_body_json(json!({"error_code": "INTERNAL_ERROR", "message": "x"})),
        )
        .mount(&server)
        .await;
    let libs = workspace(&server).await.libraries();
    let status = libs
        .wait(
            &LibraryWait {
                cluster_id: "c".into(),
                ..Default::default()
            },
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert_eq!(status.library_statuses.len(), 1);
    let e = libs
        .update_and_wait(
            LibraryUpdate {
                cluster_id: "c".into(),
                install: vec![pypi("p")],
                ..Default::default()
            },
            Duration::from_secs(5),
        )
        .await
        .unwrap_err();
    assert!(e.to_string().contains("install:"), "{e}");
}

// ------------------------------------------------------------------ commands

async fn mount_commands(server: &MockServer, results: Value) {
    Mock::given(method("GET"))
        .and(path("/api/2.1/clusters/get"))
        .and(query_param("cluster_id", "c1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(cluster("RUNNING")))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/1.2/contexts/create"))
        .and(body_partial_json(
            json!({"clusterId": "c1", "language": "python"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "ctx"})))
        .mount(server)
        .await;
    // The waiters must send every ID (the context and command too).
    Mock::given(method("GET"))
        .and(path("/api/1.2/contexts/status"))
        .and(query_param("clusterId", "c1"))
        .and(query_param("contextId", "ctx"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id": "ctx", "status": "Running"})),
        )
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/1.2/commands/execute"))
        .and(body_partial_json(
            json!({"command": "print(1)\n", "contextId": "ctx"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "cmd"})))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/1.2/commands/status"))
        .and(query_param("clusterId", "c1"))
        .and(query_param("contextId", "ctx"))
        .and(query_param("commandId", "cmd"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": "cmd", "status": "Finished", "results": results})),
        )
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/1.2/contexts/destroy"))
        .and(body_partial_json(
            json!({"clusterId": "c1", "contextId": "ctx"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(server)
        .await;
}

#[tokio::test]
async fn command_executor_runs_in_a_context() {
    let server = MockServer::start().await;
    mount_commands(&server, json!({"resultType": "text", "data": "Out[1]: 1"})).await;
    let w = workspace(&server).await;
    let ex = w
        .command_execution()
        .start("c1", Language::Python)
        .await
        .unwrap();
    assert_eq!(ex.context_id(), "ctx");
    let res = ex.execute("\n    print(1)\n").await.unwrap().unwrap();
    assert_eq!(res.text(), "1");
    assert!(!res.failed());
    res.err().unwrap();
    ex.destroy().await.unwrap();
}

#[tokio::test]
async fn high_level_execute_turns_failures_into_error_results() {
    let server = MockServer::start().await;
    mount_commands(
        &server,
        json!({"resultType": "error", "summary": "<b>java.lang.RuntimeException: nope</b>"}),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/api/2.1/clusters/get"))
        .and(query_param("cluster_id", "stopped"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"state": "TERMINATED"})))
        .mount(&server)
        .await;
    let w = workspace(&server).await;
    let ex = new_command_executor(w.api_client().clone());
    let res = ex.execute("c1", Language::Python, "print(1)").await;
    assert!(res.failed());
    assert_eq!(res.error(), "nope");
    assert!(res.err().unwrap_err().to_string().contains("nope"));
    let res = ex.execute("stopped", Language::Python, "x").await;
    assert!(
        res.summary
            .unwrap()
            .contains("has to be running or resizing, but is TERMINATED")
    );
    let res = ex.execute("missing", Language::Python, "x").await;
    assert!(res.failed());
}

#[tokio::test]
async fn high_level_execute_without_results() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/1.2/commands/status"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id": "cmd", "status": "Finished"})),
        )
        .mount(&server)
        .await;
    mount_commands(&server, json!({})).await;
    let w = workspace(&server).await;
    let res = new_command_executor(w.api_client().clone())
        .execute("c1", Language::Python, "print(1)")
        .await;
    assert_eq!(res.summary.as_deref(), Some("Command has no results"));
}

#[test]
fn results_accessors() {
    let mut table = Results::default()
        .with_result_type(ResultType::Table)
        .with_data(json!([[1, "a"], [2, "b"]]));
    assert_eq!(table.scan(), Some(vec![json!(1), json!("a")]));
    assert_eq!(table.scan(), Some(vec![json!(2), json!("b")]));
    assert_eq!(table.scan(), None);
    assert_eq!(table.text(), "");
    assert_eq!(table.error(), "");
    assert!(
        Results::default()
            .with_result_type(ResultType::Text)
            .scan()
            .is_none()
    );
    let err = Results::default()
        .with_result_type(ResultType::Error)
        .with_summary("x")
        .with_cause("ErrorMessage=table not found\n");
    assert_eq!(err.error(), "table not found");
    assert_eq!(trim_leading_whitespace("  a\n    b"), "a\n  b\n");
}
