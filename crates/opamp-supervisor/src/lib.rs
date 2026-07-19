//! The Supervisor Host: a single Rust process that hosts multiple **Supervisor** instances as
//! **plugins** behind a hexagonal core (see [`SPECIFICATION.md`](../../docs/SPECIFICATION.md)).
//!
//! This crate is a **skeleton**. It fixes the shape the specification calls for — a domain that talks
//! to the Server over OpAMP through one port and to each Managed Agent through another, with plugins as
//! the adapters — but implements no behaviour yet. The OpAMP client loop, the concrete Collector and
//! Custom (foreign-agent) supervisor plugins, and the async runtime arrive in a later change (ADR-0005).

pub mod host;
pub mod ports;
