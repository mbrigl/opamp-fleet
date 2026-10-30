//! What the Server and the Client implement identically that is this project's own rather than
//! OpAMP's (ADR-0008): the version this build reports and its grammar, the platform vocabulary,
//! when a certificate counts as ending and what a certificate is called (ADR-0032), and the
//! bootstrap certificate's PKCS#12 file (ADR-0022), and the host a certificate that names none is
//! taken for (ADR-0015), and the proofs a renewal and an enrolment request carry in their CSR
//! (ADR-0015 clauses 18, 20 and 21), what a package signature and a floor signature cover
//! (ADR-0027, ADR-0028), the token buckets a sender is counted in (ADR-0012, ADR-0035), what a
//! client certificate is known by and every issued certificate gets (`x509`), and the links a
//! kind's tree package keeps (ADR-draft-packages-signatures-and-delivery clause 57).
//! The protocol and its transport, TLS included, are in the `opamp` crate (ADR-0008).

#![forbid(unsafe_code)]

pub mod bootstrap_proof;
pub mod csr;
pub mod ending;
pub mod host;
pub mod naming;
pub mod package;
pub mod pkcs12;
pub mod platform;
pub mod rate;
pub mod renewal;
pub mod tree;
pub mod version;
pub mod x509;
