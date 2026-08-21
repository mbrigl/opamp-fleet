//! Server-offered packages (ADR-0015): resolving a download URL, downloading the artifact, and
//! **verifying it before it is applied** — the content hash always, the Ed25519 signature when the
//! operator configured a verification key. What protects an installed binary is verification, not
//! transport secrecy, so this is where the security of the feature lives.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use opamp::proto::PackageDownloadDetails;
use sha2::{Digest, Sha256};
use tracing::{info, Instrument as _};

use crate::config::ClientConfig;

/// One offered package the transport must download, verify, and hand to the Supervisor. Built by
/// the Agent state machine from a `PackageAvailable`; the raw fields it needs travel here.
#[derive(Clone, PartialEq, Eq)]
pub struct PackageDownload {
    pub name: String,
    pub version: String,
    /// The package hash the reported status refers to.
    pub hash: Vec<u8>,
    /// The `download_url` from the offer — absolute, or a path resolved against the endpoint.
    pub download_url: String,
    /// The expected SHA-256 of the artifact.
    pub content_hash: Vec<u8>,
    /// The Ed25519 signature over the artifact; empty means unsigned.
    pub signature: Vec<u8>,
    /// The headers the offer says this download needs — a referenced source's credential
    /// (ADR-0018), which the Server fills from the operator's configuration. The Baseline: *"The
    /// Agent SHOULD include the HTTP headers provided in the headers field for the GET request."*
    ///
    /// Raw pairs rather than the wire type, like every other field here: what the download needs is
    /// a name and a value, not a protobuf message.
    pub headers: Vec<(String, String)>,
}

/// Written by hand rather than derived, because a header value is a credential.
///
/// This struct travels inside `Handled`, which derives `Debug`; a single `debug!(?handled)` added
/// later would otherwise put a fleet credential in the log file that ADR-0041 writes to disk in
/// service mode. Keys are printed — they are what a diagnosis needs — and values never are.
impl std::fmt::Debug for PackageDownload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackageDownload")
            .field("name", &self.name)
            .field("version", &self.version)
            .field("hash", &self.hash)
            .field("download_url", &self.download_url)
            .field("content_hash", &self.content_hash)
            .field("signature", &self.signature)
            .field(
                "headers",
                &self
                    .headers
                    .iter()
                    .map(|(key, _)| format!("{key}: <redacted>"))
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// Resolves an offered `download_url` to an absolute URL. An absolute `http(s)://` URL is used as
/// given; a path (the Server's zero-config default) is resolved against the Client's own OpAMP
/// endpoint host, `ws(s)://` mapped to `http(s)://`.
pub fn resolve_url(download_url: &str, endpoint: &str) -> Result<String, String> {
    if download_url.starts_with("http://") || download_url.starts_with("https://") {
        return Ok(download_url.to_string());
    }
    let (scheme, rest) = endpoint
        .split_once("://")
        .ok_or_else(|| format!("cannot resolve {download_url} against endpoint {endpoint}"))?;
    let http_scheme = match scheme {
        "ws" | "http" => "http",
        "wss" | "https" => "https",
        other => return Err(format!("unexpected endpoint scheme {other}")),
    };
    let host_port = rest.split(['/', '?']).next().unwrap_or("");
    let path = if download_url.starts_with('/') {
        download_url.to_string()
    } else {
        format!("/{download_url}")
    };
    Ok(format!("{http_scheme}://{host_port}{path}"))
}

/// Refuses an offered package name that is not a safe file-name token, before it is ever joined
/// into the staging path. The Server validates a package name to letters, digits, `.`, `_`, `+`,
/// and `-` (`validate_identity_token`) before it stores one, so a name outside that set is not a
/// package the Server would legitimately offer — it is a malicious or non-conforming peer steering
/// the staged file out of this Agent's own directory: a `/`, a `\`, or a lone `..` in the name
/// would make `staging_dir.join(format!("{name}.staged"))` land somewhere else, with bytes the
/// offering peer controls and *before* verification. Refused rather than sanitized: rewriting a bad
/// name could collide two distinct packages onto one staged file, and a name this shape is a signal
/// something is wrong, not a typo to paper over.
fn ensure_safe_package_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 64 {
        return Err(format!(
            "the package name must be 1–64 characters, not {name:?}"
        ));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
    {
        return Err(format!(
            "the package name {name:?} may hold only letters, digits, '.', '_', '+', and '-'"
        ));
    }
    Ok(())
}

/// Downloads the artifact to a file and verifies it (ADR-0015). Returns the path of the verified
/// artifact, ready for the Supervisor to swap over the Managed Process's binary.
///
/// The artifact is a program — tens or hundreds of megabytes — so it is streamed to `staging_dir`
/// and hashed as it arrives, never assembled in memory. Only a signature check reads it back,
/// because Ed25519 verifies over the whole message. `staging_dir` is the receiving Agent's own
/// (ADR-0021), so the install that follows is a rename inside one filesystem.
pub async fn download_and_verify(
    package: &PackageDownload,
    config: &ClientConfig,
    staging_dir: &Path,
    progress: &Progress,
) -> Result<PathBuf, String> {
    // First, before any URL is resolved or a byte is written: the staged path is built from this
    // name, and a name that could escape the staging directory is refused outright.
    ensure_safe_package_name(&package.name)?;
    let url = resolve_url(&package.download_url, &config.endpoint)?;
    let mut builder = reqwest::Client::builder()
        .use_rustls_tls()
        // Unlike the OpAMP endpoint, an artifact URL may legitimately redirect — a mirror
        // (ADR-0018) is often a CDN that bounces the download to signed storage — so redirects are
        // allowed but bounded to a small chain. Integrity does not rest on where the bytes come
        // from: the content hash (always) and the signature (when a key is configured) are checked
        // after the download, so a redirect cannot substitute a malicious artifact.
        //
        // With offered headers in play the chain is walked by hand instead (`send_download`), so
        // the policy here is what applies to a download that carries none.
        .redirect(if package.headers.is_empty() {
            reqwest::redirect::Policy::limited(MAX_REDIRECTS)
        } else {
            reqwest::redirect::Policy::none()
        })
        // Per-operation timeouts, not one for the whole transfer: a large artifact over a modest
        // link legitimately takes minutes, and a total timeout would abort it forever while a
        // stalled connection is what actually needs cutting.
        .connect_timeout(std::time::Duration::from_secs(30))
        .read_timeout(std::time::Duration::from_secs(60));
    // Trust only, never this Client's certificate: a `download_url` may point at a mirror
    // (ADR-0018), and an identity belongs to the Server rather than to whoever hosts an artifact.
    builder = crate::tls::trust(builder, config)?;
    let client = builder
        .build()
        .map_err(|e| format!("cannot build the download client: {e}"))?;
    // A count, never a key and never a value — and the source without whatever authorises reaching
    // it. This line goes to the log file, and through the bridge to the destination the Server named
    // (ADR-0036), which is the same reason the span below carries the redacted form: a pre-signed
    // URL puts its signature in the query, and a log line is a poor place to keep one.
    let source = source_of(&url);
    info!(package = %package.name, url = %source, headers = package.headers.len(), "downloading package");
    let download = tracing::info_span!("download", source = %source);
    let mut response = send_download(&client, &url, &package.headers)
        .instrument(download.clone())
        .await?;
    if !response.status().is_success() {
        return Err(format!("{url} answered {}", response.status()));
    }

    std::fs::create_dir_all(staging_dir)
        .map_err(|e| format!("cannot create {}: {e}", staging_dir.display()))?;
    // Keep the staging directory owner-only. The artifact is verified here and then re-opened by the
    // installer (`install::write_program`, the Supervisor's swap); if another local user could write
    // into this directory they could swap the file in that window and defeat the hash and signature
    // check it already passed (TOCTOU). Owner-only closes it — the predictable `<name>.staged`
    // filename is then harmless, since no other user can reach the directory to race it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(staging_dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("cannot restrict {}: {e}", staging_dir.display()))?;
    }
    let path = staging_dir.join(format!("{}.staged", package.name));

    // Stream to disk, hashing on the way past: peak memory is one chunk, whatever the artifact
    // weighs. A failure anywhere leaves no half-written file behind for the next attempt to trip
    // over. The size ceiling stops a Server from filling the staging filesystem with an endless
    // body before the content hash — which comes only after the whole stream lands — can reject it.
    let staged = match write_stream(
        &path,
        &mut response,
        progress,
        config.max_artifact_size_bytes,
    )
    .instrument(download.clone())
    .await
    {
        Ok(hash) => hash,
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
    };
    // The bytes are in; what remains is deciding whether they are the right ones. Its own phase of
    // the trace (ADR-0090), because the hash and the signature are what a package's security rests
    // on and "it failed to install" must be able to say which of the two.
    drop(download);
    let verify = tracing::info_span!("verify", bytes = staged.len).entered();
    if let Err(e) = verify_staged(&path, &staged, package, config.package_key()) {
        let _ = std::fs::remove_file(&path);
        return Err(e);
    }
    drop(verify);
    info!(package = %package.name, version = %package.version, bytes = staged.len, "package verified");
    Ok(path)
}

/// How many redirects an artifact download will follow, whoever follows them.
const MAX_REDIRECTS: usize = 5;

/// Sends the download request, carrying the headers the offer named.
///
/// Without offered headers this is one `GET` and the client follows redirects itself. With them the
/// chain is walked here, and a header is re-attached only while the scheme, host and port are the
/// ones it was given for. The reason is narrow and worth stating: `reqwest` strips only
/// `Authorization`, `Cookie` and `Proxy-Authorization` when a redirect crosses origins, so a custom
/// credential — `X-JFrog-Art-Api`, `PRIVATE-TOKEN`, whatever the source wants — would be re-sent to
/// wherever a mirror points it. An operator names a credential for *one* host; a mirror must not be
/// able to harvest it by bouncing the download. Integrity is unaffected either way, since the
/// content hash and the signature are checked after the bytes land — this protects the credential,
/// not the artifact.
async fn send_download(
    client: &reqwest::Client,
    url: &str,
    headers: &[(String, String)],
) -> Result<reqwest::Response, String> {
    if headers.is_empty() {
        return client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("cannot download {url}: {e}"));
    }
    let origin = reqwest::Url::parse(url).map_err(|e| format!("cannot parse {url}: {e}"))?;
    let mut current = origin.clone();
    for _ in 0..=MAX_REDIRECTS {
        let mut request = client.get(current.clone());
        if same_origin(&origin, &current) {
            request = with_headers(request, headers)?;
        }
        let response = request
            .send()
            .await
            .map_err(|e| format!("cannot download {current}: {e}"))?;
        let Some(location) = redirect_target(&response) else {
            return Ok(response);
        };
        current = current
            .join(&location)
            .map_err(|e| format!("{current} redirected to an unusable location: {e}"))?;
    }
    Err(format!("{url} redirected more than {MAX_REDIRECTS} times"))
}

/// Attaches the offered headers to a request.
///
/// A header that is not a valid header fails the download loudly rather than being skipped: a
/// silently dropped credential comes back as an opaque `401` that nothing explains. The message
/// names the **key only** — it travels to the Server as `PackageStatuses.error_message` and into
/// the log file, and the value is the secret.
fn with_headers(
    mut request: reqwest::RequestBuilder,
    headers: &[(String, String)],
) -> Result<reqwest::RequestBuilder, String> {
    for (key, value) in headers {
        let name = reqwest::header::HeaderName::from_bytes(key.as_bytes()).map_err(|_| {
            format!("the offered download header {key:?} is not a valid header name")
        })?;
        let mut value = reqwest::header::HeaderValue::from_str(value).map_err(|_| {
            format!(
                "the offered download header {key:?} carries a value that is not a valid header"
            )
        })?;
        // As the OpAMP transport marks its own credential (`transport/http.rs`).
        value.set_sensitive(true);
        request = request.header(name, value);
    }
    Ok(request)
}

/// Where an artifact is being fetched from, without whatever authorises the fetch.
///
/// The span this labels leaves the host for a destination the *Server* named (ADR-0090 clause 9), and
/// a download URL is one of the few strings here that can carry a credential in plain sight: a
/// pre-signed URL puts its signature in the query. The scheme, host and path answer the question a
/// trace is read for — *which mirror served this* — and the query answers none of it.
///
/// A URL that will not parse is reported as nothing rather than as itself: the one case where the
/// query cannot be found is the case where it must not be assumed absent.
fn source_of(url: &str) -> String {
    let Ok(mut parsed) = reqwest::Url::parse(url) else {
        return "an unparseable url".to_string();
    };
    parsed.set_query(None);
    parsed.set_fragment(None);
    // A username or password in the URL itself is the other place a credential hides.
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);
    parsed.to_string()
}

/// Whether a redirect stayed where the offered headers may go: same scheme, host, and port.
fn same_origin(a: &reqwest::Url, b: &reqwest::Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

/// The `Location` of a redirect response, or `None` when the response is the artifact itself.
fn redirect_target(response: &reqwest::Response) -> Option<String> {
    if !response.status().is_redirection() {
        return None;
    }
    response
        .headers()
        .get(reqwest::header::LOCATION)?
        .to_str()
        .ok()
        .map(str::to_string)
}

/// What streaming the download to disk produced: its size and its content hash.
struct Staged {
    len: u64,
    content_hash: Vec<u8>,
}

/// How far an artifact download has got, shared with whoever reports it.
///
/// The download is a plain `await` inside the transport loop; this is how that loop learns the
/// progress of the future it is polling without the download having to know anything about
/// reporting. Cheap enough to update per chunk.
#[derive(Debug, Default)]
pub struct Progress {
    /// Bytes written so far.
    downloaded: AtomicU64,
    /// The artifact's total size, from `Content-Length`; `0` when the Server did not say.
    total: AtomicU64,
}

impl Progress {
    /// The Baseline's `PackageDownloadDetails` as of now: how far along, and how fast since
    /// `started`. Percent stays `0` while the total is unknown — a made-up percentage would be
    /// worse than none.
    pub fn details(&self, started: std::time::Instant) -> PackageDownloadDetails {
        let downloaded = self.downloaded.load(Ordering::Relaxed);
        let total = self.total.load(Ordering::Relaxed);
        let seconds = started.elapsed().as_secs_f64();
        PackageDownloadDetails {
            download_percent: if total > 0 {
                (downloaded as f64 / total as f64) * 100.0
            } else {
                0.0
            },
            download_bytes_per_second: if seconds > 0.0 {
                downloaded as f64 / seconds
            } else {
                0.0
            },
        }
    }
}

/// The message for a download that has reached `max_bytes`, or `None` while it is within the
/// ceiling. `max_bytes == 0` is never passed — the loader refuses it (a bound, not a switch).
fn over_cap(len: u64, max_bytes: u64) -> Option<String> {
    (len > max_bytes).then(|| {
        format!("the artifact exceeds the {max_bytes}-byte limit (max_artifact_size_bytes)")
    })
}

async fn write_stream(
    path: &Path,
    response: &mut reqwest::Response,
    progress: &Progress,
    max_bytes: u64,
) -> Result<Staged, String> {
    use tokio::io::AsyncWriteExt;

    // What the Server advertises, so a percentage is possible at all.
    let advertised = response.content_length().unwrap_or(0);
    progress.total.store(advertised, Ordering::Relaxed);
    // Refuse a body that says up front it is too large, before a single byte is written. A lying or
    // absent Content-Length is caught by the running check below instead.
    if let Some(e) = over_cap(advertised, max_bytes) {
        return Err(e);
    }
    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut len = 0u64;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("cannot read the download: {e}"))?
    {
        len += chunk.len() as u64;
        // Stop the moment the body crosses the ceiling — a chunked response carries no
        // Content-Length, so this running check is what bounds it at all.
        if let Some(e) = over_cap(len, max_bytes) {
            return Err(e);
        }
        hasher.update(&chunk);
        progress.downloaded.store(len, Ordering::Relaxed);
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }
    file.flush()
        .await
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(Staged {
        len,
        content_hash: hasher.finalize().to_vec(),
    })
}

/// Verifies the streamed artifact: the content hash from the stream, and — only when the policy
/// demands a signature check — the file read back, since Ed25519 verifies over the whole message.
fn verify_staged(
    path: &Path,
    staged: &Staged,
    package: &PackageDownload,
    key: Option<&[u8]>,
) -> Result<(), String> {
    if staged.content_hash != package.content_hash {
        return Err("the downloaded artifact does not match its content hash".to_string());
    }
    if !signature_required(&package.signature, key)? {
        return Ok(());
    }
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    check_signature(&bytes, &package.signature, key.unwrap_or_default())
}

/// The signature half of the verification policy (ADR-0015): the content hash must always match.
/// If a verification key is configured, the artifact MUST carry a valid signature — an unsigned or
/// badly signed artifact is refused. If no key is configured, a signature that was nonetheless
/// offered is refused (it cannot be checked); an unsigned artifact is accepted on its content hash
/// alone.
///
/// Decided without touching the artifact, so the file is only read back when it must be: `Ok(true)`
/// means a
/// signature must now be checked, `Ok(false)` that the content hash was the whole of it, `Err`
/// that the pairing of key and signature is refused outright.
fn signature_required(signature: &[u8], key: Option<&[u8]>) -> Result<bool, String> {
    match (key, signature.is_empty()) {
        (Some(_), false) => Ok(true),
        (Some(_), true) => {
            Err("a verification key is configured but the artifact is unsigned".to_string())
        }
        (None, false) => {
            Err("the artifact is signed but no verification key is configured".to_string())
        }
        (None, true) => Ok(false),
    }
}

fn check_signature(bytes: &[u8], signature: &[u8], key: &[u8]) -> Result<(), String> {
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
        .verify(bytes, signature)
        .map_err(|_| "the artifact's signature is invalid".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::KeyPair;

    /// What labels the download span must not carry what authorises the download: the span goes to
    /// a destination the Server named (ADR-0090 clause 9), and a pre-signed URL is a credential.
    #[test]
    fn the_download_source_drops_whatever_authorises_it() {
        assert_eq!(
            source_of("https://mirror.example/artifacts/agent.tar.gz?X-Amz-Signature=deadbeef"),
            "https://mirror.example/artifacts/agent.tar.gz"
        );
        assert_eq!(
            source_of("https://user:secret@mirror.example/agent.tar.gz#frag"),
            "https://mirror.example/agent.tar.gz"
        );
        assert_eq!(source_of("not a url"), "an unparseable url");
    }

    #[test]
    fn resolve_url_absolutizes_a_path_against_the_endpoint() {
        assert_eq!(
            resolve_url("/api/v1/packages/otelcol/file", "ws://host:4320/v1/opamp").unwrap(),
            "http://host:4320/api/v1/packages/otelcol/file"
        );
        assert_eq!(
            resolve_url("/api/v1/packages/x/file", "wss://host:4320/v1/opamp").unwrap(),
            "https://host:4320/api/v1/packages/x/file"
        );
        // An absolute URL is used as given.
        assert_eq!(
            resolve_url("https://cdn.example/x.bin", "ws://host/v1/opamp").unwrap(),
            "https://cdn.example/x.bin"
        );
    }

    /// A staged artifact, written where a real download would leave it.
    fn stage(dir: &tempfile::TempDir, bytes: &[u8]) -> (PathBuf, Staged) {
        let path = dir.path().join("otelcol.staged");
        std::fs::write(&path, bytes).expect("write");
        (
            path,
            Staged {
                len: bytes.len() as u64,
                content_hash: Sha256::digest(bytes).to_vec(),
            },
        )
    }

    fn offer(content_hash: Vec<u8>, signature: Vec<u8>) -> PackageDownload {
        PackageDownload {
            name: "otelcol".to_string(),
            version: "1.0.0".to_string(),
            hash: b"pkg".to_vec(),
            download_url: "/x".to_string(),
            content_hash,
            signature,
            headers: Vec::new(),
        }
    }

    /// A header value is a credential, and this struct travels inside a `Debug`-deriving type that
    /// a log line could one day print. The key is diagnosable, the value never appears.
    #[test]
    fn a_download_never_debug_prints_its_header_values() {
        let mut package = offer(vec![0u8; 32], Vec::new());
        package.headers = vec![(
            "Authorization".to_string(),
            "Bearer super-secret-value".to_string(),
        )];

        let printed = format!("{package:?}");
        assert!(
            printed.contains("Authorization"),
            "the key stays diagnosable: {printed}"
        );
        assert!(
            !printed.contains("super-secret-value"),
            "the value must never be printed: {printed}"
        );
    }

    /// Only a redirect that stays on the same scheme, host and port may carry the offered headers.
    #[test]
    fn same_origin_compares_scheme_host_and_port() {
        let parse = |url: &str| reqwest::Url::parse(url).expect("url");
        let source = parse("https://mirror.example/artifact.tar.gz");

        assert!(same_origin(&source, &parse("https://mirror.example/else")));
        // The default port is the same port.
        assert!(same_origin(&source, &parse("https://mirror.example:443/x")));
        // A different host, port, or scheme is somewhere else.
        assert!(!same_origin(&source, &parse("https://cdn.example/x")));
        assert!(!same_origin(
            &source,
            &parse("https://mirror.example:8443/x")
        ));
        assert!(!same_origin(&source, &parse("http://mirror.example/x")));
    }

    /// A package name is a file-name token, not a path: the safe set mirrors what the Server
    /// validates before it stores one, and anything that could steer the staged file elsewhere —
    /// a separator, a lone `..`, an empty or over-long name — is refused.
    #[test]
    fn a_traversing_package_name_is_refused() {
        assert!(ensure_safe_package_name("otelcol").is_ok());
        assert!(ensure_safe_package_name("otelcol-contrib_1.2.3+build").is_ok());

        for bad in [
            "",
            "../../../../etc/cron.d/x",
            "a/b",
            "a\\b",
            "with space",
            "nul\0byte",
        ] {
            assert!(
                ensure_safe_package_name(bad).is_err(),
                "expected {bad:?} to be refused"
            );
        }
        assert!(
            ensure_safe_package_name(&"x".repeat(65)).is_err(),
            "too long"
        );
        // A lone `..` is within the mirrored charset (only '.' bytes) and cannot traverse: the
        // staged name always gets a `.staged` suffix, so it joins as `...staged`, an ordinary file
        // inside the staging directory — not the parent. A separator is what escapes, and that is
        // refused above.
        assert!(ensure_safe_package_name("..").is_ok());
    }

    /// End to end at the sink: a traversing name is refused by `download_and_verify` before any URL
    /// is resolved or a byte is written, so nothing lands outside the staging directory — and the
    /// error is the one the caller reports as a failed package status.
    #[tokio::test]
    async fn download_refuses_to_stage_a_traversing_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let staging = dir.path().join("staging");
        std::fs::create_dir_all(&staging).expect("staging dir");
        let escape_target = dir.path().join("x.staged"); // where `../x` would land

        let config: ClientConfig = toml::from_str("").expect("config");
        let mut package = offer(Sha256::digest(b"x").to_vec(), Vec::new());
        package.name = "../x".to_string();

        let err = download_and_verify(&package, &config, &staging, &Progress::default())
            .await
            .expect_err("a traversing name is refused");
        assert!(err.contains("package name"), "got {err}");
        assert!(
            !escape_target.exists(),
            "nothing was written outside the staging directory"
        );
        assert!(
            std::fs::read_dir(&staging)
                .expect("read staging")
                .next()
                .is_none(),
            "nothing was staged"
        );
    }

    #[test]
    fn the_cap_triggers_only_past_the_limit() {
        assert!(over_cap(1000, 1024).is_none(), "within the ceiling is fine");
        assert!(
            over_cap(1024, 1024).is_none(),
            "exactly at the ceiling is fine"
        );
        let err = over_cap(1025, 1024).expect("past the ceiling is refused");
        assert!(err.contains("max_artifact_size_bytes"), "got {err}");
    }

    #[test]
    fn content_hash_mismatch_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (path, staged) = stage(&dir, b"artifact");

        let wrong = offer(Sha256::digest(b"other").to_vec(), Vec::new());
        assert!(verify_staged(&path, &staged, &wrong, None).is_err());

        let right = offer(staged.content_hash.clone(), Vec::new());
        assert!(verify_staged(&path, &staged, &right, None).is_ok());
    }

    #[test]
    fn signature_policy_is_enforced() {
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).expect("keygen");
        let keypair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("keypair");
        let public = keypair.public_key().as_ref().to_vec();

        let dir = tempfile::tempdir().expect("tempdir");
        let bytes = b"the-binary";
        let (path, staged) = stage(&dir, bytes);
        let hash = staged.content_hash.clone();
        let signature = keypair.sign(bytes).as_ref().to_vec();

        let check = |signature: Vec<u8>, key: Option<&[u8]>| {
            verify_staged(&path, &staged, &offer(hash.clone(), signature), key)
        };
        // Valid signature against the configured key: accepted.
        assert!(check(signature.clone(), Some(&public)).is_ok());
        // Tampered signature: refused.
        let mut bad = signature.clone();
        bad[0] ^= 0xff;
        assert!(check(bad, Some(&public)).is_err());
        // Key configured but artifact unsigned: refused.
        assert!(check(Vec::new(), Some(&public)).is_err());
        // Signed but no key to check it: refused.
        assert!(check(signature, None).is_err());
        // Unsigned and no key: accepted on the content hash alone.
        assert!(check(Vec::new(), None).is_ok());
    }
}
