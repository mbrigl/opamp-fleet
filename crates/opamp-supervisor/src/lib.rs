//! The Supervisor Host: a Rust process that hosts OpAMP supervisors (ADR-0008).
//!
//! Today it hosts one — the OpAMP-native **Collector Supervisor** ([`supervisor::Supervisor`]), which
//! owns an OpenTelemetry Collector, applies the configuration the Server distributes, and reports the
//! collector's real health and effective config back via a local OpAMP server ([`local_server`]). It
//! is feature-compatible with the upstream Go OpAMP Supervisor, its behavioural oracle.
//!
//! The plugin/hexagonal generalization — running *many* supervisors, and Custom Supervisors for
//! non-OpAMP Foreign Agents — is the next milestone (see [`SPECIFICATION.md`](../../docs/SPECIFICATION.md)).

pub mod collector;
pub mod host;
pub mod local_server;
pub mod supervisor;
pub mod uid;
