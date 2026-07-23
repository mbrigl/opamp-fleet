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
    // Held so the fleet-config file's directory outlives the test.
    _dir: tempfile::TempDir,
}

pub async fn spawn() -> TestServer {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(AppState::new(dir.path().join("fleet-config.yaml")));
    let app = server::app(state.clone());
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
            non_identifying_attributes: vec![],
        }),
        ..Default::default()
    }
}

/// A compressed follow-up report: identity and sequence number only.
pub fn compressed_report(uid: &InstanceUid, sequence_num: u64) -> AgentToServer {
    AgentToServer {
        instance_uid: uid.as_bytes().to_vec(),
        sequence_num,
        capabilities: AgentCapabilities::ReportsStatus as u64
            | AgentCapabilities::AcceptsRemoteConfig as u64,
        ..Default::default()
    }
}
