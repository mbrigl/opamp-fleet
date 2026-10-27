//! What the Server and the Client implement identically that is this project's own rather than
//! OpAMP's (ADR-0008): the version this build reports and its grammar, the platform vocabulary,
//! when a certificate counts as ending and what a certificate is called (ADR-0032), and the
//! bootstrap certificate's PKCS#12 file (ADR-0022), and the host a certificate that names none is
//! taken for (ADR-0015), and the proofs a renewal and an enrolment request carry in their CSR
//! (ADR-0015 clauses 18, 20 and 21), and what a package signature and a floor signature cover
//! (ADR-0027, ADR-0028).
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
pub mod renewal;
pub mod version;
