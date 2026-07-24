//! Two things live here. [`server_config`] builds the rustls configuration both listeners serve
//! TLS on. [`PeerCertAcceptor`] is what makes that verifier usable: client authentication stays
//! optional at the TLS layer and is required on the OpAMP *route* instead. Since ADR-0012 the
//! browser is no longer the reason — the UI has its own listener — but the Agent plane still
//! carries one route that must stay reachable without a certificate: the package download, which a
//! Client fetches presenting none (ADR-0019). Requiring the certificate in the handshake is a
//! separate decision, and that route is what it has to answer for. The acceptor carries what the
//! handshake learned into the request, where the OpAMP route can read it.
/// `None` means the peer presented no certificate — which is fine on the package download route
/// and refused on `/v1/opamp` while a client CA is configured. A certificate that is present has
    opamp::pem::certificates(&pem).map_err(|e| {
        // What the file *means* is known here and nowhere else, so the wording stays (ADR-0011).
        if e == "no certificates" {
            format!("{} contains no certificates", path.display())
        } else {
            format!("cannot parse {}: {e}", path.display())
        }
    })
    opamp::pem::private_key(&pem).map_err(|_| format!("{} contains no private key", path.display()))
            inner: rustls_acceptor(config),
/// The acceptor both planes hand their TLS connections to, with the handshake deadline stated
/// rather than inherited (ADR-0012). It is `axum_server`'s own default value; naming it here is
/// what makes it a decision, and what keeps the Operator plane — which needs no peer certificate
/// and therefore no wrapper — bounded by the same one.
pub fn rustls_acceptor(config: Arc<ServerConfig>) -> RustlsAcceptor {
    RustlsAcceptor::new(RustlsConfig::from_config(config))
        .handshake_timeout(crate::listen::TLS_HANDSHAKE_TIMEOUT)
}

