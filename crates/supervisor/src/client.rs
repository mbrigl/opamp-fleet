//! The OpAMP HTTP client — the Server-facing adapter (ADR-0004).
//!
//! Plain-HTTP transport: POST an `AgentToServer` protobuf, read a `ServerToAgent` protobuf back.

use anyhow::{Context, Result};
use opamp::transport::{CONTENT_TYPE_PROTOBUF, INSTANCE_UID_HEADER};
use opamp::v1::{AgentToServer, ServerToAgent};
use opamp::InstanceUid;

/// An OpAMP client that talks to one Server endpoint over plain HTTP.
pub struct OpampHttpClient {
    http: reqwest::Client,
    endpoint: String,
}

impl OpampHttpClient {
    /// Create a client targeting the Server's full OpAMP endpoint URL (e.g.
    /// `http://127.0.0.1:4320/v1/opamp`).
    ///
    /// # Errors
    /// Returns an error if the underlying HTTP client cannot be constructed.
    pub fn new(endpoint: impl Into<String>) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .build()
                .context("building the HTTP client")?,
            endpoint: endpoint.into(),
        })
    }

    /// Send one `AgentToServer` message and return the Server's `ServerToAgent` reply.
    ///
    /// # Errors
    /// Returns an error if the request fails, the Server responds with a non-success status, or the
    /// response body is not a valid `ServerToAgent` protobuf.
    pub async fn send(&self, uid: &InstanceUid, message: &AgentToServer) -> Result<ServerToAgent> {
        let body = opamp::encode(message);

        let response = self
            .http
            .post(&self.endpoint)
            .header(reqwest::header::CONTENT_TYPE, CONTENT_TYPE_PROTOBUF)
            .header(INSTANCE_UID_HEADER, uid.to_string())
            .body(body)
            .send()
            .await
            .context("sending AgentToServer to the Server")?
            .error_for_status()
            .context("the Server returned an error status")?;

        let bytes = response
            .bytes()
            .await
            .context("reading the ServerToAgent response body")?;

        opamp::decode::<ServerToAgent>(&bytes).context("decoding the ServerToAgent response")
    }
}
