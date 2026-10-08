//! A Collector reports through its own OpAMP client (specification G-16): what a Collector's
//! `opampextension` tells its Supervisor's Supervisor Endpoint — its description, its health, its
//! effective configuration — reaches the Server as that Supervisor's Agent, so the Collector's own
//! reporting is what makes it visible in the fleet.
//!
//! The real Server in-process, the real Client binary with a Collector Supervisor on the stub, and
//! the test itself in the extension's place: it reads the token the Client handed the process, as
//! the extension reads it from its environment, and connects to the endpoint with it.

mod common;

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fleet_server::fleet::{AgentView, AppState};
use futures_util::SinkExt;
use opamp::proto::{
    any_value, AgentConfigMap, AgentConfigObject, AgentDescription, AgentToServer, AnyValue,
    ComponentHealth, EffectiveConfig, KeyValue,
};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

/// Kills the client on drop so a failing assertion never leaks the process.
struct ClientUnderTest(Child);

impl Drop for ClientUnderTest {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn wait_until<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Some(value) = probe() {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

/// A loopback port nobody holds at the moment — what the Collector's own configuration would name
/// as its Supervisor Endpoint.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port()
}

/// Places the stub where the Collector Supervisor's bare program name resolves.
fn stage_owned_program(state_dir: &Path, supervisor: &str, program: &str) {
    let program_dir = state_dir
        .join("supervisors")
        .join(supervisor)
        .join("program");
    std::fs::create_dir_all(&program_dir).expect("create the owned program directory");
    let dest = program_dir.join(program);
    std::fs::copy(env!("CARGO_BIN_EXE_stub_agent"), &dest).expect("stage the stub binary");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))
            .expect("make the staged program executable");
    }
}

fn string_attr(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue {
            value: Some(any_value::Value::StringValue(value.to_string())),
        }),
    }
}

fn view<'a>(agents: &'a [AgentView], name: &str) -> Option<&'a AgentView> {
    agents.iter().find(|a| a.service_instance_name == name)
}

// Verifies: G-16, ADR-0034
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_collector_extensions_own_report_reaches_the_fleet_as_its_supervisors_agent() {
    opamp::tls::install_ring_provider();
    let dir = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(
        AppState::new(dir.path().join("fleet-configs")).expect("open the configuration store"),
    );
    let app = fleet_server::agent_app(state.clone(), fleet_server::transport::Admission::open());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the server");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    let state_dir = dir.path().join("client-state");
    let marker = dir.path().join("collector-marker");
    let program = Path::new(env!("CARGO_BIN_EXE_stub_agent"))
        .file_name()
        .expect("the stub has a file name")
        .to_string_lossy()
        .into_owned();
    stage_owned_program(&state_dir, "otelcol", &program);
    let port = free_port();
    let config_path = dir.path().join("supervisor.toml");
    std::fs::write(
        &config_path,
        format!(
            concat!(
                "endpoint = \"ws://{addr}/v1/opamp\"\n",
                "state_dir = {state_dir:?}\n",
                "heartbeat_interval_secs = 1\n",
                "[[supervisor]]\n",
                "type = \"collector\"\n",
                "name = \"otelcol\"\n",
                "binary = {program:?}\n",
                "endpoint_port = {port}\n",
                "args = [\"--touch\", {marker:?}]\n",
                "{identity}",
            ),
            addr = addr,
            state_dir = state_dir.to_string_lossy(),
            program = program,
            port = port,
            marker = marker.to_string_lossy(),
            identity = common::client_identity(dir.path()),
        ),
    )
    .expect("write supervisor.toml");
    let _client = ClientUnderTest(
        Command::new(env!("CARGO_BIN_EXE_supervisor"))
            .arg("--config")
            .arg(&config_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the client"),
    );

    // A Collector runs once it has a configuration, so the fleet gives it one — aimed at its type
    // alone, which the Client's own Agent does not share.
    let collector_type = wait_until("the Collector's Supervisor to appear as an Agent", || {
        view(&state.snapshot(), "otelcol").map(|a| a.service_name.clone())
    })
    .await;
    state
        .save_configuration(
            "collector",
            fleet_server::configs::Revision {
                selector: Default::default(),
                body: "receivers: {}\n".to_string(),
                role: String::new(),
                service_name: collector_type,
            },
        )
        .expect("save the Collector's configuration");
    state
        .rollout_configuration("collector")
        .expect("roll the Collector's configuration out");

    // The Collector is running, and it was handed the token its extension presents.
    let token = wait_until("the Collector to start with its endpoint token", || {
        std::fs::read_to_string(&marker)
            .ok()?
            .lines()
            .find_map(|l| l.strip_prefix("token=").map(str::to_string))
    })
    .await;

    // The extension's report: what only the Collector knows about itself.
    let mut request = format!("ws://127.0.0.1:{port}/v1/opamp")
        .into_client_request()
        .expect("request");
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {token}").parse().expect("header"),
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("the extension connects to the Supervisor Endpoint");
    let report = AgentToServer {
        instance_uid: opamp::uid::InstanceUid::default().as_bytes().to_vec(),
        sequence_num: 1,
        agent_description: Some(AgentDescription {
            identifying_attributes: Vec::new(),
            non_identifying_attributes: vec![string_attr("collector.pipeline", "traces/otlp")],
        }),
        health: Some(ComponentHealth {
            healthy: true,
            status: "pipelines running".to_string(),
            ..Default::default()
        }),
        effective_config: Some(EffectiveConfig {
            config_map: Some(AgentConfigMap {
                config_map: [(
                    String::new(),
                    AgentConfigObject {
                        body: b"receivers: {otlp: {}}\n".to_vec(),
                        content_type: "text/yaml".to_string(),
                        ..Default::default()
                    },
                )]
                .into_iter()
                .collect(),
            }),
        }),
        ..Default::default()
    };
    socket
        .send(Message::Binary(
            opamp::frame::encode_within(&report, opamp::frame::DEFAULT_MAX_MESSAGE_SIZE)
                .expect("within the limit")
                .into(),
        ))
        .await
        .expect("the extension reports");

    // All three reach the fleet, on the Supervisor's own Agent.
    let reported_by = wait_until("the extension's report to reach the Server", || {
        let agents = state.snapshot();
        let agent = view(&agents, "otelcol")?;
        let reached = agent
            .non_identifying_attributes
            .get("collector.pipeline")
            .is_some_and(|value| value == "traces/otlp")
            && agent.health_status == "pipelines running"
            && agent.effective_config.contains("receivers: {otlp: {}}");
        reached.then(|| agent.service_instance_name.clone())
    })
    .await;
    assert_eq!(
        reported_by, "otelcol",
        "the report is folded into the Supervisor's Agent, not presented as an Agent of its own"
    );
    assert_eq!(
        state.snapshot().len(),
        2,
        "the Client and its one Supervisor, and no Agent for the extension's own instance_uid"
    );
}
