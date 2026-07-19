//! The shared OpAMP wire layer (ADR-0005).
//!
//! Both ends of the protocol — the [`opamp-server`](../opamp-server) binary and the Rust Supervisor
//! ([`opamp-supervisor`](../opamp-supervisor)) — need the same OpAMP message types and the same
//! WebSocket framing. Generating and framing them **once**, here, keeps a single vendored copy of the
//! specification's `.proto` and one `protoc` invocation, so the two implementations cannot drift on the
//! wire format (the drift hazard ADR-0006 guards against, now shared across two crates).

/// The OpAMP protobuf types, generated from `proto/opamp/v1` by [`build.rs`](../build.rs). The `.proto`
/// package is `opamp.proto.v1`, so prost nests the types under those modules; the flat re-export lets
/// callers write `opamp_proto::proto::AgentToServer`.
pub mod proto {
    pub mod opamp {
        pub mod proto {
            // Generated code: we do not control its formatting or doc-comment style, so lint it loosely.
            #[allow(clippy::all, clippy::pedantic)]
            pub mod v1 {
                include!(concat!(env!("OUT_DIR"), "/opamp.proto.v1.rs"));
            }
        }
    }
    pub use opamp::proto::v1::*;
}

pub mod frame;
