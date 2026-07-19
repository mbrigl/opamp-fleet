//! The hexagonal **ports** the supervision domain depends on (skeleton).
//!
//! The domain is written against these boundaries, never against a concrete agent, so bringing a new
//! kind of Managed Agent under management is a new adapter — a **plugin** — not a change to the core.
//! The traits below are intentionally minimal placeholders; they will grow (async, richer error types,
//! health/effective-config reporting) as the implementation lands (ADR-0005).

use std::error::Error;

/// A Managed Agent's configuration, as bytes — the Server distributes it and a plugin applies it. Its
/// meaning (a Collector YAML, a foreign agent's own format) is the plugin's concern, not the domain's.
pub type Config = Vec<u8>;

/// The Managed-Agent-facing driven port: what a plugin must do to place a real process — an
/// OpenTelemetry Collector, or a non-OpAMP Foreign Agent — under management.
pub trait ManagedAgent {
    /// Start the Managed Agent.
    fn start(&mut self) -> Result<(), Box<dyn Error>>;
    /// Apply a configuration and make it take effect (for a Collector: write it and restart).
    fn apply_config(&mut self, config: &Config) -> Result<(), Box<dyn Error>>;
    /// Stop the Managed Agent.
    fn stop(&mut self) -> Result<(), Box<dyn Error>>;
}

/// A **Supervisor plugin**: one unit inside the host that manages exactly one Managed Agent and appears
/// to the Server as one Agent. Concrete plugins (a Collector Supervisor, a Custom Supervisor) implement
/// this behind the same ports.
pub trait Supervisor: Send {
    /// A short, stable name for logs and the fleet view (e.g. `"collector"`, `"custom:nginx"`).
    fn name(&self) -> &str;
}
