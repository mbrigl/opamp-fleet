//! OpAMP Fleet Supervisor Host library.
//!
//! The [`Supervisor`] holds one Managed Agent's state and builds/handles OpAMP messages; the
//! [`OpampHttpClient`] carries those messages to the Server over plain HTTP (ADR-0004). The
//! `supervisor-host` binary wires them into a report loop.

pub mod client;
mod supervisor;

pub use client::OpampHttpClient;
pub use supervisor::{Supervisor, DEFAULT_POLL};
