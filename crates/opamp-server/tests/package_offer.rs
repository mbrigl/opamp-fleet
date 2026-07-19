//! End-to-end proof of software distribution (ADR-0018): a package-capable agent that reports an
//! out-of-date package set is offered the package over the OpAMP WebSocket, with a `download_url`, its
//! content hash, and the aggregate hash; and the offered file is served from the Server's own surface.
//! No Docker is involved — a plain WebSocket client and a router `oneshot` stand in for a sidecar.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use futures_util::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tower::util::ServiceExt;

use opamp::config::ConfigSource;
use opamp::fleet::Fleet;
use opamp::frame;
use opamp::packages::{PackageSource, PackageSpec};
use opamp::proto::{AgentCapabilities, AgentToServer, PackageStatuses, PackageType, ServerToAgent};
use opamp::server::{self, AppState, ServerOffers};
use opamp::ui::{self, UiState};

const BINARY: &[u8] = b"OTELCOL-BINARY-BYTES-v2";

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("opamp-pkgit-{}-{}", tag, std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn package_source() -> Arc<PackageSource> {
    let dir = temp_dir("src");
    let path = dir.join("otelcol");
    std::fs::write(&path, BINARY).unwrap();
    Arc::new(
        PackageSource::load(
            vec![PackageSpec {
                name: "otelcol".to_string(),
                version: "2.0.0".to_string(),
                package_type: PackageType::TopLevel,
                path,
            }],
            "http://dev:4321".to_string(),
            Some("s3cret".to_string()),
        )
        .unwrap(),
    )
}

#[tokio::test]
async fn a_package_capable_agent_is_offered_the_package_and_can_download_it() {
    let dir = temp_dir("cfg");
    let cfg_path = dir.join("collector.yaml");
    std::fs::write(&cfg_path, b"exporters:\n  debug:\n").unwrap();
    let config = Arc::new(ConfigSource::new(&cfg_path));
    config.reload().unwrap();

    let packages = package_source();
    let fleet = Arc::new(Fleet::new());
    let (pushes, _) = tokio::sync::broadcast::channel(16);
    let app_state = Arc::new(AppState::new(
        config.clone(),
        fleet.clone(),
        pushes.clone(),
        ServerOffers::default(),
        packages.clone(),
        None,
    ));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, server::router(app_state))
            .await
            .unwrap();
    });

    let url = format!("ws://{addr}{}", server::LISTEN_PATH);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("agent connects");

    // Report as a package-capable agent that holds no package yet (empty aggregate hash), so the Server
    // sees a difference and must offer.
    let report = AgentToServer {
        instance_uid: vec![0x01, 0x02, 0x03, 0x04],
        sequence_num: 1,
        capabilities: AgentCapabilities::ReportsStatus as u64
            | AgentCapabilities::AcceptsPackages as u64
            | AgentCapabilities::ReportsPackageStatuses as u64,
        package_statuses: Some(PackageStatuses::default()),
        ..Default::default()
    };
    ws.send(Message::Binary(frame::encode(&report).into()))
        .await
        .unwrap();

    // The reply must carry the package offer.
    let frame = tokio::time::timeout(Duration::from_secs(3), ws.next())
        .await
        .expect("server replies")
        .expect("a frame")
        .expect("a valid message");
    let bytes = match frame {
        Message::Binary(b) => b,
        other => panic!("expected a binary frame, got {other:?}"),
    };
    let reply: ServerToAgent = frame::decode(&bytes).expect("decode reply");
    let available = reply.packages_available.expect("the package is offered");
    assert_eq!(available.all_packages_hash, packages.all_packages_hash());
    let pkg = &available.packages["otelcol"];
    assert_eq!(pkg.version, "2.0.0");
    let file = pkg.file.as_ref().expect("a downloadable file");
    assert_eq!(file.download_url, "http://dev:4321/packages/otelcol");
    assert_eq!(file.content_hash, Sha256::digest(BINARY).to_vec());

    // The offered file is served from the Server's own surface, with the offered bytes.
    let ui_router = ui::router(UiState {
        fleet,
        config,
        pushes,
        packages,
    });
    let resp = ui_router
        .oneshot(
            Request::builder()
                .uri("/packages/otelcol")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        body.as_ref(),
        BINARY,
        "the download serves the offered bytes"
    );
}

#[tokio::test]
async fn an_agent_reporting_the_current_hash_is_not_offered_again() {
    let dir = temp_dir("cfg2");
    let cfg_path = dir.join("collector.yaml");
    std::fs::write(&cfg_path, b"exporters:\n  debug:\n").unwrap();
    let config = Arc::new(ConfigSource::new(&cfg_path));
    config.reload().unwrap();

    let packages = package_source();
    let fleet = Arc::new(Fleet::new());
    let (pushes, _) = tokio::sync::broadcast::channel(16);
    let app_state = Arc::new(AppState::new(
        config,
        fleet,
        pushes,
        ServerOffers::default(),
        packages.clone(),
        None,
    ));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, server::router(app_state))
            .await
            .unwrap();
    });
    let url = format!("ws://{addr}{}", server::LISTEN_PATH);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    // The agent already reports the Server's aggregate hash — the loop has converged, so no offer.
    let report = AgentToServer {
        instance_uid: vec![0x09],
        sequence_num: 1,
        capabilities: AgentCapabilities::ReportsStatus as u64
            | AgentCapabilities::AcceptsPackages as u64
            | AgentCapabilities::ReportsPackageStatuses as u64,
        package_statuses: Some(PackageStatuses {
            server_provided_all_packages_hash: packages.all_packages_hash().to_vec(),
            ..Default::default()
        }),
        ..Default::default()
    };
    ws.send(Message::Binary(frame::encode(&report).into()))
        .await
        .unwrap();

    let frame = tokio::time::timeout(Duration::from_secs(3), ws.next())
        .await
        .expect("server replies")
        .expect("a frame")
        .expect("a valid message");
    let bytes = match frame {
        Message::Binary(b) => b,
        other => panic!("expected a binary frame, got {other:?}"),
    };
    let reply: ServerToAgent = frame::decode(&bytes).unwrap();
    assert!(
        reply.packages_available.is_none(),
        "a converged agent is not offered the package again"
    );
}
