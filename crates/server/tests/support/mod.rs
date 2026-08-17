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
    /// The Agent plane (ADR-0066): `/v1/opamp` and the package download.
    pub addr: SocketAddr,
    /// The Operator plane (ADR-0066): the REST API, its docs, and the UI — a second listener, as
    /// in a real deployment, so a test that calls the wrong one finds out.
    pub rest_addr: SocketAddr,
    pub state: Arc<AppState>,
    // Held so the store directories outlive the test. Public so a test binary that wires its own
    // AppState (e.g. package delivery) can hand over the temp dir it kept alive.
    pub _dir: tempfile::TempDir,
}

#[allow(dead_code)] // each integration-test binary uses a different subset of this scaffolding
pub async fn spawn() -> TestServer {
    spawn_with(None, None).await
}

/// The same real router, with the OpAMP endpoint's credential check active (ADR-0013).
#[allow(dead_code)] // each integration-test binary uses a different subset of this scaffolding
pub async fn spawn_with_auth(auth: Option<server::transport::OpampAuth>) -> TestServer {
    spawn_with(auth, None).await
}

/// The same real router with a tightened message size limit, for the tests that drive the
/// Baseline's size rules without moving megabytes around.
#[allow(dead_code)] // each integration-test binary uses a different subset of this scaffolding
pub async fn spawn_with_limit(limit: usize) -> TestServer {
    spawn_full(None, None, limit, DEFAULT_STALE_AFTER).await
}

/// The same real router with a tightened staleness budget (ADR-0038), for the tests that need an
/// Agent to fall silent without waiting out the real one.
#[allow(dead_code)] // each integration-test binary uses a different subset of this scaffolding
pub async fn spawn_with_stale_after(stale_after: std::time::Duration) -> TestServer {
    spawn_full(
        None,
        None,
        opamp::frame::DEFAULT_MAX_MESSAGE_SIZE,
        stale_after,
    )
    .await
}

/// The Server's own default, restated here so a scaffolded Server behaves like a real one.
const DEFAULT_STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(90);

/// The full shape: optional credential check (ADR-0013) and optional connection-settings offer
/// (ADR-0014).
#[allow(dead_code)] // each integration-test binary uses a different subset of this scaffolding
pub async fn spawn_with(
    auth: Option<server::transport::OpampAuth>,
    offer: Option<server::fleet::ConnectionOffer>,
) -> TestServer {
    spawn_full(
        auth,
        offer,
        opamp::frame::DEFAULT_MAX_MESSAGE_SIZE,
        DEFAULT_STALE_AFTER,
    )
    .await
}

async fn spawn_full(
    auth: Option<server::transport::OpampAuth>,
    offer: Option<server::fleet::ConnectionOffer>,
    limit: usize,
    stale_after: std::time::Duration,
) -> TestServer {
    // What main() does at startup: without a process provider, reqwest refuses to build a client.
    server::tls::install_ring_provider();
    let dir = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(
        AppState::new(dir.path().join("fleet-configs"))
            .expect("open the configuration store")
            .with_connection_offer(offer)
            .with_max_message_size(limit)
            .with_stale_after(stale_after),
    );
    let (addr, rest_addr) = serve(
        state.clone(),
        server::transport::Admission::new(auth, false),
    )
    .await;
    TestServer {
        addr,
        rest_addr,
        state,
        _dir: dir,
    }
}

/// Serves the two planes on ephemeral ports, exactly as the binary serves them (ADR-0066), and
/// answers with the address of each. The Operator plane is open, as it is without `[rest.auth]`.
#[allow(dead_code)] // each integration-test binary uses a different subset of this scaffolding
pub async fn serve(
    state: Arc<AppState>,
    admission: server::transport::Admission,
) -> (SocketAddr, SocketAddr) {
    serve_guarded(state, admission, None).await
}

/// The same two planes, with the Operator plane's credential check active (ADR-0067).
#[allow(dead_code)] // each integration-test binary uses a different subset of this scaffolding
pub async fn serve_guarded(
    state: Arc<AppState>,
    admission: server::transport::Admission,
    operator_auth: Option<server::api::OperatorAuth>,
) -> (SocketAddr, SocketAddr) {
    let agents = server::agent_app(state.clone(), admission);
    let operators = server::operator_app(state, operator_auth);
    let agent_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the Agent plane");
    let operator_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the Operator plane");
    let addr = agent_listener.local_addr().expect("local addr");
    let rest_addr = operator_listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(agent_listener, agents)
            .await
            .expect("serve the Agent plane");
    });
    tokio::spawn(async move {
        axum::serve(operator_listener, operators)
            .await
            .expect("serve the Operator plane");
    });
    (addr, rest_addr)
}

/// The Agent type every scaffolded Agent reports as `service.name` (ADR-0033). It is a constant
/// because a type describes a *kind* of Agent: the test fleet is one kind of thing on many hosts,
/// which is also what makes one package able to reach several of them (ADR-0034).
#[allow(dead_code)] // each integration-test binary uses a different subset of this scaffolding
pub const AGENT_TYPE: &str = "io.opentelemetry.collector";

/// A full status report for one Agent, the way a fresh Client sends it. `name` is the operator's
/// name for it — `service.instance.name` — and is what tells two Agents apart; their *type* is the
/// shared [`AGENT_TYPE`].
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
                    value: Some(any_value::Value::StringValue(AGENT_TYPE.to_string())),
                }),
            }],
            non_identifying_attributes: vec![
                // The operator's name for this Agent (ADR-0033) — what distinguishes it from its
                // neighbours, now that `service.name` says only what kind of thing it is.
                KeyValue {
                    key: "service.instance.name".to_string(),
                    value: Some(AnyValue {
                        value: Some(any_value::Value::StringValue(name.to_string())),
                    }),
                },
                KeyValue {
                    key: "os.type".to_string(),
                    value: Some(AnyValue {
                        value: Some(any_value::Value::StringValue("linux".to_string())),
                    }),
                },
                // The other half of the Platform a package is fitted against (ADR-0031). An Agent
                // reporting no `host.arch` fits no artifact at all, which is its own test.
                KeyValue {
                    key: "host.arch".to_string(),
                    value: Some(AnyValue {
                        value: Some(any_value::Value::StringValue("amd64".to_string())),
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

/// Stores **and rolls out** a Configuration through the REST API v1, the way an operator (or
/// portal) does — two calls since ADR-0061, because saving alone distributes nothing and the
/// rollout act is what assigns it to every currently matching Agent.
#[allow(dead_code)]
pub async fn distribute(rest_addr: SocketAddr, name: &str, selector: &[(&str, &str)], body: &str) {
    distribute_with_role(rest_addr, name, selector, body, "").await;
}

/// [`distribute`] with the Baseline's `AgentConfigObject.role` set (ADR-0016); an empty role is the
/// ordinary top-level configuration and stays out of the request.
pub async fn distribute_with_role(
    rest_addr: SocketAddr,
    name: &str,
    selector: &[(&str, &str)],
    body: &str,
    role: &str,
) {
    let selector: std::collections::BTreeMap<&str, &str> = selector.iter().copied().collect();
    let mut spec = serde_json::json!({ "selector": selector, "body": body });
    if !role.is_empty() {
        spec["role"] = role.into();
    }
    let client = reqwest::Client::new();
    let response = client
        .put(format!("http://{rest_addr}/api/v1/configurations/{name}"))
        .json(&spec)
        .send()
        .await
        .expect("put the configuration");
    assert_eq!(response.status(), 200, "the configuration is accepted");
    let response = client
        .post(format!(
            "http://{rest_addr}/api/v1/configurations/{name}/rollout"
        ))
        .send()
        .await
        .expect("roll out the configuration");
    assert_eq!(response.status(), 200, "the configuration is rolled out");
}

/// The same real router with own-telemetry destinations to offer (ADR-0036).
#[allow(dead_code)] // each integration-test binary uses a different subset of this scaffolding
pub async fn spawn_with_telemetry(offer: server::fleet::TelemetryOffer) -> TestServer {
    server::tls::install_ring_provider();
    let dir = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(
        AppState::new(dir.path().join("fleet-configs"))
            .expect("open the configuration store")
            .with_telemetry_offer(offer),
    );
    let (addr, rest_addr) = serve(state.clone(), server::transport::Admission::open()).await;
    TestServer {
        addr,
        rest_addr,
        state,
        _dir: dir,
    }
}
