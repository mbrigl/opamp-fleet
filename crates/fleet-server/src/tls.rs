//! The listeners' TLS material (ADR-0038, ADR-0059), read from the files `[tls]` and `[enrolment]`
//! name and handed to `opamp`'s listener (ADR-0036).
//!
//! What is the Server's here is which material and why. The Agent plane requires a client
//! certificate in the handshake, verified against the client CA and, while `[enrolment]` is set,
//! the bootstrap CA. The Operator plane serves the same certificate and asks for none: a browser
//! reaches it with a password ([`crate::api`]). Which CA issued the certificate a connection
//! carries decides what it may do, and [`Issuers`] tells the two apart.

use std::path::Path;

use opamp::server::listen::{ClientAuth, ServerTls};
use opamp::tls::Identity;
use x509_parser::prelude::{FromDer, X509Certificate};

use crate::config::{EnrolmentConfig, TlsConfig};
use crate::revocation::{name_hash, Authority};

/// The material each plane serves with, and who issued what the Agent plane admits.
pub struct PlaneTls {
    pub agent: ServerTls,
    pub operator: ServerTls,
    pub issuers: Issuers,
    /// The CAs whose certificates can be revoked (ADR-0065 clause 3).
    pub authorities: Vec<Authority>,
}

/// The subjects of both CA files. Without a bootstrap CA the handshake trusts the client CA alone,
/// so every certificate it accepted belongs to a member. With one, a member is only a certificate
/// issued directly by a CA of `client_ca_file`; any other — one from the bootstrap CA, or from an
/// intermediate below either — may only enrol, so a chain cannot lift a bootstrap certificate into
/// the fleet.
#[derive(Clone, Debug, Default)]
pub struct Issuers {
    members: Vec<Vec<u8>>,
    bootstrap: Vec<Vec<u8>>,
}

/// What the certificate a connection carries makes it (ADR-0059).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Peer {
    /// A certificate from the client CA: a member of the fleet.
    Member,
    /// A bootstrap certificate: it may only enrol.
    Enrolling {
        subject: String,
        /// SHA-256 of the certificate, hex.
        fingerprint: String,
    },
}

impl Issuers {
    /// What `certificate` (DER, already verified by the handshake) makes its connection.
    #[must_use]
    pub fn classify(&self, certificate: &[u8]) -> Peer {
        use sha2::{Digest, Sha256};
        let Ok((_, parsed)) = X509Certificate::from_der(certificate) else {
            // The handshake verified it, so it parses; a certificate that does not is no member.
            return Peer::Enrolling {
                subject: String::new(),
                fingerprint: hex::encode(Sha256::digest(certificate)),
            };
        };
        let issuer = parsed.issuer().as_raw();
        let member = self.bootstrap.is_empty()
            || self
                .members
                .iter()
                .any(|subject| subject.as_slice() == issuer);
        if member {
            return Peer::Member;
        }
        Peer::Enrolling {
            subject: parsed.subject().to_string(),
            fingerprint: hex::encode(Sha256::digest(certificate)),
        }
    }
}

/// The material both planes serve with.
///
/// # Errors
/// Returns an error naming the file that cannot be read or holds nothing usable, and refuses a
/// bootstrap CA that shares a subject with the client CA, since the two could not be told apart.
pub fn server_tls(
    tls: &TlsConfig,
    enrolment: Option<&EnrolmentConfig>,
) -> Result<PlaneTls, String> {
    let cert_pem = read(&tls.cert_file)?;
    opamp::tls::certificates(&cert_pem).map_err(|e| in_file(&tls.cert_file, &e))?;
    let key_pem = read(&tls.key_file)?;
    opamp::tls::private_key(&key_pem)
        .map_err(|_| format!("{} contains no private key", tls.key_file.display()))?;
    let client_ca_file = tls
        .client_ca_file
        .as_ref()
        .ok_or("[tls] client_ca_file is required")?;
    let mut ca_pem = read(client_ca_file)?;
    let client_names = subjects(client_ca_file, &ca_pem)?;
    let mut authorities = authorities_of("client", &client_names);
    let client_subjects: Vec<Vec<u8>> = client_names.into_iter().map(|(raw, _)| raw).collect();
    let mut issuers = Issuers::default();
    if let Some(enrolment) = enrolment {
        let bootstrap_pem = read(&enrolment.bootstrap_ca_file)?;
        let bootstrap_names = subjects(&enrolment.bootstrap_ca_file, &bootstrap_pem)?;
        authorities.extend(authorities_of("bootstrap", &bootstrap_names));
        let bootstrap: Vec<Vec<u8>> = bootstrap_names.into_iter().map(|(raw, _)| raw).collect();
        if bootstrap
            .iter()
            .any(|subject| client_subjects.contains(subject))
        {
            return Err(format!(
                "[enrolment] bootstrap_ca_file {} shares a CA with [tls] client_ca_file — a \
                 bootstrap certificate must be told apart from an issued one",
                enrolment.bootstrap_ca_file.display()
            ));
        }
        if !ca_pem.ends_with(b"\n") {
            ca_pem.push(b'\n');
        }
        ca_pem.extend_from_slice(&bootstrap_pem);
        issuers.bootstrap = bootstrap;
        issuers.members = client_subjects;
    }
    let identity = Identity { cert_pem, key_pem };
    Ok(PlaneTls {
        agent: ServerTls {
            identity: identity.clone(),
            client_auth: ClientAuth::Required { ca_pem },
        },
        operator: ServerTls {
            identity,
            client_auth: ClientAuth::None,
        },
        issuers,
        authorities,
    })
}

fn authorities_of(role: &str, names: &[(Vec<u8>, String)]) -> Vec<Authority> {
    names
        .iter()
        .map(|(raw, name)| Authority {
            role: role.to_string(),
            subject: name_hash(raw),
            name: name.clone(),
        })
        .collect()
}

/// The raw subject of every certificate in a CA file, and its text.
fn subjects(path: &Path, pem: &[u8]) -> Result<Vec<(Vec<u8>, String)>, String> {
    opamp::tls::certificates(pem)
        .map_err(|e| in_file(path, &e))?
        .iter()
        .map(|der| {
            X509Certificate::from_der(der.as_ref())
                .map(|(_, cert)| (cert.subject().as_raw().to_vec(), cert.subject().to_string()))
                .map_err(|e| format!("cannot parse {}: {e}", path.display()))
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair};

    fn ca(name: &str, issuer: Option<&Issuer<'_, KeyPair>>) -> (Vec<u8>, Vec<u8>, KeyPair) {
        let key = KeyPair::generate().expect("key");
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
        params.distinguished_name.push(DnType::CommonName, name);
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let cert = match issuer {
            None => params.self_signed(&key),
            Some(issuer) => params.signed_by(&key, issuer),
        }
        .expect("ca");
        let subject = X509Certificate::from_der(cert.der())
            .expect("parse")
            .1
            .subject()
            .as_raw()
            .to_vec();
        (cert.der().to_vec(), subject, key)
    }

    fn leaf(issuer: &Issuer<'_, KeyPair>) -> Vec<u8> {
        let key = KeyPair::generate().expect("key");
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
        params.distinguished_name.push(DnType::CommonName, "host");
        params.signed_by(&key, issuer).expect("leaf").der().to_vec()
    }

    /// A bootstrap certificate issued through an intermediate is still no member: with a bootstrap
    /// CA configured, membership is a certificate issued directly by the client CA, and nothing
    /// else (ADR-0059 clause 21).
    /// Verifies: ADR-0059
    #[test]
    fn a_certificate_not_issued_by_the_client_ca_only_enrols() {
        let (client_der, client_subject, client_key) = ca("client CA", None);
        let client = Issuer::from_ca_cert_der(&client_der.into(), client_key).expect("issuer");
        let (root_der, root_subject, root_key) = ca("bootstrap CA", None);
        let root = Issuer::from_ca_cert_der(&root_der.into(), root_key).expect("issuer");
        let (intermediate_der, _, intermediate_key) = ca("bootstrap intermediate", Some(&root));
        let intermediate =
            Issuer::from_ca_cert_der(&intermediate_der.into(), intermediate_key).expect("issuer");
        let issuers = Issuers {
            members: vec![client_subject],
            bootstrap: vec![root_subject],
        };
        assert_eq!(issuers.classify(&leaf(&client)), Peer::Member);
        assert!(matches!(
            issuers.classify(&leaf(&root)),
            Peer::Enrolling { .. }
        ));
        assert!(
            matches!(
                issuers.classify(&leaf(&intermediate)),
                Peer::Enrolling { .. }
            ),
            "an intermediate lifted a bootstrap certificate into the fleet"
        );
        assert_eq!(
            Issuers::default().classify(&leaf(&intermediate)),
            Peer::Member,
            "without a bootstrap CA the handshake trusts the client CA alone"
        );
    }
}
