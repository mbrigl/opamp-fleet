//! A minimal HTTP/1.1 `GET` client for downloading a package file (ADR-0018).
//!
//! Package delivery is an HTTP GET against a `download_url` the Server offers (the OpAMP spec models
//! packages as `DownloadableFile`, not inline bytes). Rather than pull a full HTTP-client dependency for
//! one request, this speaks just enough HTTP/1.1 over the tokio TCP/TLS stack the supervisor already
//! links: a single `GET` with `Connection: close`, reading the body by `Content-Length` (or to EOF). A
//! `https://` server is validated exactly as the OpAMP `wss://` client is — a custom CA, the insecure
//! development option, or the bundled webpki roots ([`crate::tls::client_config`], ADR-0012).
//!
//! Scope (ADR-0018): no redirects, no chunked transfer-encoding, no range/resume — the download is served
//! by this project's own Server over its authenticated `:4321` surface, which sends a plain
//! `Content-Length` body. A response this client cannot parse is an error, not a silent partial download.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::TlsConnector;

/// The overall time budget for one download — the connect, request, and body read together. Bounds a
/// stalled or malicious server so a package offer cannot hang the OpAMP loop indefinitely.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);

/// The largest package body this client will accept, so a runaway `Content-Length` (or an endless
/// EOF-terminated stream) cannot exhaust memory. Generous enough for a collector binary.
const MAX_BODY_BYTES: usize = 512 * 1024 * 1024;

/// A parsed `http`/`https` URL, split into the pieces the request needs.
struct Target {
    tls: bool,
    host: String,
    port: u16,
    /// The path plus any query, e.g. `/packages/otelcol`; always begins with `/`.
    path: String,
}

/// Downloads the bytes at `url` over HTTP/1.1 `GET`, sending `headers` (e.g. the `Authorization` the
/// Server put in the offer). A `https://` server is validated per `ca_cert` / `insecure` (ADR-0012).
/// Returns the response body on a `2xx` status; any other status, a malformed response, or a transport
/// error is an `Err`.
pub async fn get(
    url: &str,
    headers: &[(String, String)],
    ca_cert: Option<&[u8]>,
    insecure: bool,
) -> Result<Vec<u8>, String> {
    tokio::time::timeout(DOWNLOAD_TIMEOUT, fetch(url, headers, ca_cert, insecure))
        .await
        .map_err(|_| format!("timed out downloading {url}"))?
}

async fn fetch(
    url: &str,
    headers: &[(String, String)],
    ca_cert: Option<&[u8]>,
    insecure: bool,
) -> Result<Vec<u8>, String> {
    let target = parse_url(url)?;
    let request = build_request(&target, headers);
    let tcp = TcpStream::connect((target.host.as_str(), target.port))
        .await
        .map_err(|e| format!("cannot connect to {}:{}: {e}", target.host, target.port))?;

    // The TLS and plaintext branches read into one buffer through the same helper; the only difference is
    // the stream type, so each branch does its own I/O and shares [`read_response`].
    let raw = if target.tls {
        let config = crate::tls::client_config(ca_cert, insecure)?;
        let server_name = ServerName::try_from(target.host.clone())
            .map_err(|e| format!("invalid TLS server name {}: {e}", target.host))?;
        let mut stream = TlsConnector::from(Arc::clone(&config))
            .connect(server_name, tcp)
            .await
            .map_err(|e| format!("TLS handshake with {} failed: {e}", target.host))?;
        exchange(&mut stream, &request).await?
    } else {
        let mut stream = tcp;
        exchange(&mut stream, &request).await?
    };

    parse_response(&raw)
}

/// Writes the request and reads the whole response (headers and body) into one buffer. Works over any
/// async stream, so the TLS and plaintext paths share it.
async fn exchange<S>(stream: &mut S, request: &[u8]) -> Result<Vec<u8>, String>
where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
{
    stream
        .write_all(request)
        .await
        .map_err(|e| format!("cannot send download request: {e}"))?;
    stream
        .flush()
        .await
        .map_err(|e| format!("cannot flush download request: {e}"))?;
    read_response(stream).await
}

/// Reads the response into memory, stopping at `Content-Length` bytes of body when the header is present
/// or at EOF otherwise, and never past [`MAX_BODY_BYTES`].
async fn read_response<S>(stream: &mut S) -> Result<Vec<u8>, String>
where
    S: AsyncReadExt + Unpin,
{
    let mut buf = Vec::new();
    let mut chunk = [0u8; 64 * 1024];
    // The end of the body once known: header-block length + Content-Length. `None` until the headers are
    // fully read (or when there is no Content-Length, in which case EOF terminates the body).
    let mut body_end: Option<usize> = None;

    loop {
        if let Some(end) = body_end {
            if buf.len() >= end {
                buf.truncate(end);
                break;
            }
        }
        let n = stream
            .read(&mut chunk)
            .await
            .map_err(|e| format!("cannot read download response: {e}"))?;
        if n == 0 {
            break; // EOF — the whole response for a Content-Length-less or Connection: close body.
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > MAX_BODY_BYTES {
            return Err(format!("download exceeded the {MAX_BODY_BYTES}-byte limit"));
        }
        if body_end.is_none() {
            if let Some(header_len) = header_block_len(&buf) {
                if let Some(len) = content_length(&buf[..header_len])? {
                    body_end = Some(header_len + len);
                }
            }
        }
    }
    Ok(buf)
}

/// Splits the `2xx` body out of a raw HTTP/1.1 response, erroring on any other status or a malformed one.
fn parse_response(raw: &[u8]) -> Result<Vec<u8>, String> {
    let header_len = header_block_len(raw)
        .ok_or_else(|| "malformed HTTP response: no header terminator".to_string())?;
    let head = &raw[..header_len];
    let status = status_code(head)?;
    if !(200..300).contains(&status) {
        return Err(format!("download failed: HTTP status {status}"));
    }
    if content_length(head)?.is_some_and(|len| header_len + len > raw.len()) {
        return Err("download truncated: fewer body bytes than Content-Length".to_string());
    }
    Ok(raw[header_len..].to_vec())
}

/// The byte length of the header block including the terminating `\r\n\r\n`, or `None` until it has all
/// arrived.
fn header_block_len(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

/// The HTTP status code from the status line (`HTTP/1.1 200 OK`).
fn status_code(head: &[u8]) -> Result<u16, String> {
    let text = String::from_utf8_lossy(head);
    let line = text.lines().next().unwrap_or_default();
    line.split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| format!("malformed HTTP status line: {line:?}"))
}

/// The `Content-Length` header value, if present. A malformed value is an error rather than a silent
/// fall-back to reading until EOF, which could accept a truncated body.
fn content_length(head: &[u8]) -> Result<Option<usize>, String> {
    let text = String::from_utf8_lossy(head);
    for line in text.lines() {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                return value
                    .trim()
                    .parse()
                    .map(Some)
                    .map_err(|_| format!("malformed Content-Length: {value:?}"));
            }
        }
    }
    Ok(None)
}

/// Builds the `GET` request bytes, with the supplied headers plus `Host` and `Connection: close`.
fn build_request(target: &Target, headers: &[(String, String)]) -> Vec<u8> {
    let mut req = format!("GET {} HTTP/1.1\r\n", target.path);
    // Include the port in Host only when it is non-default, matching what servers expect.
    let default_port = if target.tls { 443 } else { 80 };
    if target.port == default_port {
        req.push_str(&format!("Host: {}\r\n", target.host));
    } else {
        req.push_str(&format!("Host: {}:{}\r\n", target.host, target.port));
    }
    req.push_str("User-Agent: opamp-supervisor\r\n");
    req.push_str("Accept: */*\r\n");
    for (key, value) in headers {
        // Skip a header that would collide with the ones this client controls.
        if key.eq_ignore_ascii_case("host") || key.eq_ignore_ascii_case("connection") {
            continue;
        }
        req.push_str(&format!("{key}: {value}\r\n"));
    }
    req.push_str("Connection: close\r\n\r\n");
    req.into_bytes()
}

/// Parses an `http`/`https` URL into [`Target`]. Only the scheme, host, optional port, and path/query are
/// needed; anything else (userinfo, fragment) is not expected on a package `download_url` and is not
/// handled.
fn parse_url(url: &str) -> Result<Target, String> {
    let (tls, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (false, rest)
    } else {
        return Err(format!("unsupported download URL scheme: {url}"));
    };

    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err(format!("download URL has no host: {url}"));
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => {
            let port = port
                .parse()
                .map_err(|_| format!("invalid port in download URL: {url}"))?;
            (host, port)
        }
        None => (authority, if tls { 443 } else { 80 }),
    };
    Ok(Target {
        tls,
        host: host.to_string(),
        port,
        path: path.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_tls_urls_with_and_without_ports() {
        let plain = parse_url("http://dev:4321/packages/otelcol").unwrap();
        assert!(!plain.tls);
        assert_eq!(plain.host, "dev");
        assert_eq!(plain.port, 4321);
        assert_eq!(plain.path, "/packages/otelcol");

        let tls = parse_url("https://fleet.example/packages/otelcol?v=2").unwrap();
        assert!(tls.tls);
        assert_eq!(tls.host, "fleet.example");
        assert_eq!(tls.port, 443, "https defaults to 443");
        assert_eq!(tls.path, "/packages/otelcol?v=2");

        // No path → root.
        assert_eq!(parse_url("http://host:8080").unwrap().path, "/");
        assert_eq!(parse_url("http://host").unwrap().port, 80);
    }

    #[test]
    fn rejects_unsupported_schemes_and_empty_hosts() {
        assert!(parse_url("ftp://host/x").is_err());
        assert!(parse_url("host/x").is_err());
        assert!(parse_url("http:///path").is_err());
        assert!(parse_url("http://host:notaport/x").is_err());
    }

    #[test]
    fn request_carries_host_offered_headers_and_close() {
        let target = parse_url("http://dev:4321/packages/otelcol").unwrap();
        let req = build_request(
            &target,
            &[("Authorization".to_string(), "Bearer s3cret".to_string())],
        );
        let text = String::from_utf8(req).unwrap();
        assert!(text.starts_with("GET /packages/otelcol HTTP/1.1\r\n"));
        assert!(text.contains("Host: dev:4321\r\n"), "{text}");
        assert!(text.contains("Authorization: Bearer s3cret\r\n"));
        assert!(text.ends_with("Connection: close\r\n\r\n"));
    }

    #[test]
    fn request_omits_default_port_from_host() {
        let target = parse_url("https://fleet.example/p").unwrap();
        let text = String::from_utf8(build_request(&target, &[])).unwrap();
        assert!(text.contains("Host: fleet.example\r\n"), "{text}");
    }

    #[test]
    fn parse_response_returns_the_body_on_2xx() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";
        assert_eq!(parse_response(raw).unwrap(), b"hello");
    }

    #[test]
    fn parse_response_rejects_non_2xx_and_truncation() {
        let not_found = b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";
        assert!(parse_response(not_found).unwrap_err().contains("404"));

        let short = b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nhi";
        assert!(parse_response(short).unwrap_err().contains("truncated"));

        assert!(parse_response(b"no headers here").is_err());
    }

    #[test]
    fn content_length_is_case_insensitive_and_optional() {
        assert_eq!(
            content_length(b"HTTP/1.1 200 OK\r\ncontent-length: 42\r\n\r\n").unwrap(),
            Some(42)
        );
        assert_eq!(content_length(b"HTTP/1.1 200 OK\r\n\r\n").unwrap(), None);
        assert!(content_length(b"HTTP/1.1 200 OK\r\nContent-Length: x\r\n\r\n").is_err());
    }
}
