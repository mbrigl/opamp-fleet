//! The REST API v1 as the integration contract (ADR-0012): Configuration CRUD, loud rejection of
//! invalid input, and the OpenAPI document any portal generates a client from.

mod support;

use support::spawn;

fn url(addr: std::net::SocketAddr, path: &str) -> String {
    format!("http://{addr}{path}")
}

#[tokio::test]
async fn configurations_crud_round_trips() {
    let server = spawn().await;
    let client = reqwest::Client::new();

    // Nothing yet.
    let list: serde_json::Value = client
        .get(url(server.addr, "/api/v1/configurations"))
        .send()
        .await
        .expect("list")
        .json()
        .await
        .expect("json");
    assert_eq!(list.as_array().expect("array").len(), 0);

    // Create; the stored resource comes back, body normalized to a trailing newline.
    let put = client
        .put(url(server.addr, "/api/v1/configurations/base"))
        .json(&serde_json::json!({ "selector": { "os.type": "linux" }, "body": "receivers: {}" }))
        .send()
        .await
        .expect("put");
    assert_eq!(put.status(), 200);
    let stored: serde_json::Value = put.json().await.expect("json");
    assert_eq!(stored["name"], "base");
    assert_eq!(stored["selector"]["os.type"], "linux");
    assert_eq!(stored["body"], "receivers: {}\n");

    // Read back, singly and as the list.
    let got: serde_json::Value = client
        .get(url(server.addr, "/api/v1/configurations/base"))
        .send()
        .await
        .expect("get")
        .json()
        .await
        .expect("json");
    assert_eq!(got, stored);
    let list: serde_json::Value = client
        .get(url(server.addr, "/api/v1/configurations"))
        .send()
        .await
        .expect("list")
        .json()
        .await
        .expect("json");
    assert_eq!(list.as_array().expect("array").len(), 1);

    // Delete; a second delete and a read find nothing.
    let deleted = client
        .delete(url(server.addr, "/api/v1/configurations/base"))
        .send()
        .await
        .expect("delete");
    assert_eq!(deleted.status(), 204);
    let again = client
        .delete(url(server.addr, "/api/v1/configurations/base"))
        .send()
        .await
        .expect("delete again");
    assert_eq!(again.status(), 404);
    let gone = client
        .get(url(server.addr, "/api/v1/configurations/base"))
        .send()
        .await
        .expect("get");
    assert_eq!(gone.status(), 404);
}

#[tokio::test]
async fn invalid_configurations_are_rejected_loudly() {
    let server = spawn().await;
    let client = reqwest::Client::new();

    for (name, body) in [
        ("Bad Name", "x: 1"), // grammar violation
        ("con", "x: 1"),      // Windows reserved device name
        ("ok-name", "   \n"), // empty body
    ] {
        let response = client
            .put(url(
                server.addr,
                &format!("/api/v1/configurations/{}", urlencoding(name)),
            ))
            .json(&serde_json::json!({ "body": body }))
            .send()
            .await
            .expect("put");
        assert_eq!(response.status(), 400, "{name:?} must be rejected");
        let error: serde_json::Value = response.json().await.expect("an error body");
        assert!(error["error"].is_string());
    }
}

fn urlencoding(s: &str) -> String {
    s.replace(' ', "%20")
}

#[tokio::test]
async fn the_openapi_document_describes_the_contract() {
    let server = spawn().await;
    let response = reqwest::Client::new()
        .get(url(server.addr, "/api/v1/openapi.json"))
        .send()
        .await
        .expect("get");
    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
    let document: serde_json::Value = response.json().await.expect("json");
    let paths = document["paths"].as_object().expect("paths");
    assert!(paths.contains_key("/api/v1/agents"));
    assert!(paths.contains_key("/api/v1/configurations"));
    assert!(paths.contains_key("/api/v1/configurations/{name}"));
    // The resource schemas ride along, so a client can be generated without the source.
    assert!(document["components"]["schemas"]["Configuration"].is_object());
    assert!(document["components"]["schemas"]["AgentView"].is_object());
}

#[tokio::test]
async fn configurations_survive_a_server_restart() {
    // The store is the persistence: a new AppState over the same directory restores everything.
    let dir = tempfile::tempdir().expect("tempdir");
    let store_dir = dir.path().join("fleet-configs");
    {
        let state = server::fleet::AppState::new(store_dir.clone()).expect("open");
        state
            .put_configuration(server::configs::Configuration {
                name: "keeper".to_string(),
                selector: std::collections::BTreeMap::new(),
                body: "receivers: {}\n".to_string(),
            })
            .expect("put");
    }
    let reopened = server::fleet::AppState::new(store_dir).expect("reopen");
    let restored = reopened.configurations().list();
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].name, "keeper");
}
