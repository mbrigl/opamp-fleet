//! OpAMP WebSocket message framing (ADR-0006).
//!
//! The OpAMP specification defines every WebSocket message as a *header* — a Varint-encoded unsigned
//! 64-bit integer, 1–10 bytes long — followed by the Protobuf-encoded message. In this protocol
//! version the header is always `0`. `opamp-go` performed this framing for the Go server; here we own
//! it, and a decoder that assumed a bare Protobuf payload would fail silently against a real agent.
//!
//! We reuse `prost`'s LEB128 varint codec rather than hand-rolling one: the framing header uses the
//! same encoding as the Protobuf field tags `prost` already reads and writes, so there is no reason
//! to risk a second, independent implementation.

use std::fmt;

use prost::Message;

/// The largest WebSocket message we accept, framing header included. The specification requires the
/// Server to enforce a message size limit; agents send small status reports, so a generous cap turns
/// away only garbage and runaway payloads, never a legitimate report.
pub const MAX_MESSAGE_SIZE: usize = 1 << 20; // 1 MiB

/// The header value this protocol version mandates. A non-zero header is reserved for future
/// versions; receiving one means the peer speaks a protocol we do not.
const HEADER: u64 = 0;

/// Why a received frame could not be turned into a message.
#[derive(Debug)]
pub enum FrameError {
    /// The frame exceeds [`MAX_MESSAGE_SIZE`].
    TooLarge(usize),
    /// The header varint is cut off, or nothing follows it.
    Truncated,
    /// The header is not `0`; the peer speaks a protocol version we do not implement.
    UnexpectedHeader(u64),
    /// The Protobuf payload did not decode into the expected message type.
    Decode(prost::DecodeError),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::TooLarge(n) => {
                write!(
                    f,
                    "message of {n} bytes exceeds the {MAX_MESSAGE_SIZE}-byte limit"
                )
            }
            FrameError::Truncated => write!(f, "frame ended before a complete header was read"),
            FrameError::UnexpectedHeader(h) => {
                write!(
                    f,
                    "unexpected framing header {h} (this protocol version requires {HEADER})"
                )
            }
            FrameError::Decode(e) => write!(f, "cannot decode protobuf payload: {e}"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Decodes one OpAMP WebSocket frame — `varint(header) || protobuf` — into a message.
pub fn decode<M: Message + Default>(frame: &[u8]) -> Result<M, FrameError> {
    if frame.len() > MAX_MESSAGE_SIZE {
        return Err(FrameError::TooLarge(frame.len()));
    }
    let mut cursor = frame;
    let header = prost::encoding::decode_varint(&mut cursor).map_err(|_| FrameError::Truncated)?;
    if header != HEADER {
        return Err(FrameError::UnexpectedHeader(header));
    }
    // `decode_varint` advanced `cursor` past the header; the remainder is the Protobuf payload.
    M::decode(cursor).map_err(FrameError::Decode)
}

/// Encodes a message into one OpAMP WebSocket frame — `varint(0) || protobuf`.
pub fn encode<M: Message>(msg: &M) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + msg.encoded_len());
    prost::encoding::encode_varint(HEADER, &mut out);
    // Encoding into a `Vec` cannot fail: it grows to fit, and `encoded_len` reserved enough.
    msg.encode(&mut out)
        .expect("encoding a protobuf message into a Vec is infallible");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{AgentToServer, ServerToAgent};

    #[test]
    fn round_trips_a_message() {
        let msg = ServerToAgent {
            instance_uid: vec![1, 2, 3, 4],
            flags: 7,
            ..Default::default()
        };
        let frame = encode(&msg);
        // A zero header is a single 0x00 byte, so the frame is one byte longer than the payload.
        assert_eq!(frame[0], 0x00);
        assert_eq!(frame.len(), 1 + msg.encoded_len());

        let decoded: ServerToAgent = decode(&frame).expect("decode");
        assert_eq!(decoded, msg);
    }

    #[test]
    fn decodes_an_empty_payload() {
        // A bare header with no payload is a valid, default-valued message.
        let decoded: AgentToServer = decode(&[0x00]).expect("decode");
        assert_eq!(decoded, AgentToServer::default());
    }

    #[test]
    fn rejects_a_non_zero_header() {
        // Header varint 1 followed by an empty payload.
        let err = decode::<AgentToServer>(&[0x01]).expect_err("must reject");
        assert!(matches!(err, FrameError::UnexpectedHeader(1)));
    }

    #[test]
    fn rejects_a_truncated_header() {
        // 0x80 has the varint continuation bit set but no following byte.
        let err = decode::<AgentToServer>(&[0x80]).expect_err("must reject");
        assert!(matches!(err, FrameError::Truncated));
    }

    #[test]
    fn rejects_an_oversized_frame() {
        let frame = vec![0u8; MAX_MESSAGE_SIZE + 1];
        let err = decode::<AgentToServer>(&frame).expect_err("must reject");
        assert!(matches!(err, FrameError::TooLarge(n) if n == MAX_MESSAGE_SIZE + 1));
    }
}
