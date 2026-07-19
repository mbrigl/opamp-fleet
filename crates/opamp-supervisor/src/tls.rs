//! The TLS connector for the OpAMP `wss://` client (ADR-0012).
//!
//! For a plain `ws://` server this is unused. For `wss://` the supervisor validates the server
//! certificate against either the platform's default roots (the common case), a **custom CA** given in
//! the configuration, or — with the **insecure** development option — nothing at all.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error, RootCertStore, SignatureScheme};
use tokio_tungstenite::Connector;

/// Builds the TLS connector for the OpAMP client. Returns `None` for the platform-default roots (used
/// automatically for a `wss://` server), `Some` for a custom CA or the insecure option. The rustls
/// crypto provider must already be installed for the process (the binary does this at startup).
pub fn connector(ca_cert: Option<&[u8]>, insecure: bool) -> Result<Option<Connector>, String> {
    if insecure {
        let config = ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyServerCert))
            .with_no_client_auth();
        return Ok(Some(Connector::Rustls(Arc::new(config))));
    }
    let Some(pem) = ca_cert else {
        return Ok(None);
    };
    let mut roots = RootCertStore::empty();
    let mut added = 0;
    for cert in rustls_pemfile::certs(&mut &pem[..]) {
        let cert = cert.map_err(|e| format!("cannot read CA certificate: {e}"))?;
        roots
            .add(cert)
            .map_err(|e| format!("cannot add CA certificate: {e}"))?;
        added += 1;
    }
    if added == 0 {
        return Err("the configured TLS CA certificate contained no certificates".to_string());
    }
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Some(Connector::Rustls(Arc::new(config))))
}

/// A certificate verifier that accepts anything — only for the `insecure` development option. Signature
/// checks defer to the ring provider so the handshake still completes.
#[derive(Debug)]
struct AcceptAnyServerCert;

impl ServerCertVerifier for AcceptAnyServerCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    #[test]
    fn no_ca_and_not_insecure_uses_default_roots() {
        provider();
        assert!(connector(None, false).unwrap().is_none());
    }

    #[test]
    fn insecure_builds_a_connector() {
        provider();
        assert!(connector(None, true).unwrap().is_some());
    }

    #[test]
    fn an_empty_ca_bundle_is_rejected() {
        provider();
        assert!(connector(Some(b"not a certificate"), false).is_err());
    }
}
