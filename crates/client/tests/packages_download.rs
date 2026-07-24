use axum::response::Redirect;
/// A server with the responses the download tests need.
        )
        // A mirror that redirects the download to where the bytes actually live (the CDN pattern).
        .route("/redirect", get(|| async { Redirect::to("/artifact") }))
/// An artifact URL may legitimately redirect — a mirror (ADR-0019) is often a CDN that bounces the
/// download to signed storage — so the download follows it. Reaching the artifact (and then failing
/// only on the deliberately wrong content hash) proves the redirect was followed, not refused.
#[tokio::test]
async fn a_download_follows_a_redirect_to_the_mirror() {
        &download(format!("http://{addr}/redirect")),
        &small_cap_config(8192),
    .expect_err("the wrong content hash still fails — but only after following the redirect");
    assert!(
        err.contains("content hash"),
        "the redirect was followed to the artifact and streamed: {err}"
    );
}

        &small_cap_config(8192),
    assert!(
        err.contains("content hash"),
        &small_cap_config(8192),
        &small_cap_config(8192),
    assert!(
        err.contains("content hash"),
        &small_cap_config(8192),
    assert!(
        err.contains("content hash"),
        &small_cap_config(8192),
