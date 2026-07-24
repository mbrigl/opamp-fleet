//! The plain-HTTP transport end to end (ADR-0007): protobuf in, protobuf out, gzip accepted, the
//! config offer gated by the hash comparison.

mod support;

use std::io::Write;

use opamp::proto::{
    AgentToServer, AgentToServerFlags, RemoteConfigStatus, RemoteConfigStatuses,
    ServerErrorResponseType, ServerToAgent, ServerToAgentFlags,
};
use opamp::uid::InstanceUid;
use prost::Message;
use server::fleet::SERVER_CAPABILITIES;
use support::{compressed_report, distribute, full_report, spawn};

const PROTOBUF: &str = "application/x-protobuf";

async fn exchange(client: &reqwest::Client, url: &str, msg: &AgentToServer) -> ServerToAgent {
    let response = client
        .post(url)
        .header("content-type", PROTOBUF)
        .body(msg.encode_to_vec())
        .send()
        .await
        .expect("post");
    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some(PROTOBUF)
    );
    ServerToAgent::decode(response.bytes().await.expect("body").as_ref()).expect("decode")
}

#[tokio::test]
async fn a_report_is_answered_and_the_agent_appears_in_the_fleet() {
    let server = spawn().await;
    let url = format!("http://{}/v1/opamp", server.addr);
    let client = reqwest::Client::new();
    let uid = InstanceUid::default();

    let reply = exchange(&client, &url, &full_report(&uid, "itest", 1)).await;
    assert_eq!(reply.instance_uid, uid.as_bytes());
    assert_eq!(reply.capabilities, SERVER_CAPABILITIES);
    assert!(reply.remote_config.is_none(), "nothing to offer yet");
    assert_eq!(reply.flags, 0, "a full report needs no recovery");

    let agents: serde_json::Value = serde_json::from_slice(
        &client
            .get(format!("http://{}/api/v1/agents", server.addr))
            .send()
            .await
            .expect("get")
            .bytes()
            .await
            .expect("body"),
    )
    .expect("json");
    assert_eq!(agents.as_array().expect("array").len(), 1);
    assert_eq!(agents[0]["service_name"], "itest");
    assert_eq!(agents[0]["transport"], "http");
    assert_eq!(agents[0]["connected"], true);
    assert_eq!(agents[0]["identifying_attributes"]["service.name"], "itest");
    // The OS column prefers the human-readable description over the bare os.type.
    assert_eq!(agents[0]["os"], "Testix 1.0 LTS");
    assert_eq!(agents[0]["non_identifying_attributes"]["os.type"], "linux");
    let capabilities = agents[0]["capabilities"].as_array().expect("capabilities");
    assert!(capabilities.contains(&serde_json::json!("ReportsStatus")));
    assert!(capabilities.contains(&serde_json::json!("AcceptsRemoteConfig")));
}

#[tokio::test]
async fn the_offer_is_gated_by_the_config_hash() {
    let server = spawn().await;
    let url = format!("http://{}/v1/opamp", server.addr);
    let client = reqwest::Client::new();
    let uid = InstanceUid::default();
    exchange(&client, &url, &full_report(&uid, "itest", 1)).await;

    // The operator distributes a configuration through the REST API; the fleet view names the
    // hash this Agent's composed configuration should have.
    distribute(server.addr, "fleet", &[], "receivers: {}\n").await;
    let agents: serde_json::Value = serde_json::from_slice(
        &client
            .get(format!("http://{}/api/v1/agents", server.addr))
            .send()
            .await
            .expect("get")
            .bytes()
            .await
            .expect("body"),
    )
    .expect("json");
    let hash_hex = agents[0]["desired_hash"]
        .as_str()
        .expect("hash")
        .to_string();
    assert_eq!(agents[0]["matched_configurations"][0], "fleet");

    // The next poll gets the offer — the Configuration as a named entry.
    let reply = exchange(&client, &url, &compressed_report(&uid, 2)).await;
    let offer = reply.remote_config.expect("an offer");
    assert_eq!(hex::encode(&offer.config_hash), hash_hex);
    let map = offer.config.as_ref().expect("a config map");
    assert!(map.config_map.contains_key("fleet"));

    // The Agent reports it applied — and is never offered the same configuration again.
    let mut ack = compressed_report(&uid, 3);
    ack.remote_config_status = Some(RemoteConfigStatus {
        last_remote_config_hash: offer.config_hash.clone(),
        status: RemoteConfigStatuses::Applied as i32,
        error_message: String::new(),
    });
    let reply = exchange(&client, &url, &ack).await;
    assert!(
        reply.remote_config.is_none(),
        "no redundant reconfiguration"
    );
}

#[tokio::test]
async fn gzip_request_bodies_are_accepted() {
    let server = spawn().await;
    let client = reqwest::Client::new();
    let uid = InstanceUid::default();

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(&full_report(&uid, "gzipped", 1).encode_to_vec())
        .expect("gzip");
    let body = encoder.finish().expect("gzip finish");

    let response = client
        .post(format!("http://{}/v1/opamp", server.addr))
        .header("content-type", PROTOBUF)
        .header("content-encoding", "gzip")
        .body(body)
        .send()
        .await
        .expect("post");
    assert_eq!(response.status(), 200);
    let reply =
        ServerToAgent::decode(response.bytes().await.expect("body").as_ref()).expect("decode");
    assert_eq!(reply.instance_uid, uid.as_bytes());
}

#[tokio::test]
async fn transport_detection_rejects_a_missing_protobuf_content_type() {
    let server = spawn().await;
    let response = reqwest::Client::new()
        .post(format!("http://{}/v1/opamp", server.addr))
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .expect("post");
    assert_eq!(response.status(), 415);
}

#[tokio::test]
async fn malformed_reports_get_a_bad_request_error_response() {
    let server = spawn().await;
    let client = reqwest::Client::new();

    // Garbage bytes: not an AgentToServer at all.
    let response = client
        .post(format!("http://{}/v1/opamp", server.addr))
        .header("content-type", PROTOBUF)
        .body(vec![0xffu8; 32])
        .send()
        .await
        .expect("post");
    let reply =
        ServerToAgent::decode(response.bytes().await.expect("body").as_ref()).expect("decode");
    let error = reply.error_response.expect("an error response");
    assert_eq!(error.r#type, ServerErrorResponseType::BadRequest as i32);

    // A syntactically valid message with an illegal instance_uid (not 16 bytes).
    let msg = AgentToServer {
        instance_uid: vec![1, 2, 3],
        sequence_num: 1,
        ..Default::default()
    };
    let reply = exchange(&client, &format!("http://{}/v1/opamp", server.addr), &msg).await;
    let error = reply.error_response.expect("an error response");
    assert_eq!(error.r#type, ServerErrorResponseType::BadRequest as i32);
}

#[tokio::test]
async fn a_sequence_gap_demands_a_full_report() {
    let server = spawn().await;
    let url = format!("http://{}/v1/opamp", server.addr);
    let client = reqwest::Client::new();
    let uid = InstanceUid::default();
    exchange(&client, &url, &full_report(&uid, "gappy", 1)).await;

    // Sequence 2 lost somewhere; a compressed 3 arrives.
    let reply = exchange(&client, &url, &compressed_report(&uid, 3)).await;
    assert_ne!(
        reply.flags & ServerToAgentFlags::ReportFullState as u64,
        0,
        "the recovery path for lost state"
    );
}

#[tokio::test]
async fn an_unknown_compressed_agent_is_asked_for_full_state_and_can_request_identity() {
    let server = spawn().await;
    let url = format!("http://{}/v1/opamp", server.addr);
    let client = reqwest::Client::new();

    // A compressed report from an Agent the Server has never seen.
    let uid = InstanceUid::default();
    let reply = exchange(&client, &url, &compressed_report(&uid, 7)).await;
    assert_ne!(reply.flags & ServerToAgentFlags::ReportFullState as u64, 0);

    // An Agent starting with a temporary identity and RequestInstanceUid gets a fresh one.
    let temporary = InstanceUid::default();
    let mut msg = full_report(&temporary, "newborn", 1);
    msg.flags = AgentToServerFlags::RequestInstanceUid as u64;
    let reply = exchange(&client, &url, &msg).await;
    let assigned = reply.agent_identification.expect("an assigned identity");
    assert_eq!(assigned.new_instance_uid.len(), 16);
    assert_ne!(assigned.new_instance_uid, temporary.as_bytes().to_vec());
    assert_eq!(reply.instance_uid, assigned.new_instance_uid);
}
