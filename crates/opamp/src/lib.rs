//! Shared OpAMP wire contract and domain helpers for OpAMP Fleet.
//!
//! This crate is the single owner of the OpAMP wire types both deployables exchange (the `server`
//! and the `supervisor` binaries) plus the shared domain helpers named in the specification
//! vocabulary — Instance UID, Config hash, Capabilities. The wire types are generated at build time
//! from the vendored `open-telemetry/opamp-spec` protobuf (ADR-0004).

/// The OpAMP protocol version this project targets on the wire.
pub const OPAMP_SPEC_VERSION: &str = "0.12.0";

/// The generated OpAMP protobuf types (package `opamp.proto.v1`).
pub mod v1 {
    // Generated code — hold it to its own standards, not ours.
    #![allow(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
    #![allow(rustdoc::all)]
    include!(concat!(env!("OUT_DIR"), "/opamp.proto.v1.rs"));
}

mod capabilities;
mod config;
mod instance_uid;
pub mod transport;

pub use capabilities::{required_agent_capabilities, server_capabilities};
pub use config::{config_hash, hex};
pub use instance_uid::InstanceUid;

/// Encode any OpAMP protobuf message into a byte buffer for the wire.
///
/// Encoding into an in-memory `Vec` cannot fail, so this does not return a `Result`.
#[must_use]
pub fn encode<M: prost::Message>(message: &M) -> Vec<u8> {
    let mut buf = Vec::with_capacity(message.encoded_len());
    message
        .encode(&mut buf)
        .expect("encoding into a Vec is infallible");
    buf
}

/// Decode an OpAMP protobuf message received from the wire.
///
/// # Errors
/// Returns [`prost::DecodeError`] if `buf` is not a valid encoding of `M`.
pub fn decode<M: prost::Message + Default>(buf: &[u8]) -> Result<M, prost::DecodeError> {
    M::decode(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use v1::{AgentToServer, ServerToAgent};

    #[test]
    fn agent_to_server_round_trips_through_the_wire() {
        let uid = InstanceUid::generate();
        let msg = AgentToServer {
            instance_uid: uid.to_vec(),
            sequence_num: 7,
            capabilities: required_agent_capabilities(),
            ..Default::default()
        };

        let bytes = encode(&msg);
        let decoded: AgentToServer = decode(&bytes).expect("decode");

        assert_eq!(decoded.instance_uid, uid.to_vec());
        assert_eq!(decoded.sequence_num, 7);
        assert_eq!(decoded.capabilities, required_agent_capabilities());
    }

    #[test]
    fn decode_rejects_garbage() {
        // 0xFF is an invalid protobuf tag/wire-type lead byte.
        let err = decode::<ServerToAgent>(&[0xff, 0xff, 0xff]);
        assert!(err.is_err());
    }
}
