//! Plain-HTTP transport constants shared by both ends (ADR-0004).
//!
//! The Supervisor Host POSTs an `AgentToServer` protobuf to the Server's OpAMP endpoint and reads a
//! `ServerToAgent` protobuf back. Both ends agree on the path, content type, and headers here.

/// The Server's OpAMP HTTP endpoint path.
pub const OPAMP_HTTP_PATH: &str = "/v1/opamp";

/// The `Content-Type` for OpAMP protobuf bodies, required by the specification for plain HTTP.
pub const CONTENT_TYPE_PROTOBUF: &str = "application/x-protobuf";

/// The header carrying the Instance UID in its canonical UUID string form.
pub const INSTANCE_UID_HEADER: &str = "OpAMP-Instance-UID";

/// The Server's default listen port (also the UI port).
pub const DEFAULT_PORT: u16 = 4320;
