//! What `opamp-fleetctl pki token-init` makes on a SoftHSM2 token is what the Server signs with: the
//! Server, configured from `token.toml.fragment`, opens the token, loads both CAs by their labels
//! and signs a host certificate that `client-ca.pem` verifies
//! (ADR-draft-the-operator-program-opamp-fleetctl clause 8). The program is run as an operator runs
//! it; without `FLEET_TEST_PKCS11_MODULE` the test says so and skips.
#![cfg(unix)]

#[path = "../../fleet-server/tests/support/token.rs"]
mod token;

use std::process::Command;
use std::sync::Arc;

use fleet_server::ca::{BootstrapCa, ClientCa, UseCase};

/// The sections of `token.toml.fragment`, read as the Server reads them.
#[derive(serde::Deserialize)]
struct Fragment {
    pkcs11: fleet_server::config::Pkcs11Config,
    client_ca: fleet_server::config::ClientCaConfig,
    enrolment: fleet_server::config::EnrolmentConfig,
    audit: fleet_server::config::AuditConfig,
}

/// Verifies: ADR-draft-the-operator-program-opamp-fleetctl, ADR-draft-the-servers-signing-keys-may-live-on-a-pkcs11-token-with-softhsm2-as-the-planned-one
#[test]
fn the_server_signs_with_what_token_init_made() {
    let Some(test) = token::TestToken::named() else {
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("token");
    let made = Command::new(env!("CARGO_BIN_EXE_opamp-fleetctl"))
        .args(["pki", "token-init", "--pkcs11-module"])
        .arg(&test.config.module)
        .arg("--token-label")
        .arg(&test.config.token_label)
        .arg("--pin-file")
        .arg(&test.config.pin_file)
        .arg("--out")
        .arg(&out)
        .output()
        .expect("run opamp-fleetctl");
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
    let fragment: Fragment = toml::from_str(
        &std::fs::read_to_string(out.join("token.toml.fragment")).expect("fragment"),
    )
    .expect("the fragment reads as server.toml's sections");
    assert!(fragment.client_ca.key_file.is_none());
    let audit_label = fragment.audit.key_label.clone().expect("an audit key");

    let token = fleet_server::pkcs11::Token::open(&fragment.pkcs11).expect("open the token");
    token
        .key("[audit] key_label", &audit_label)
        .expect("the audit key");
    let recorded = Arc::new(token::Recorded::default());
    token.set_audit(recorded.clone());
    let ca = ClientCa::from_config_on(&fragment.client_ca, Some(&token)).expect("the client CA");
    assert!(
        BootstrapCa::from_config_on(&fragment.enrolment, Some(&fragment.client_ca), Some(&token))
            .expect("the bootstrap CA")
            .is_some(),
        "the Server issues bootstrap certificates with the token's key"
    );
    let csr = rcgen::CertificateParams::new(Vec::<String>::new())
        .expect("params")
        .serialize_request(&rcgen::KeyPair::generate().expect("key"))
        .expect("csr")
        .pem()
        .expect("pem");
    let signed = ca
        .sign(
            &csr,
            "host-1",
            &fleet_server::revocation::SubjectValues::default(),
            UseCase::MemberCsr,
        )
        .expect("signed through the token");
    let der = |pem: &str| {
        opamp::tls::certificates(pem.as_bytes()).expect("pem")[0]
            .as_ref()
            .to_vec()
    };
    let ca_pem = std::fs::read_to_string(out.join("client-ca.pem")).expect("client CA");
    let (leaf, issuer) = (der(&signed.pem), der(&ca_pem));
    use x509_parser::prelude::{FromDer, X509Certificate};
    let (_, leaf) = X509Certificate::from_der(&leaf).expect("leaf");
    let (_, issuer) = X509Certificate::from_der(&issuer).expect("issuer");
    assert!(leaf.verify_signature(Some(issuer.public_key())).is_ok());
    assert_eq!(recorded.events("pkcs11.signed").len(), 1);
}
