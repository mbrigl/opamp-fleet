//! Authentication on the OpAMP endpoint (ADR-0013): Basic and Bearer pass, everything else is
//! answered 401 — on the plain-HTTP POST and on the WebSocket upgrade alike — while REST API
//! and UI stay open.

mod support;

use base64::Engine as _;
use opamp::proto::ServerToAgent;
use opamp::uid::InstanceUid;
use prost::Message as _;
use server::config::AuthConfig;
use server::transport::OpampAuth;
use support::{full_report, spawn_with_auth, TestServer};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::Error as WsError;

const PROTOBUF: &str = "application/x-protobuf";

/// A server accepting one Bearer token and one Basic user.
async fn spawn_guarded() -> TestServer {
    let auth: AuthConfig = toml::from_str(
        r#"
        bearer_tokens = ["good-token"]
        [basic_users]
        fleet = "secret"
        "#,
    )
    .expect("parse");
    spawn_with_auth(Some(OpampAuth::from_config(&auth))).await
}

async fn post(server: &TestServer, authorization: Option<&str>) -> reqwest::Response {
    let uid = InstanceUid::default();
    let mut request = reqwest::Client::new()
        .post(format!("http://{}/v1/opamp", server.addr))
        .header("content-type", PROTOBUF)
        .body(full_report(&uid, "auth-test", 1).encode_to_vec());
    if let Some(value) = authorization {
        request = request.header("authorization", value);
    }
    request.send().await.expect("post")
}

#[tokio::test]
async fn a_request_without_credentials_is_answered_401_with_a_challenge() {
    let server = spawn_guarded().await;

    let response = post(&server, None).await;
    assert_eq!(response.status(), 401);
    let challenge = response
        .headers()
        .get("www-authenticate")
        .and_then(|v| v.to_str().ok())
        .expect("a WWW-Authenticate challenge");
    assert!(challenge.contains("Basic"), "got {challenge:?}");
    assert!(challenge.contains("Bearer"), "got {challenge:?}");

    // Wrong credentials fare no better than none.
    assert_eq!(post(&server, Some("Bearer bad-token")).await.status(), 401);
    assert_eq!(post(&server, Some("Basic Zmxl")).await.status(), 401);
}

#[tokio::test]
async fn both_configured_schemes_authenticate_a_plain_http_exchange() {
    let server = spawn_guarded().await;

    let bearer = post(&server, Some("Bearer good-token")).await;
    assert_eq!(bearer.status(), 200);
    ServerToAgent::decode(bearer.bytes().await.expect("body").as_ref()).expect("decode");

    let encoded = base64::engine::general_purpose::STANDARD.encode("fleet:secret");
    let basic = post(&server, Some(&format!("Basic {encoded}"))).await;
    assert_eq!(basic.status(), 200);
}

#[tokio::test]
async fn the_websocket_upgrade_is_checked_before_it_completes() {
    let server = spawn_guarded().await;
    let url = format!("ws://{}/v1/opamp", server.addr);

    // No credentials: the upgrade never happens, the handshake is answered 401.
    match tokio_tungstenite::connect_async(&url).await {
        Err(WsError::Http(response)) => assert_eq!(response.status(), 401),
        other => panic!("expected a 401 handshake rejection, got {other:?}"),
    }

    // With a valid credential the connection comes up and speaks OpAMP as ever.
    let mut request = url.as_str().into_client_request().expect("request");
    request
        .headers_mut()
        .insert(AUTHORIZATION, "Bearer good-token".parse().expect("header"));
    let (_socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("an authenticated upgrade succeeds");
}

#[tokio::test]
async fn the_rest_api_stays_open_when_the_opamp_endpoint_is_guarded() {
    let server = spawn_guarded().await;
    let response = reqwest::Client::new()
        .get(format!("http://{}/api/v1/agents", server.addr))
        .send()
        .await
        .expect("get");
    assert_eq!(
        response.status(),
        200,
        "operator auth is a separate decision"
    );
}
