//! Package delivery (ADR-0015): the REST upload + download endpoints, and the hash-gated
//! `PackagesAvailable` offer toward capable Agents.

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
/// an artifact may be stored under for these Agents to be offered it (ADR-0031).
const HOST: &str = "os=linux&arch=amd64";

async fn upload(server: &TestServer, name: &str, version: &str, artifact: &[u8]) {
    upload_for(server, name, HOST, version, artifact).await;
}

async fn upload_for(
    server: &TestServer,
    name: &str,
    platform: &str,
    version: &str,
    artifact: &[u8],
) {
    let response = reqwest::Client::new()
        .put(format!(
            "http://{}/api/v1/packages/{name}?version={version}&{platform}",
            server.addr
        ))
        .body(artifact.to_vec())
        .send()
        .await
        .expect("put package");
    assert_eq!(response.status(), 200, "upload should succeed");
    // An uploaded artifact is inert until it says which kind of Agent it is for (ADR-0034), so the
    // helper arms it for the type the scaffolded fleet reports. Tests about the fit itself set it
    // themselves, or deliberately leave it unset.
    let response = reqwest::Client::new()
        .put(format!(
            "http://{}/api/v1/packages/{name}/type",
            server.addr
        ))
        .json(&serde_json::json!({ "service_name": support::AGENT_TYPE }))
        .send()
        .await
        .expect("put agent type");
    assert_eq!(
        response.status(),
        200,
        "setting the agent type should succeed"
    );
}

/// ADR-0019 through the API: the rollback is one action that says what it will do, it refuses
/// rather than silently doing nothing when there is nothing to go back to, and the restored
/// artifact is what the fleet is then offered.
#[tokio::test]
async fn a_package_rolls_back_to_the_version_it_replaced() {
    let (server, _scratch) = spawn_with_packages().await;
    let client = reqwest::Client::new();
    let rollback = || {
        client
            .post(format!(
                "http://{}/api/v1/packages/otelcol/rollback?{HOST}",
                server.addr
            ))
            .send()
    };

    upload(&server, "otelcol", "0.156.0", b"old-binary").await;
    assert_eq!(
        rollback().await.expect("rollback").status(),
        409,
        "a package at its first upload has nothing to go back to"
    );

    upload(&server, "otelcol", "0.157.0", b"new-binary").await;
    let listed: serde_json::Value = client
        .get(format!("http://{}/api/v1/packages", server.addr))
        .send()
        .await
        .expect("list")
        .json()
        .await
        .expect("json");
    // A version belongs to one platform's artifact, not to the package: the name has as many
    // versions as it has variants (ADR-0031).
    assert_eq!(listed[0]["variants"][0]["os"], "linux");
    assert_eq!(listed[0]["variants"][0]["arch"], "amd64");
    assert_eq!(listed[0]["variants"][0]["version"], "0.157.0");
    assert_eq!(
        listed[0]["variants"][0]["previous_version"], "0.156.0",
        "the list says what a rollback would put back"
    );

    let response = rollback().await.expect("rollback");
    assert_eq!(response.status(), 200);
    let rolled: serde_json::Value = response.json().await.expect("json");
    assert_eq!(rolled["variants"][0]["version"], "0.156.0");
    assert_eq!(rolled["variants"][0]["previous_version"], "0.157.0");

    // The restored artifact is what is served — so it is what the fleet installs.
    let served = client
        .get(format!(
            "http://{}/api/v1/packages/otelcol/file?{HOST}",
            server.addr
        ))
        .send()
        .await
        .expect("download")
        .bytes()
        .await
        .expect("bytes");
    assert_eq!(served.as_ref(), b"old-binary");

    assert_eq!(
        client
            .post(format!(
                "http://{}/api/v1/packages/nothing-here/rollback?{HOST}",
                server.addr
            ))
            .send()
            .await
            .expect("rollback")
            .status(),
        404
    );
}

#[tokio::test]
async fn an_uploaded_package_is_offered_downloaded_and_gated() {
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
    // download_base was empty, so the URL is a path the Client resolves against its endpoint.
    assert_eq!(
        file.download_url, "/api/v1/packages/otelcol/file?os=linux&arch=amd64",
        "the download names the platform, because the name alone no longer names one file"
    );
    assert_eq!(file.content_hash, sha256(b"the-new-binary"));

    // The artifact downloads byte-for-byte.
    let downloaded = reqwest::Client::new()
        .get(format!(
            "http://{}/api/v1/packages/otelcol/file?{HOST}",
            server.addr
        ))
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

/// A package is a *program*: an `otelcol-contrib` binary weighs hundreds of megabytes, so the
/// upload route must not be bounded by the framework's 2 MiB default, and the artifact must reach
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
            "http://{}/api/v1/packages/otelcol/file?{HOST}",
            server.addr
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
    assert_eq!(sha256(&artifact).len(), 32);
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
    let app = server::app(state, server::transport::Admission::open());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    let response = reqwest::Client::new()
        .put(format!(
            "http://{addr}/api/v1/packages/otelcol?version=1.0.0&{HOST}"
        ))
        .body(vec![0u8; 8192])
        .send()
        .await
        .expect("put package");
    assert_eq!(response.status(), 413);
}

fn sha256(bytes: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).to_vec()
}

/// A helper mirroring how an operator aims a rollout: name the package, then set its Selector.
async fn set_selector(
    server: &TestServer,
    name: &str,
    pairs: &[(&str, &str)],
) -> reqwest::Response {
    let selector: std::collections::BTreeMap<&str, &str> = pairs.iter().copied().collect();
    reqwest::Client::new()
        .put(format!(
            "http://{}/api/v1/packages/{name}/selector",
            server.addr
        ))
        .json(&serde_json::json!({ "selector": selector }))
        .send()
        .await
        .expect("put selector")
}

/// The point of ADR-0017: an artifact reaches the Agents its Selector matches and no others, so a
/// binary rollout can be tried on part of the fleet first.
#[tokio::test]
async fn a_selector_aims_a_package_at_part_of_the_fleet() {
    let (server, _scratch) = spawn_with_packages().await;
    upload(&server, "otelcol", "2.0.0", b"the-new-binary").await;
    assert_eq!(
        set_selector(&server, "otelcol", &[("os.type", "linux")])
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
        set_selector(&server, "otelcol", &[("os.type", "linux")])
            .await
            .status(),
        200
    );
    assert_eq!(
        set_selector(&server, "otelcol-win", &[("os.type", "windows")])
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
        "only the package this Agent's Selector matches"
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

/// The canary shape an operator actually wants: a fleet-wide package, plus a narrower one for the
/// hosts a rollout starts on. The more specific Selector wins for those hosts, and the fleet-wide
/// one keeps serving everyone else.
#[tokio::test]
async fn a_narrower_selector_overrides_the_fleet_wide_package_for_the_agents_it_names() {
    let (server, _scratch) = spawn_with_packages().await;
    upload(&server, "otelcol", "2.0.0", b"the-fleet-binary").await;
    upload(&server, "otelcol-canary", "3.0.0", b"the-canary-binary").await;
    // otelcol keeps the empty Selector: everyone. The canary names one attribute.
    assert_eq!(
        set_selector(
            &server,
            "otelcol-canary",
            &[("service.instance.name", "canary-host")]
        )
        .await
        .status(),
        200
    );

    async fn offered_to(server: &TestServer, name: &str) -> Vec<String> {
        let uid = InstanceUid::default();
        let mut report = full_report(&uid, name, 1);
        report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
        let offer = exchange(server, &report)
            .await
            .packages_available
            .expect("an offer");
        let mut names: Vec<String> = offer.packages.keys().cloned().collect();
        names.sort();
        names
    }

    assert_eq!(
        offered_to(&server, "canary-host").await,
        ["otelcol-canary"],
        "the named host gets the canary, and only it"
    );
    assert_eq!(
        offered_to(&server, "ordinary-host").await,
        ["otelcol"],
        "everyone else keeps the fleet-wide package"
    );
}

/// Two equally specific Selectors reaching one Agent is the one case with no defensible answer:
/// the Server offers neither and says so in the fleet view, rather than picking at random.
#[tokio::test]
async fn equally_specific_selectors_offer_nothing_and_are_reported() {
    let (server, _scratch) = spawn_with_packages().await;
    upload(&server, "otelcol", "2.0.0", b"one").await;
    upload(&server, "otelcol-next", "3.0.0", b"two").await;
    // Both name exactly one attribute, and both match the Agent below.
    assert_eq!(
        set_selector(&server, "otelcol", &[("os.type", "linux")])
            .await
            .status(),
        200
    );
    assert_eq!(
        set_selector(
            &server,
            "otelcol-next",
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

/// ADR-0018: a package may live somewhere else. The Server stores the reference, offers that
/// address verbatim with the operator's checksum and headers, and has nothing of its own to serve.
#[tokio::test]
async fn a_referenced_package_is_offered_from_its_source_and_not_from_here() {
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
    let response = reqwest::Client::new()
        .put(format!(
            "http://{}/api/v1/packages/otelcol/source",
            server.addr
        ))
        .json(&serde_json::json!({
            "url": url,
            "sha256": hex::encode(&digest),
            "version": "0.157.0",
            "os": "linux",
            "arch": "amd64",
            "headers": { "Authorization": "Bearer release-token" }
        }))
        .send()
        .await
        .expect("put source");
    assert_eq!(response.status(), 200);
    // A referenced package is armed the same way an uploaded one is (ADR-0034) — the type belongs
    // to the name, and where the bytes live has nothing to do with which Agents they are for.
    let response = reqwest::Client::new()
        .put(format!(
            "http://{}/api/v1/packages/otelcol/type",
            server.addr
        ))
        .json(&serde_json::json!({ "service_name": support::AGENT_TYPE }))
        .send()
        .await
        .expect("put agent type");
    assert_eq!(response.status(), 200);

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
            "http://{}/api/v1/packages/otelcol/file?{HOST}",
            server.addr
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

    let put_source = |url: String| async move {
        reqwest::Client::new()
            .put(format!(
                "http://{}/api/v1/packages/otelcol/source",
                server.addr
            ))
            .json(&serde_json::json!({
                "url": url,
                "sha256": hex::encode(sha256(b"x")),
                "version": "1.0.0",
                "os": "linux",
                "arch": "amd64"
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

/// ADR-0034 through the API: an uploaded artifact reaches nobody until it says which kind of Agent
/// it is for, and then it reaches only that kind — whatever its Selector does or does not say.
#[tokio::test]
async fn a_package_is_inert_until_it_names_its_agent_type_and_then_fits_only_that_type() {
    let (server, _scratch) = spawn_with_packages().await;

    // Uploaded and nothing else. No Selector, which before ADR-0034 meant the whole fleet.
    let response = reqwest::Client::new()
        .put(format!(
            "http://{}/api/v1/packages/otelcol?version=2.0.0&{HOST}",
            server.addr
        ))
        .body(b"the-binary".to_vec())
        .send()
        .await
        .expect("put package");
    assert_eq!(response.status(), 200);

    let offered_now = || async {
        let uid = InstanceUid::default();
        let mut report = full_report(&uid, "edge-01", 1);
        report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
        exchange(&server, &report).await.packages_available
    };
    assert!(
        offered_now().await.is_none(),
        "an untyped package is inert, not fleet-wide"
    );

    let set_type = |service_name: &'static str| async move {
        reqwest::Client::new()
            .put(format!(
                "http://{}/api/v1/packages/otelcol/type",
                server.addr
            ))
            .json(&serde_json::json!({ "service_name": service_name }))
            .send()
            .await
            .expect("put agent type")
    };

    // An empty type is refused where it is written, rather than silently disarming the package.
    assert_eq!(set_type("").await.status(), 400);

    // Typed for something else: still nothing, and the Selector never got a say.
    assert_eq!(set_type("promtail").await.status(), 200);
    assert!(
        offered_now().await.is_none(),
        "a package built for another type is not a candidate"
    );

    // Typed for what this fleet reports: offered, and the view says so.
    let armed = set_type(support::AGENT_TYPE).await;
    assert_eq!(armed.status(), 200);
    let view: serde_json::Value = armed.json().await.expect("json");
    assert_eq!(view["service_name"], support::AGENT_TYPE);
    let offer = offered_now().await.expect("an offer");
    assert!(offer.packages.contains_key("otelcol"));
}

/// The silent no-op ADR-0034 named: a package can target nobody through a mistyped Agent type, a
/// platform the fleet does not run, or a Selector that matches no one — and none of the three is an
/// upload error, so without a count nothing says it. The number is the Server's own resolution, so
/// it cannot claim a reach the fleet does not get.
#[tokio::test]
async fn a_package_says_how_many_agents_it_reaches() {
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
            .unwrap_or_else(|| panic!("no package {name} in the list"))["targeted_agents"]
            .as_i64()
            .expect("targeted_agents")
    }

    // Uploaded and armed for the type this fleet reports: it reaches the one Agent there is.
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
        set_selector(&server, "otelcol", &[("env", "prod")])
            .await
            .status(),
        200
    );
    assert_eq!(reach(&server, "otelcol").await, 0);

    // An artifact for a platform this fleet does not run reaches nobody either.
    upload_for(
        &server,
        "fluentbit",
        "os=windows&arch=amd64",
        "2.0.0",
        b"exe",
    )
    .await;
    assert_eq!(reach(&server, "fluentbit").await, 0);

    // Both packages are stored and both reach nobody — which is exactly the state an operator
    // needs shown, because nothing about the store itself looks wrong.
    let all = list(&server).await;
    let counts: Vec<(&str, i64)> = all
        .as_array()
        .expect("array")
        .iter()
        .map(|p| {
            (
                p["name"].as_str().expect("name"),
                p["targeted_agents"].as_i64().expect("count"),
            )
        })
        .collect();
    assert_eq!(counts, [("fluentbit", 0), ("otelcol", 0)]);
}

/// ADR-0042 reaches packages, not just Configurations — which is the case it exists for. A binary
/// rollout starts on the hosts an operator moved into the canary ring, and moving one in needs no
/// access to that host.
#[tokio::test]
async fn a_label_aims_a_package_at_part_of_the_fleet() {
    let (server, _scratch) = spawn_with_packages().await;
    let canary = InstanceUid::default();
    let rest = InstanceUid::default();
    exchange(&server, &full_report(&canary, "canary-host", 1)).await;
    exchange(&server, &full_report(&rest, "other-host", 1)).await;

    upload(&server, "otelcol", "2.0.0", b"the-new-binary").await;
    assert_eq!(
        set_selector(&server, "otelcol", &[("rollout", "canary")])
            .await
            .status(),
        200
    );

    // Nobody reports `rollout`, so the aimed package reaches no one — and the count says so.
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
