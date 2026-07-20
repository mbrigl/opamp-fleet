//! Instance UID — the identity of an Agent instance (specification vocabulary).
//!
//! Generated as a UUID v7 and stable across restarts. On the wire it is the 16-byte
//! `instance_uid` field; in HTTP headers and logs it is the canonical UUID string.

use std::fmt;

use uuid::Uuid;

/// The identity of an Agent instance: a UUID v7, 16 bytes on the wire.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct InstanceUid(Uuid);

impl InstanceUid {
    /// Generate a fresh Instance UID using the UUID v7 spec.
    #[must_use]
    pub fn generate() -> Self {
        Self(Uuid::now_v7())
    }

    /// Build an Instance UID from its 16-byte wire representation.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(Uuid::from_bytes(bytes))
    }

    /// Build an Instance UID from a wire byte slice, if it is exactly 16 bytes long.
    #[must_use]
    pub fn from_slice(bytes: &[u8]) -> Option<Self> {
        <[u8; 16]>::try_from(bytes).ok().map(Self::from_bytes)
    }

    /// Parse an Instance UID from its canonical UUID string form.
    ///
    /// # Errors
    /// Returns [`uuid::Error`] if `s` is not a valid UUID.
    pub fn parse_str(s: &str) -> Result<Self, uuid::Error> {
        Uuid::parse_str(s).map(Self)
    }

    /// The 16-byte wire representation.
    #[must_use]
    pub fn as_bytes(&self) -> [u8; 16] {
        *self.0.as_bytes()
    }

    /// The 16-byte wire representation as an owned vector (for protobuf `bytes` fields).
    #[must_use]
    pub fn to_vec(&self) -> Vec<u8> {
        self.0.as_bytes().to_vec()
    }
}

impl fmt::Display for InstanceUid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Canonical hyphenated UUID, e.g. for the OpAMP-Instance-UID header.
        write!(f, "{}", self.0.as_hyphenated())
    }
}

impl fmt::Debug for InstanceUid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "InstanceUid({})", self.0.as_hyphenated())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_round_trip() {
        let uid = InstanceUid::generate();
        assert_eq!(uid.to_vec().len(), 16);
        assert_eq!(InstanceUid::from_bytes(uid.as_bytes()), uid);
        assert_eq!(InstanceUid::from_slice(&uid.to_vec()), Some(uid));
    }

    #[test]
    fn string_round_trip() {
        let uid = InstanceUid::generate();
        assert_eq!(InstanceUid::parse_str(&uid.to_string()).unwrap(), uid);
    }

    #[test]
    fn wrong_length_slice_is_rejected() {
        assert_eq!(InstanceUid::from_slice(&[0u8; 15]), None);
    }
}
