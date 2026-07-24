//! Shared scaffolding for the transport integration tests: the real router on an ephemeral port.

use std::net::SocketAddr;
use std::sync::Arc;

use opamp::proto::{
    any_value, AgentCapabilities, AgentDescription, AgentToServer, AnyValue, KeyValue,
};
use opamp::uid::InstanceUid;
use server::fleet::AppState;

#[allow(dead_code)] // each integration-test binary uses a different subset of this scaffolding
pub struct TestServer {
    pub addr: SocketAddr,
    pub state: Arc<AppState>,
    // Held so the Configuration store's directory outlives the test.
    _dir: tempfile::TempDir,
}

#[allow(dead_code)] // each integration-test binary uses a different subset of this scaffolding
pub async fn spawn() -> TestServer {
    spawn_with_auth(None).await
}

/// The same real router, with the OpAMP endpoint's credential check active (ADR-0013).
#[allow(dead_code)] // each integration-test binary uses a different subset of this scaffolding
pub async fn spawn_with_auth(auth: Option<server::transport::OpampAuth>) -> TestServer {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(
        AppState::new(dir.path().join("fleet-configs")).expect("open the configuration store"),
    );
    let app = server::app(state.clone(), auth);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    TestServer {
        addr,
        state,
        _dir: dir,
    }
}

/// A full status report for one Agent, the way a fresh Client sends it.
#[allow(dead_code)] // each integration-test binary uses a different subset of this scaffolding
pub fn full_report(uid: &InstanceUid, name: &str, sequence_num: u64) -> AgentToServer {
    AgentToServer {
        instance_uid: uid.as_bytes().to_vec(),
        sequence_num,
        capabilities: AgentCapabilities::ReportsStatus as u64
            | AgentCapabilities::AcceptsRemoteConfig as u64
            | AgentCapabilities::ReportsEffectiveConfig as u64
            | AgentCapabilities::ReportsRemoteConfig as u64,
        agent_description: Some(AgentDescription {
            identifying_attributes: vec![KeyValue {
                key: "service.name".to_string(),
                value: Some(AnyValue {
                    value: Some(any_value::Value::StringValue(name.to_string())),
                }),
            }],
            non_identifying_attributes: vec![
                KeyValue {
                    key: "os.type".to_string(),
                    value: Some(AnyValue {
                        value: Some(any_value::Value::StringValue("linux".to_string())),
                    }),
                },
                KeyValue {
                    key: "os.description".to_string(),
                    value: Some(AnyValue {
                        value: Some(any_value::Value::StringValue("Testix 1.0 LTS".to_string())),
                    }),
                },
            ],
        }),
        ..Default::default()
    }
}

/// A compressed follow-up report: identity and sequence number only.
#[allow(dead_code)] // each integration-test binary uses a different subset of this scaffolding
pub fn compressed_report(uid: &InstanceUid, sequence_num: u64) -> AgentToServer {
    AgentToServer {
        instance_uid: uid.as_bytes().to_vec(),
        sequence_num,
        capabilities: AgentCapabilities::ReportsStatus as u64
            | AgentCapabilities::AcceptsRemoteConfig as u64,
        ..Default::default()
    }
}

/// Stores a Configuration through the REST API v1, the way an operator (or portal) does.
#[allow(dead_code)]
pub async fn distribute(addr: SocketAddr, name: &str, selector: &[(&str, &str)], body: &str) {
    let selector: std::collections::BTreeMap<&str, &str> = selector.iter().copied().collect();
    let response = reqwest::Client::new()
        .put(format!("http://{addr}/api/v1/configurations/{name}"))
        .json(&serde_json::json!({ "selector": selector, "body": body }))
        .send()
        .await
        .expect("put the configuration");
    assert_eq!(response.status(), 200, "the configuration is accepted");
}
