    let mut builder = reqwest::Client::builder()
        .use_rustls_tls()
        // Unlike the OpAMP endpoint, an artifact URL may legitimately redirect — a mirror
        // (ADR-0019) is often a CDN that bounces the download to signed storage — so redirects are
        // allowed but bounded to a small chain. Integrity does not rest on where the bytes come
        // from: the content hash (always) and the signature (when a key is configured) are checked
        // after the download, so a redirect cannot substitute a malicious artifact.
