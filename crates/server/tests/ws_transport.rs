//! The WebSocket transport end to end (ADR-0012): framed exchange, the pushed offer on a config
//! change, and disconnect handling.

mod support;

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use opamp::frame;
use opamp::proto::{AgentDisconnect, RemoteConfigStatus, RemoteConfigStatuses, ServerToAgent};
use opamp::uid::InstanceUid;
use server::fleet::SERVER_CAPABILITIES;
use tokio_tungstenite::tungstenite::Message;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(addr: std::net::SocketAddr) -> Socket {
    let (socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/v1/opamp"))
        .await
        .expect("connect");
    socket
}

/// The limit is not what these tests are about; they use the recommended default.
const LIMIT: usize = frame::DEFAULT_MAX_MESSAGE_SIZE;

async fn send(socket: &mut Socket, msg: &opamp::proto::AgentToServer) {
    socket
        .send(Message::Binary(
            frame::encode_within(msg, LIMIT)
                .expect("within the limit")
                .into(),
        ))
        .await
        .expect("send");
}

async fn recv(socket: &mut Socket) -> ServerToAgent {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("a message within five seconds")
            .expect("an open connection")
            .expect("a frame");
        match message {
            Message::Binary(data) => return frame::decode(&data, LIMIT).expect("decode"),
            // Control frames are not OpAMP messages.
            _ => continue,
        }
    }
}

#[tokio::test]
async fn a_framed_report_is_answered() {
    let server = spawn().await;
    let mut socket = connect(server.addr).await;
    let uid = InstanceUid::default();

    send(&mut socket, &full_report(&uid, "ws-test", 1)).await;
    let reply = recv(&mut socket).await;
    assert_eq!(reply.instance_uid, uid.as_bytes());
    assert_eq!(reply.capabilities, SERVER_CAPABILITIES);

    let agents = server.state.snapshot();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].transport, "websocket");
    assert!(agents[0].connected);
}

#[tokio::test]
async fn a_config_change_is_pushed_without_the_agent_asking() {
    let server = spawn().await;
    let mut socket = connect(server.addr).await;
    let uid = InstanceUid::default();
    send(&mut socket, &full_report(&uid, "pushed", 1)).await;
    let first = recv(&mut socket).await;
    assert!(first.remote_config.is_none());

    // The operator distributes a configuration; the connected Agent hears about it immediately.
    distribute(server.rest_addr, "fleet", &[], "exporters: {}\n").await;

    let pushed = recv(&mut socket).await;
    let offer = pushed.remote_config.expect("a pushed offer");
    assert!(!offer.config_hash.is_empty());

    // The Agent acknowledges; re-distributing the same configuration pushes nothing again.
    let mut ack = compressed_report(&uid, 2);
    ack.remote_config_status = Some(RemoteConfigStatus {
        last_remote_config_hash: offer.config_hash.clone(),
        status: RemoteConfigStatuses::Applied as i32,
        error_message: String::new(),
    });
    send(&mut socket, &ack).await;
    let reply = recv(&mut socket).await;
    assert!(reply.remote_config.is_none());

    distribute(server.rest_addr, "fleet", &[], "exporters: {}\n").await;
    let nothing = tokio::time::timeout(Duration::from_millis(500), socket.next()).await;
    assert!(nothing.is_err(), "no redundant reconfiguration is pushed");
}

/// ADR-0025: the operator's role reaches the Agent in `AgentConfigObject.role`, verbatim, and a
    let server = spawn().await;
    let mut socket = connect(server.addr).await;
    let uid = InstanceUid::default();
    distribute(server.rest_addr, "base", &[], "receivers: {}\n").await;
    distribute_with_role(
        server.rest_addr,
        "ruleset",
        &[],
        "rules: []\n",
        "supplementary",
    )
    .await;
    let mut socket = connect(server.addr).await;
    distribute(server.rest_addr, "base", &[], "receivers: {}\n").await;
            status: RemoteConfigStatuses::Applied as i32,
            error_message: String::new(),
        });
        server.rest_addr,
    let pushed = recv(&mut socket).await;
    let nothing = tokio::time::timeout(Duration::from_millis(500), socket.next()).await;
#[tokio::test]
async fn a_restart_is_pushed_to_a_connected_agent_as_its_own_frame() {
    let server = spawn().await;
    let mut socket = connect(server.addr).await;
    let uid = InstanceUid::default();
    let mut report = full_report(&uid, "restartable", 1);
    report.capabilities |= opamp::proto::AgentCapabilities::AcceptsRestartCommand as u64;
    send(&mut socket, &report).await;
    recv(&mut socket).await;

    let response = reqwest::Client::new()
        .post(format!(
            "http://{}/api/v1/agents/{uid}/restart",
            server.rest_addr
        ))
        .send()
        .await
        .expect("post");
    assert_eq!(response.status(), 202);

    let pushed = recv(&mut socket).await;
    let command = pushed.command.expect("the pushed restart command");
    assert_eq!(command.r#type, opamp::proto::CommandType::Restart as i32);
    assert!(pushed.remote_config.is_none(), "command-only frame");
}

#[tokio::test]
async fn agent_disconnect_and_socket_loss_mark_the_agent_disconnected() {
    let server = spawn().await;

    // Polite goodbye: agent_disconnect in the final message.
    let mut socket = connect(server.addr).await;
    let uid = InstanceUid::default();
    send(&mut socket, &full_report(&uid, "leaver", 1)).await;
    recv(&mut socket).await;
    let mut goodbye = compressed_report(&uid, 2);
    goodbye.agent_disconnect = Some(AgentDisconnect {});
    send(&mut socket, &goodbye).await;
    recv(&mut socket).await;
    assert!(!server.state.snapshot()[0].connected);

    // Abrupt loss: the connection dies, the Server notices.
    let mut socket = connect(server.addr).await;
    let uid = InstanceUid::default();
    send(&mut socket, &full_report(&uid, "vanisher", 1)).await;
    recv(&mut socket).await;
    drop(socket);
    let vanished = || {
        server
            .state
            .snapshot()
            .into_iter()
            .map(|a| !a.connected)
            .unwrap_or(false)
    };
    for _ in 0..50 {
        if vanished() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the server never noticed the lost connection");
}

#[tokio::test]
async fn a_duplicate_uid_on_a_second_connection_is_rekeyed() {
    // The Baseline's duplicate detection: the same instance_uid alive on another connection
    // (bad UID generators, cloned VMs) — the Server rekeys the newcomer via AgentIdentification.
    let server = spawn().await;
    let mut first = connect(server.addr).await;
    let uid = InstanceUid::default();
    send(&mut first, &full_report(&uid, "original", 1)).await;
    let reply = recv(&mut first).await;
    assert!(reply.agent_identification.is_none());

    let mut second = connect(server.addr).await;
    send(&mut second, &full_report(&uid, "clone", 1)).await;
    let reply = recv(&mut second).await;
    let assigned = reply
        .agent_identification
        .expect("the duplicate is rekeyed");
    assert_eq!(assigned.new_instance_uid.len(), 16);
    assert_ne!(assigned.new_instance_uid, uid.as_bytes().to_vec());

    // Two distinct, connected Agents — the incumbent kept its identity.
    let agents = server.state.snapshot();
    assert_eq!(agents.len(), 2);
    assert!(agents.iter().all(|a| a.connected));
    assert_eq!(
        agents
            .iter()
            .filter(|a| a.instance_uid == uid.to_string())
            .count(),
        1
    );

    // Only the owning connection may take its Agent down: closing the second connection leaves
    // the original Agent connected, and vice versa (the regression the ownership guard fixes).
    let new_uid = InstanceUid::from_wire(&assigned.new_instance_uid).expect("valid uid");
    send(&mut second, &full_report(&new_uid, "clone", 1)).await;
    recv(&mut second).await;
    drop(second);
    let original_stays = || {
        let agents = server.state.snapshot();
        let disconnected = |name: &str| {
            agents
                .iter()
        };
        (disconnected("clone") && !disconnected("original")).then_some(())
    };
    for _ in 0..50 {
        if original_stays().is_some() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the rekeyed clone never went down alone — or took the original with it");
}

#[tokio::test]
async fn two_agents_share_one_connection() {
    // The multiplexing provision of ADR-0014: n Agents over one connection, told apart by
    // instance_uid alone.
    let server = spawn().await;
    let mut socket = connect(server.addr).await;
    let first = InstanceUid::default();
    let second = InstanceUid::default();

    send(&mut socket, &full_report(&first, "left", 1)).await;
    let reply = recv(&mut socket).await;
    assert_eq!(reply.instance_uid, first.as_bytes());

    send(&mut socket, &full_report(&second, "right", 1)).await;
    let reply = recv(&mut socket).await;
    assert_eq!(reply.instance_uid, second.as_bytes());

    let agents = server.state.snapshot();
    assert_eq!(agents.len(), 2);
    assert!(agents.iter().all(|a| a.connected));
}

/// The Baseline (message size limits): a WebSocket message past the receive limit is malformed,
/// and the Server closes the connection with status code 1009 rather than acting on it.
#[tokio::test]
async fn an_oversized_frame_closes_the_connection_with_1009() {
    let server = support::spawn_with_limit(1024).await;
    let mut socket = connect(server.addr).await;

    socket
        .send(Message::Binary(vec![0u8; 4096].into()))
        .await
        .expect("send an oversized frame");

    // The close frame is what the peer sees; anything before it would mean the Server processed
    // a message it must have refused.
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("an answer within five seconds")
            .expect("an open connection");
        match message {
            Ok(Message::Close(Some(close))) => {
                assert_eq!(
                    u16::from(close.code),
                    1009,
                    "the Baseline names 1009 (Message Too Big)"
                );
                return;
            }
            Ok(Message::Binary(_)) => panic!("the Server answered an oversized frame"),
            // tungstenite surfaces the close as an error on the next read once it has been
            // handled; either way the connection must be gone, not serving.
            Ok(_) => continue,
            Err(e) => panic!("the connection failed without a 1009 close: {e}"),
        }
    }
}
