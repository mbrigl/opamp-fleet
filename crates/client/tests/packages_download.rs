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
async fn spawn() -> SocketAddr {
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
        .route("/artifact", get(|| async { vec![0u8; 4096] }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
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
