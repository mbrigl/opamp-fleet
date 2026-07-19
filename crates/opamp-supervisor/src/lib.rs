//! The Supervisor Host: a Rust process that hosts many OpAMP supervisors as plugins behind a hexagonal
//! core (ADR-0009).
//!
//! The OpAMP client loop ([`supervisor::Supervisor`]) is the domain, written against the
//! [`agent::ManagedAgent`] port. Concrete agent types are adapters (plugins): [`collector_agent`] wraps
//! an OpenTelemetry Collector (the Collector Supervisor, ADR-0008); [`process_agent`] manages a
//! non-OpAMP Foreign Agent (the Custom Supervisor). The [`host`] runs many supervisors — one OpAMP Agent
//! each — declared in a [`config`] file. Adding a kind of agent is a new adapter, not a change to the
//! domain.

pub mod agent;
pub mod collector;
pub mod collector_agent;
pub mod config;
pub mod download;
pub mod health;
pub mod host;
pub mod local_server;
pub mod process_agent;
pub mod supervisor;
pub mod tls;
pub mod uid;
