//! What the Server and the Client implement identically that is this project's own rather than
//! OpAMP's (ADR-0011, ADR-0031): the version this build reports and its grammar, the PEM readers,
//! and the platform vocabulary. The protocol itself is in the `opamp` crate.

pub mod pem;
pub mod platform;
pub mod version;
