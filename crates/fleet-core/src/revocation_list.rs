//! The list of revoked certificates the Server hands a host marked as a Gateway, and the Gateway
//! reads (SEC-admission-by-client-certificate §34, CLT-modes-and-gateway §14): where it is served
//! and the shape of its JSON body. The Server writes the body and the Gateway parses it, so both
//! ends name the same types (DEV-six-crates-tokio-axum §16).
//!
//! The body is `{"certificates":[{"issuer":…,"serial":…},…]}`: each certificate by the SHA-256 of
//! its issuer's DER-encoded name and its serial in lowercase hex ([`crate::x509::read::id`]). The
//! fields are written in this order, which the `ETag` the Server derives from the body depends on.

use serde::{Deserialize, Serialize};

/// Where the Server serves the list, on its Agent plane.
pub const PATH: &str = "/v1/gateway/revocations";

/// The list's body.
#[derive(Debug, Serialize, Deserialize)]
pub struct ListBody {
    /// The revoked certificates.
    pub certificates: Vec<Entry>,
}

/// One revoked certificate.
#[derive(Debug, Serialize, Deserialize)]
pub struct Entry {
    /// The SHA-256 of the issuer's DER-encoded name, in hex.
    pub issuer: String,
    /// The serial, in lowercase hex without leading zeros.
    pub serial: String,
}
