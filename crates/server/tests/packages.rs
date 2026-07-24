//! Package delivery (ADR-0018, reorganised into Sets by ADR-0019): the REST Set + entry routes,
//! and the hash-gated `PackagesAvailable` offer toward capable Agents.

mod support;

use opamp::proto::{
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
/// an entry may be stored under for these Agents to be offered it (ADR-0019).
const HOST: &str = "linux/amd64";

    format!(
    )
}

/// The artifact download of one Set — on the **Agent plane** (ADR-0023), which is where the
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
    let response = reqwest::Client::new()
}

    let response = reqwest::Client::new()
        .put(format!(
    assert_eq!(response.status(), 200, "upload should succeed");
}

fn sha256(bytes: &[u8]) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).to_vec()
}

#[tokio::test]
    let (server, _scratch) = spawn_with_packages().await;
    let uid = InstanceUid::default();
    let server_ref = &server;
    let offered = |sequence: u64| async move {
        let mut report = full_report(&uid, "collector", sequence);
        report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    };


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

/// ADR-0023: the offered `download_url` is a path the Client resolves against **its own OpAMP
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
    assert!(
        reply.packages_available.is_none(),
    );
}

/// An entry belongs to a Set: uploading toward an identity nobody created is a 404, not a package
/// conjured out of a URL (ADR-0019 — the identity is stated at creation).
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
    let (server, _scratch) = spawn_with_packages().await;

    // full_report describes an Agent with os.type = linux (see the test scaffolding).
    let targeted = InstanceUid::default();
    let mut report = full_report(&targeted, "collector", 1);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;

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
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    let offer = exchange(&server, &report)
        .await
        .packages_available
        .expect("the matching Agent is offered it");
    assert!(!offer.all_packages_hash.is_empty());
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
    let (server, _scratch) = spawn_with_packages().await;
        report.capabilities |= AgentCapabilities::AcceptsPackages as u64;

        report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
        let offer = exchange(server, &report)
            .await
            .packages_available
            .expect("an offer");
    }

    assert_eq!(
        "3.0.0",
        "the named host gets the canary version"
    );
    assert_eq!(
        "2.0.0",
        "everyone else keeps the fleet-wide version"
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

/// ADR-0018: an entry may live somewhere else. The Server stores the reference, offers that
/// address verbatim with the operator's checksum and headers, and has nothing of its own to serve.
#[tokio::test]
async fn a_referenced_entry_is_offered_from_its_source_and_not_from_here() {
    let (server, _scratch) = spawn_with_packages().await;
    let uid = InstanceUid::default();

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

    // The offer names the source, carries the operator's hash, and passes the headers on.
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
/// ADR-0019 in place of ADR-0019's late typing: the Agent type is identity, stated at creation —
async fn a_set_reaches_only_agents_of_its_type() {
    let (server, _scratch) = spawn_with_packages().await;
    let uid = InstanceUid::default();
    let mut report = full_report(&uid, "edge-01", 1);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
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
        .expect("put entry");
    assert_eq!(response.status(), 200);
    let response = reqwest::Client::new()
        ))
        .send()
        .await
    assert_eq!(response.status(), 200);
    let mut report = full_report(&uid, "edge-01", 2);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
            .packages_available
            .is_none(),
    // The same artifact under this fleet's type reaches it.
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    let offer = exchange(&server, &report)
        .await
        .packages_available
        .expect("an offer");
    let (server, _scratch) = spawn_with_packages().await;
    let uid = InstanceUid::default();
        .await
        .packages_available
        .expect("an offer");
        .await
        .packages_available
/// The silent no-op ADR-0019 named: a Set can target nobody through a mistyped Agent type, a
/// platform the fleet does not run, or a Selector that matches no one — and none of the three is
/// a rejected upload, so without a count nothing says it.
async fn a_set_says_how_many_agents_it_reaches() {
    let (server, _scratch) = spawn_with_packages().await;
    let uid = InstanceUid::default();
    assert_eq!(response.status(), 200);
async fn a_label_aims_a_set_at_part_of_the_fleet() {
    let (server, _scratch) = spawn_with_packages().await;
            server.rest_addr
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    let offer = exchange(&server, &report)
        .await
        .packages_available
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
            .await
            .packages_available
            .is_none(),
    let (server, _scratch) = spawn_with_packages().await;
    let uid = InstanceUid::default();
        report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
            .get(format!("http://{}/api/v1/packages", server.rest_addr))

    assert_eq!(response.status(), 200);
    // the Server's rule, which is exactly what the UI renders as a greyed-out control.
    let frozen_delete = reqwest::Client::new()
        .delete(format!(
            "{}/entries/{HOST}",
        ))
        .send()
        .await
        .expect("delete entry");
    assert_eq!(frozen_delete.status(), 409);
    // The Selector is not bytes, and stays editable.


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
    assert_eq!(refused.status(), 400);
    let (server, _scratch) = spawn_with_packages().await;
    let (server, _scratch) = spawn_with_packages().await;
    let (server, _scratch) = spawn_with_packages().await;
    let (server, _scratch) = spawn_with_packages().await;
        report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
        report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
            .await
            .packages_available
            .expect("an offer");
    let (server, _scratch) = spawn_with_packages().await;
        .expect("put entry");
    assert_eq!(refused.status(), 400);
    let (server, _scratch) = spawn_with_packages().await;
    let uid = InstanceUid::default();
    let mut report = full_report(&uid, "edge-01", 1);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
        report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    let view = &server.state.snapshot()[0];
    let (server, _scratch) = spawn_with_packages().await;
    let uid = InstanceUid::default();
    let mut report = full_report(&uid, "edge-01", 1);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
    let (server, _scratch) = spawn_with_packages().await;
    let uid = InstanceUid::default();
    let mut report = full_report(&uid, "edge-01", 1);
    report.capabilities |= AgentCapabilities::AcceptsPackages as u64;
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
