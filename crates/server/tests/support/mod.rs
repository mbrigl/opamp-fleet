//! Shared scaffolding for the transport integration tests: the real router on an ephemeral port.

use std::net::SocketAddr;
use std::sync::Arc;

use axum_server::accept::DefaultAcceptor;
use opamp::proto::{
    any_value, AgentCapabilities, AgentDescription, AgentToServer, AnyValue, KeyValue,
};
use opamp::uid::InstanceUid;
use server::fleet::AppState;

#[allow(dead_code)] // each integration-test binary uses a different subset of this scaffolding
pub struct TestServer {
    /// The Agent plane (ADR-0023): `/v1/opamp` and the package download.
    pub addr: SocketAddr,
    /// The Operator plane (ADR-0023): the REST API, its docs, and the UI — a second listener, as
    /// in a real deployment, so a test that calls the wrong one finds out.
    pub rest_addr: SocketAddr,
    pub state: Arc<AppState>,
}

#[allow(dead_code)] // each integration-test binary uses a different subset of this scaffolding
pub async fn spawn() -> TestServer {
#[allow(dead_code)] // each integration-test binary uses a different subset of this scaffolding
/// The same real router with a tightened message size limit, for the tests that drive the
/// Baseline's size rules without moving megabytes around.
#[allow(dead_code)] // each integration-test binary uses a different subset of this scaffolding
pub async fn spawn_with_limit(limit: usize) -> TestServer {
}

#[allow(dead_code)] // each integration-test binary uses a different subset of this scaffolding
#[allow(dead_code)] // each integration-test binary uses a different subset of this scaffolding
) -> TestServer {
}

async fn spawn_full(
    limit: usize,
    let dir = tempfile::tempdir().expect("tempdir");
            .with_connection_offer(offer)
    let (addr, rest_addr) = serve(
    )
    .await;
    TestServer {
        addr,
        rest_addr,
        state,
        _dir: dir,
    }
}

/// Serves the two planes on ephemeral ports, exactly as the binary serves them (ADR-0023), and
#[allow(dead_code)] // each integration-test binary uses a different subset of this scaffolding
pub async fn serve(
    state: Arc<AppState>,
    admission: server::transport::Admission,
) -> (SocketAddr, SocketAddr) {
#[allow(dead_code)] // each integration-test binary uses a different subset of this scaffolding
    state: Arc<AppState>,
    admission: server::transport::Admission,
) -> (SocketAddr, SocketAddr) {
    let agents = server::agent_app(state.clone(), admission);
    let agent_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind the Agent plane");
    let operator_listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("bind the Operator plane");
    let addr = agent_listener.local_addr().expect("local addr");
    let rest_addr = operator_listener.local_addr().expect("local addr");
    // Through `listen::plane`, as the binary serves them (ADR-0023), so the whole suite runs
    // against a Server whose connection setup is bounded the way a real one's is.
    let handle = axum_server::Handle::new();
    tokio::spawn(
        server::listen::plane(agent_listener, DefaultAcceptor::new(), handle.clone())
            .serve(agents.into_make_service()),
    );
    tokio::spawn(
        server::listen::plane(operator_listener, DefaultAcceptor::new(), handle)
            .serve(operators.into_make_service()),
    );
    (addr, rest_addr)
}

#[allow(dead_code)] // each integration-test binary uses a different subset of this scaffolding
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
                }),
            }],
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
pub async fn distribute(rest_addr: SocketAddr, name: &str, selector: &[(&str, &str)], body: &str) {
    distribute_with_role(rest_addr, name, selector, body, "").await;
/// [`distribute`] with the Baseline's `AgentConfigObject.role` set (ADR-0011); an empty role is the
    rest_addr: SocketAddr,
        .put(format!("http://{rest_addr}/api/v1/configurations/{name}"))
            "http://{rest_addr}/api/v1/configurations/{name}/rollout"
#[allow(dead_code)] // each integration-test binary uses a different subset of this scaffolding
    let dir = tempfile::tempdir().expect("tempdir");
    let (addr, rest_addr) = serve(state.clone(), server::transport::Admission::open()).await;
        rest_addr,
