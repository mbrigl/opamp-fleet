//! The shared OpAMP wire layer (ADR-0006).
//!
//! Both ends of the protocol — the [`server`](../server) and the [`client`](../client) — need the
//! same OpAMP message types and the same WebSocket framing. Generating and framing them **once**,
//! here, keeps a single vendored copy of the Baseline's `.proto` and one codegen invocation, so the
//! two ends cannot drift on the wire format.

/// The OpAMP protobuf types, generated from the vendored Baseline schema by
/// [`build.rs`](../build.rs). The `.proto` package is `opamp.proto.v1`; the generated types are
/// exposed flat so callers write `opamp::proto::AgentToServer`.
pub mod proto {
    // Generated code: we do not control its formatting or doc-comment style, so lint it loosely.
    #![allow(clippy::all, clippy::pedantic)]
    include!(concat!(env!("OUT_DIR"), "/opamp.proto.v1.rs"));
}

pub mod frame;
pub mod uid;
