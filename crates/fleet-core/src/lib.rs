//! What the Server and the Client implement identically that is this project's own rather than
//! OpAMP's (ADR-0037): the version this build reports and its grammar, and the platform vocabulary. The
//! protocol and its transport, TLS included, are in the `opamp` crate (ADR-0036).

pub mod package;
pub mod platform;
pub mod renewal;
pub mod version;
