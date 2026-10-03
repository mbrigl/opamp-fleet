//! The listener's TLS material (ADR-0012, ADR-0022), read from the files `[tls]` names and handed
//! to `opamp`'s listener (ADR-0009).
//!
//! What is the Server's here is which material and why. A configured `client_ca_file` turns mutual
//! TLS on, but only as **optional** at the TLS layer. The Agent plane carries one route that must
//! stay reachable without a certificate: the package download, which a Client fetches presenting
//! none (ADR-0028). So the OpAMP route requires the certificate instead, by reading the
//! [`PeerCertificate`](opamp::server::listen::PeerCertificate) the listener puts into each request.

use std::path::Path;

use opamp::server::listen::{ClientAuth, ServerTls};
use opamp::tls::Identity;

use crate::config::TlsConfig;

/// The material both planes serve with.
///
/// # Errors
/// Returns an error naming the file that cannot be read or holds nothing usable.
pub fn server_tls(tls: &TlsConfig) -> Result<ServerTls, String> {
    let cert_pem = read(&tls.cert_file)?;
    opamp::tls::certificates(&cert_pem).map_err(|e| in_file(&tls.cert_file, &e))?;
    let key_pem = read(&tls.key_file)?;
    opamp::tls::private_key(&key_pem)
        .map_err(|_| format!("{} contains no private key", tls.key_file.display()))?;
    let client_auth = match &tls.client_ca_file {
        None => ClientAuth::None,
        Some(ca_file) => {
            let ca_pem = read(ca_file)?;
            opamp::tls::certificates(&ca_pem).map_err(|e| in_file(ca_file, &e))?;
            ClientAuth::Optional { ca_pem }
        }
    };
    Ok(ServerTls {
        identity: Identity { cert_pem, key_pem },
        client_auth,
    })
}

fn read(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

/// What the file *means* is known here and nowhere else, so the wording names it.
fn in_file(path: &Path, error: &str) -> String {
    if error == "no certificates" {
        format!("{} contains no certificates", path.display())
    } else {
        format!("cannot parse {}: {error}", path.display())
    }
}
