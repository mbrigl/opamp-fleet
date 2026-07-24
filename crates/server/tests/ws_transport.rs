//! The WebSocket transport end to end (ADR-0007): framed exchange, the pushed offer on a config
//! change, and disconnect handling.

mod support;

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use opamp::frame;
use opamp::proto::{AgentDisconnect, RemoteConfigStatus, RemoteConfigStatuses, ServerToAgent};
use opamp::uid::InstanceUid;
use server::fleet::SERVER_CAPABILITIES;
use support::{compressed_report, distribute, full_report, spawn};
use tokio_tungstenite::tungstenite::Message;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(addr: std::net::SocketAddr) -> Socket {
    let (socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/v1/opamp"))
        .await
        .expect("connect");
    socket
}

async fn send(socket: &mut Socket, msg: &opamp::proto::AgentToServer) {
    socket
        .send(Message::Binary(frame::encode(msg).into()))
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
            Message::Binary(data) => return frame::decode(&data).expect("decode"),
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
    distribute(server.addr, "fleet", &[], "exporters: {}\n").await;

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

    distribute(server.addr, "fleet", &[], "exporters: {}\n").await;
    let nothing = tokio::time::timeout(Duration::from_millis(500), socket.next()).await;
    assert!(nothing.is_err(), "no redundant reconfiguration is pushed");
}

#[tokio::test]
async fn selectors_target_a_subset_and_compose_named_entries() {
    // ADR-0012: every matching Configuration is one named entry of the offered config map; an
    // Agent outside every Selector is left alone.
    let server = spawn().await;
    let mut socket = connect(server.addr).await;
    let left = InstanceUid::default();
    let right = InstanceUid::default();
    send(&mut socket, &full_report(&left, "left", 1)).await;
    recv(&mut socket).await;
    send(&mut socket, &full_report(&right, "right", 1)).await;
    recv(&mut socket).await;

    // A fleet-wide Configuration (empty Selector) reaches both Agents.
    distribute(server.addr, "base", &[], "receivers: {}\n").await;
    let mut offered = std::collections::HashMap::new();
    for _ in 0..2 {
        let pushed = recv(&mut socket).await;
        let offer = pushed.remote_config.clone().expect("a pushed offer");
        offered.insert(pushed.instance_uid.clone(), offer);
    }
    assert!(offered.contains_key(left.as_bytes().as_slice()));
    assert!(offered.contains_key(right.as_bytes().as_slice()));

    // Both acknowledge their offers (sequence numbers are per Agent).
    for uid in [&left, &right] {
        let offer = &offered[&uid.as_bytes().to_vec()];
        let mut ack = compressed_report(uid, 2);
        ack.remote_config_status = Some(RemoteConfigStatus {
            last_remote_config_hash: offer.config_hash.clone(),
            status: RemoteConfigStatuses::Applied as i32,
            error_message: String::new(),
        });
        send(&mut socket, &ack).await;
        recv(&mut socket).await;
    }

    // A Configuration selecting `service.name = left` reaches only that Agent, composed with the
    // fleet-wide one as two named entries.
    distribute(
        server.addr,
        "left-only",
        &[("service.name", "left")],
        "exporters: {}\n",
    )
    .await;
    let pushed = recv(&mut socket).await;
    assert_eq!(pushed.instance_uid, left.as_bytes());
    let map = pushed
        .remote_config
        .expect("an offer for left")
        .config
        .expect("a config map");
    let mut names: Vec<&str> = map.config_map.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(names, ["base", "left-only"]);

    // The unmatched Agent hears nothing — it keeps running what it already runs (goal 9).
    let nothing = tokio::time::timeout(Duration::from_millis(500), socket.next()).await;
    assert!(nothing.is_err(), "no push toward the unmatched agent");

    let views = server.state.snapshot();
    let view = |name: &str| {
        views
            .iter()
            .find(|a| a.service_name == name)
            .expect("a known agent")
    };
    assert_eq!(view("left").matched_configurations, ["base", "left-only"]);
    assert_eq!(view("right").matched_configurations, ["base"]);
    assert!(view("right").in_sync, "still running its composed set");
    assert!(!view("left").in_sync, "owes the new composition");
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
            .find(|a| a.service_name == "vanisher")
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
async fn two_agents_share_one_connection() {
    // The multiplexing provision of ADR-0003: n Agents over one connection, told apart by
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
