//! What the Server and the Client implement identically that is this project's own rather than
//! OpAMP's (DEV-six-crates-tokio-axum): the version this build reports and its grammar, the platform vocabulary,
//! when a certificate counts as ending and what a certificate is called (OPS-operator-program-fleetctl), and the
//! bootstrap certificate's PKCS#12 file (SEC-bootstrap-certificates-and-enrolment), and the host a certificate that names none is
//! taken for (CLT-modes-and-gateway), and the proofs a renewal and an enrolment request carry in their CSR
//! (CLT-modes-and-gateway §18, §20 and §21), what a package signature and a floor signature cover
//! (PKG-signed-packages-and-delivery, PKG-versions-high-water-mark-floor), the token buckets a sender is counted in (SEC-tls13-bounded-input-chosen-planes, CLT-gateway-rate-limits-hosts), where the
//! Server serves the Gateways' revocation list and the shape of its body (SEC-admission-by-client-certificate §34), and what a
//! client certificate is known by and every issued certificate gets (`x509`). What only the Client
//! and the operator tool share is in `fleet-util` (DEV-six-crates-tokio-axum).
//! The protocol and its transport, TLS included, are in the `opamp` crate (DEV-six-crates-tokio-axum).

#![forbid(unsafe_code)]

pub mod bootstrap_proof;
pub mod csr;
pub mod ending;
pub mod host;
pub mod naming;
pub mod package;
#[cfg(feature = "pkcs11")]
pub mod pkcs11;
pub mod pkcs12;
pub mod platform;
pub mod rate;
pub mod renewal;
pub mod revocation_list;
pub mod version;
pub mod x509;
