//! The artifact-download size ceiling (ADR-0015): a Server cannot make the Client fill its staging
//! filesystem before the content hash — which comes only after the whole stream lands — can reject
//! the body. The cap is enforced both from an over-large `Content-Length` up front and, for a
//! chunked response that advertises none, while the bytes stream in.

use std::net::SocketAddr;

use axum::body::Body;
use axum::response::Redirect;
use axum::routing::get;
use axum::Router;
use client::config::ClientConfig;
use client::packages::{download_and_verify, PackageDownload, Progress};
use futures_util::stream;

/// A server with the responses the download tests need.
///
/// The listener is bound first so the routes can name the address they redirect to: the
/// cross-origin case needs a second origin on the same process, and `localhost` versus `127.0.0.1`
/// is one — same socket, different host in the URL, which is exactly what the header rule compares.
async fn spawn() -> SocketAddr {
    // What main() does at startup: without a process provider, reqwest refuses to build a client.
    client::tls::install_ring_provider();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let elsewhere = format!("http://localhost:{}/refuses-credentials", addr.port());
    let app = Router::new()
        // A known-length body: `Content-Length` says up front it is too big.
        .route("/known", get(|| async { vec![0u8; 4096] }))
        // A chunked body: no `Content-Length`, so only the running check can bound it. Eight 1 KiB
        // chunks, streamed, well past a 1 KiB ceiling.
        .route(
            "/chunked",
            get(|| async {
                let chunks = (0..8).map(|_| Ok::<_, std::io::Error>(vec![0u8; 1024]));
                Body::from_stream(stream::iter(chunks))
            }),
        )
        // A mirror that redirects the download to where the bytes actually live (the CDN pattern).
        .route("/redirect", get(|| async { Redirect::to("/artifact") }))
        .route("/artifact", get(|| async { vec![0u8; 4096] }))
        // A source that will not serve the artifact without the credential the operator configured
        // for it (ADR-0018) — what a private mirror looks like.
        .route(
            "/guarded",
            get(|headers: axum::http::HeaderMap| async move {
                match headers.get(axum::http::header::AUTHORIZATION) {
                    Some(value) if value == "Bearer artifact-token" => {
                        (axum::http::StatusCode::OK, vec![0u8; 4096])
                    }
                    _ => (axum::http::StatusCode::UNAUTHORIZED, Vec::new()),
                }
            }),
        )
        // A redirect that stays on this origin, to the source that needs the credential.
        .route("/to-guarded", get(|| async { Redirect::to("/guarded") }))
        // A mirror that bounces the download to a *different* origin — the harvesting shape.
        .route("/leaks", get(|| async move { Redirect::to(&elsewhere) }))
        // The other origin, which refuses anything carrying someone else's credential. Inverted on
        // purpose: it makes a leaked header show up as a `401` and a withheld one as a body that
        // reaches the hash check, so the two outcomes are told apart by the error and no hashing is
        // needed in the test.
        .route(
            "/refuses-credentials",
            get(|headers: axum::http::HeaderMap| async move {
                if headers.contains_key("x-api-key") {
                    (axum::http::StatusCode::UNAUTHORIZED, Vec::new())
                } else {
                    (axum::http::StatusCode::OK, vec![0u8; 4096])
                }
            }),
        );
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    addr
}

fn small_cap_config(max_artifact_size_bytes: u64) -> ClientConfig {
    ClientConfig {
        max_artifact_size_bytes,
        ..ClientConfig::default()
    }
}

fn download(url: String) -> PackageDownload {
    PackageDownload {
        name: "otelcol".to_string(),
        version: "1.0.0".to_string(),
        hash: b"pkg".to_vec(),
        download_url: url,
        // Never reached: the size ceiling refuses the body before any hash is computed.
        content_hash: vec![0u8; 32],
        signature: Vec::new(),
        headers: Vec::new(),
    }
}

/// The same download, carrying the headers the offer named.
fn download_with(url: String, headers: &[(&str, &str)]) -> PackageDownload {
    PackageDownload {
        headers: headers
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect(),
        ..download(url)
    }
}

/// A body whose `Content-Length` already exceeds the ceiling is refused before a byte is written,
/// and nothing is left staged.
#[tokio::test]
async fn a_body_too_large_by_its_content_length_is_refused() {
    let addr = spawn().await;
    let staging = tempfile::tempdir().expect("tempdir");
    let config = small_cap_config(1024);

    let err = download_and_verify(
        &download(format!("http://{addr}/known")),
        &config,
        staging.path(),
        &Progress::default(),
    )
    .await
    .expect_err("an over-large artifact must be refused");
    assert!(err.contains("max_artifact_size_bytes"), "got {err}");
    assert!(
        !staging.path().join("otelcol.staged").exists(),
        "the staged file is cleaned up on refusal"
    );
}

/// A chunked body that advertises no length is stopped the moment the stream crosses the ceiling —
/// the case an attacker would use to dodge the up-front check.
#[tokio::test]
async fn a_chunked_body_is_stopped_once_it_crosses_the_ceiling() {
    let addr = spawn().await;
    let staging = tempfile::tempdir().expect("tempdir");
    let config = small_cap_config(1024);

    let err = download_and_verify(
        &download(format!("http://{addr}/chunked")),
        &config,
        staging.path(),
        &Progress::default(),
    )
    .await
    .expect_err("a streaming body past the ceiling must be refused");
    assert!(err.contains("max_artifact_size_bytes"), "got {err}");
    assert!(!staging.path().join("otelcol.staged").exists());
}

/// The ceiling does not get in the way of an ordinary artifact that fits under it.
#[tokio::test]
async fn a_body_within_the_ceiling_streams_through_to_verification() {
    let addr = spawn().await;
    let staging = tempfile::tempdir().expect("tempdir");
    // A 4 KiB known body under an 8 KiB ceiling reaches the hash check — which then fails on the
    // deliberately wrong content_hash, proving the size gate let it past.
    let config = small_cap_config(8192);

    let err = download_and_verify(
        &download(format!("http://{addr}/known")),
        &config,
        staging.path(),
        &Progress::default(),
    )
    .await
    .expect_err("the wrong content hash still fails, but past the size gate");
    assert!(
        !err.contains("max_artifact_size_bytes"),
        "a body under the ceiling must not be refused for size: {err}"
    );
}

/// An artifact URL may legitimately redirect — a mirror (ADR-0018) is often a CDN that bounces the
/// download to signed storage — so the download follows it. Reaching the artifact (and then failing
/// only on the deliberately wrong content hash) proves the redirect was followed, not refused.
#[tokio::test]
async fn a_download_follows_a_redirect_to_the_mirror() {
    let addr = spawn().await;
    let staging = tempfile::tempdir().expect("tempdir");

    let err = download_and_verify(
        &download(format!("http://{addr}/redirect")),
        &small_cap_config(8192),
        staging.path(),
        &Progress::default(),
    )
    .await
    .expect_err("the wrong content hash still fails — but only after following the redirect");
    assert!(
        err.contains("content hash"),
        "the redirect was followed to the artifact and streamed: {err}"
    );
}

/// The Baseline: *"The Agent SHOULD include the HTTP headers provided in the headers field for the
/// GET request."* A referenced source (ADR-0018) may be a private mirror, and the Server fills those
/// headers from what the operator configured — so a download that drops them cannot fetch the
/// artifact at all. Reaching the content-hash check (which then fails on the deliberately wrong
/// hash) is what proves the credential travelled; before this was implemented the same call failed
/// with `401 Unauthorized`.
#[tokio::test]
async fn a_download_carries_the_headers_the_offer_named() {
    let addr = spawn().await;
    let staging = tempfile::tempdir().expect("tempdir");

    let err = download_and_verify(
        &download_with(
            format!("http://{addr}/guarded"),
            &[("Authorization", "Bearer artifact-token")],
        ),
        &small_cap_config(8192),
        staging.path(),
        &Progress::default(),
    )
    .await
    .expect_err("the wrong content hash still fails — but only after the source served the bytes");
    assert!(
        err.contains("content hash"),
        "the credential reached the source and the artifact streamed: {err}"
    );
}

/// And without them the same source refuses — the guard is real, not a route that always answers.
#[tokio::test]
async fn a_download_without_the_headers_is_refused_by_a_guarded_source() {
    let addr = spawn().await;
    let staging = tempfile::tempdir().expect("tempdir");

    let err = download_and_verify(
        &download(format!("http://{addr}/guarded")),
        &small_cap_config(8192),
        staging.path(),
        &Progress::default(),
    )
    .await
    .expect_err("a guarded source refuses an unauthenticated download");
    assert!(err.contains("401"), "got {err}");
}

/// A header the operator named for one host must not follow a redirect to another. `reqwest` strips
/// only `Authorization`, `Cookie` and `Proxy-Authorization` across origins, so a custom credential
/// would otherwise be handed to wherever a mirror points — which is how a mirror harvests it. The
/// second origin here refuses anything carrying the token, so a leak surfaces as `401` and the
/// correct behaviour reaches the hash check.
#[tokio::test]
async fn an_offered_header_does_not_follow_a_redirect_to_another_origin() {
    let addr = spawn().await;
    let staging = tempfile::tempdir().expect("tempdir");

    let err = download_and_verify(
        &download_with(
            format!("http://{addr}/leaks"),
            &[("x-api-key", "operator-secret")],
        ),
        &small_cap_config(8192),
        staging.path(),
        &Progress::default(),
    )
    .await
    .expect_err("the wrong content hash still fails — but only after the redirect was followed");
    assert!(
        err.contains("content hash"),
        "the credential must not travel to the redirect target: {err}"
    );
}

/// Following the chain by hand must not break the ordinary mirror: a same-origin redirect still
/// carries the credential, because that is the host it was given for.
#[tokio::test]
async fn an_offered_header_survives_a_redirect_within_the_same_origin() {
    let addr = spawn().await;
    let staging = tempfile::tempdir().expect("tempdir");

    let err = download_and_verify(
        &download_with(
            format!("http://{addr}/to-guarded"),
            &[("Authorization", "Bearer artifact-token")],
        ),
        &small_cap_config(8192),
        staging.path(),
        &Progress::default(),
    )
    .await
    .expect_err("the wrong content hash still fails — but only after the guarded source served");
    assert!(
        err.contains("content hash"),
        "a same-origin redirect keeps the credential: {err}"
    );
}

/// A header the offer names that is not a valid HTTP header fails the download loudly rather than
/// being skipped, and the message names the key without its value.
#[tokio::test]
async fn an_unusable_offered_header_fails_the_download_by_name() {
    let addr = spawn().await;
    let staging = tempfile::tempdir().expect("tempdir");

    let err = download_and_verify(
        &download_with(
            format!("http://{addr}/known"),
            &[("no good", "super-secret-value")],
        ),
        &small_cap_config(8192),
        staging.path(),
        &Progress::default(),
    )
    .await
    .expect_err("an unusable header is refused rather than dropped");
    assert!(err.contains("no good"), "the key is named: {err}");
    assert!(
        !err.contains("super-secret-value"),
        "the value must never appear: {err}"
    );
}

/// The staging directory is kept owner-only, so the verified artifact cannot be swapped for another
/// between the hash check and the installer re-opening it (TOCTOU). It starts deliberately wide here
/// and must be narrowed; the hardening happens before a byte is written, so even a failing download
/// leaves it owner-only.
#[cfg(unix)]
#[tokio::test]
async fn the_staging_directory_is_kept_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let addr = spawn().await;
    let scratch = tempfile::tempdir().expect("tempdir");
    let staging = scratch.path().join("packages");
    std::fs::create_dir_all(&staging).expect("mkdir");
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755)).expect("widen");

    // Fails on the deliberately wrong content hash — the directory is hardened regardless.
    let _ = download_and_verify(
        &download(format!("http://{addr}/known")),
        &small_cap_config(8192),
        &staging,
        &Progress::default(),
    )
    .await;

    let mode = std::fs::metadata(&staging)
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o700,
        "the staging directory must be owner-only"
    );
}
