//! rudimentary UI. Both belong to the Operator plane and are served on its own listener
//! (ADR-0012); the one exception, the Agent-facing artifact download, is [`download_router`].
//!

use std::sync::Arc;

use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};


        .routes(routes!(restart_agent))
}

/// The one route of `/api/v1` that is not the operator's: the artifact bytes an Agent downloads.
/// It is served on the **Agent plane** (ADR-0012), because that is the audience — the
/// `download_url` in a package offer is a path the Client resolves against its own OpAMP endpoint
/// (ADR-0028), so this listener is where the offer already points. It keeps its `/api/v1` path,
/// which every published Set's `download_url` names.
///
/// Consequently it is not in the OpenAPI document: that document describes the Operator plane.
pub fn download_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route(
            get(download_package),
        )
        .with_state(state)
}

/// The bundled UI: one embedded page, no frontend toolchain (ADR-0009).
async fn index() -> Html<&'static str> {
    Html(include_str!("../static/index.html"))
}

}

}

/// Queues a restart of the Agent's Managed Process, delivered as the protocol's restart command
/// on the Agent's next exchange — immediately over WebSocket, on the next poll over plain HTTP.
#[utoipa::path(
    post,
    path = "/api/v1/agents/{instance_uid}/restart",
    tag = "fleet",
    params(("instance_uid" = String, Path, description = "The Agent's Instance UID")),
    responses(
        (status = 202, description = "Restart queued"),
        (status = 400, description = "Malformed Instance UID", body = ErrorBody),
        (status = 404, description = "No such Agent", body = ErrorBody),
        (status = 409, description = "The Agent does not declare AcceptsRestartCommand", body = ErrorBody)
    )
)]
async fn restart_agent(
    Path(instance_uid): Path<String>,
) -> Response {
    let Some(uid) = opamp::uid::InstanceUid::parse(&instance_uid) else {
            StatusCode::BAD_REQUEST,
            format!("{instance_uid:?} is not an Instance UID"),
        );
    };
    match state.request_restart(&uid) {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(RestartError::UnknownAgent) => error(StatusCode::NOT_FOUND, format!("no agent {uid}")),
        Err(RestartError::NoCapability) => error(
            StatusCode::CONFLICT,
            format!("agent {uid} does not declare AcceptsRestartCommand"),
        ),
    }
}

        (status = 404, description = "No such Agent", body = ErrorBody),
    Path(instance_uid): Path<String>,
) -> Response {
    let Some(uid) = opamp::uid::InstanceUid::parse(&instance_uid) else {
            StatusCode::BAD_REQUEST,
            format!("{instance_uid:?} is not an Instance UID"),
        );
    };
    tag = "fleet",
    params(("instance_uid" = String, Path, description = "The Agent's Instance UID")),
    Path(instance_uid): Path<String>,
) -> Response {
    let Some(uid) = opamp::uid::InstanceUid::parse(&instance_uid) else {
            StatusCode::BAD_REQUEST,
            format!("{instance_uid:?} is not an Instance UID"),
        );
    };
            StatusCode::BAD_REQUEST,
        (status = 400, description = "Malformed Instance UID", body = ErrorBody),
        (status = 404, description = "No such Agent", body = ErrorBody),
    Path(instance_uid): Path<String>,
) -> Response {
    let Some(uid) = opamp::uid::InstanceUid::parse(&instance_uid) else {
            StatusCode::BAD_REQUEST,
            format!("{instance_uid:?} is not an Instance UID"),
        );
    };
    /// The Baseline's `AgentConfigObject.role` (ADR-0025); absent means top-level configuration.
}

            StatusCode::BAD_REQUEST,
    if body.trim().is_empty() {
            StatusCode::BAD_REQUEST,
    }
    if !body.ends_with('\n') {
        body.push('\n');
    }
        }
        }
    }
}
#[derive(Deserialize)]
            StatusCode::BAD_REQUEST,
            StatusCode::BAD_REQUEST,
/// Serves an entry's artifact bytes — the `download_url` the Agent is offered points here.
///
/// `200` with the bytes, `400` for a missing or invalid platform or identity, `404` for a Set
/// without an uploaded artifact for that platform.
