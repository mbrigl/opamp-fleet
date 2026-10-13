//! What the Server and the Client implement identically that is this project's own rather than
//! OpAMP's (ADR-0010): the version this build reports and its grammar, the platform vocabulary,
//! when a certificate counts as ending and what a certificate is called (ADR-0028).
//! The protocol and its transport, TLS included, are in the `opamp` crate (ADR-0010).

pub mod ending;
pub mod naming;
pub mod package;
pub mod platform;
pub mod renewal;
pub mod version;
