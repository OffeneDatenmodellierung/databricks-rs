//! Go's convenience helpers for SQL statements, workspace files and DBFS
//! (#15).
//!
//! Run with `--all-features` (CI does).

#![cfg(all(feature = "files", feature = "sql", feature = "workspace"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use community_databricks_sdk::core::config::HostMetadata;
use community_databricks_sdk::core::http::{Binary, Bytes};
use community_databricks_sdk::service::files::{FileMode, MAX_DBFS_BLOCK_SIZE};
use community_databricks_sdk::service::sql::ExecuteStatementRequest;
use community_databricks_sdk::service::workspace::{
    DownloadOptions, ExportFormat, ExportResponse, Import, ImportFormat, Language, UploadOptions,
};
use community_databricks_sdk::{Config, Error, WorkspaceClient};
use futures_util::stream;
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, header_regex, method, path, query_param};
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

/// Replies from `bodies` in order, repeating the last.
struct Sequence(Vec<Value>, Arc<AtomicUsize>);

impl Respond for Sequence {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let i = self.1.fetch_add(1, Ordering::SeqCst).min(self.0.len() - 1);
        ResponseTemplate::new(200).set_body_json(&self.0[i])
    }
}

fn statement(state: &str) -> Value {
    json!({"statement_id": "s1", "status": {"state": state}})
}

// --------------------------------------------------------------------- SQL

#[tokio::test]
async fn execute_and_wait_polls_until_succeeded() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/2.0/sql/statements"))
        .respond_with(ResponseTemplate::new(200).set_body_json(statement("PENDING")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/sql/statements/s1"))
        .respond_with(Sequence(
            vec![
                statement("RUNNING"),
                json!({"statement_id": "s1", "status": {"state": "SUCCEEDED"},
                       "result": {"row_count": 1}}),
            ],
            Arc::default(),
        ))
        .expect(2)
        .mount(&server)
        .await;
    let res = workspace(&server)
        .await
        .statement_execution()
        .execute_and_wait(ExecuteStatementRequest::new("select 1", "wh"))
        .await
        .unwrap();
    assert_eq!(res.statement_id.as_deref(), Some("s1"));
    assert!(res.result.is_some());
}

#[tokio::test]
async fn execute_and_wait_reports_terminal_failures() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/2.0/sql/statements"))
        .and(body_partial_json(json!({"statement": "bad"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "statement_id": "s2",
            "status": {"state": "FAILED", "error": {"error_code": "BAD_REQUEST", "message": "syntax"}}
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/2.0/sql/statements"))
        .and(body_partial_json(json!({"statement": "ok"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(statement("SUCCEEDED")))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/2.0/sql/statements"))
        .and(body_partial_json(json!({"statement": "later"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(statement("RUNNING")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/sql/statements/s1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(statement("CANCELED")))
        .mount(&server)
        .await;
    let sql = workspace(&server).await.statement_execution();
    let e = sql
        .execute_and_wait(ExecuteStatementRequest::new("bad", "wh"))
        .await
        .unwrap_err();
    assert!(e.to_string().contains("FAILED: BAD_REQUEST syntax"), "{e}");
    let ok = sql
        .execute_and_wait(ExecuteStatementRequest::new("ok", "wh"))
        .await
        .unwrap();
    assert_eq!(ok.statement_id.as_deref(), Some("s1"));
    let e = sql
        .execute_and_wait_with_timeout(
            ExecuteStatementRequest::new("later", "wh"),
            Duration::from_secs(5),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(e, Error::OperationFailed(ref m) if m == "CANCELED"),
        "{e}"
    );
}

// --------------------------------------------------------------- workspace

#[tokio::test]
async fn upload_sends_multipart_with_inferred_language() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/2.0/workspace/import"))
        .and(header_regex(
            "content-type",
            "^multipart/form-data; boundary=[0-9a-f-]{36}$",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(2)
        .mount(&server)
        .await;
    let ws = workspace(&server).await.workspace();
    ws.upload(
        "/Users/a/nb.py",
        "print(1)",
        UploadOptions::default()
            .overwrite()
            .format(ImportFormat::Source),
    )
    .await
    .unwrap();
    ws.write_file("/Users/a/data.csv", b"a,b\n".to_vec())
        .await
        .unwrap();
    let reqs = server.received_requests().await.unwrap();
    let first = String::from_utf8_lossy(&reqs[0].body);
    for part in [
        "name=\"path\"\r\n\r\n/Users/a/nb.py\r\n",
        "name=\"content\"; filename=\"content\"\r\nContent-Type: application/octet-stream\r\n\r\nprint(1)\r\n",
        "name=\"format\"\r\n\r\nSOURCE\r\n",
        "name=\"language\"\r\n\r\nPYTHON\r\n",
        "name=\"overwrite\"\r\n\r\ntrue\r\n",
    ] {
        assert!(first.contains(part), "missing {part:?} in {first}");
    }
    assert!(first.trim_end().ends_with("--"));
    let second = String::from_utf8_lossy(&reqs[1].body);
    assert!(
        second.contains("name=\"format\"\r\n\r\nAUTO\r\n"),
        "{second}"
    );
    assert!(!second.contains("name=\"language\""), "{second}");
    // An explicit language is kept when the extension doesn't say.
    let opts = UploadOptions::default().language(Language::Scala);
    assert_eq!(opts.language, Some(Language::Scala));
}

#[tokio::test]
async fn download_and_read_file_stream_the_export() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/workspace/export"))
        .and(query_param("path", "/Users/a/nb"))
        .and(query_param("direct_download", "true"))
        .and(query_param("format", "JUPYTER"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"{\"cells\":[]}".to_vec()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/workspace/export"))
        .and(query_param("path", "/Users/a/f.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"hello".to_vec()))
        .mount(&server)
        .await;
    let ws = workspace(&server).await.workspace();
    let nb = ws
        .download(
            "/Users/a/nb",
            DownloadOptions::default().format(ExportFormat::Jupyter),
        )
        .await
        .unwrap();
    assert_eq!(nb.bytes().await.unwrap(), "{\"cells\":[]}");
    assert_eq!(ws.read_file("/Users/a/f.txt").await.unwrap(), "hello");
}

#[tokio::test]
async fn recursive_list_walks_directories_and_skips_missing_ones() {
    let server = MockServer::start().await;
    let list = |p: &'static str, body: Value| {
        Mock::given(method("GET"))
            .and(path("/api/2.0/workspace/list"))
            .and(query_param("path", p))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
    };
    list(
        "/r",
        json!({"objects": [
            {"path": "/r/a", "object_type": "DIRECTORY"},
            {"path": "/r/gone", "object_type": "DIRECTORY"},
            {"path": "/r/nb", "object_type": "NOTEBOOK"}
        ]}),
    )
    .mount(&server)
    .await;
    list(
        "/r/a",
        json!({"objects": [{"path": "/r/a/f", "object_type": "FILE"}]}),
    )
    .mount(&server)
    .await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/workspace/list"))
        .and(query_param("path", "/r/gone"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({"error_code": "RESOURCE_DOES_NOT_EXIST", "message": "gone"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/workspace/list"))
        .and(query_param("path", "/denied"))
        .respond_with(
            ResponseTemplate::new(403)
                .set_body_json(json!({"error_code": "PERMISSION_DENIED", "message": "no"})),
        )
        .mount(&server)
        .await;
    let ws = workspace(&server).await.workspace();
    let all = ws.recursive_list("/r").await.unwrap();
    let paths: Vec<_> = all.iter().filter_map(|o| o.path.as_deref()).collect();
    assert_eq!(paths, ["/r/nb", "/r/a/f"]);
    let e = ws.recursive_list("/denied").await.unwrap_err();
    assert!(e.to_string().contains("list /denied:"), "{e}");
}

#[test]
fn notebook_imports_and_export_bytes() {
    let imp = Import::python_notebook_overwrite("/Users/a/nb", "\n    x = 1\n    print(x)\n");
    assert_eq!(imp.path, "/Users/a/nb");
    assert_eq!(imp.overwrite, Some(true));
    assert_eq!(imp.format, Some(ImportFormat::Source));
    assert_eq!(imp.language, Some(Language::Python));
    // "x = 1\nprint(x)\n", base64-encoded.
    assert_eq!(imp.content.as_deref(), Some("eCA9IDEKcHJpbnQoeCkK"));
    let resp = ExportResponse::default().with_content("aGVsbG8=");
    assert_eq!(resp.bytes().unwrap(), b"hello");
    assert!(
        ExportResponse::default()
            .with_content("%%")
            .bytes()
            .is_err()
    );
}

// -------------------------------------------------------------------- DBFS

#[tokio::test]
async fn dbfs_read_file_reads_in_blocks() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/dbfs/get-status"))
        .and(query_param("path", "/tmp/f"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"path": "/tmp/f", "file_size": 11})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/dbfs/read"))
        .and(query_param("path", "/tmp/f"))
        .and(query_param("offset", "0"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"bytes_read": 11, "data": "aGVsbG8gd29ybGQ="})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/dbfs/get-status"))
        .and(query_param("path", "/tmp/dir"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"is_dir": true})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/dbfs/get-status"))
        .and(query_param("path", "/tmp/short"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"file_size": 5})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/dbfs/read"))
        .and(query_param("path", "/tmp/short"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"bytes_read": 0})))
        .mount(&server)
        .await;
    let dbfs = workspace(&server).await.dbfs();
    assert_eq!(dbfs.read_file("/tmp/f").await.unwrap(), b"hello world");
    let mut h = dbfs.open("/tmp/f", FileMode::READ).await.unwrap();
    assert_eq!(h.path(), "/tmp/f");
    assert_eq!(h.size(), Some(11));
    let mut buf = [0u8; 20];
    assert_eq!(h.read(&mut buf).await.unwrap(), 11);
    assert_eq!(h.read(&mut buf).await.unwrap(), 0);
    assert!(h.write(b"x").await.is_err());
    assert!(h.close().await.is_err());
    let e = dbfs.open("/tmp/dir", FileMode::READ).await.unwrap_err();
    assert!(e.to_string().contains("cannot open directory"), "{e}");
    let e = dbfs.read_file("/tmp/short").await.unwrap_err();
    assert!(
        e.to_string()
            .contains("unexpected EOF at offset 0 (size 5)"),
        "{e}"
    );
    let e = dbfs
        .open("/tmp/f", FileMode::READ | FileMode::WRITE)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("must specify"), "{e}");
    assert!(dbfs.open("/tmp/f", FileMode::default()).await.is_err());
}

#[tokio::test]
async fn dbfs_write_file_and_write_from_add_blocks() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/2.0/dbfs/create"))
        .and(body_partial_json(
            json!({"path": "/tmp/out", "overwrite": true}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"handle": 7})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/2.0/dbfs/create"))
        .and(body_partial_json(json!({"path": "/tmp/new"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"handle": 8})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/2.0/dbfs/add-block"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/2.0/dbfs/close"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let dbfs = workspace(&server).await.dbfs();
    dbfs.write_file("/tmp/out", b"hello").await.unwrap();

    // A stream spanning two DBFS blocks.
    let mut h = dbfs.open("/tmp/new", FileMode::WRITE).await.unwrap();
    let big = vec![b'x'; MAX_DBFS_BLOCK_SIZE + 10];
    let body = Binary::from_stream(stream::iter([
        Ok::<_, std::io::Error>(Bytes::from(big[..600_000].to_vec())),
        Ok(Bytes::from(big[600_000..].to_vec())),
    ]));
    assert_eq!(h.write_from(body).await.unwrap(), big.len() as u64);
    let mut buf = [0u8; 1];
    assert!(h.read(&mut buf).await.is_err());
    assert!(h.read_all().await.is_err());
    h.close().await.unwrap();

    let reqs = server.received_requests().await.unwrap();
    let blocks: Vec<(i64, usize)> = reqs
        .iter()
        .filter(|r| r.url.path() == "/api/2.0/dbfs/add-block")
        .map(|r| {
            let b: Value = serde_json::from_slice(&r.body).unwrap();
            (
                b["handle"].as_i64().unwrap(),
                b["data"].as_str().unwrap().len(),
            )
        })
        .collect();
    // "hello", then a full block and the 10-byte rest (base64 lengths).
    assert_eq!(blocks, [(7, 8), (8, 1_398_104), (8, 16)]);
    let closes = reqs
        .iter()
        .filter(|r| r.url.path() == "/api/2.0/dbfs/close")
        .count();
    assert_eq!(closes, 2);
}

#[tokio::test]
async fn dbfs_recursive_list() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/dbfs/list"))
        .and(query_param("path", "/d"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"files": [
            {"path": "/d/sub", "is_dir": true},
            {"path": "/d/gone", "is_dir": true},
            {"path": "/d/a", "is_dir": false}
        ]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/dbfs/list"))
        .and(query_param("path", "/d/sub"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"files": [{"path": "/d/sub/b"}]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/dbfs/list"))
        .and(query_param("path", "/d/gone"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({"error_code": "RESOURCE_DOES_NOT_EXIST", "message": "gone"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/dbfs/list"))
        .and(query_param("path", "/x"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({"error_code": "INVALID_PARAMETER_VALUE", "message": "bad"})),
        )
        .mount(&server)
        .await;
    let dbfs = workspace(&server).await.dbfs();
    let files = dbfs.recursive_list("/d").await.unwrap();
    let paths: Vec<_> = files.iter().filter_map(|f| f.path.as_deref()).collect();
    assert_eq!(paths, ["/d/a", "/d/sub/b"]);
    assert!(dbfs.recursive_list("/x").await.is_err());
}
