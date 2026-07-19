//! End-to-end proof of the initial server's goal: an OpAMP agent that connects over the WebSocket
//! transport and reports its state becomes visible in the fleet, with its status, through the REST API
//! — the same path the dev sidecars take (ADR-0006, ADR-0007). No Docker is involved: a plain
//! WebSocket client stands in for a sidecar.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::Request;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tower::util::ServiceExt;

use opamp::api;
use opamp::config::ConfigSource;
use opamp::fleet::Fleet;
use opamp::frame;
use opamp::packages::PackageSource;
use opamp::proto::{
    any_value, AgentDescription, AgentToServer, AnyValue, ComponentHealth, KeyValue,
    RemoteConfigStatus, RemoteConfigStatuses,
};
use opamp::server::{self, AppState, ServerOffers};
use opamp::ui::UiState;

#[tokio::test]
async fn a_connected_agent_appears_in_the_fleet_with_its_status() {
    // A configuration for the server to distribute, so the agent can report holding it (in sync).
    let dir = std::env::temp_dir().join(format!("opamp-it-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cfg_path = dir.join("collector.yaml");
    std::fs::write(&cfg_path, b"exporters:\n  debug:\n").unwrap();
    let config = Arc::new(ConfigSource::new(&cfg_path));
    config.reload().unwrap();
    let want_hash = config.current().unwrap().config_hash;

    let fleet = Arc::new(Fleet::new());
    let (pushes, _) = tokio::sync::broadcast::channel(16);
    let app_state = Arc::new(AppState::new(
        config.clone(),
        fleet.clone(),
        pushes.clone(),
        ServerOffers::default(),
        Arc::new(PackageSource::empty()),
        None,
    ));

    // Serve the OpAMP endpoint on an ephemeral port and connect a WebSocket client to it.
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
        .expect("agent connects to the OpAMP endpoint");

    // Report as a sidecar would: identity, health, and an APPLIED config status for the distributed
    // configuration's hash.
    let report = AgentToServer {
        instance_uid: vec![0xab, 0xcd, 0xef, 0x01],
        sequence_num: 1,
        agent_description: Some(AgentDescription {
            identifying_attributes: vec![KeyValue {
                key: "service.name".into(),
                value: Some(AnyValue {
                    value: Some(any_value::Value::StringValue("otelcol".into())),
                }),
            }],
            non_identifying_attributes: vec![],
        }),
        health: Some(ComponentHealth {
            healthy: true,
            status: "running".into(),
            ..Default::default()
        }),
        remote_config_status: Some(RemoteConfigStatus {
            last_remote_config_hash: want_hash,
            status: RemoteConfigStatuses::Applied as i32,
            error_message: String::new(),
        }),
        ..Default::default()
    };
    ws.send(Message::Binary(frame::encode(&report).into()))
        .await
        .unwrap();

    // Wait for the server's reply, which confirms it processed (and folded) our report.
    tokio::time::timeout(Duration::from_secs(3), ws.next())
        .await
        .expect("server replies within the timeout")
        .expect("a reply frame")
        .expect("a valid websocket message");

    // Query the REST API over the same fleet state and assert the agent is now visible with its status.
    let api_router = api::router(UiState {
        fleet: fleet.clone(),
        config: config.clone(),
        pushes: pushes.clone(),
        packages: Arc::new(PackageSource::empty()),
    });
    let resp = api_router
        .oneshot(
            Request::builder()
                .uri("/api/fleet")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json = String::from_utf8_lossy(&body);

    assert!(json.contains("abcdef01"), "the agent's uid appears: {json}");
    assert!(
        json.contains("\"config_status\":\"APPLIED\""),
        "its config status is APPLIED: {json}"
    );
    assert!(json.contains("\"healthy\":true"), "it is healthy: {json}");
    assert!(
        json.contains("\"in_sync\":true"),
        "it holds the distributed configuration: {json}"
    );
}
