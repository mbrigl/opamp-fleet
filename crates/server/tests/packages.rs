//! Package delivery (ADR-0015, reorganised into Sets by ADR-0052): the REST Set + entry routes,
//! and the hash-gated `PackagesAvailable` offer toward capable Agents.

mod support;

use opamp::proto::{
    AgentCapabilities, PackageStatus, PackageStatusEnum, PackageStatuses, ServerCapabilities,
    ServerToAgent,
};
use opamp::uid::InstanceUid;
use prost::Message as _;
use server::fleet::{AppState, PackageOffering};
use server::packages::PackageStore;
use std::sync::Arc;
use support::{full_report, TestServer};

const PROTOBUF: &str = "application/x-protobuf";

/// A Server with package delivery armed over a temp store; returns the server and its temp dir.
async fn spawn_with_packages() -> (TestServer, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = PackageStore::open(dir.path().join("packages")).expect("store");
    let state = Arc::new(
        AppState::new(dir.path().join("fleet-configs"))
            .expect("configs")
            .with_packages(Some(PackageOffering::new(store, String::new()))),
    );
    let app = server::app(state.clone(), server::transport::Admission::open());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    (
        TestServer {
            addr,
            state,
            _dir: dir,
        },
        tempfile::tempdir().expect("scratch"),
    )
}

async fn exchange(server: &TestServer, msg: &opamp::proto::AgentToServer) -> ServerToAgent {
    let response = reqwest::Client::new()
        .post(format!("http://{}/v1/opamp", server.addr))
        .header("content-type", PROTOBUF)
        .body(msg.encode_to_vec())
        .send()
        .await
        .expect("post");
    assert_eq!(response.status(), 200);
    ServerToAgent::decode(response.bytes().await.expect("body").as_ref()).expect("decode")
}

/// The platform the test fleet reports (see `support::full_report`), and therefore the only one
/// an entry may be stored under for these Agents to be offered it (ADR-0031).
const HOST: &str = "linux/amd64";

/// The base of one Set's routes: `/api/v1/packages/<name>/<type>/<version>` (ADR-0052) — the
/// identity is the path, stated at creation and never edited.
fn set_url(server: &TestServer, name: &str, version: &str) -> String {
    format!(
        "http://{}/api/v1/packages/{name}/{}/{version}",
        server.addr,
        support::AGENT_TYPE
    )
}

/// `PUT /api/v1/packages/{name}/{type}/{version}` — creates the Set, as a draft.
async fn create_set(server: &TestServer, name: &str, version: &str) {
    let response = reqwest::Client::new()
        .put(set_url(server, name, version))
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("put set");
    assert_eq!(response.status(), 200, "creating the set should succeed");
}

/// `PUT …/entries/{os}/{arch}` — stores one platform's artifact into a draft Set.
async fn upload_entry(
    server: &TestServer,
    name: &str,
    version: &str,
    platform: &str,
    artifact: &[u8],
) -> reqwest::Response {
    reqwest::Client::new()
        .put(format!(
            "{}/entries/{platform}",
            set_url(server, name, version)
        ))
        .body(artifact.to_vec())
        .send()
        .await
        .expect("put entry")
}

/// `PUT …/publication` — what starts a rollout, and what withdraws it.
async fn publish(server: &TestServer, name: &str, version: &str, published: bool) {
    let response = reqwest::Client::new()
        .put(format!("{}/publication", set_url(server, name, version)))
        .json(&serde_json::json!({ "published": published }))
        .send()
        .await
        .expect("put publication");
    assert_eq!(response.status(), 200, "publishing should succeed");
}

/// Create + upload + publish in one go: the ordinary path a released Set takes.
async fn upload(server: &TestServer, name: &str, version: &str, artifact: &[u8]) {
    create_set(server, name, version).await;
    let response = upload_entry(server, name, version, HOST, artifact).await;
    assert_eq!(response.status(), 200, "upload should succeed");
    publish(server, name, version, true).await;
}

fn sha256(bytes: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).to_vec()
}

/// `PUT …/selector` — how an operator aims a Set, in any state.
async fn set_selector(
    server: &TestServer,
    name: &str,
    version: &str,
    pairs: &[(&str, &str)],
) -> reqwest::Response {
    let selector: std::collections::BTreeMap<&str, &str> = pairs.iter().copied().collect();
    reqwest::Client::new()
        .put(format!("{}/selector", set_url(server, name, version)))
        .json(&serde_json::json!({ "selector": selector }))
        .send()
        .await
        .expect("put selector")
}

/// ADR-0052 in place of ADR-0019: versions are first-class Sets, and the rollback is a publication
/// move — retract the newest published version and the fleet falls back to the one still
/// published beneath it, without anyone producing an old artifact again.
#[tokio::test]
async fn retracting_the_newest_version_rolls_the_fleet_back() {
    let (server, _scratch) = spawn_with_packages().await;
    upload(&server, "otelcol", "0.156.0", b"old-binary").await;
    upload(&server, "otelcol", "0.157.0", b"new-binary").await;

    let uid = InstanceUid::default();
    let server_ref = &server;
    let offered = |sequence: u64| async move {
        let mut report = full_report(&uid, "collector", sequence);
        report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
        exchange(server_ref, &report)
            .await
            .packages_available
            .expect("an offer")
    };

    // Both versions are published and equally aimed: the greater one wins.
    assert_eq!(offered(1).await.packages["otelcol"].version, "0.157.0");

    // The rollback: one request, naming the version it withdraws. The old Set is still published,
    // so the fleet falls back to it — and its artifact is still here to serve.
    publish(&server, "otelcol", "0.157.0", false).await;
    let fallback = offered(2).await;
    assert_eq!(fallback.packages["otelcol"].version, "0.156.0");
    let served = reqwest::Client::new()
        .get(format!(
            "{}/file?os=linux&arch=amd64",
            set_url(&server, "otelcol", "0.156.0")
        ))
        .send()
        .await
        .expect("download")
        .bytes()
        .await
        .expect("bytes");
    assert_eq!(served.as_ref(), b"old-binary");
}

#[tokio::test]
async fn an_uploaded_set_is_offered_downloaded_and_gated() {
    let (server, _scratch) = spawn_with_packages().await;
    let uid = InstanceUid::default();
    let mut report = full_report(&uid, "collector", 1);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;

    // Nothing uploaded yet: no offer, and the capability is not declared.
    let reply = exchange(&server, &report).await;
    assert!(reply.packages_available.is_none());
    assert_eq!(
        reply.capabilities & ServerCapabilities::OffersPackages as u64,
        0
    );

    upload(&server, "otelcol", "1.2.3", b"the-new-binary").await;

    // Now the offer arrives, declares the capability, and carries a working download URL.
    let reply = exchange(&server, &report).await;
    assert_ne!(
        reply.capabilities & ServerCapabilities::OffersPackages as u64,
        0
    );
    let offer = reply.packages_available.expect("an offer");
    assert!(!offer.all_packages_hash.is_empty());
    let available = &offer.packages["otelcol"];
    assert_eq!(available.version, "1.2.3");
    let file = available.file.as_ref().expect("a downloadable file");
    // download_base was empty, so the URL is a path the Client resolves against its endpoint —
    // and it names the whole identity, so two versions never serve each other's bytes.
    assert_eq!(
        file.download_url,
        format!(
            "/api/v1/packages/otelcol/{}/1.2.3/file?os=linux&arch=amd64",
            support::AGENT_TYPE
        )
    );
    assert_eq!(file.content_hash, sha256(b"the-new-binary"));

    // The artifact downloads byte-for-byte.
    let downloaded = reqwest::Client::new()
        .get(format!("http://{}{}", server.addr, file.download_url))
        .send()
        .await
        .expect("download")
        .bytes()
        .await
        .expect("bytes");
    assert_eq!(downloaded.as_ref(), b"the-new-binary");

    // Reporting the offered aggregate hash as installed silences the offer (the Baseline's gate).
    let mut installed = full_report(&uid, "collector", 2);
    installed.capabilities |= AgentCapabilities::AcceptsPackages as u64
        | AgentCapabilities::ReportsPackageStatuses as u64;
    installed.package_statuses = Some(PackageStatuses {
        packages: [(
            "otelcol".to_string(),
            PackageStatus {
                name: "otelcol".to_string(),
                agent_has_version: "1.2.3".to_string(),
                status: PackageStatusEnum::Installed as i32,
                ..Default::default()
            },
        )]
        .into(),
        server_provided_all_packages_hash: offer.all_packages_hash.clone(),
        error_message: String::new(),
    });
    let reply = exchange(&server, &installed).await;
    assert!(
        reply.packages_available.is_none(),
        "a matching reported hash stops the offer"
    );
}

#[tokio::test]
async fn no_offer_without_the_capability() {
    let (server, _scratch) = spawn_with_packages().await;
    upload(&server, "otelcol", "1.0.0", b"bin").await;
    let uid = InstanceUid::default();
    // full_report declares no AcceptsPackages.
    let reply = exchange(&server, &full_report(&uid, "incapable", 1)).await;
    assert!(
        reply.packages_available.is_none(),
        "capability negotiation is binding"
    );
}

/// An entry belongs to a Set: uploading toward an identity nobody created is a 404, not a package
/// conjured out of a URL (ADR-0052 — the identity is stated at creation).
#[tokio::test]
async fn an_entry_needs_its_set_first() {
    let (server, _scratch) = spawn_with_packages().await;
    let response = upload_entry(&server, "otelcol", "1.0.0", HOST, b"bytes").await;
    assert_eq!(response.status(), 404);
}

/// A package is a *program*: an `otelcol-contrib` binary weighs hundreds of megabytes, so the
/// entry route must not be bounded by the framework's 2 MiB default, and the artifact must reach
/// the Agent unchanged whatever its size.
#[tokio::test]
async fn an_artifact_larger_than_the_framework_default_uploads_and_downloads_intact() {
    let (server, _scratch) = spawn_with_packages().await;

    // Past axum's 2 MiB default body limit — the limit that used to make a real binary
    // undeliverable — and not a round number, so a truncation would show.
    let artifact: Vec<u8> = (0..(5 * 1024 * 1024 + 17))
        .map(|i| (i % 251) as u8)
        .collect();
    upload(&server, "otelcol", "1.2.3", &artifact).await;

    let downloaded = reqwest::Client::new()
        .get(format!(
            "{}/file?os=linux&arch=amd64",
            set_url(&server, "otelcol", "1.2.3")
        ))
        .send()
        .await
        .expect("download");
    assert_eq!(
        downloaded
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok()),
        Some(artifact.len().to_string().as_str()),
        "the length is advertised, so the Agent can size the transfer"
    );
    let bytes = downloaded.bytes().await.expect("bytes");
    assert_eq!(bytes.len(), artifact.len());
    assert_eq!(bytes.as_ref(), artifact.as_slice(), "byte-identical");
}

/// The upload limit is a configured bound, not an accident of the framework: past it the API
/// refuses rather than buffering whatever arrives.
#[tokio::test]
async fn an_artifact_past_the_configured_limit_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = PackageStore::open(dir.path().join("packages")).expect("store");
    let state = Arc::new(
        AppState::new(dir.path().join("fleet-configs"))
            .expect("configs")
            .with_packages(Some(PackageOffering::new(store, String::new())))
            .with_max_package_size(4096),
    );
    let app = server::app(state.clone(), server::transport::Admission::open());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    let server = TestServer {
        addr,
        state,
        _dir: dir,
    };

    create_set(&server, "otelcol", "1.0.0").await;
    let response = upload_entry(&server, "otelcol", "1.0.0", HOST, &vec![0u8; 8192]).await;
    assert_eq!(response.status(), 413);
}

/// The point of ADR-0017: an artifact reaches the Agents its Selector matches and no others, so a
/// binary rollout can be tried on part of the fleet first.
#[tokio::test]
async fn a_selector_aims_a_set_at_part_of_the_fleet() {
    let (server, _scratch) = spawn_with_packages().await;
    upload(&server, "otelcol", "2.0.0", b"the-new-binary").await;
    assert_eq!(
        set_selector(&server, "otelcol", "2.0.0", &[("os.type", "linux")])
            .await
            .status(),
        200
    );

    // full_report describes an Agent with os.type = linux (see the test scaffolding).
    let targeted = InstanceUid::default();
    let mut report = full_report(&targeted, "collector", 1);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    let reply = exchange(&server, &report).await;
    let offer = reply
        .packages_available
        .expect("the matching Agent is offered it");
    assert!(offer.packages.contains_key("otelcol"));
    assert!(!offer.all_packages_hash.is_empty());

    // An Agent that reports something else is offered nothing at all — not an empty offer, no
    // offer: it keeps running what it runs (goal 9, applied to software).
    let other = InstanceUid::default();
    let mut elsewhere = full_report(&other, "windows-box", 1);
    elsewhere.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    if let Some(description) = elsewhere.agent_description.as_mut() {
        for attribute in &mut description.non_identifying_attributes {
            if attribute.key == "os.type" {
                attribute.value = Some(opamp::proto::AnyValue {
                    value: Some(opamp::proto::any_value::Value::StringValue(
                        "windows".to_string(),
                    )),
                });
            }
        }
    }
    let reply = exchange(&server, &elsewhere).await;
    assert!(
        reply.packages_available.is_none(),
        "an Agent outside the Selector is offered nothing"
    );
}

/// The aggregate hash gates re-offering, and it is per Agent: computed over the whole store it
/// would never match what a targeted Agent was actually sent, and the Server would re-offer for ever.
#[tokio::test]
async fn the_aggregate_hash_an_agent_echoes_is_the_one_it_was_offered() {
    let (server, _scratch) = spawn_with_packages().await;
    upload(&server, "otelcol", "2.0.0", b"for-linux").await;
    upload(&server, "otelcol-win", "2.0.0", b"for-windows").await;
    assert_eq!(
        set_selector(&server, "otelcol", "2.0.0", &[("os.type", "linux")])
            .await
            .status(),
        200
    );
    assert_eq!(
        set_selector(&server, "otelcol-win", "2.0.0", &[("os.type", "windows")])
            .await
            .status(),
        200
    );

    let uid = InstanceUid::default();
    let mut report = full_report(&uid, "collector", 1);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64
        | AgentCapabilities::ReportsPackageStatuses as u64;
    let offer = exchange(&server, &report)
        .await
        .packages_available
        .expect("an offer");
    assert_eq!(
        offer.packages.len(),
        1,
        "only the Set this Agent's Selector matches"
    );

    // Echoing exactly that aggregate settles it — the Server must not keep re-offering.
    let mut installed = full_report(&uid, "collector", 2);
    installed.capabilities |= AgentCapabilities::AcceptsPackages as u64
        | AgentCapabilities::ReportsPackageStatuses as u64;
    installed.package_statuses = Some(PackageStatuses {
        packages: [(
            "otelcol".to_string(),
            PackageStatus {
                name: "otelcol".to_string(),
                agent_has_version: "2.0.0".to_string(),
                status: PackageStatusEnum::Installed as i32,
                ..Default::default()
            },
        )]
        .into(),
        server_provided_all_packages_hash: offer.all_packages_hash.clone(),
        error_message: String::new(),
    });
    assert!(
        exchange(&server, &installed)
            .await
            .packages_available
            .is_none(),
        "the Agent is in sync with what it was offered"
    );
}

/// The canary shape an operator actually wants, now inside one name (ADR-0052): the fleet-wide
/// version and the canary version are two Sets of the same package, the narrower Selector wins for
/// the ring, and widening it finishes the rollout because the greater version wins the tie.
#[tokio::test]
async fn a_canary_ring_is_one_selector_edit_across_two_versions_of_one_name() {
    let (server, _scratch) = spawn_with_packages().await;
    upload(&server, "otelcol", "2.0.0", b"the-fleet-binary").await;
    upload(&server, "otelcol", "3.0.0", b"the-canary-binary").await;
    assert_eq!(
        set_selector(
            &server,
            "otelcol",
            "3.0.0",
            &[("service.instance.name", "canary-host")]
        )
        .await
        .status(),
        200
    );

    async fn version_offered_to(server: &TestServer, name: &str) -> String {
        let uid = InstanceUid::default();
        let mut report = full_report(&uid, name, 1);
        report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
        let offer = exchange(server, &report)
            .await
            .packages_available
            .expect("an offer");
        offer.packages["otelcol"].version.clone()
    }

    assert_eq!(
        version_offered_to(&server, "canary-host").await,
        "3.0.0",
        "the named host gets the canary version"
    );
    assert_eq!(
        version_offered_to(&server, "ordinary-host").await,
        "2.0.0",
        "everyone else keeps the fleet-wide version"
    );

    // The rollout finishes: widening the canary Selector to everyone lets the greater version win.
    assert_eq!(
        set_selector(&server, "otelcol", "3.0.0", &[])
            .await
            .status(),
        200
    );
    assert_eq!(version_offered_to(&server, "finished-host").await, "3.0.0");
}

/// Two equally specific Selectors on *different names* reaching one Agent is still the one case
/// with no defensible answer: the Server offers neither and says so in the fleet view.
#[tokio::test]
async fn equally_specific_selectors_offer_nothing_and_are_reported() {
    let (server, _scratch) = spawn_with_packages().await;
    upload(&server, "otelcol", "2.0.0", b"one").await;
    upload(&server, "otelcol-next", "3.0.0", b"two").await;
    // Both name exactly one attribute, and both match the Agent below.
    assert_eq!(
        set_selector(&server, "otelcol", "2.0.0", &[("os.type", "linux")])
            .await
            .status(),
        200
    );
    assert_eq!(
        set_selector(
            &server,
            "otelcol-next",
            "3.0.0",
            &[("os.description", "Testix 1.0 LTS")]
        )
        .await
        .status(),
        200
    );

    let uid = InstanceUid::default();
    let mut report = full_report(&uid, "collector", 1);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    let reply = exchange(&server, &report).await;
    assert!(
        reply.packages_available.is_none(),
        "an ambiguous target is offered nothing"
    );

    let view = &server.state.snapshot()[0];
    let conflict = view
        .package_conflict
        .as_ref()
        .expect("the fleet view says why");
    assert!(
        conflict.contains("otelcol") && conflict.contains("otelcol-next"),
        "the reason names both packages: {conflict}"
    );
}

/// ADR-0018: an entry may live somewhere else. The Server stores the reference, offers that
/// address verbatim with the operator's checksum and headers, and has nothing of its own to serve.
#[tokio::test]
async fn a_referenced_entry_is_offered_from_its_source_and_not_from_here() {
    let (server, _scratch) = spawn_with_packages().await;

    // A source the probe can reach: a tiny server standing in for a release page.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let source_addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut scratch = [0u8; 1024];
            let _ = stream.read(&mut scratch).await;
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                .await;
        }
    });

    let url = format!("http://{source_addr}/releases/otelcol.tar.gz");
    let digest = sha256(b"the-artifact-we-never-see");
    create_set(&server, "otelcol", "0.157.0").await;
    let response = reqwest::Client::new()
        .put(format!(
            "{}/entries/{HOST}/source",
            set_url(&server, "otelcol", "0.157.0")
        ))
        .json(&serde_json::json!({
            "url": url,
            "sha256": hex::encode(&digest),
            "headers": { "Authorization": "Bearer release-token" }
        }))
        .send()
        .await
        .expect("put source");
    assert_eq!(response.status(), 200);
    publish(&server, "otelcol", "0.157.0", true).await;

    // The offer names the source, carries the operator's hash, and passes the headers on.
    let uid = InstanceUid::default();
    let mut report = full_report(&uid, "collector", 1);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    let offer = exchange(&server, &report)
        .await
        .packages_available
        .expect("an offer");
    let file = offer.packages["otelcol"]
        .file
        .as_ref()
        .expect("a downloadable file");
    assert_eq!(file.download_url, url, "the Agent is sent to the source");
    assert_eq!(
        file.content_hash, digest,
        "the operator's checksum, unaltered"
    );
    let headers = file.headers.as_ref().expect("headers ride along");
    assert_eq!(headers.headers[0].key, "Authorization");

    // And this Server has nothing to hand out: it never downloaded the artifact.
    let local = reqwest::Client::new()
        .get(format!(
            "{}/file?os=linux&arch=amd64",
            set_url(&server, "otelcol", "0.157.0")
        ))
        .send()
        .await
        .expect("get");
    assert_eq!(
        local.status(),
        404,
        "a referenced artifact is not served from here"
    );
}

/// The probe is a typo catch, and only a definitive refusal counts as one: a source this Server
/// cannot reach at all says nothing about whether the Agents can.
#[tokio::test]
async fn a_source_that_refuses_the_probe_is_rejected_but_an_unreachable_one_is_not() {
    let (server, _scratch) = spawn_with_packages().await;
    create_set(&server, "otelcol", "1.0.0").await;

    // A source that answers 404 — the shape of a mistyped release path.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let refusing = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut scratch = [0u8; 1024];
            let _ = stream.read(&mut scratch).await;
            let _ = stream
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await;
        }
    });

    let server_ref = &server;
    let put_source = |url: String| async move {
        reqwest::Client::new()
            .put(format!(
                "{}/entries/{HOST}/source",
                set_url(server_ref, "otelcol", "1.0.0")
            ))
            .json(&serde_json::json!({
                "url": url,
                "sha256": hex::encode(sha256(b"x")),
            }))
            .send()
            .await
            .expect("put source")
    };

    let refused = put_source(format!("http://{refusing}/typo.tar.gz")).await;
    assert_eq!(refused.status(), 400);
    let body = refused.text().await.expect("body");
    assert!(
        body.contains("404"),
        "the refusal quotes what the source said: {body}"
    );

    // Port 1 answers nothing at all: stored anyway, because the fleet may reach what we cannot.
    let unreachable = put_source("http://127.0.0.1:1/otelcol.tar.gz".to_string()).await;
    assert_eq!(unreachable.status(), 200);
}

/// ADR-0052 in place of ADR-0034's late typing: the Agent type is identity, stated at creation —
/// there is no untyped state — and a Set built for another type is never a candidate, whatever
/// its Selector says.
#[tokio::test]
async fn a_set_reaches_only_agents_of_its_type() {
    let (server, _scratch) = spawn_with_packages().await;

    // A Set for a different kind of Agent, complete and published.
    let response = reqwest::Client::new()
        .put(format!(
            "http://{}/api/v1/packages/promtail/promtail/1.0.0",
            server.addr
        ))
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("put set");
    assert_eq!(response.status(), 200);
    let response = reqwest::Client::new()
        .put(format!(
            "http://{}/api/v1/packages/promtail/promtail/1.0.0/entries/{HOST}",
            server.addr
        ))
        .body(b"the-binary".to_vec())
        .send()
        .await
        .expect("put entry");
    assert_eq!(response.status(), 200);
    let response = reqwest::Client::new()
        .put(format!(
            "http://{}/api/v1/packages/promtail/promtail/1.0.0/publication",
            server.addr
        ))
        .json(&serde_json::json!({ "published": true }))
        .send()
        .await
        .expect("publish");
    assert_eq!(response.status(), 200);

    let uid = InstanceUid::default();
    let mut report = full_report(&uid, "edge-01", 1);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    assert!(
        exchange(&server, &report)
            .await
            .packages_available
            .is_none(),
        "a Set built for another type is not a candidate"
    );

    // The same artifact under this fleet's type reaches it.
    upload(&server, "otelcol", "1.0.0", b"the-binary").await;
    let mut report = full_report(&uid, "edge-01", 2);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    let offer = exchange(&server, &report)
        .await
        .packages_available
        .expect("an offer");
    assert!(offer.packages.contains_key("otelcol"));
}

/// The silent no-op ADR-0034 named: a Set can target nobody through a mistyped Agent type, a
/// platform the fleet does not run, or a Selector that matches no one — and none of the three is
/// a rejected upload, so without a count nothing says it.
#[tokio::test]
async fn a_set_says_how_many_agents_it_reaches() {
    let (server, _scratch) = spawn_with_packages().await;
    let uid = InstanceUid::default();
    exchange(&server, &support::full_report(&uid, "one", 1)).await;

    async fn list(server: &TestServer) -> serde_json::Value {
        reqwest::Client::new()
            .get(format!("http://{}/api/v1/packages", server.addr))
            .send()
            .await
            .expect("list packages")
            .json()
            .await
            .expect("json")
    }

    async fn reach(server: &TestServer, name: &str) -> i64 {
        list(server)
            .await
            .as_array()
            .expect("array")
            .iter()
            .find(|p| p["name"] == name)
            .unwrap_or_else(|| panic!("no set {name} in the list"))["targeted_agents"]
            .as_i64()
            .expect("targeted_agents")
    }

    // Uploaded under the type this fleet reports: it reaches the one Agent there is.
    upload(&server, "otelcol", "1.0.0", b"binary").await;
    assert_eq!(reach(&server, "otelcol").await, 1);

    // A second Agent on the same platform doubles it.
    exchange(
        &server,
        &support::full_report(&InstanceUid::default(), "two", 1),
    )
    .await;
    assert_eq!(reach(&server, "otelcol").await, 2);

    // A Selector that matches nobody: still stored, still valid, reaching no one — the case that
    // was invisible before.
    assert_eq!(
        set_selector(&server, "otelcol", "1.0.0", &[("env", "prod")])
            .await
            .status(),
        200
    );
    assert_eq!(reach(&server, "otelcol").await, 0);

    // An entry for a platform this fleet does not run reaches nobody either.
    create_set(&server, "fluentbit", "2.0.0").await;
    let response = upload_entry(&server, "fluentbit", "2.0.0", "windows/amd64", b"exe").await;
    assert_eq!(response.status(), 200);
    publish(&server, "fluentbit", "2.0.0", true).await;
    assert_eq!(reach(&server, "fluentbit").await, 0);
}

/// ADR-0042 reaches packages, not just Configurations — which is the case it exists for. A binary
/// rollout starts on the hosts an operator moved into the canary ring, and moving one in needs no
/// access to that host.
#[tokio::test]
async fn a_label_aims_a_set_at_part_of_the_fleet() {
    let (server, _scratch) = spawn_with_packages().await;
    let canary = InstanceUid::default();
    let rest = InstanceUid::default();
    exchange(&server, &full_report(&canary, "canary-host", 1)).await;
    exchange(&server, &full_report(&rest, "other-host", 1)).await;

    upload(&server, "otelcol", "2.0.0", b"the-new-binary").await;
    assert_eq!(
        set_selector(&server, "otelcol", "2.0.0", &[("rollout", "canary")])
            .await
            .status(),
        200
    );

    // Nobody reports `rollout`, so the aimed Set reaches no one — and the count says so.
    async fn reach(server: &TestServer) -> i64 {
        let list: serde_json::Value = reqwest::Client::new()
            .get(format!("http://{}/api/v1/packages", server.addr))
            .send()
            .await
            .expect("list")
            .json()
            .await
            .expect("json");
        list[0]["targeted_agents"].as_i64().expect("count")
    }
    assert_eq!(reach(&server).await, 0);

    // One call moves the first host into the ring.
    let labelled = reqwest::Client::new()
        .put(format!(
            "http://{}/api/v1/agents/{canary}/labels",
            server.addr
        ))
        .json(&serde_json::json!({ "labels": { "rollout": "canary" } }))
        .send()
        .await
        .expect("put labels");
    assert_eq!(labelled.status(), 200);
    assert_eq!(
        reach(&server).await,
        1,
        "exactly the ring, and nothing else"
    );

    // And the offer follows: the labelled Agent is given the artifact, its neighbour is not.
    let mut report = full_report(&canary, "canary-host", 2);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    let offer = exchange(&server, &report)
        .await
        .packages_available
        .expect("the canary host is offered the package");
    assert!(offer.packages.contains_key("otelcol"));

    let mut report = full_report(&rest, "other-host", 2);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    assert!(
        exchange(&server, &report)
            .await
            .packages_available
            .is_none(),
        "the host outside the ring is offered nothing"
    );
}

/// ADR-0052 through the API, from the operator's side: every saved Set is a draft, publishing an
/// empty one is refused, releasing and retracting are their own requests, and a published Set's
/// entries are immutable while its Selector stays editable.
#[tokio::test]
async fn a_set_is_a_draft_until_published_and_immutable_while_published() {
    let (server, _scratch) = spawn_with_packages().await;
    let uid = InstanceUid::default();
    exchange(&server, &full_report(&uid, "edge-01", 1)).await;

    async fn offered_now(
        server: &TestServer,
        uid: &InstanceUid,
        sequence: u64,
    ) -> Option<opamp::proto::PackagesAvailable> {
        let mut report = full_report(uid, "edge-01", sequence);
        report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
        exchange(server, &report).await.packages_available
    }
    async fn view(server: &TestServer, name: &str) -> serde_json::Value {
        reqwest::Client::new()
            .get(format!("http://{}/api/v1/packages", server.addr))
            .send()
            .await
            .expect("list")
            .json::<serde_json::Value>()
            .await
            .expect("json")
            .as_array()
            .expect("array")
            .iter()
            .find(|p| p["name"] == name)
            .unwrap_or_else(|| panic!("no set {name}"))
            .clone()
    }

    // An empty Set cannot be published: it contains one or more entries by definition.
    create_set(&server, "otelcol", "2.0.0").await;
    let empty = reqwest::Client::new()
        .put(format!(
            "{}/publication",
            set_url(&server, "otelcol", "2.0.0")
        ))
        .json(&serde_json::json!({ "published": true }))
        .send()
        .await
        .expect("publish");
    assert_eq!(empty.status(), 409, "an empty set cannot be published");

    let response = upload_entry(&server, "otelcol", "2.0.0", HOST, b"the-binary").await;
    assert_eq!(response.status(), 200);

    let staged = view(&server, "otelcol").await;
    assert_eq!(staged["published"], false, "saving stages the set");
    assert_eq!(
        staged["targeted_agents"], 1,
        "and its aim can be checked before it starts, which is what staging is for"
    );
    assert!(
        offered_now(&server, &uid, 2).await.is_none(),
        "a draft reaches nobody, however complete it is"
    );

    // The release is its own act, and the fleet has the package on the next exchange.
    publish(&server, "otelcol", "2.0.0", true).await;
    assert_eq!(view(&server, "otelcol").await["published"], true);
    let offer = offered_now(&server, &uid, 3)
        .await
        .expect("the released package");
    assert!(offer.packages.contains_key("otelcol"));

    // While published, the bytes are frozen: writing or deleting an entry answers 409 —
    // the Server's rule, which is exactly what the UI renders as a greyed-out control.
    let frozen = upload_entry(&server, "otelcol", "2.0.0", HOST, b"other-bytes").await;
    assert_eq!(frozen.status(), 409, "published entries are immutable");
    let frozen_delete = reqwest::Client::new()
        .delete(format!(
            "{}/entries/{HOST}",
            set_url(&server, "otelcol", "2.0.0")
        ))
        .send()
        .await
        .expect("delete entry");
    assert_eq!(frozen_delete.status(), 409);
    // The Selector is not bytes, and stays editable.
    assert_eq!(
        set_selector(&server, "otelcol", "2.0.0", &[("os.type", "linux")])
            .await
            .status(),
        200
    );

    // And it is reversible: retracting stops the offer. Nothing here uninstalls anything — an
    // Agent that already took it keeps running it (ADR-0017).
    publish(&server, "otelcol", "2.0.0", false).await;
    assert_eq!(view(&server, "otelcol").await["published"], false);
    assert!(
        offered_now(&server, &uid, 4).await.is_none(),
        "a retracted package is not handed to an Agent that has not taken it"
    );

    // Releasing a Set that does not exist is a 404, not a Set conjured out of a URL.
    let missing = reqwest::Client::new()
        .put(format!(
            "{}/publication",
            set_url(&server, "nosuch", "1.0.0")
        ))
        .json(&serde_json::json!({ "published": true }))
        .send()
        .await
        .expect("put publication");
    assert_eq!(missing.status(), 404);
}

/// A source URL that steers the probe at the cloud metadata endpoint — or another never-legitimate
/// internal address — is refused (SSRF). The URL and its headers are entirely caller-supplied, so
/// without this the Server could be made to read `169.254.169.254` and reflect the answer back.
#[tokio::test]
async fn a_source_url_aimed_at_an_internal_address_is_refused() {
    let (server, _scratch) = spawn_with_packages().await;
    create_set(&server, "otelcol", "1.0.0").await;

    let server_ref = &server;
    let put_source = |url: &str| {
        let url = url.to_string();
        async move {
            reqwest::Client::new()
                .put(format!(
                    "{}/entries/{HOST}/source",
                    set_url(server_ref, "otelcol", "1.0.0")
                ))
                .json(&serde_json::json!({
                    "url": url,
                    "sha256": hex::encode(sha256(b"x")),
                }))
                .send()
                .await
                .expect("put source")
        }
    };

    // The cloud metadata endpoint (link-local), the shared/CGNAT metadata address, and a scheme the
    // probe must never follow.
    for url in [
        "http://169.254.169.254/latest/meta-data/",
        "http://100.100.100.200/latest/meta-data/",
        "file:///etc/passwd",
    ] {
        let response = put_source(url).await;
        assert_eq!(response.status(), 400, "{url} must be refused");
    }
}

/// The store has a whole-store ceiling, so a caller cannot fill the disk by uploading artifact after
/// artifact under distinct names: once the store is at its limit, the next upload is refused.
#[tokio::test]
async fn the_package_store_has_a_total_size_ceiling() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = PackageStore::open(dir.path().join("packages")).expect("store");
    // A ceiling of 10 KiB: the first 8 KiB artifact fits, a second does not.
    let state = Arc::new(
        AppState::new(dir.path().join("fleet-configs"))
            .expect("configs")
            .with_packages(Some(PackageOffering::new(store, String::new())))
            .with_max_total_package_bytes(10 * 1024),
    );
    let app = server::app(state.clone(), server::transport::Admission::open());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    let server = TestServer {
        addr,
        state,
        _dir: dir,
    };

    // The first artifact fits under the ceiling.
    create_set(&server, "one", "1.0.0").await;
    let first = upload_entry(&server, "one", "1.0.0", HOST, &vec![0u8; 8 * 1024]).await;
    assert_eq!(
        first.status(),
        200,
        "the first artifact is within the ceiling"
    );

    // A second distinct name would take the store past the ceiling — refused, and nothing is left
    // staged for it.
    create_set(&server, "two", "1.0.0").await;
    let second = upload_entry(&server, "two", "1.0.0", HOST, &vec![0u8; 8 * 1024]).await;
    assert_eq!(
        second.status(),
        507,
        "the second upload is refused: it would exceed the store ceiling"
    );
}
