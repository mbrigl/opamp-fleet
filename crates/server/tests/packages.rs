//! Package delivery (ADR-0015): the REST upload + download endpoints, and the hash-gated
//! `PackagesAvailable` offer toward capable Agents.

mod support;

use opamp::proto::{
    AgentCapabilities, PackageStatus, PackageStatusEnum, PackageStatuses, ServerCapabilities,
    ServerToAgent,
};
use opamp::uid::InstanceUid;
use prost::Message as _;
use server::fleet::{AppState, PackageOffering};
use server::packages::PackageStore;
use std::sync::Arc;
use support::{full_report, TestServer};

const PROTOBUF: &str = "application/x-protobuf";

/// A Server with package delivery armed over a temp store; returns the server and its temp dir.
async fn spawn_with_packages() -> (TestServer, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = PackageStore::open(dir.path().join("packages")).expect("store");
    let state = Arc::new(
        AppState::new(dir.path().join("fleet-configs"))
            .expect("configs")
            .with_packages(Some(PackageOffering::new(store, String::new()))),
    );
    let app = server::app(state.clone(), None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    (
        TestServer {
            addr,
            state,
            _dir: dir,
        },
        tempfile::tempdir().expect("scratch"),
    )
}

async fn exchange(server: &TestServer, msg: &opamp::proto::AgentToServer) -> ServerToAgent {
    let response = reqwest::Client::new()
        .post(format!("http://{}/v1/opamp", server.addr))
        .header("content-type", PROTOBUF)
        .body(msg.encode_to_vec())
        .send()
        .await
        .expect("post");
    assert_eq!(response.status(), 200);
    ServerToAgent::decode(response.bytes().await.expect("body").as_ref()).expect("decode")
}

async fn upload(server: &TestServer, name: &str, version: &str, artifact: &[u8]) {
    let response = reqwest::Client::new()
        .put(format!(
            "http://{}/api/v1/packages/{name}?version={version}",
            server.addr
        ))
        .body(artifact.to_vec())
        .send()
        .await
        .expect("put package");
    assert_eq!(response.status(), 200, "upload should succeed");
}

#[tokio::test]
async fn an_uploaded_package_is_offered_downloaded_and_gated() {
    let (server, _scratch) = spawn_with_packages().await;
    let uid = InstanceUid::default();
    let mut report = full_report(&uid, "collector", 1);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;

    // Nothing uploaded yet: no offer, and the capability is not declared.
    let reply = exchange(&server, &report).await;
    assert!(reply.packages_available.is_none());
    assert_eq!(
        reply.capabilities & ServerCapabilities::OffersPackages as u64,
        0
    );

    upload(&server, "otelcol", "1.2.3", b"the-new-binary").await;

    // Now the offer arrives, declares the capability, and carries a working download URL.
    let reply = exchange(&server, &report).await;
    assert_ne!(
        reply.capabilities & ServerCapabilities::OffersPackages as u64,
        0
    );
    let offer = reply.packages_available.expect("an offer");
    assert!(!offer.all_packages_hash.is_empty());
    let available = &offer.packages["otelcol"];
    assert_eq!(available.version, "1.2.3");
    let file = available.file.as_ref().expect("a downloadable file");
    // download_base was empty, so the URL is a path the Client resolves against its endpoint.
    assert_eq!(file.download_url, "/api/v1/packages/otelcol/file");
    assert_eq!(file.content_hash, sha256(b"the-new-binary"));

    // The artifact downloads byte-for-byte.
    let downloaded = reqwest::Client::new()
        .get(format!(
            "http://{}/api/v1/packages/otelcol/file",
            server.addr
        ))
        .send()
        .await
        .expect("download")
        .bytes()
        .await
        .expect("bytes");
    assert_eq!(downloaded.as_ref(), b"the-new-binary");

    // Reporting the offered aggregate hash as installed silences the offer (the Baseline's gate).
    let mut installed = full_report(&uid, "collector", 2);
    installed.capabilities |= AgentCapabilities::AcceptsPackages as u64
        | AgentCapabilities::ReportsPackageStatuses as u64;
    installed.package_statuses = Some(PackageStatuses {
        packages: [(
            "otelcol".to_string(),
            PackageStatus {
                name: "otelcol".to_string(),
                agent_has_version: "1.2.3".to_string(),
                status: PackageStatusEnum::Installed as i32,
                ..Default::default()
            },
        )]
        .into(),
        server_provided_all_packages_hash: offer.all_packages_hash.clone(),
        error_message: String::new(),
    });
    let reply = exchange(&server, &installed).await;
    assert!(
        reply.packages_available.is_none(),
        "a matching reported hash stops the offer"
    );
}

#[tokio::test]
async fn no_offer_without_the_capability() {
    let (server, _scratch) = spawn_with_packages().await;
    upload(&server, "otelcol", "1.0.0", b"bin").await;
    let uid = InstanceUid::default();
    // full_report declares no AcceptsPackages.
    let reply = exchange(&server, &full_report(&uid, "incapable", 1)).await;
    assert!(
        reply.packages_available.is_none(),
        "capability negotiation is binding"
    );
}

/// A package is a *program*: an `otelcol-contrib` binary weighs hundreds of megabytes, so the
/// upload route must not be bounded by the framework's 2 MiB default, and the artifact must reach
/// the Agent unchanged whatever its size.
#[tokio::test]
async fn an_artifact_larger_than_the_framework_default_uploads_and_downloads_intact() {
    let (server, _scratch) = spawn_with_packages().await;

    // Past axum's 2 MiB default body limit — the limit that used to make a real binary
    // undeliverable — and not a round number, so a truncation would show.
    let artifact: Vec<u8> = (0..(5 * 1024 * 1024 + 17))
        .map(|i| (i % 251) as u8)
        .collect();
    upload(&server, "otelcol", "1.2.3", &artifact).await;

    let downloaded = reqwest::Client::new()
        .get(format!(
            "http://{}/api/v1/packages/otelcol/file",
            server.addr
        ))
        .send()
        .await
        .expect("download");
    assert_eq!(
        downloaded
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok()),
        Some(artifact.len().to_string().as_str()),
        "the length is advertised, so the Agent can size the transfer"
    );
    let bytes = downloaded.bytes().await.expect("bytes");
    assert_eq!(bytes.len(), artifact.len());
    assert_eq!(bytes.as_ref(), artifact.as_slice(), "byte-identical");
    assert_eq!(sha256(&artifact).len(), 32);
}

/// The upload limit is a configured bound, not an accident of the framework: past it the API
/// refuses rather than buffering whatever arrives.
#[tokio::test]
async fn an_artifact_past_the_configured_limit_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = PackageStore::open(dir.path().join("packages")).expect("store");
    let state = Arc::new(
        AppState::new(dir.path().join("fleet-configs"))
            .expect("configs")
            .with_packages(Some(PackageOffering::new(store, String::new())))
            .with_max_package_size(4096),
    );
    let app = server::app(state, None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    let response = reqwest::Client::new()
        .put(format!(
            "http://{addr}/api/v1/packages/otelcol?version=1.0.0"
        ))
        .body(vec![0u8; 8192])
        .send()
        .await
        .expect("put package");
    assert_eq!(response.status(), 413);
}

fn sha256(bytes: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).to_vec()
}
