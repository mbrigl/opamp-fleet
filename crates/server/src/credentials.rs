//! The credential check both planes use (ADR-0013, ADR-0067).
//!
//! One primitive, because the two planes ask the same question of a request — does this
//! `Authorization` header match something configured? — and the *answer* is what differs: the Agent
//! plane pairs it with a client certificate (ADR-0035), the Operator plane guards a browser. What
//! must not differ is how the comparison is made, which is why it is written once.

use axum::http::{header, HeaderMap};

/// Accepted `Authorization` values, precomputed, plus the challenge a refusal carries.
pub struct Credentials {
    /// Every header value that authenticates, in full — so the request path is one string
    /// comparison per candidate and never a parse.
    accepted: Vec<String>,
    /// The `WWW-Authenticate` value a `401` answers with (RFC 9110).
    challenge: String,
}

impl Credentials {
    pub fn new(accepted: Vec<String>, challenge: String) -> Self {
        Credentials {
            accepted,
            challenge,
        }
    }

    pub fn challenge(&self) -> &str {
        &self.challenge
    }

    /// Whether the request's `Authorization` header matches any configured credential.
    pub fn permits(&self, headers: &HeaderMap) -> bool {
        let presented = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        self.accepted
            .iter()
            // Constant-time per candidate, so a comparison never leaks how far it matched.
            .any(|accepted| {
                constant_time_eq::constant_time_eq(accepted.as_bytes(), presented.as_bytes())
            })
    }
}
