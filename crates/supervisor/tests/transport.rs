//! Transport-level test for the OpAMP HTTP client (ADR-0004): a minimal stub HTTP server checks
//! that `OpampHttpClient` POSTs a decodable `AgentToServer` and correctly decodes the
//! `ServerToAgent` reply — over a real TCP/HTTP round trip, without pulling in the Server crate.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use opamp::v1::{AgentConfigFile, AgentConfigMap, AgentRemoteConfig, AgentToServer, ServerToAgent};
use opamp::InstanceUid;
use supervisor::OpampHttpClient;

#[tokio::test]
async fn client_posts_report_and_decodes_reply() {
    let response = server_to_agent_offering(b"receivers: {}");
    let (url, received) = spawn_stub_server(response);

    let client = OpampHttpClient::new(url).unwrap();
    let uid = InstanceUid::generate();
    let message = AgentToServer {
        instance_uid: uid.to_vec(),
        sequence_num: 1,
        capabilities: opamp::required_agent_capabilities(),
        ..Default::default()
    };

    let reply = client.send(&uid, &message).await.expect("send");

    // The Server received exactly what we sent.
    let got = received
        .recv_timeout(Duration::from_secs(5))
        .expect("server received a message");
    assert_eq!(got.instance_uid, uid.to_vec());
    assert_eq!(got.sequence_num, 1);
    assert_eq!(got.capabilities, opamp::required_agent_capabilities());

    // The reply decoded, carrying the offered config and its hash.
    let remote_config = reply.remote_config.expect("reply has remote_config");
    assert_eq!(
        remote_config.config_hash,
        opamp::config_hash(b"receivers: {}")
    );
}

fn server_to_agent_offering(body: &[u8]) -> ServerToAgent {
    let mut config_map = HashMap::new();
    config_map.insert(
        String::new(),
        AgentConfigFile {
            body: body.to_vec(),
            content_type: "text/plain".to_string(),
        },
    );
    ServerToAgent {
        remote_config: Some(AgentRemoteConfig {
            config: Some(AgentConfigMap { config_map }),
            config_hash: opamp::config_hash(body),
        }),
        ..Default::default()
    }
}

/// Bind a one-shot HTTP/1.1 server on an ephemeral port. It reads one request, decodes the
/// `AgentToServer` body, forwards it over the returned channel, then replies with `response`.
fn spawn_stub_server(response: ServerToAgent) -> (String, Receiver<AgentToServer>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();

        // Read until the end of the headers, then the exact Content-Length body.
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        let header_end = loop {
            let n = stream.read(&mut chunk).unwrap();
            if n == 0 {
                return;
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(pos) = find(&buf, b"\r\n\r\n") {
                break pos;
            }
        };

        let headers = String::from_utf8_lossy(&buf[..header_end]).to_lowercase();
        let content_length: usize = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);

        let body_start = header_end + 4;
        while buf.len() < body_start + content_length {
            let n = stream.read(&mut chunk).unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }

        let request = opamp::decode::<AgentToServer>(&buf[body_start..body_start + content_length])
            .expect("decode AgentToServer");
        tx.send(request).unwrap();

        let response_body = opamp::encode(&response);
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/x-protobuf\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            response_body.len()
        );
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(&response_body).unwrap();
        stream.flush().unwrap();
    });

    (format!("http://{addr}/v1/opamp"), rx)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}
