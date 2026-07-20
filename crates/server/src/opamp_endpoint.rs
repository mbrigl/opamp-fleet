//! The OpAMP HTTP endpoint (ADR-0004 transport, ADR-0005 runtime).
//!
//! Accepts an `AgentToServer` protobuf and returns a `ServerToAgent` protobuf, driving the fleet
//! control loop in [`crate::fleet::Fleet`].

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use opamp::transport::CONTENT_TYPE_PROTOBUF;
use opamp::v1::AgentToServer;
use opamp::InstanceUid;
use tracing::{debug, warn};

use crate::fleet::Fleet;

/// Handle `POST /v1/opamp`.
pub async fn handle(State(fleet): State<Arc<Fleet>>, body: Bytes) -> impl IntoResponse {
    let message = match opamp::decode::<AgentToServer>(&body) {
        Ok(message) => message,
        Err(err) => {
            warn!(error = %err, "rejected malformed AgentToServer");
            return (StatusCode::BAD_REQUEST, "malformed AgentToServer").into_response();
        }
    };

    let Some(uid) = InstanceUid::from_slice(&message.instance_uid) else {
        warn!("rejected AgentToServer without a 16-byte instance_uid");
        return (StatusCode::BAD_REQUEST, "instance_uid must be 16 bytes").into_response();
    };

    debug!(%uid, sequence_num = message.sequence_num, "received report");
    let reply = fleet.process(uid, message);

    (
        [(header::CONTENT_TYPE, CONTENT_TYPE_PROTOBUF)],
        opamp::encode(&reply),
    )
        .into_response()
}
