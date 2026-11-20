//! What both planes' endpoints answer alike, and how they read the peer's address: plain HTTP
//! helpers that hold no state, so that `transport` and `api` share them without naming each other
//! (SRV-two-use-case-layers §4).

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Request};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};

/// The peer's address, as the listener put it into the request.
pub fn peer_ip(request: &Request) -> Option<std::net::IpAddr> {
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip())
}

/// The answer while the Server cannot take a decision now — too many password hashes under way on
/// the Operator plane, or the audit record unavailable: `503`, try again in a second.
pub fn busy() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::RETRY_AFTER, "1".to_string())],
        "the Server cannot take this now — retry in a moment",
    )
        .into_response()
}

/// The answer to a peer in back-off: `429` with the seconds it still has to wait.
pub fn throttled(retry_after: u64) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, retry_after.to_string())],
        "too many failed attempts from this address — retry later",
    )
        .into_response()
}
