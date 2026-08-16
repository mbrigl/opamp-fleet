//! Gateway Mode end to end (ADR-0037): the real Server, a real Gateway, and Agents reaching one
//! through the other.
//!
//! What these prove is the part the design rests on — that the Server sees Agents rather than
//! connections. Two downstream peers on two transports arrive as two Agents over **one** upstream
//! connection, and each gets its own replies back.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use client::config::ClientConfig;
use client::service::runtime::shutdown_channel;
use futures_util::{SinkExt, StreamExt};
use opamp::proto::{AgentCapabilities, AgentToServer, ServerToAgent};
use opamp::uid::InstanceUid;
use prost::Message as _;
use server::fleet::AppState;
use tokio_tungstenite::tungstenite::Message;

/// The real Server on an ephemeral port.
async fn spawn_server() -> (SocketAddr, Arc<AppState>, tempfile::TempDir) {
    // What main() does at startup: without a process provider, reqwest refuses to build a client.
    client::tls::install_ring_provider();
    let dir = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(AppState::new(dir.path().join("fleet-configs")).expect("state"));
    let app = server::agent_app(state.clone(), server::transport::Admission::open());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    (addr, state, dir)
}

/// A Gateway pointed at that Server, listening on its own ephemeral port.
async fn spawn_gateway(
    server: SocketAddr,
    cap: usize,
) -> (SocketAddr, tokio::sync::watch::Sender<bool>) {
    spawn_gateway_with_limit(server, cap, None).await
}

/// The same, with the message size limit the tests about that limit need — pushing 64 MiB through
/// a socket that is already refusing it tests the sender's patience, not the Gateway.
async fn spawn_gateway_with_limit(
    server: SocketAddr,
    cap: usize,
    max_message_size: Option<usize>,
) -> (SocketAddr, tokio::sync::watch::Sender<bool>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let listen = listener.local_addr().expect("addr");
    drop(listener); // the Gateway binds it itself; this only reserves a free port number

    let limit = max_message_size
        .map(|bytes| format!("max_message_size_bytes = {bytes}"))
        .unwrap_or_default();
    let toml = format!(
        r#"
        endpoint = "ws://{server}/v1/opamp"
        {limit}
        [gateway]
        listen = "{listen}"
        upstream_connections = {cap}
        "#
    );
    let config: ClientConfig = toml::from_str(&toml).expect("gateway config");
    let (tx, shutdown) = shutdown_channel();
    tokio::spawn(async move {
        client::gateway::run(Arc::new(config), shutdown)
            .await
            .expect("gateway");
    });
    // Give the listener a moment to bind before anyone dials it.
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(listen).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    (listen, tx)
}

fn report(uid: &InstanceUid, sequence: u64) -> AgentToServer {
    AgentToServer {
        instance_uid: uid.as_bytes().to_vec(),
        sequence_num: sequence,
        capabilities: AgentCapabilities::ReportsStatus as u64,
        ..Default::default()
    }
}

/// Two Agents, two downstream transports, one upstream connection — and the Server tells them
/// apart by `instance_uid` alone, which is the whole premise of Gateway Mode.
#[tokio::test]
async fn two_agents_reach_the_server_over_one_folded_connection() {
    let (server, state, _dir) = spawn_server().await;
    let (gateway, _stop) = spawn_gateway(server, 10).await;

    // One downstream peer on WebSocket.
    let ws_uid = InstanceUid::default();
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{gateway}/v1/opamp"))
        .await
        .expect("connect to the gateway");
    let frame = opamp::frame::encode_within(&report(&ws_uid, 1), 64 << 20).expect("encode");
    socket
        .send(Message::Binary(frame.into()))
        .await
        .expect("send");
    let reply = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .expect("a reply in time")
        .expect("a message")
        .expect("no error");
    let Message::Binary(payload) = reply else {
        panic!("expected a binary reply")
    };
    let reply = opamp::frame::decode::<ServerToAgent>(&payload, 64 << 20).expect("decode");
    assert_eq!(
        InstanceUid::from_wire(&reply.instance_uid),
        Some(ws_uid),
        "the reply came back addressed to the Agent that asked"
    );

    // A second downstream peer, on the other transport.
    let http_uid = InstanceUid::default();
    let response = reqwest::Client::new()
        .post(format!("http://{gateway}/v1/opamp"))
        .header(reqwest::header::CONTENT_TYPE, "application/x-protobuf")
        .body(report(&http_uid, 1).encode_to_vec())
        .send()
        .await
        .expect("send");
    assert!(response.status().is_success(), "{:?}", response.status());
    let reply =
        ServerToAgent::decode(response.bytes().await.expect("body")).expect("decode the reply");
    assert_eq!(InstanceUid::from_wire(&reply.instance_uid), Some(http_uid));

    // The Server saw two Agents, not two connections and not one Agent.
    let agents = state.snapshot();
    assert_eq!(agents.len(), 2, "two Agents behind one Gateway");
    let uids: Vec<String> = agents.iter().map(|a| a.instance_uid.clone()).collect();
    assert!(uids.contains(&ws_uid.to_string()));
    assert!(uids.contains(&http_uid.to_string()));
}

/// The pool grows lazily to its cap and no further: one Agent means one upstream connection, even
/// with a cap of ten (ADR-0037 rule 5).
#[tokio::test]
async fn one_agent_opens_one_upstream_connection() {
    let (server, state, _dir) = spawn_server().await;
    let (gateway, _stop) = spawn_gateway(server, 10).await;

    let uid = InstanceUid::default();
    for sequence in 1..=3 {
        let response = reqwest::Client::new()
            .post(format!("http://{gateway}/v1/opamp"))
            .header(reqwest::header::CONTENT_TYPE, "application/x-protobuf")
            .body(report(&uid, sequence).encode_to_vec())
            .send()
            .await
            .expect("send");
        assert!(response.status().is_success());
    }

    assert_eq!(state.snapshot().len(), 1, "three reports, one Agent");
}

/// A single downstream connection is bounded in how many Agents it may carry: past the cap a
/// report for a new Agent is dropped rather than growing the routing state, while the Agents already
/// carried keep being served. This is what stops one hostile peer streaming endless fabricated
/// `instance_uid`s from inflating the registry and pool maps without limit.
#[tokio::test]
async fn a_downstream_connection_carries_no_more_than_its_agent_cap() {
    let (server, state, _dir) = spawn_server().await;

    // A Gateway with a cap of two, built directly so the TOML carries `max_carried_agents`.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let listen = listener.local_addr().expect("addr");
    drop(listener);
    let toml = format!(
        r#"
        endpoint = "ws://{server}/v1/opamp"
        [gateway]
        listen = "{listen}"
        max_carried_agents = 2
        "#
    );
    let config: ClientConfig = toml::from_str(&toml).expect("gateway config");
    let (_stop, shutdown) = shutdown_channel();
    tokio::spawn(async move {
        client::gateway::run(Arc::new(config), shutdown)
            .await
            .expect("gateway");
    });
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(listen).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{listen}/v1/opamp"))
        .await
        .expect("connect to the gateway");
    let uids: Vec<InstanceUid> = (0..3).map(|_| InstanceUid::default()).collect();

    // The first two Agents are within the cap: carried, forwarded, and answered.
    for uid in &uids[..2] {
        let frame = opamp::frame::encode_within(&report(uid, 1), 64 << 20).expect("encode");
        socket
            .send(Message::Binary(frame.into()))
            .await
            .expect("send");
        let reply = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("a reply in time")
            .expect("a message")
            .expect("no error");
        assert!(
            matches!(reply, Message::Binary(_)),
            "the carried Agent is answered"
        );
    }

    // The third is past the cap: its report is dropped, so nothing comes back for it.
    let frame = opamp::frame::encode_within(&report(&uids[2], 1), 64 << 20).expect("encode");
    socket
        .send(Message::Binary(frame.into()))
        .await
        .expect("send");
    assert!(
        tokio::time::timeout(Duration::from_millis(500), socket.next())
            .await
            .is_err(),
        "a report past the Agent cap must not be answered"
    );

    // The Server saw exactly the two Agents within the cap, never the third.
    let seen: Vec<String> = state
        .snapshot()
        .iter()
        .map(|a| a.instance_uid.clone())
        .collect();
    assert_eq!(
        seen.len(),
        2,
        "only the capped number of Agents reached the Server"
    );
    assert!(seen.contains(&uids[0].to_string()));
    assert!(seen.contains(&uids[1].to_string()));
    assert!(
        !seen.contains(&uids[2].to_string()),
        "the Agent past the cap was dropped"
    );
}

/// A downstream peer that speaks the wrong content type is refused by the Gateway rather than
/// forwarded — the Baseline's rule for the plain-HTTP transport, enforced per hop.
#[tokio::test]
async fn a_downstream_peer_without_the_protobuf_content_type_is_refused() {
    let (server, _state, _dir) = spawn_server().await;
    let (gateway, _stop) = spawn_gateway(server, 10).await;

    let response = reqwest::Client::new()
        .post(format!("http://{gateway}/v1/opamp"))
        .body(report(&InstanceUid::default(), 1).encode_to_vec())
        .send()
        .await
        .expect("send");
    assert_eq!(
        response.status(),
        reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE
    );
}

/// A gzipped report reaches the Server through the Gateway.
///
/// The regression: accepting `Content-Encoding: gzip` is a Baseline MUST for anything serving this
/// protocol, and a Gateway *is* an OpAMP server downstream (ADR-0037). It implemented the rule
/// nowhere — the Server's endpoint had it, this one handed the compressed bytes straight to the
/// protobuf decoder — so a Client that compressed reached the Server directly and was refused the
/// moment a Gateway was put in front of it. One reading of the rule now serves both endpoints
/// (ADR-0044).
#[tokio::test]
async fn a_downstream_peer_may_gzip_its_report() {
    let (server, state, _dir) = spawn_server().await;
    let (gateway, _stop) = spawn_gateway(server, 10).await;

    let uid = InstanceUid::default();
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut encoder, &report(&uid, 1).encode_to_vec()).expect("compress");
    let body = encoder.finish().expect("finish gzip");

    let response = reqwest::Client::new()
        .post(format!("http://{gateway}/v1/opamp"))
        .header(reqwest::header::CONTENT_TYPE, "application/x-protobuf")
        .header(reqwest::header::CONTENT_ENCODING, "gzip")
        .body(body)
        .send()
        .await
        .expect("send");
    assert!(response.status().is_success(), "{:?}", response.status());
    let reply =
        ServerToAgent::decode(response.bytes().await.expect("body")).expect("decode the reply");
    assert_eq!(InstanceUid::from_wire(&reply.instance_uid), Some(uid));

    // Through the hop and all the way: the Server holds the Agent, not just the Gateway.
    let agents = state.snapshot();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].instance_uid, uid.to_string());
}

/// The other half of that MUST: the size limit applies *after* decompression, so a few kilobytes
/// of gzip cannot buy the hop gigabytes of memory. Refused rather than expanded.
#[tokio::test]
async fn a_gzip_bomb_is_refused_by_the_gateway() {
    let (server, state, _dir) = spawn_server().await;
    let (gateway, _stop) = spawn_gateway(server, 10).await;

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut encoder, &vec![0u8; 128 << 20]).expect("compress");
    let body = encoder.finish().expect("finish gzip");
    assert!(
        body.len() < 1 << 20,
        "the compressed form must be far under the limit for this to test anything"
    );

    let response = reqwest::Client::new()
        .post(format!("http://{gateway}/v1/opamp"))
        .header(reqwest::header::CONTENT_TYPE, "application/x-protobuf")
        .header(reqwest::header::CONTENT_ENCODING, "gzip")
        .body(body)
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);
    assert!(
        state.snapshot().is_empty(),
        "nothing was forwarded upstream"
    );
}

/// An oversized message closes the downstream socket with 1009, the status the Baseline names.
///
/// The regression: a Gateway is an OpAMP server to the Agents behind it (ADR-0037), and
/// `docs/CONFORMANCE.md` claims the `1009 Message Too Big` close as implemented. The Server's
/// endpoint did it; this one hung up with no status at all, so a downstream Client saw its
/// connection drop and could not tell an oversized report from a Gateway that had died.
#[tokio::test]
async fn an_oversized_downstream_message_closes_with_1009() {
    let (server, _state, _dir) = spawn_server().await;
    let (gateway, _stop) = spawn_gateway_with_limit(server, 10, Some(4096)).await;

    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{gateway}/v1/opamp"))
        .await
        .expect("connect to the gateway");
    // Past the limit the socket refuses to buffer it, which is where the close comes from.
    socket
        .send(Message::Binary(vec![0u8; 8192].into()))
        .await
        .expect("send");

    let close = loop {
        let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .expect("a close in time")
            .expect("a message");
        match message {
            Ok(Message::Close(frame)) => break frame,
            Ok(_) => continue,
            Err(e) => panic!("expected a close frame, got {e}"),
        }
    };
    let frame = close.expect("the Gateway named a reason rather than hanging up silently");
    assert_eq!(
        u16::from(frame.code),
        1009,
        "the Baseline names 1009 (Message Too Big)"
    );
}
