//! Binary request and response bodies: file download/upload, plain-text
//! exports, and the `TransferOwnership` path fix (see
//! `codegen/ir_patches.json`).
//!
//! Run with `--all-features` (CI does).

#![cfg(all(
    feature = "billing",
    feature = "dashboards",
    feature = "files",
    feature = "serving",
    feature = "sql"
))]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use community_databricks_sdk::core::config::HostMetadata;
use community_databricks_sdk::core::http::{Binary, Bytes};
use community_databricks_sdk::service::billing::DownloadRequest as UsageDownloadRequest;
use community_databricks_sdk::service::dashboards::DownloadMessageAttachmentVisualizationRequest;
use community_databricks_sdk::service::files::{DownloadRequest, UploadRequest};
use community_databricks_sdk::service::serving::{
    ExportMetricsRequest, ExternalFunctionRequest, ExternalFunctionRequestHttpMethod,
    GetOpenApiRequest,
};
use community_databricks_sdk::service::sql::{OwnableObjectType, TransferOwnershipRequest};
use community_databricks_sdk::{AccountClient, Config, ErrorKind, WorkspaceClient};
use futures_util::{StreamExt, stream};
use serde_json::json;
use wiremock::matchers::{body_bytes, body_json, header, method, path, query_param};
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

/// 503 for the first `fail` requests, then 200.
struct FailThenOk {
    fail: usize,
    seen: Arc<AtomicUsize>,
}

impl Respond for FailThenOk {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        if self.seen.fetch_add(1, Ordering::SeqCst) < self.fail {
            ResponseTemplate::new(503)
                .set_body_json(json!({"error_code": "TEMPORARILY_UNAVAILABLE", "message": "busy"}))
        } else {
            ResponseTemplate::new(200)
        }
    }
}

#[tokio::test]
async fn download_streams_the_body_and_reads_headers() {
    let server = MockServer::start().await;
    let body = vec![7u8; 100_000];
    Mock::given(method("GET"))
        .and(path("/api/2.0/fs/files/Volumes/c/s/v/data%20file.bin"))
        .and(header("accept", "application/octet-stream"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/octet-stream")
                .insert_header("last-modified", "Wed, 24 Sep 2026 10:00:00 GMT")
                .set_body_bytes(body.clone()),
        )
        .expect(1)
        .mount(&server)
        .await;
    let resp = workspace(&server)
        .await
        .files()
        .download(DownloadRequest::new("/Volumes/c/s/v/data file.bin"))
        .await
        .unwrap();
    assert_eq!(resp.content_length, Some(100_000));
    assert_eq!(
        resp.content_type.as_deref(),
        Some("application/octet-stream")
    );
    assert_eq!(
        resp.last_modified.as_deref(),
        Some("Wed, 24 Sep 2026 10:00:00 GMT")
    );
    assert!(resp.contents.is_stream());
    // Read chunk by chunk, as a caller writing to disk would.
    let mut chunks = resp.contents.into_stream();
    let mut got = Vec::new();
    while let Some(chunk) = chunks.next().await {
        got.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(got, body);
}

#[tokio::test]
async fn download_errors_are_decoded_as_api_errors() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/fs/files/Volumes/missing"))
        .respond_with(ResponseTemplate::new(404).set_body_json(
            json!({"error_code": "NOT_FOUND", "message": "The file does not exist"}),
        ))
        .mount(&server)
        .await;
    let e = workspace(&server)
        .await
        .files()
        .download(DownloadRequest::new("/Volumes/missing"))
        .await
        .unwrap_err();
    assert!(e.is(ErrorKind::NotFound), "{e}");
}

#[tokio::test]
async fn buffered_upload_is_replayed_on_retry() {
    let server = MockServer::start().await;
    let seen = Arc::new(AtomicUsize::new(0));
    Mock::given(method("PUT"))
        .and(path("/api/2.0/fs/files/Volumes/c/s/v/out.csv"))
        .and(query_param("overwrite", "true"))
        .and(header("content-type", "application/octet-stream"))
        .and(body_bytes(b"a,b\n1,2\n".to_vec()))
        .respond_with(FailThenOk {
            fail: 1,
            seen: seen.clone(),
        })
        .expect(2)
        .mount(&server)
        .await;
    workspace(&server)
        .await
        .files()
        .upload(UploadRequest::new("a,b\n1,2\n", "/Volumes/c/s/v/out.csv").with_overwrite(true))
        .await
        .unwrap();
    assert_eq!(seen.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn streamed_upload_is_sent_once_and_never_retried() {
    let server = MockServer::start().await;
    let seen = Arc::new(AtomicUsize::new(0));
    Mock::given(method("PUT"))
        .and(path("/api/2.0/fs/files/Volumes/big.bin"))
        .and(body_bytes(b"part1part2".to_vec()))
        .respond_with(FailThenOk {
            fail: 1,
            seen: seen.clone(),
        })
        .expect(1)
        .mount(&server)
        .await;
    let body = Binary::from_stream(stream::iter([
        Ok::<_, std::io::Error>(Bytes::from_static(b"part1")),
        Ok(Bytes::from_static(b"part2")),
    ]));
    let e = workspace(&server)
        .await
        .files()
        .upload(UploadRequest::new(body, "/Volumes/big.bin"))
        .await
        .unwrap_err();
    assert!(e.is(ErrorKind::TemporarilyUnavailable), "{e}");
    assert_eq!(seen.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn plain_text_responses_use_their_accept_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/serving-endpoints/ep/metrics"))
        .and(header("accept", "text/plain"))
        .respond_with(ResponseTemplate::new(200).set_body_string("requests_total 3\n"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/serving-endpoints/ep/openapi"))
        .and(header("accept", "text/plain"))
        .respond_with(ResponseTemplate::new(200).set_body_string("openapi: 3.1.0"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/2.0/external-function"))
        .and(header("accept", "text/plain"))
        .and(header("content-type", "application/json"))
        .and(body_json(
            json!({"connection_name": "gh", "method": "GET", "path": "/user"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string("{\"login\":\"x\"}"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/genie/spaces/s/conversations/c/messages/m/attachments/a/download-visualization"))
        .and(header("accept", "application/octet-stream"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"PNG".to_vec()))
        .expect(1)
        .mount(&server)
        .await;
    let w = workspace(&server).await;
    let serving = w.serving_endpoints();
    let metrics = serving
        .export_metrics(ExportMetricsRequest::new("ep"))
        .await
        .unwrap();
    assert_eq!(
        metrics.contents.bytes().await.unwrap(),
        "requests_total 3\n"
    );
    let spec = serving
        .get_open_api(GetOpenApiRequest::new("ep"))
        .await
        .unwrap();
    assert_eq!(spec.contents.bytes().await.unwrap(), "openapi: 3.1.0");
    let out = serving
        .http_request(
            ExternalFunctionRequest::default()
                .with_connection_name("gh")
                .with_method(ExternalFunctionRequestHttpMethod::Get)
                .with_path("/user"),
        )
        .await
        .unwrap();
    assert_eq!(out.contents.bytes().await.unwrap(), "{\"login\":\"x\"}");
    let png = w
        .genie()
        .download_message_attachment_visualization(
            DownloadMessageAttachmentVisualizationRequest::new(
                "spaces/s/conversations/c/messages/m/attachments/a",
            ),
        )
        .await
        .unwrap();
    assert_eq!(png.contents.bytes().await.unwrap(), "PNG");
}

#[tokio::test]
async fn account_usage_download() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/2.0/accounts/acc/usage/download"))
        .and(query_param("start_month", "2026-08"))
        .and(query_param("end_month", "2026-09"))
        .and(header("accept", "text/plain"))
        .respond_with(ResponseTemplate::new(200).set_body_string("workspaceId,usage\n1,2\n"))
        .expect(1)
        .mount(&server)
        .await;
    let a = AccountClient::new(cfg(&server).account("acc"))
        .await
        .unwrap();
    let csv = a
        .billable_usage()
        .download(UsageDownloadRequest::new("2026-09", "2026-08"))
        .await
        .unwrap();
    assert_eq!(
        csv.contents.bytes().await.unwrap(),
        "workspaceId,usage\n1,2\n"
    );
}

#[tokio::test]
async fn transfer_ownership_puts_the_object_id_in_the_path() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/2.0/preview/sql/permissions/query/q-1/transfer"))
        .and(body_json(json!({"new_owner": "a@example.com"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message": "Success"})))
        .expect(1)
        .mount(&server)
        .await;
    workspace(&server)
        .await
        .dbsql_permissions()
        .transfer_ownership(
            TransferOwnershipRequest::new("q-1", OwnableObjectType::Query)
                .with_new_owner("a@example.com"),
        )
        .await
        .unwrap();
}
