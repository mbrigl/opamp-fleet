//! The listener an OpAMP endpoint is served on (ADR-0009): TLS from the material the application
//! hands it, the bounds on connection setup of ADR-0012, and what the handshake learned carried
//! into every request.
//!
//! Every other limit an endpoint enforces starts at a request. A peer that completes the TCP
//! connection, sends a request line and falls silent reaches none of them, and would hold a task and
//! hyper's read buffer for as long as it likes. hyper defaults its HTTP/1 `header_read_timeout` to
//! 30 seconds, but the default is inert while no `Timer` is installed, and neither `axum::serve`
//! nor `axum_server` installs one. Configuring the timeout without a timer is worse: hyper panics.
//! So [`Listener::serve`] installs both, and every OpAMP listener goes through it.
//!
//! The bound is on **connection setup**, not on a request. A WebSocket session is long-lived and a
//! package download is as large as its artifact; a request timeout would break exactly those and
//! leave the slow peer untouched.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ConnectInfo;
use axum::{Extension, Router};
use axum_server::accept::Accept;
use axum_server::tls_rustls::{RustlsAcceptor, RustlsConfig};
use axum_server::Server;
use hyper_util::rt::TokioTimer;
use rustls::pki_types::CertificateDer;
use rustls::server::WebPkiClientVerifier;
use rustls::ServerConfig;
use tokio::net::TcpStream;

pub use axum_server::Handle;

use crate::tls::{certificates, private_key, root_store, Identity};

/// How long a connection may take to send its request line and headers: hyper's own default,
/// with the timer that makes it take effect. It bounds the request line and the headers only.
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the TLS handshake may take before the connection is dropped — `axum_server`'s default,
/// stated so that every listener visibly has one.
pub const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Whether a peer has to present a client certificate, and the CA it is verified against.
#[derive(Clone, Debug)]
pub enum ClientAuth {
    /// Server-authenticated TLS only.
    None,
    /// A certificate the peer offers must verify, but the handshake succeeds without one. The
    /// application decides per route whether to require it, through [`PeerCertificate`]. A
    /// listener that also serves a route reached without a certificate needs this.
    Optional { ca_pem: Vec<u8> },
    /// The handshake fails without a certificate that verifies. For a listener that speaks only
    /// OpAMP, where the CA is an access-control boundary.
    Required { ca_pem: Vec<u8> },
}

/// What a listener serves TLS with.
#[derive(Clone, Debug)]
pub struct ServerTls {
    /// The certificate chain and key this listener presents.
    pub identity: Identity,
    pub client_auth: ClientAuth,
}

impl ServerTls {
    /// The rustls configuration for this material, with HTTP/2 and HTTP/1.1 offered by ALPN.
    ///
    /// # Errors
    /// Returns an error naming the part — certificate, key, or client CA — that cannot be used.
    pub fn rustls_config(&self) -> Result<Arc<ServerConfig>, String> {
        let certs = certificates(&self.identity.cert_pem)
            .map_err(|e| format!("the TLS certificate: {e}"))?;
        let key = private_key(&self.identity.key_pem).map_err(|e| format!("the TLS key: {e}"))?;
        let builder = match &self.client_auth {
            ClientAuth::None => ServerConfig::builder().with_no_client_auth(),
            ClientAuth::Optional { ca_pem } | ClientAuth::Required { ca_pem } => {
                let roots = root_store(ca_pem).map_err(|e| format!("the client CA: {e}"))?;
                let verifier = WebPkiClientVerifier::builder(Arc::new(roots));
                let verifier = if matches!(self.client_auth, ClientAuth::Optional { .. }) {
                    verifier.allow_unauthenticated()
                } else {
                    verifier
                };
                let verifier = verifier
                    .build()
                    .map_err(|e| format!("cannot build the client verifier: {e}"))?;
                ServerConfig::builder().with_client_cert_verifier(verifier)
            }
        };
        let mut config = builder
            .with_single_cert(certs, key)
            .map_err(|e| format!("cannot use the TLS certificate and key: {e}"))?;
        // `RustlsConfig::from_config` leaves ALPN to the caller, unlike the from-PEM constructors,
        // and without it every HTTP/2 client fails the negotiation.
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Ok(Arc::new(config))
    }
}

/// What the TLS handshake proved about the peer, in every request on that connection.
///
/// `None` means the peer presented no certificate, or the listener serves no TLS. A certificate
/// that is present has been verified against the client CA: rustls refuses a bad one during the
/// handshake, so this type never carries an unverified certificate. It proves membership of
/// whoever holds the CA's certificates, never which Agent is speaking.
#[derive(Clone, Debug, Default)]
pub struct PeerCertificate(pub Option<CertificateDer<'static>>);

impl PeerCertificate {
    #[must_use]
    pub fn present(&self) -> bool {
        self.0.is_some()
    }
}

/// A bound TCP listener, ready to serve a router.
pub struct Listener {
    listener: std::net::TcpListener,
    handle: Handle,
    tls: Option<Arc<ServerConfig>>,
    header_read_timeout: Duration,
}

impl Listener {
    /// A plain listener. `handle` drains it on shutdown, and one handle may drain several.
    #[must_use]
    pub fn new(listener: std::net::TcpListener, handle: Handle) -> Self {
        Listener {
            listener,
            handle,
            tls: None,
            header_read_timeout: HEADER_READ_TIMEOUT,
        }
    }

    /// Serves TLS with `config`, usually built by [`ServerTls::rustls_config`].
    #[must_use]
    pub fn with_tls(mut self, config: Arc<ServerConfig>) -> Self {
        self.tls = Some(config);
        self
    }

    /// Replaces [`HEADER_READ_TIMEOUT`] — the seam a test drives, since nothing else makes a
    /// 30-second bound observable in a suite.
    #[must_use]
    pub fn with_header_read_timeout(mut self, timeout: Duration) -> Self {
        self.header_read_timeout = timeout;
        self
    }

    /// Serves `router` until the handle shuts the listener down. Every request carries the peer's
    /// address as `ConnectInfo<SocketAddr>` and the handshake's [`PeerCertificate`].
    ///
    /// # Errors
    /// Returns the I/O error that stopped the listener.
    pub async fn serve(self, router: Router) -> io::Result<()> {
        let server = axum_server::from_tcp(self.listener).handle(self.handle);
        match self.tls {
            None => {
                bounded(server.acceptor(Plain), self.header_read_timeout)
                    .serve(router.into_make_service())
                    .await
            }
            Some(config) => {
                let acceptor = RustlsAcceptor::new(RustlsConfig::from_config(config))
                    .handshake_timeout(TLS_HANDSHAKE_TIMEOUT);
                bounded(server.acceptor(Tls(acceptor)), self.header_read_timeout)
                    .serve(router.into_make_service())
                    .await
            }
        }
    }
}

/// The header-read bound, with the timer it needs first: without the timer hyper discards the
/// default, and with the timeout set but no timer it panics.
fn bounded<A>(mut server: Server<A>, header_read_timeout: Duration) -> Server<A> {
    server
        .http_builder()
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(header_read_timeout);
    server
}

/// The per-connection extensions, attached to the router that serves the connection. The router is
/// what `into_make_service` hands an acceptor, and `Router::layer` attaches an extension without a
/// direct dependency on `tower`.
fn with_connection(
    service: Router,
    peer: Option<SocketAddr>,
    certificate: Option<CertificateDer<'static>>,
) -> Router {
    let service = service.layer(Extension(PeerCertificate(certificate)));
    match peer {
        Some(peer) => service.layer(Extension(ConnectInfo(peer))),
        None => service,
    }
}

#[derive(Clone)]
struct Plain;

impl Accept<TcpStream, Router> for Plain {
    type Stream = TcpStream;
    type Service = Router;
    type Future = std::future::Ready<io::Result<(TcpStream, Router)>>;

    fn accept(&self, stream: TcpStream, service: Router) -> Self::Future {
        let peer = stream.peer_addr().ok();
        std::future::ready(Ok((stream, with_connection(service, peer, None))))
    }
}

#[derive(Clone)]
struct Tls(RustlsAcceptor);

impl Accept<TcpStream, Router> for Tls {
    type Stream = <RustlsAcceptor as Accept<TcpStream, Router>>::Stream;
    type Service = Router;
    type Future = Pin<Box<dyn Future<Output = io::Result<(Self::Stream, Router)>> + Send>>;

    fn accept(&self, stream: TcpStream, service: Router) -> Self::Future {
        let inner = self.0.clone();
        Box::pin(async move {
            let peer = stream.peer_addr().ok();
            let (stream, service) = inner.accept(stream, service).await?;
            // What is here was verified against the client CA: rustls completed the handshake, and
            // a certificate it could not chain never gets this far.
            let certificate = stream
                .get_ref()
                .1
                .peer_certificates()
                .and_then(|chain| chain.first().cloned());
            Ok((stream, with_connection(service, peer, certificate)))
        })
    }
}
