//! Shared-token authentication for both listeners (ADR-0012).
//!
//! When an auth token is configured, every request — the OpAMP WebSocket upgrade on `:4320` and every
//! UI/API request on `:4321` — must carry `Authorization: Bearer <token>`. When no token is configured
//! the server authenticates nobody, exactly as before (ADR-0006/0007); the token is opt-in.

use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// Whether a request is authorized: always, when no token is configured; otherwise only with a matching
/// `Authorization: Bearer <token>` header. The comparison is constant-time so a wrong token cannot be
/// recovered by timing.
pub fn authorized(expected: &Option<String>, headers: &HeaderMap) -> bool {
    match expected {
        None => true,
        Some(token) => bearer(headers).is_some_and(|got| constant_time_eq(got, token)),
    }
}

/// The bearer token from an `Authorization` header, if present and well-formed.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

/// A length-independent, constant-time byte comparison, so token checking does not leak the token
/// through response timing.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// An `axum` middleware that rejects unauthenticated requests with `401` when a token is configured — the
/// guard on the UI/API listener (ADR-0012), including the mutating restart and config-write endpoints.
pub async fn require_auth(
    State(expected): State<Option<String>>,
    request: Request,
    next: Next,
) -> Response {
    if authorized(&expected, request.headers()) {
        next.run(request).await
    } else {
        (StatusCode::UNAUTHORIZED, "unauthorized").into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_auth(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, value.parse().unwrap());
        h
    }

    #[test]
    fn no_token_configured_allows_everyone() {
        assert!(authorized(&None, &HeaderMap::new()));
    }

    #[test]
    fn a_configured_token_requires_a_matching_bearer() {
        let expected = Some("s3cret".to_string());
        assert!(authorized(&expected, &with_auth("Bearer s3cret")));
        assert!(!authorized(&expected, &with_auth("Bearer wrong")));
        assert!(
            !authorized(&expected, &with_auth("s3cret")),
            "missing Bearer prefix"
        );
        assert!(
            !authorized(&expected, &HeaderMap::new()),
            "no header at all"
        );
    }

    #[test]
    fn constant_time_eq_matches_only_equal_strings() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "abcd"));
    }
}
