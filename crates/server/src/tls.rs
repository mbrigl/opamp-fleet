//! The listener's TLS, and the mutual half of it (ADR-0007, ADR-0035).
//!
//! Two things live here. [`server_config`] builds the rustls configuration the listener serves
//! with — the certificate and key of ADR-0007, plus the optional client verifier that turns mutual
//! TLS on. [`PeerCertAcceptor`] is what makes that verifier usable on *this* listener: one port
//! serves OpAMP, the REST API, and the UI (ADR-0005), so client authentication has to stay optional
//! at the TLS layer — a browser presents nothing — and be required on the OpAMP route instead. The
//! acceptor carries what the handshake learned into the request, where that route can read it.

use std::future::Future;
use std::io;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use axum::{Extension, Router};
use axum_server::accept::Accept;
use axum_server::tls_rustls::{RustlsAcceptor, RustlsConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::config::TlsConfig;

/// What the TLS handshake proved about the peer, carried into every request on that connection.
///
/// `None` means the peer presented no certificate — which is fine for the UI and the REST API and
/// refused on `/v1/opamp` while a client CA is configured. A certificate that is present has
/// already been verified against that CA: rustls refuses a bad one during the handshake, so this
/// type never carries an unverified certificate.
#[derive(Clone, Debug, Default)]
pub struct PeerCertificate(pub Option<CertificateDer<'static>>);

impl PeerCertificate {
    /// The peer's certificate subject, for the record the fleet row shows. Deliberately *not* an
    /// identity check: a certificate proves fleet membership, never which Agent is speaking
    /// (ADR-0035), all the more so behind a Gateway, where it belongs to the Gateway.
    pub fn present(&self) -> bool {
        self.0.is_some()
    }
}

/// The rustls configuration the listener serves with. `client_ca_file` is what turns mutual TLS
/// on; without it this is exactly the server-authenticated TLS of ADR-0007.
///
/// The verifier is built with `allow_unauthenticated`, so the handshake succeeds with or without a
/// client certificate and one that *is* offered must verify. Requiring it here instead would
/// refuse every browser reaching the UI on the same port.
pub fn server_config(tls: &TlsConfig) -> Result<Arc<ServerConfig>, String> {
    let certs = read_certs(&tls.cert_file)?;
    let key = read_key(&tls.key_file)?;

    let builder = match &tls.client_ca_file {
        None => ServerConfig::builder().with_no_client_auth(),
        Some(ca_file) => {
            let mut roots = RootCertStore::empty();
            for cert in read_certs(ca_file)? {
                roots.add(cert).map_err(|e| {
                    format!("cannot trust a certificate from {}: {e}", ca_file.display())
                })?;
            }
            let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
                .allow_unauthenticated()
                .build()
                .map_err(|e| format!("cannot build the client verifier: {e}"))?;
            ServerConfig::builder().with_client_cert_verifier(verifier)
        }
    };

    let mut config = builder
        .with_single_cert(certs, key)
        .map_err(|e| format!("cannot use the TLS certificate and key: {e}"))?;
    // `RustlsConfig::from_config` leaves ALPN to the caller, unlike the from-PEM constructors —
    // and without it every HTTP/2 client fails the negotiation.
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

fn read_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>, String> {
    let pem = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let certs: Result<Vec<_>, _> = rustls_pemfile::certs(&mut pem.as_slice()).collect();
    let certs = certs.map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
    if certs.is_empty() {
        return Err(format!("{} contains no certificates", path.display()));
    }
    Ok(certs)
}

fn read_key(path: &Path) -> Result<PrivateKeyDer<'static>, String> {
    let pem = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    rustls_pemfile::private_key(&mut pem.as_slice())
        .map_err(|e| format!("cannot parse {}: {e}", path.display()))?
        .ok_or_else(|| format!("{} contains no private key", path.display()))
}

/// Wraps the rustls acceptor to put the handshake's [`PeerCertificate`] into every request on the
/// connection it accepted.
///
/// It is specialised to axum's `Router` on purpose: `Router::layer` is what attaches a
/// per-connection extension without pulling `tower` in as a direct dependency, and the Router is
/// what `into_make_service` hands the acceptor anyway.
#[derive(Clone)]
pub struct PeerCertAcceptor {
    inner: RustlsAcceptor,
}

impl PeerCertAcceptor {
    pub fn new(config: Arc<ServerConfig>) -> Self {
        PeerCertAcceptor {
            inner: RustlsAcceptor::new(RustlsConfig::from_config(config)),
        }
    }
}

impl<I> Accept<I, Router> for PeerCertAcceptor
where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Stream = <RustlsAcceptor as Accept<I, Router>>::Stream;
    type Service = Router;
    type Future = Pin<Box<dyn Future<Output = io::Result<(Self::Stream, Router)>> + Send>>;

    fn accept(&self, stream: I, service: Router) -> Self::Future {
        let inner = self.inner.clone();
        Box::pin(async move {
            let (stream, service) = inner.accept(stream, service).await?;
            // Whatever is here has already been verified against the configured CA — rustls
            // completed the handshake, and a certificate it could not chain never gets this far.
            let peer = stream
                .get_ref()
                .1
                .peer_certificates()
                .and_then(|chain| chain.first().cloned());
            Ok((stream, service.layer(Extension(PeerCertificate(peer)))))
        })
    }
}
