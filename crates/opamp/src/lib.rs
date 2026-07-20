//! Shared OpAMP wire contract and domain helpers for OpAMP Fleet.
//!
//! This crate is the single owner of the OpAMP wire types both deployables exchange (the `server`
//! and the `supervisor` binaries) plus the shared domain helpers named in the specification
//! vocabulary — Instance UID, Config hash, Capabilities.
//!
//! ADR-0003 establishes this crate as the shared library of the workspace. The wire types
//! (prost-generated from the vendored `opamp-spec` proto) and the domain helpers are filled in by
//! ADR-0004; the crate is intentionally a thin placeholder until then.

/// The OpAMP protocol version this project targets on the wire.
pub const OPAMP_SPEC_VERSION: &str = "0.12.0";
