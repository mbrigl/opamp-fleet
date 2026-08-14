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
    assert!(
        stored.get("published").is_none() && stored.get("pending_changes").is_none(),
        "ADR-0061: content has one state — saved; rollout is a fact about Agents: {stored}"
    );

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

/// ADR-0016: `role` is optional on the way in and absent on the way out when unset, so every
/// stored Configuration and every generated client keeps working unchanged.
#[tokio::test]
async fn a_configuration_carries_an_optional_role() {
    let server = spawn().await;
    let client = reqwest::Client::new();

    // Omitted: accepted, and absent from the response.
    let stored: serde_json::Value = client
        .put(url(server.addr, "/api/v1/configurations/base"))
        .json(&serde_json::json!({ "body": "receivers: {}" }))
        .send()
        .await
        .expect("put")
        .json()
        .await
        .expect("json");
    assert!(
        stored.get("role").is_none(),
        "an unset role stays out of the JSON: {stored}"
    );

    // Set: stored verbatim and returned.
    let stored: serde_json::Value = client
        .put(url(server.addr, "/api/v1/configurations/ruleset"))
        .json(&serde_json::json!({ "body": "rules: []", "role": "supplementary" }))
        .send()
        .await
        .expect("put")
        .json()
        .await
        .expect("json");
    assert_eq!(stored["role"], "supplementary");

    let got: serde_json::Value = client
        .get(url(server.addr, "/api/v1/configurations/ruleset"))
        .send()
        .await
        .expect("get")
        .json()
        .await
        .expect("json");
    assert_eq!(got["role"], "supplementary");

    // A value this project has no word for is carried, not rejected — the vocabulary is
    // Agent-type-specific and the Server never guesses at one.
    let stored: serde_json::Value = client
        .put(url(server.addr, "/api/v1/configurations/other"))
        .json(&serde_json::json!({ "body": "x", "role": "some-agents-own-word" }))
        .send()
        .await
        .expect("put")
        .json()
        .await
        .expect("json");
    assert_eq!(stored["role"], "some-agents-own-word");
}

/// ADR-0061 over the wire: saving proposes, the rollout act distributes — resource-wide or per
/// Agent — and a later edit waits as `update` until the next act pins it.
#[tokio::test]
async fn a_configuration_waits_until_it_is_rolled_out() {
    let server = spawn().await;
    let client = reqwest::Client::new();
    let uid = opamp::uid::InstanceUid::default();
    report(&client, server.addr, &support::full_report(&uid, "host", 1)).await;

    // Saved: complete, aimed at everybody — a visible candidate, assigned to nobody.
    let put = client
        .put(url(server.addr, "/api/v1/configurations/fleet"))
        .json(&serde_json::json!({ "body": "receivers: {}" }))
        .send()
        .await
        .expect("put");
    assert_eq!(put.status(), 200);
    let view = agent_view(&client, server.addr, &uid).await;
    assert_eq!(view["matched_configurations"][0], "fleet", "a candidate");
    assert!(
        view["assigned_configurations"]
            .as_array()
            .expect("array")
            .is_empty(),
        "saving distributes nothing: {view}"
    );
    assert_eq!(view["pending_configurations"][0]["name"], "fleet");
    assert_eq!(view["pending_configurations"][0]["change"], "new");

    // The resource-level act: rolled out to every currently matching Agent.
    let rollout: serde_json::Value = client
        .post(url(server.addr, "/api/v1/configurations/fleet/rollout"))
        .send()
        .await
        .expect("rollout")
        .json()
        .await
        .expect("json");
    assert_eq!(rollout["assigned_agents"], 1);
    let view = agent_view(&client, server.addr, &uid).await;
    assert_eq!(view["assigned_configurations"][0], "fleet");
    assert!(view["pending_configurations"]
        .as_array()
        .expect("array")
        .is_empty());

    // Edited: the Agent keeps its pinned revision; the edit waits as an update.
    let put = client
        .put(url(server.addr, "/api/v1/configurations/fleet"))
        .json(&serde_json::json!({ "body": "receivers: {}\nexporters: {}" }))
        .send()
        .await
        .expect("edit");
    assert_eq!(put.status(), 200);
    let view = agent_view(&client, server.addr, &uid).await;
    assert_eq!(
        view["pending_configurations"][0]["change"], "update",
        "the edit is saved, waiting, not in force: {view}"
    );

    // The per-Agent act releases the edit to this one Agent.
    let per_agent = client
        .post(url(server.addr, &format!("/api/v1/agents/{uid}/rollout")))
        .json(&serde_json::json!({ "configuration": "fleet" }))
        .send()
        .await
        .expect("rollout to agent");
    assert_eq!(per_agent.status(), 200);
    let view: serde_json::Value = per_agent.json().await.expect("json");
    assert!(view["pending_configurations"]
        .as_array()
        .expect("array")
        .is_empty());

    // A rollout of a name the store does not hold is 404, never a silent create.
    let missing = client
        .post(url(server.addr, "/api/v1/configurations/missing/rollout"))
        .send()
        .await
        .expect("rollout missing");
    assert_eq!(missing.status(), 404);
    let missing = client
        .post(url(server.addr, &format!("/api/v1/agents/{uid}/rollout")))
        .json(&serde_json::json!({ "configuration": "missing" }))
        .send()
        .await
        .expect("rollout missing to agent");
    assert_eq!(missing.status(), 404);
}

/// ADR-0061 point 6: an Agent that appears after the rollout act waits — it surfaces as pending
/// and receives nothing until an act of its own. The bulk act must be repeated (or the per-Agent
/// one pressed) for latecomers.
#[tokio::test]
async fn an_agent_that_appears_later_waits() {
    let server = spawn().await;
    let client = reqwest::Client::new();
    let early = opamp::uid::InstanceUid::default();
    report(
        &client,
        server.addr,
        &support::full_report(&early, "early", 1),
    )
    .await;

    client
        .put(url(server.addr, "/api/v1/configurations/fleet"))
        .json(&serde_json::json!({ "body": "receivers: {}" }))
        .send()
        .await
        .expect("put");
    let rollout: serde_json::Value = client
        .post(url(server.addr, "/api/v1/configurations/fleet/rollout"))
        .send()
        .await
        .expect("rollout")
        .json()
        .await
        .expect("json");
    assert_eq!(rollout["assigned_agents"], 1);

    // The latecomer enrols: a candidate, pending, assigned nothing.
    let late = opamp::uid::InstanceUid::default();
    report(
        &client,
        server.addr,
        &support::full_report(&late, "late", 1),
    )
    .await;
    let view = agent_view(&client, server.addr, &late).await;
    assert!(
        view["assigned_configurations"]
            .as_array()
            .expect("array")
            .is_empty(),
        "enrolment distributes nothing: {view}"
    );
    assert_eq!(view["pending_configurations"][0]["change"], "new");

    // Its own act — the empty body releases everything waiting.
    let rolled = client
        .post(url(server.addr, &format!("/api/v1/agents/{late}/rollout")))
        .send()
        .await
        .expect("rollout to agent");
    assert_eq!(rolled.status(), 200);
    let view: serde_json::Value = rolled.json().await.expect("json");
    assert_eq!(view["assigned_configurations"][0], "fleet");
}

/// ADR-0054 over the wire: a Configuration stating an Agent type reaches only Agents reporting
/// that `service.name`, whatever its Selector says.
#[tokio::test]
async fn a_typed_configuration_reaches_only_agents_of_its_type() {
    let server = spawn().await;
    let client = reqwest::Client::new();
    let uid = opamp::uid::InstanceUid::default();
    report(&client, server.addr, &support::full_report(&uid, "host", 1)).await;

    for (name, service_name) in [
        ("for-collectors", support::AGENT_TYPE),
        ("for-clients", "opamp-fleet-client"),
    ] {
        let put = client
            .put(url(server.addr, &format!("/api/v1/configurations/{name}")))
            .json(&serde_json::json!({ "body": "x", "service_name": service_name }))
            .send()
            .await
            .expect("put");
        assert_eq!(put.status(), 200);
    }

    assert_eq!(
        matched_configurations(&client, server.addr, &uid).await,
        ["for-collectors"],
        "the type is a fit, not an aim: the other type's Configuration is no candidate"
    );

    // And the per-Agent act refuses the one that does not fit (ADR-0061).
    let refused = client
        .post(url(server.addr, &format!("/api/v1/agents/{uid}/rollout")))
        .json(&serde_json::json!({ "configuration": "for-clients" }))
        .send()
        .await
        .expect("rollout");
    assert_eq!(
        refused.status(),
        409,
        "a non-candidate is refused, not assigned"
    );
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
    // Forgetting an Agent is part of the contract a portal generates against (ADR-0039), and the
    // description is where the "reaches no host" caveat has to be readable.
    let forget = &paths["/api/v1/agents/{instance_uid}"]["delete"];
    assert!(forget.is_object(), "DELETE on an Agent is described");
    assert!(
        forget["description"]
            .as_str()
            .expect("description")
            .contains("Nothing happens on the host"),
        "the description says what it does not do"
    );
    assert!(paths.contains_key("/api/v1/configurations"));
    assert!(paths.contains_key("/api/v1/configurations/{name}"));
    assert!(
        paths.contains_key("/api/v1/configurations/{name}/rollout"),
        "the rollout act is part of the contract (ADR-0061)"
    );
    assert!(paths.contains_key("/api/v1/agents/{instance_uid}/rollout"));
    assert!(
        !paths.contains_key("/api/v1/configurations/{name}/publication"),
        "publication left the contract with ADR-0061"
    );
    // The resource schemas ride along, so a client can be generated without the source.
    assert!(document["components"]["schemas"]["ConfigurationView"].is_object());
    assert!(document["components"]["schemas"]["ConfigurationSpec"].is_object());
    assert!(document["components"]["schemas"]["AgentView"].is_object());
}

#[tokio::test]
async fn the_docs_page_and_its_vendored_renderer_are_served_same_origin() {
    let server = spawn().await;
    let client = reqwest::Client::new();

    // The docs page renders the OpenAPI document and pulls its renderer from this same origin.
    let page = client
        .get(url(server.addr, "/api/v1/docs"))
        .send()
        .await
        .expect("get docs");
    assert_eq!(page.status(), 200);
    assert!(page
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/html")));
    let html = page.text().await.expect("html");
    assert!(
        html.contains("spec-url=\"/api/v1/openapi.json\""),
        "points at the document"
    );
    assert!(
        html.contains("/api/v1/docs/redoc.js"),
        "loads the vendored renderer"
    );

    // The vendored bundle is served as JavaScript — no CDN, so the docs work offline.
    let js = client
        .get(url(server.addr, "/api/v1/docs/redoc.js"))
        .send()
        .await
        .expect("get renderer");
    assert_eq!(js.status(), 200);
    assert!(js
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("javascript")));
    assert!(
        js.text().await.expect("js").len() > 100_000,
        "the real bundle, not a stub"
    );
}

#[tokio::test]
async fn the_ui_fonts_are_served_same_origin() {
    let server = spawn().await;
    let client = reqwest::Client::new();

    // The vendored IBM Plex Sans faces the UI references — no CDN, so the page works offline.
    for path in ["/fonts/ibm-plex-sans-400.woff2", "/fonts/ibm-plex-sans-600.woff2"] {
        let font = client
            .get(url(server.addr, path))
            .send()
            .await
            .expect("get font");
        assert_eq!(font.status(), 200);
        assert!(font
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct == "font/woff2"));
        let bytes = font.bytes().await.expect("bytes");
        assert_eq!(&bytes[..4], b"wOF2", "a real woff2, not a stub");
    }
}

#[tokio::test]
async fn configurations_survive_a_server_restart() {
    // The store is the persistence: a new AppState over the same directory restores everything.
    let dir = tempfile::tempdir().expect("tempdir");
    let store_dir = dir.path().join("fleet-configs");
    {
        let state = server::fleet::AppState::new(store_dir.clone()).expect("open");
        state
            .save_configuration(
                "keeper",
                server::configs::Revision {
                    selector: std::collections::BTreeMap::new(),
                    body: "receivers: {}\n".to_string(),
                    role: String::new(),
                    service_name: String::new(),
                },
            )
            .expect("put");
    }
    let reopened = server::fleet::AppState::new(store_dir).expect("reopen");
    let restored = reopened.configurations().list();
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].name, "keeper");
    assert_eq!(restored[0].saved.body, "receivers: {}\n");
}

/// ADR-0039. The gate, over the wire: an Agent that just reported is doing its job, and forgetting
/// it would have its configuration offered again — which restarts a Managed Process.
#[tokio::test]
async fn forgetting_an_agent_that_is_still_reporting_is_refused() {
    let server = spawn().await;
    let client = reqwest::Client::new();
    let uid = opamp::uid::InstanceUid::default();
    report(&client, server.addr, &support::full_report(&uid, "live", 1)).await;

    let refused = client
        .delete(url(server.addr, &format!("/api/v1/agents/{uid}")))
        .send()
        .await
        .expect("delete");
    assert_eq!(refused.status(), 409);
    let body: serde_json::Value = refused.json().await.expect("json");
    assert!(
        body["error"]
            .as_str()
            .expect("message")
            .contains("still reporting"),
        "the refusal says why: {body}"
    );
    assert_eq!(agents(&client, server.addr).await.len(), 1, "the row stays");
}

/// ADR-0039, points 1 and 3: forgetting drops the record and reaches no host, so a Client that is
/// still running simply comes back — and the Server, which now knows nothing about it, asks for
/// full state exactly as it does for any Agent it has never seen.
#[tokio::test]
async fn a_silent_agent_is_forgotten_and_returns_as_a_stranger() {
    // A zero budget makes the Agent silent the moment the clock ticks past its last report; the
    // rule under test is the comparison, not the duration.
    let server = support::spawn_with_stale_after(std::time::Duration::ZERO).await;
    let client = reqwest::Client::new();
    let uid = opamp::uid::InstanceUid::default();
    report(&client, server.addr, &support::full_report(&uid, "gone", 1)).await;
    assert_eq!(agents(&client, server.addr).await.len(), 1);
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let forgotten = client
        .delete(url(server.addr, &format!("/api/v1/agents/{uid}")))
        .send()
        .await
        .expect("delete");
    assert_eq!(forgotten.status(), 204);
    assert!(
        agents(&client, server.addr).await.is_empty(),
        "the row is gone"
    );

    // Nothing happened on the host, so the Client reports again — and is a stranger.
    let reply = report(&client, server.addr, &support::compressed_report(&uid, 2)).await;
    assert_ne!(
        reply.flags & opamp::proto::ServerToAgentFlags::ReportFullState as u64,
        0,
        "an Agent the Server does not know is asked for full state"
    );
    assert_eq!(
        agents(&client, server.addr).await.len(),
        1,
        "and it is back"
    );
}

/// The two ways to ask for something that is not there. `404` rather than a silent `204`: the
/// restart endpoint answers the same condition the same way, and an operator who mistypes a UID
/// should be told, not thanked.
#[tokio::test]
async fn forgetting_what_is_not_there_is_reported() {
    let server = spawn().await;
    let client = reqwest::Client::new();

    let unknown = client
        .delete(url(
            server.addr,
            &format!("/api/v1/agents/{}", opamp::uid::InstanceUid::default()),
        ))
        .send()
        .await
        .expect("delete");
    assert_eq!(unknown.status(), 404);

    let malformed = client
        .delete(url(server.addr, "/api/v1/agents/not-a-uid"))
        .send()
        .await
        .expect("delete");
    assert_eq!(malformed.status(), 400);
}

/// The body-less `POST` routes (`restart`, `rollback`) are CORS "simple requests": a cross-origin
/// page can fire them without a preflight. Fetch Metadata refuses the cross-site ones — a browser
/// stamps `Sec-Fetch-Site` and cannot let a page forge it — while same-origin and non-browser
/// callers pass. The guard runs before the handler, so it decides regardless of the target.
#[tokio::test]
async fn a_cross_site_state_changing_post_is_refused() {
    let server = spawn().await;
    let client = reqwest::Client::new();
    let uid = opamp::uid::InstanceUid::default();

    // A browser marks a cross-site request: refused with 403, whether or not the Agent exists.
    let cross = client
        .post(url(server.addr, &format!("/api/v1/agents/{uid}/restart")))
        .header("sec-fetch-site", "cross-site")
        .send()
        .await
        .expect("restart");
    assert_eq!(cross.status(), 403, "a cross-site restart is refused");

    // The same-site case from the bundled UI passes the guard — the request reaches the handler,
    // which then answers 404 for an Agent that is not there. The point is it is *not* 403.
    let same_origin = client
        .post(url(server.addr, &format!("/api/v1/agents/{uid}/restart")))
        .header("sec-fetch-site", "same-origin")
        .send()
        .await
        .expect("restart");
    assert_ne!(
        same_origin.status(),
        403,
        "a same-origin restart is not a CSRF"
    );
    assert_eq!(
        same_origin.status(),
        404,
        "it reaches the handler: no such agent"
    );

    // A non-browser client (curl, a portal) sends no Sec-Fetch header and is unaffected.
    let no_header = client
        .post(url(server.addr, &format!("/api/v1/agents/{uid}/restart")))
        .send()
        .await
        .expect("restart");
    assert_ne!(
        no_header.status(),
        403,
        "a client with no fetch metadata is not a CSRF"
    );
}

/// The fleet as the REST API shows it.
async fn agents(client: &reqwest::Client, addr: std::net::SocketAddr) -> Vec<serde_json::Value> {
    client
        .get(url(addr, "/api/v1/agents"))
        .send()
        .await
        .expect("agents")
        .json()
        .await
        .expect("json")
}

/// One OpAMP report over plain HTTP, the way a Client sends it.
async fn report(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    msg: &opamp::proto::AgentToServer,
) -> opamp::proto::ServerToAgent {
    use prost::Message;
    let response = client
        .post(url(addr, "/v1/opamp"))
        .header("content-type", "application/x-protobuf")
        .body(msg.encode_to_vec())
        .send()
        .await
        .expect("post");
    assert_eq!(response.status(), 200);
    opamp::proto::ServerToAgent::decode(response.bytes().await.expect("body").as_ref())
        .expect("decode")
}

/// ADR-0042, and the whole point of it: a rollout ring becomes a Server-side decision. The Agent
/// reports nothing about `rollout`, so before the label the canary Configuration cannot reach it —
/// and moving it into the ring is one API call rather than an edit and a restart on that host.
#[tokio::test]
async fn a_label_moves_an_agent_into_a_rollout_ring() {
    let server = spawn().await;
    let client = reqwest::Client::new();
    let uid = opamp::uid::InstanceUid::default();
    report(&client, server.addr, &support::full_report(&uid, "host", 1)).await;

    // A Configuration aimed at the canary ring. Nothing reports `rollout`, so it proposes
    // itself to nobody — and since ADR-0061 even a match only proposes.
    let put = client
        .put(url(server.addr, "/api/v1/configurations/canary"))
        .json(&serde_json::json!({ "selector": { "rollout": "canary" }, "body": "receivers: {}" }))
        .send()
        .await
        .expect("put");
    assert_eq!(put.status(), 200);
    assert!(
        matched_configurations(&client, server.addr, &uid)
            .await
            .is_empty(),
        "an attribute nobody reports reaches nobody"
    );

    // One call, no host access: the Agent is in the ring.
    let labelled = set_labels(
        &client,
        server.addr,
        &uid,
        serde_json::json!({"rollout": "canary"}),
    )
    .await;
    assert_eq!(labelled.status(), 200);
    let view: serde_json::Value = labelled.json().await.expect("json");
    assert_eq!(view["labels"]["rollout"], "canary");
    assert_eq!(
        matched_configurations(&client, server.addr, &uid).await,
        ["canary"],
        "the label is matched exactly like a reported attribute"
    );

    // And out again: an empty map clears them.
    assert_eq!(
        set_labels(&client, server.addr, &uid, serde_json::json!({}))
            .await
            .status(),
        200
    );
    assert!(matched_configurations(&client, server.addr, &uid)
        .await
        .is_empty());
}

/// The crux (ADR-0042 point 3): reported attributes decide which artifact fits a machine, so a
/// label may not restate one. Refused where it is written, naming the key — not quietly ignored.
#[tokio::test]
async fn a_label_may_not_restate_what_the_agent_reports() {
    let server = spawn().await;
    let client = reqwest::Client::new();
    let uid = opamp::uid::InstanceUid::default();
    report(&client, server.addr, &support::full_report(&uid, "host", 1)).await;

    // `os.type` is reported by this Agent and chooses which artifact it is offered (ADR-0031).
    let refused = set_labels(
        &client,
        server.addr,
        &uid,
        serde_json::json!({"os.type": "windows"}),
    )
    .await;
    assert_eq!(refused.status(), 409);
    let body: serde_json::Value = refused.json().await.expect("json");
    let message = body["error"].as_str().expect("message");
    assert!(message.contains("os.type"), "it names the key: {message}");

    // An empty value is a mistake rather than an intent, and is refused where it is written.
    assert_eq!(
        set_labels(
            &client,
            server.addr,
            &uid,
            serde_json::json!({"rollout": ""})
        )
        .await
        .status(),
        400
    );

    // Nothing was stored by either attempt.
    let agents = agents(&client, server.addr).await;
    assert!(
        agents[0]["labels"].as_object().expect("labels").is_empty(),
        "a refused set leaves nothing behind"
    );
}

/// A Supervisor whose Managed Process will not start still reads Connected — truthfully, the
/// Supervisor lives — so the reported health is the only place an operator can see that nothing
/// is actually running. The view must carry it whole: the flag, the Agent's own status string,
/// and the reason (`ComponentHealth.last_error`, which the Baseline says SHOULD be set when
/// unhealthy).
#[tokio::test]
async fn the_view_carries_the_reported_health_and_its_reason() {
    let server = spawn().await;
    let client = reqwest::Client::new();
    let uid = opamp::uid::InstanceUid::default();

    // Before any health report, the view claims nothing either way.
    report(&client, server.addr, &support::full_report(&uid, "host", 1)).await;
    let view = &agents(&client, server.addr).await[0];
    assert_eq!(view["healthy"], false);
    assert_eq!(view["health_status"], "");
    assert_eq!(view["health_error"], "");

    // The Supervisor's process would not start: unhealthy, with the situation and the reason.
    let mut unhealthy = support::full_report(&uid, "host", 2);
    unhealthy.capabilities |= opamp::proto::AgentCapabilities::ReportsHealth as u64;
    unhealthy.health = Some(opamp::proto::ComponentHealth {
        healthy: false,
        status: "no process installed".to_string(),
        last_error: "cannot spawn otelcol: No such file or directory".to_string(),
        ..Default::default()
    });
    report(&client, server.addr, &unhealthy).await;
    let view = &agents(&client, server.addr).await[0];
    assert_eq!(view["healthy"], false);
    assert_eq!(view["health_status"], "no process installed");
    assert_eq!(
        view["health_error"],
        "cannot spawn otelcol: No such file or directory"
    );

    // The process came up: the finding clears with the next report.
    let mut healthy = support::full_report(&uid, "host", 3);
    healthy.capabilities |= opamp::proto::AgentCapabilities::ReportsHealth as u64;
    healthy.health = Some(opamp::proto::ComponentHealth {
        healthy: true,
        status: "running".to_string(),
        ..Default::default()
    });
    report(&client, server.addr, &healthy).await;
    let view = &agents(&client, server.addr).await[0];
    assert_eq!(view["healthy"], true);
    assert_eq!(view["health_status"], "running");
    assert_eq!(view["health_error"], "");
}

/// Labels are the operator's decision, not something the Server learned, so forgetting an Agent
/// (ADR-0039) does not undo them: a host that comes back is in the ring it was put in.
#[tokio::test]
async fn forgetting_an_agent_keeps_its_labels() {
    let server = support::spawn_with_stale_after(std::time::Duration::ZERO).await;
    let client = reqwest::Client::new();
    let uid = opamp::uid::InstanceUid::default();
    report(&client, server.addr, &support::full_report(&uid, "host", 1)).await;
    assert_eq!(
        set_labels(
            &client,
            server.addr,
            &uid,
            serde_json::json!({"rollout": "canary"})
        )
        .await
        .status(),
        200
    );

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let forgotten = client
        .delete(url(server.addr, &format!("/api/v1/agents/{uid}")))
        .send()
        .await
        .expect("delete");
    assert_eq!(forgotten.status(), 204);
    assert!(agents(&client, server.addr).await.is_empty());

    // It comes back — still in its ring, without anyone re-labelling it.
    report(&client, server.addr, &support::full_report(&uid, "host", 2)).await;
    let agents = agents(&client, server.addr).await;
    assert_eq!(agents[0]["labels"]["rollout"], "canary");
}

/// One Agent's row of the fleet view.
async fn agent_view(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    uid: &opamp::uid::InstanceUid,
) -> serde_json::Value {
    agents(client, addr)
        .await
        .into_iter()
        .find(|a| a["instance_uid"] == uid.to_string())
        .expect("the agent is in the fleet")
}

/// The Configurations currently matching one Agent — the candidates — as the fleet view reports
/// them.
async fn matched_configurations(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    uid: &opamp::uid::InstanceUid,
) -> Vec<String> {
    agents(client, addr)
        .await
        .into_iter()
        .find(|a| a["instance_uid"] == uid.to_string())
        .expect("the agent is in the fleet")["matched_configurations"]
        .as_array()
        .expect("array")
        .iter()
        .map(|v| v.as_str().expect("name").to_string())
        .collect()
}

async fn set_labels(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    uid: &opamp::uid::InstanceUid,
    labels: serde_json::Value,
) -> reqwest::Response {
    client
        .put(url(addr, &format!("/api/v1/agents/{uid}/labels")))
        .json(&serde_json::json!({ "labels": labels }))
        .send()
        .await
        .expect("put labels")
}
