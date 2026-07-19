//! The Supervisor Host's own health-check endpoint (ADR-0013).
//!
//! An optional, loopback HTTP endpoint that answers `200` when the Host can do its job and `503` when it
//! cannot — the same two conditions the Go supervisor checks: it can **persist state to disk** (the
//! storage directory is writable) and it can **generate a configuration** (its configuration file still
//! reads and parses). It is a liveness/readiness probe for a local orchestrator, not a public surface, so
//! it is plain HTTP and served by a minimal hand-rolled responder — no HTTP framework dependency.

use std::path::{Path, PathBuf};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{error, info, warn};

/// Serves the health-check endpoint on `endpoint` until the process stops. Binding failure is logged and
/// the endpoint simply does not come up — it must never take the Host down.
pub async fn serve(endpoint: String, storage_dir: PathBuf, config_path: PathBuf) {
    let listener = match TcpListener::bind(&endpoint).await {
        Ok(listener) => listener,
        Err(e) => {
            error!(endpoint, error = %e, "cannot bind the health-check endpoint");
            return;
        }
    };
    info!(endpoint, "health-check endpoint listening");
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(handle(stream, storage_dir.clone(), config_path.clone()));
            }
            Err(e) => warn!(error = %e, "health-check accept failed"),
        }
    }
}

/// Answers one connection: reads (and ignores) the request head, then writes `200` or `503` by the
/// current health, and closes.
async fn handle(mut stream: TcpStream, storage_dir: PathBuf, config_path: PathBuf) {
    // Drain the request head best-effort so the client is not reset before it reads the response.
    let mut scratch = [0u8; 1024];
    let _ = stream.read(&mut scratch).await;

    let response = http_response(healthy(&storage_dir, &config_path));
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Whether the Host can do its job: persist to its storage directory and read+parse its configuration.
fn healthy(storage_dir: &Path, config_path: &Path) -> bool {
    storage_writable(storage_dir) && config_generatable(config_path)
}

/// Whether the storage directory can be written — the "persist state to disk" condition. Probes by
/// writing and removing a file, which is what catches a read-only or full volume.
fn storage_writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".healthcheck-probe");
    match std::fs::write(&probe, b"ok") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Whether the Host configuration still reads and parses — the "generate the agent's configuration"
/// condition: the Host derives every managed agent's config from it, so an unreadable or invalid file
/// means it can no longer produce one.
fn config_generatable(config_path: &Path) -> bool {
    match std::fs::read(config_path) {
        Ok(bytes) => crate::config::HostConfig::parse(&bytes).is_ok(),
        Err(_) => false,
    }
}

/// A minimal HTTP/1.1 response for the probe: `200 ok` when healthy, `503 unhealthy` otherwise.
fn http_response(healthy: bool) -> String {
    let (status, body) = if healthy {
        ("200 OK", "ok\n")
    } else {
        ("503 Service Unavailable", "unhealthy\n")
    };
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_writable_probes_the_directory() {
        let dir = std::env::temp_dir().join("opamp-sup-health-ok");
        assert!(storage_writable(&dir));
        // The probe file is cleaned up, not left behind.
        assert!(!dir.join(".healthcheck-probe").exists());
    }

    #[test]
    fn config_generatable_tracks_the_config_file() {
        let good = std::env::temp_dir().join("opamp-sup-health-good.yaml");
        std::fs::write(
            &good,
            b"supervisors:\n  - type: custom\n    name: a\n    command: [\"true\"]\n    config_path: /tmp/a\n",
        )
        .unwrap();
        assert!(config_generatable(&good));
        let _ = std::fs::remove_file(&good);

        assert!(!config_generatable(Path::new(
            "/tmp/opamp-sup-health-absent.yaml"
        )));

        let bad = std::env::temp_dir().join("opamp-sup-health-bad.yaml");
        std::fs::write(&bad, b"this: is: not: valid: yaml:\n").unwrap();
        assert!(!config_generatable(&bad));
        let _ = std::fs::remove_file(&bad);
    }

    #[test]
    fn http_response_reflects_health() {
        assert!(http_response(true).starts_with("HTTP/1.1 200 OK"));
        assert!(http_response(false).starts_with("HTTP/1.1 503"));
    }
}
