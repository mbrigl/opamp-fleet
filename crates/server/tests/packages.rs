//! Package delivery (ADR-0028, reorganised into Sets by ADR-0028): the REST Set + entry routes,
//! and the hash-gated `PackagesAvailable` offer toward capable Agents.

mod support;

use opamp::proto::{
    AgentCapabilities, AgentToServer, PackageStatus, PackageStatusEnum, PackageStatuses,
    ServerCapabilities, ServerToAgent,
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
    );
    let (addr, rest_addr) =
        support::serve(state.clone(), server::transport::Admission::open()).await;
    (
        TestServer {
            addr,
            rest_addr,
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
/// an entry may be stored under for these Agents to be offered it (ADR-0028).
const HOST: &str = "linux/amd64";

    format!(
    )
}

/// The artifact download of one Set — on the **Agent plane** (ADR-0012), which is where the
/// `download_url` in an offer points and the one `/api/v1` route the Operator plane does not serve.
    format!(
    )
}

    let response = reqwest::Client::new()
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("put set");
    assert_eq!(response.status(), 200, "creating the set should succeed");
}

/// `PUT …/entries/{os}/{arch}` — stores one platform's artifact into a Set.
async fn upload_entry(
    server: &TestServer,
    version: &str,
    platform: &str,
    artifact: &[u8],
) -> reqwest::Response {
    reqwest::Client::new()
        .put(format!(
            "{}/entries/{platform}",
        ))
        .body(artifact.to_vec())
        .send()
        .await
        .expect("put entry")
}

    let response = reqwest::Client::new()
        .send()
        .await
        .expect("post rollout");
    assert_eq!(response.status(), 200, "the rollout should succeed");
    response.json().await.expect("json")
}

    let response = reqwest::Client::new()
        .put(format!(
/// Create + upload in one go: the Set is complete — and still reaches nobody until a rollout act
/// names it (ADR-0027).
    assert_eq!(response.status(), 200, "upload should succeed");
}

fn sha256(bytes: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).to_vec()
}

/// ADR-0028's versions under ADR-0027: versions are first-class Sets, and the act names the one
/// the operator releases — no one produces an old artifact again, and no publication state is
/// juggled. An Agent that has reported nothing installed takes either of them; what happens once
/// it *has* reported is ADR-0027's, tested below.
#[tokio::test]
async fn the_act_names_the_version_it_releases() {
    let (server, _scratch) = spawn_with_packages().await;
    let uid = InstanceUid::default();
    let server_ref = &server;
    let offered = |sequence: u64| async move {
        let mut report = full_report(&uid, "collector", sequence);
        report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
        exchange(server_ref, &report).await.packages_available
    };

    // The Agent is known first — a rollout act assigns to the fleet as it is.
    assert!(offered(1).await.is_none());

    // Both versions are saved; the act names the one the operator releases.
    assert_eq!(
        "0.157.0"
    );

    // The same act, pointed at the older version. This Agent reports no package statuses, so it
    // has nothing installed to be held against (ADR-0027) and the older Set still reaches it —
    // and its artifact is still here.
    let fallback = offered(3).await.expect("the fallback offer");
    let served = reqwest::Client::new()
        .get(format!(
            "{}?os=linux&arch=amd64",
        ))
        .send()
        .await
        .expect("download")
        .bytes()
        .await
        .expect("bytes");
    assert_eq!(served.as_ref(), b"old-binary");
}

/// ADR-0012: the offered `download_url` is a path the Client resolves against **its own OpAMP
/// endpoint**, so the artifact has to be served by the listener the Agents already talk to — not by
/// the Operator plane, which is where authentication is going and where no Agent will ever look.
#[tokio::test]
async fn the_artifact_is_served_where_the_agents_are_and_not_on_the_operator_plane() {
    let (server, _scratch) = spawn_with_packages().await;
    let path = format!(
        support::AGENT_TYPE
    );

    let served = reqwest::Client::new()
        .get(format!("http://{}{path}", server.addr))
        .send()
        .await
        .expect("download");
    assert_eq!(served.status(), 200);
    assert_eq!(
        served.bytes().await.expect("bytes").as_ref(),
        b"the-new-binary"
    );

    let elsewhere = reqwest::Client::new()
        .get(format!("http://{}{path}", server.rest_addr))
        .send()
        .await
        .expect("request");
    assert_eq!(
        elsewhere.status(),
        404,
        "one resource, one address: the Operator plane does not serve artifacts"
    );
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

    assert_eq!(
        1,
        "the act assigns the one known Agent"
    );

    // Now the offer arrives, declares the capability, and carries a working download URL.
    let reply = exchange(&server, &report).await;
    assert_ne!(
        reply.capabilities & ServerCapabilities::OffersPackages as u64,
        0
    );
    let offer = reply.packages_available.expect("an offer");
    assert!(!offer.all_packages_hash.is_empty());
    assert_eq!(available.version, "1.2.3");
    let file = available.file.as_ref().expect("a downloadable file");
    // download_base was empty, so the URL is a path the Client resolves against its endpoint —
    // and it names the whole identity, so two versions never serve each other's bytes.
    assert_eq!(
        file.download_url,
        format!(
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
            PackageStatus {
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
    let uid = InstanceUid::default();
    // full_report declares no AcceptsPackages.
    exchange(&server, &full_report(&uid, "incapable", 1)).await;
    let reply = exchange(&server, &full_report(&uid, "incapable", 2)).await;
    assert!(
        reply.packages_available.is_none(),
        "capability negotiation is binding, whatever is assigned"
    );
}

/// An entry belongs to a Set: uploading toward an identity nobody created is a 404, not a package
/// conjured out of a URL (ADR-0028 — the identity is stated at creation).
#[tokio::test]
async fn an_entry_needs_its_set_first() {
    let (server, _scratch) = spawn_with_packages().await;
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

    let downloaded = reqwest::Client::new()
        .get(format!(
            "{}?os=linux&arch=amd64",
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
            .with_max_package_size(4096),
    );
    let (addr, rest_addr) =
        support::serve(state.clone(), server::transport::Admission::open()).await;
    let server = TestServer {
        addr,
        rest_addr,
        state,
        _dir: dir,
    };

    assert_eq!(response.status(), 413);
}

#[tokio::test]
async fn a_selector_aims_a_rollout_at_part_of_the_fleet() {
    let (server, _scratch) = spawn_with_packages().await;

    // full_report describes an Agent with os.type = linux (see the test scaffolding).
    let targeted = InstanceUid::default();
    let mut report = full_report(&targeted, "collector", 1);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    exchange(&server, &report).await;

    // A second Agent that reports another platform.
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
    exchange(&server, &elsewhere).await;

    assert_eq!(
        1,
    );

    let mut report = full_report(&targeted, "collector", 2);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    let offer = exchange(&server, &report)
        .await
        .packages_available
        .expect("the matching Agent is offered it");
    assert!(!offer.all_packages_hash.is_empty());

    // The Agent outside the aim is offered nothing at all — not an empty offer, no offer: it
    // keeps running what it runs (goal 9, applied to software).
    let mut elsewhere2 = full_report(&other, "windows-box", 2);
    elsewhere2.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    let reply = exchange(&server, &elsewhere2).await;
    assert!(
        reply.packages_available.is_none(),
    );
}

/// The aggregate hash gates re-offering, and it is per Agent: computed over the whole store it
/// would never match what a targeted Agent was actually sent, and the Server would re-offer for ever.
#[tokio::test]
async fn the_aggregate_hash_an_agent_echoes_is_the_one_it_was_offered() {
    let (server, _scratch) = spawn_with_packages().await;
    let uid = InstanceUid::default();
    let mut report = full_report(&uid, "collector", 1);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64
        | AgentCapabilities::ReportsPackageStatuses as u64;
    exchange(&server, &report).await;

    assert_eq!(
        0,
    );

    let mut report = full_report(&uid, "collector", 2);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64
        | AgentCapabilities::ReportsPackageStatuses as u64;
    let offer = exchange(&server, &report)
        .await
        .packages_available
        .expect("an offer");
    assert_eq!(
        offer.packages.len(),
        1,
    );

    // Echoing exactly that aggregate settles it — the Server must not keep re-offering.
    let mut installed = full_report(&uid, "collector", 3);
    installed.capabilities |= AgentCapabilities::AcceptsPackages as u64
        | AgentCapabilities::ReportsPackageStatuses as u64;
    installed.package_statuses = Some(PackageStatuses {
        packages: [(
            PackageStatus {
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

#[tokio::test]
async fn a_canary_ring_is_a_selector_aim_and_two_acts() {
    let (server, _scratch) = spawn_with_packages().await;
    let canary = InstanceUid::default();
    let ordinary = InstanceUid::default();
    for (uid, name) in [(&canary, "canary-host"), (&ordinary, "ordinary-host")] {
        let mut report = full_report(uid, name, 1);
        report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
        exchange(&server, &report).await;
    }


    async fn version_offered_to(
        server: &TestServer,
        uid: &InstanceUid,
        name: &str,
        sequence: u64,
    ) -> String {
        let mut report = full_report(uid, name, sequence);
        report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
        let offer = exchange(server, &report)
            .await
            .packages_available
            .expect("an offer");
    }

    assert_eq!(
        version_offered_to(&server, &canary, "canary-host", 2).await,
        "3.0.0",
        "the named host gets the canary version"
    );
    assert_eq!(
        version_offered_to(&server, &ordinary, "ordinary-host", 2).await,
        "2.0.0",
        "everyone else keeps the fleet-wide version"
    );

    assert_eq!(
        version_offered_to(&server, &ordinary, "ordinary-host", 3).await,
        "3.0.0"
    );
}

#[tokio::test]
    let (server, _scratch) = spawn_with_packages().await;

    let uid = InstanceUid::default();
    let mut report = full_report(&uid, "collector", 1);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    let reply = exchange(&server, &report).await;
    assert!(
        reply.packages_available.is_none(),
    );

    let view = &server.state.snapshot()[0];
    let conflict = view
        .package_conflict
        .as_ref()
        .expect("the fleet view says why");
    assert!(
    );
}

/// ADR-0028: an entry may live somewhere else. The Server stores the reference, offers that
/// address verbatim with the operator's checksum and headers, and has nothing of its own to serve.
#[tokio::test]
async fn a_referenced_entry_is_offered_from_its_source_and_not_from_here() {
    let (server, _scratch) = spawn_with_packages().await;
    let uid = InstanceUid::default();
    let mut hello = full_report(&uid, "collector", 1);
    hello.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    exchange(&server, &hello).await;

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
            "{}/entries/{HOST}/source",
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
    assert_eq!(
        1
    );

    // The offer names the source, carries the operator's hash, and passes the headers on.
    let mut report = full_report(&uid, "collector", 2);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    let offer = exchange(&server, &report)
        .await
        .packages_available
        .expect("an offer");
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

    let server_ref = &server;
    let put_source = |url: String| async move {
        reqwest::Client::new()
            .put(format!(
                "{}/entries/{HOST}/source",
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

/// ADR-0028 in place of ADR-0028's late typing: the Agent type is identity, stated at creation —
/// there is no untyped state — and a Set built for another type fits nobody here: its rollout
/// act assigns no one, whatever its Selector says.
#[tokio::test]
async fn a_set_reaches_only_agents_of_its_type() {
    let (server, _scratch) = spawn_with_packages().await;
    let uid = InstanceUid::default();
    let mut report = full_report(&uid, "edge-01", 1);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    exchange(&server, &report).await;

    // A Set for a different kind of Agent, complete — and its act assigns nobody.
    let response = reqwest::Client::new()
        .put(format!(
            server.rest_addr
        ))
        .json(&serde_json::json!({}))
        .send()
        .await
        .expect("put set");
    assert_eq!(response.status(), 200);
    let response = reqwest::Client::new()
        .put(format!(
            server.rest_addr
        ))
        .body(b"the-binary".to_vec())
        .send()
        .await
        .expect("put entry");
    assert_eq!(response.status(), 200);
    let response = reqwest::Client::new()
        ))
        .send()
        .await
    assert_eq!(response.status(), 200);
    assert_eq!(
        outcome["assigned_agents"], 0,
    );

    let mut report = full_report(&uid, "edge-01", 2);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    assert!(
        exchange(&server, &report)
            .await
            .packages_available
            .is_none(),
        "nothing was assigned, so nothing is offered"
    );

    // The same artifact under this fleet's type reaches it.
    let mut report = full_report(&uid, "edge-01", 3);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    let offer = exchange(&server, &report)
        .await
        .packages_available
        .expect("an offer");
}

/// ADR-0027 end to end: a Set reaches an Agent only as an **upgrade**. What the Agent reports
/// installed is the fourth matching test, so the count, the per-Agent act and the bulk act all
/// refuse to move a host backwards — or to move it nowhere at all. The assignment path is
/// deliberately exempt: an installed package stays in the Agent's offer, or the Agent would be
/// told the package is no longer wanted.
#[tokio::test]
async fn a_set_reaches_an_agent_only_as_an_upgrade() {
    let (server, _scratch) = spawn_with_packages().await;
    let uid = InstanceUid::default();

    /// A report that says "I run this version of this package".
    fn running(uid: &InstanceUid, sequence: u64, version: &str) -> AgentToServer {
        let mut report = full_report(uid, "collector", sequence);
        report.capabilities |= AgentCapabilities::AcceptsPackages as u64
            | AgentCapabilities::ReportsPackageStatuses as u64;
        report.package_statuses = Some(PackageStatuses {
            packages: [(
                PackageStatus {
                    agent_has_version: version.to_string(),
                    status: PackageStatusEnum::Installed as i32,
                    ..Default::default()
                },
            )]
            .into(),
            // Empty: this Agent is never in sync with an offer, so the hash gate never silences
            // one and every exchange shows what it would be offered.
            server_provided_all_packages_hash: Vec::new(),
            error_message: String::new(),
        });
        report
    }

    /// The two counts the Set view carries (ADR-0027 point 19): whom it aims at, and whom it
    /// would actually reach.
    async fn counts(server: &TestServer, version: &str) -> (i64, i64) {
        let list: serde_json::Value = reqwest::Client::new()
            .send()
            .await
            .expect("list")
            .json()
            .await
            .expect("json");
        let row = list
            .as_array()
            .expect("array")
            .iter()
            .clone();
        (
            row["targeted_agents"].as_i64().expect("targeted_agents"),
        )
    }

    exchange(&server, &running(&uid, 1, "1.0.0")).await;

    assert_eq!(counts(&server, "1.0.0").await, (1, 0));
    assert_eq!(
        0,
        "the bulk act skips an Agent it would not move"
    );

    // The per-Agent act says so rather than doing nothing quietly.
    let refused = reqwest::Client::new()
        .post(format!(
            "http://{}/api/v1/agents/{uid}/rollout",
            server.rest_addr
        ))
        .send()
        .await
        .expect("rollout to agent");
    assert_eq!(refused.status(), 409);
    let body: serde_json::Value = refused.json().await.expect("json");
    assert!(
        body["error"]
            .as_str()
            .expect("error")
            .contains("not an upgrade"),
        "{body}"
    );

    assert_eq!(counts(&server, "2.0.0").await, (1, 1));
    let offer = exchange(&server, &running(&uid, 2, "1.0.0"))
        .await
        .packages_available
        .expect("an offer");

    // And once the Agent reports it installed, the assignment keeps composing the offer — the
    // Set the Agent runs must not vanish from its desired state (ADR-0027 point 17).
    let offer = exchange(&server, &running(&uid, 3, "2.0.0"))
        .await
        .packages_available
        .expect("the assignment still composes an offer");

    assert_eq!(counts(&server, "2.0.0").await, (1, 0));
    assert_eq!(counts(&server, "1.0.0").await, (1, 0));
}

/// The silent no-op ADR-0028 named: a Set can target nobody through a mistyped Agent type, a
/// platform the fleet does not run, or a Selector that matches no one — and none of the three is
/// a rejected upload, so without a count nothing says it.
#[tokio::test]
async fn a_set_says_how_many_agents_it_reaches() {
    let (server, _scratch) = spawn_with_packages().await;
    let uid = InstanceUid::default();
    exchange(&server, &support::full_report(&uid, "one", 1)).await;

    async fn list(server: &TestServer) -> serde_json::Value {
        reqwest::Client::new()
            .send()
            .await
            .json()
            .await
            .expect("json")
    }

        list(server)
            .await
            .as_array()
            .expect("array")
            .iter()
            .as_i64()
            .expect("targeted_agents")
    }


    // A second Agent on the same platform doubles it.
    exchange(
        &server,
        &support::full_report(&InstanceUid::default(), "two", 1),
    )
    .await;

    // A Selector that matches nobody: still stored, still valid, reaching no one — the case that
    // was invisible before.

    assert_eq!(response.status(), 200);
}

/// ADR-0026 reaches packages, not just Configurations — which is the case it exists for. A binary
/// access to that host.
#[tokio::test]
async fn a_label_aims_a_set_at_part_of_the_fleet() {
    let (server, _scratch) = spawn_with_packages().await;
    let canary = InstanceUid::default();
    let rest = InstanceUid::default();
    exchange(&server, &full_report(&canary, "canary-host", 1)).await;
    exchange(&server, &full_report(&rest, "other-host", 1)).await;


    async fn reach(server: &TestServer) -> i64 {
        let list: serde_json::Value = reqwest::Client::new()
            .send()
            .await
            .expect("list")
            .json()
            .await
            .expect("json");
        list[0]["targeted_agents"].as_i64().expect("count")
    }
    assert_eq!(reach(&server).await, 0);

    let labelled = reqwest::Client::new()
        .put(format!(
            "http://{}/api/v1/agents/{canary}/labels",
            server.rest_addr
        ))
        .json(&serde_json::json!({ "labels": { "rollout": "canary" } }))
        .send()
        .await
        .expect("put labels");
    assert_eq!(labelled.status(), 200);
    assert_eq!(
        reach(&server).await,
        1,
    );

    let mut report = full_report(&canary, "canary-host", 2);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    let offer = exchange(&server, &report)
        .await
        .packages_available
        .expect("the canary host is offered the package");

    let mut report = full_report(&rest, "other-host", 2);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    assert!(
        exchange(&server, &report)
            .await
            .packages_available
            .is_none(),
    );
}

/// ADR-0027 through the API, from the operator's side: a saved Set waits, rolling out an empty
/// one is refused, the act is its own request — and an assigned Set's entries are immutable
/// while its Selector stays editable.
#[tokio::test]
async fn a_set_waits_until_rolled_out_and_is_immutable_while_assigned() {
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
        reqwest::Client::new()
            .get(format!("http://{}/api/v1/packages", server.rest_addr))
            .send()
            .await
            .expect("list")
            .json::<serde_json::Value>()
            .await
            .expect("json")
            .as_array()
            .expect("array")
            .iter()
            .clone()
    }

        .send()
        .await
        .expect("rollout");

    assert_eq!(response.status(), 200);

    assert!(
        staged.get("published").is_none(),
        "ADR-0027: there is no publication state to show: {staged}"
    );
    assert_eq!(
    );
    assert!(
        offered_now(&server, &uid, 2).await.is_none(),
        "a saved Set reaches nobody, however complete it is"
    );

    // Its entries are still editable: nothing is assigned yet.
    assert_eq!(editable.status(), 200, "an unassigned set is editable");

    // The act is its own request, and the fleet has the package on the next exchange.
    let offer = offered_now(&server, &uid, 3)
        .await
        .expect("the released package");

    // While assigned, the bytes are frozen: writing or deleting an entry answers 409 —
    // the Server's rule, which is exactly what the UI renders as a greyed-out control.
    assert_eq!(frozen.status(), 409, "assigned entries are immutable");
    let frozen_delete = reqwest::Client::new()
        .delete(format!(
            "{}/entries/{HOST}",
        ))
        .send()
        .await
        .expect("delete entry");
    assert_eq!(frozen_delete.status(), 409);
    // The Selector is not bytes, and stays editable.

    // Deleting the Set removes its assignments with it: the offer is withdrawn, and nothing is
    // uninstalled — an Agent that already took it keeps running it (ADR-0028).
    let deleted = reqwest::Client::new()
        .send()
        .await
        .expect("delete set");
    assert_eq!(deleted.status(), 204);
    assert!(
        offered_now(&server, &uid, 4).await.is_none(),
        "a deleted set is not handed to an Agent that has not taken it"
    );

    // Rolling out a Set that does not exist is a 404, not a Set conjured out of a URL.
    let missing = reqwest::Client::new()
        .send()
        .await
        .expect("rollout");
    assert_eq!(missing.status(), 404);
}

/// A source URL that steers the probe at the cloud metadata endpoint — or another never-legitimate
/// internal address — is refused (SSRF). The URL and its headers are entirely caller-supplied, so
/// without this the Server could be made to read `169.254.169.254` and reflect the answer back.
#[tokio::test]
async fn a_source_url_aimed_at_an_internal_address_is_refused() {
    let (server, _scratch) = spawn_with_packages().await;

    let server_ref = &server;
    let put_source = |url: &str| {
        let url = url.to_string();
        async move {
            reqwest::Client::new()
                .put(format!(
                    "{}/entries/{HOST}/source",
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
            .with_max_total_package_bytes(10 * 1024),
    );
    let (addr, rest_addr) =
        support::serve(state.clone(), server::transport::Admission::open()).await;
    let server = TestServer {
        addr,
        rest_addr,
        state,
        _dir: dir,
    };

    // The first artifact fits under the ceiling.
    assert_eq!(
        first.status(),
        200,
        "the first artifact is within the ceiling"
    );

    assert_eq!(
        second.status(),
        507,
        "the second upload is refused: it would exceed the store ceiling"
    );
}
    server: &TestServer,
    name: &str,
    pairs: &[(&str, &str)],
) -> reqwest::Response {
    let selector: std::collections::BTreeMap<&str, &str> = pairs.iter().copied().collect();
    reqwest::Client::new()
        .json(&serde_json::json!({ "selector": selector }))
        .send()
        .await
    let (server, _scratch) = spawn_with_packages().await;
    let refused = reqwest::Client::new()
    assert_eq!(refused.status(), 400);
    let body: serde_json::Value = refused.json().await.expect("json");
    assert!(
    let (server, _scratch) = spawn_with_packages().await;
    let (server, _scratch) = spawn_with_packages().await;
    let (server, _scratch) = spawn_with_packages().await;
    let missing = reqwest::Client::new()
    assert_eq!(missing.status(), 404);
    let deleted = reqwest::Client::new()
    assert_eq!(deleted.status(), 204);
    let (server, _scratch) = spawn_with_packages().await;
        let mut report = full_report(uid, name, 1);
        report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
        exchange(&server, &report).await;
    }
        report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
            .await
            .packages_available
            .expect("an offer");
    let (server, _scratch) = spawn_with_packages().await;
    let refused = reqwest::Client::new()
        ))
        .body(b"the-binary".to_vec())
        .send()
        .await
        .expect("put entry");
    assert_eq!(refused.status(), 400);
    let body: serde_json::Value = refused.json().await.expect("json");
    assert!(
        body["error"]
            .as_str()
            .expect("error")
    let (server, _scratch) = spawn_with_packages().await;
    let uid = InstanceUid::default();
    let mut report = full_report(&uid, "edge-01", 1);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    exchange(&server, &report).await;

        server: &TestServer,
        uid: &InstanceUid,
        sequence: u64,
    ) -> Option<opamp::proto::PackagesAvailable> {
        let mut report = full_report(uid, "edge-01", sequence);
        report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
        exchange(server, &report).await.packages_available
    }
        .await
        .expect("the released package");
    let view = &server.state.snapshot()[0];
        .expect("the assignment still composes an offer");
    let (server, _scratch) = spawn_with_packages().await;
    let uid = InstanceUid::default();
    let mut report = full_report(&uid, "edge-01", 1);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    exchange(&server, &report).await;

        .post(format!(
            "http://{}/api/v1/agents/{uid}/rollout",
            server.rest_addr
        ))
        .send()
        .await
        .expect("rollout to agent");
    let (server, _scratch) = spawn_with_packages().await;
    let uid = InstanceUid::default();
    let mut report = full_report(&uid, "edge-01", 1);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    exchange(&server, &report).await;

    let mut report = full_report(&uid, "edge-01", 2);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    let offer = exchange(&server, &report)
        .await
        .packages_available
        .expect("an offer");
    assert_eq!(
    let (server, _scratch) = spawn_with_packages().await;
    let uid = InstanceUid::default();
    let mut report = full_report(&uid, "edge-01", 1);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    exchange(&server, &report).await;
    let mut report = full_report(&uid, "edge-01", 2);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    let offer = exchange(&server, &report)
        .await
        .packages_available
        .expect("an offer");
    let (server, _scratch) = spawn_with_packages().await;
    let uid = InstanceUid::default();
    let mut report = full_report(&uid, "edge-01", 1);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    exchange(&server, &report).await;

    let view = &server.state.snapshot()[0];
    assert_eq!(response.status(), 200);
    let response = reqwest::Client::new()
        .put(format!(
    assert_eq!(response.status(), 200);

    let view = &server.state.snapshot()[0];
    let response = reqwest::Client::new()
        .put(format!(
    assert_eq!(response.status(), 200);
    let view = &server.state.snapshot()[0];
