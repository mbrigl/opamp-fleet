//! The OpAMP Fleet Server: an OpAMP server for OpenTelemetry Collector fleets.
//!
//! The crate is split into a thin binary ([`main`](../src/main.rs)) and this library so the protocol
//! handling can be exercised from unit tests — the behavioural check ADR-0006 calls for, since we own
//! the OpAMP server side ourselves and have no reference implementation to fall back on.

pub mod api;
pub mod config;
pub mod fleet;
pub mod server;
pub mod ui;

// The OpAMP wire layer — generated Protobuf types and WebSocket framing — lives in the shared
// `opamp-proto` crate (ADR-0005). Re-export it so this crate's modules and the binary keep referring to
// `opamp::proto` / `opamp::frame` (equivalently `crate::proto` / `crate::frame`).
pub use opamp_proto::{frame, proto};
