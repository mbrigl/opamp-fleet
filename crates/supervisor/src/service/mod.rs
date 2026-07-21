//! Running the Supervisor Host as a native OS service (ADR-0006).
//!
//! This module owns everything about the OS-service side: the daemon [`runtime`], the cross-platform
//! [`manager`] wrapper (install/uninstall/start/stop/status), and — on Windows only — the SCM
//! [`windows`] runtime shim. It exposes a deliberately narrow [`ServiceControl`] seam (start/stop/
//! query only), the single interface ADR-0007's Updater will depend on, so the self-update mechanism
//! never reaches into service internals.

pub mod manager;
pub mod runtime;
#[cfg(windows)]
pub mod windows;

use anyhow::Result;

pub use manager::NativeService;

/// Whether a service is installed system-wide or only for the current user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceLevel {
    /// A machine-wide service (systemd system unit / launchd `LaunchDaemon` / Windows `LocalSystem`).
    System,
    /// A per-user service (systemd `--user` / launchd `LaunchAgent`).
    User,
}

/// The liveness of the installed service, as far as the platform manager reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    /// The service is not installed.
    NotInstalled,
    /// The service is installed but not running.
    Stopped,
    /// The service is running.
    Running,
}

impl ServiceState {
    /// A short human-readable description for the `service status` command.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            ServiceState::NotInstalled => "not installed",
            ServiceState::Stopped => "installed, stopped",
            ServiceState::Running => "running",
        }
    }
}

/// The seam ADR-0007's Updater drives the service through — start, stop, and query only.
///
/// Keeping this trait minimal is what lets the self-update logic be unit-tested with a fake control
/// (no real service) and keeps the `update` module decoupled from the service backends.
pub trait ServiceControl {
    /// Start the installed service.
    ///
    /// # Errors
    /// Returns an error if the platform manager cannot start the service.
    fn start(&self) -> Result<()>;

    /// Stop the installed service, holding it stopped (no manager-driven auto-restart).
    ///
    /// # Errors
    /// Returns an error if the platform manager cannot stop the service.
    fn stop(&self) -> Result<()>;

    /// Query the service's current state.
    ///
    /// # Errors
    /// Returns an error if the platform manager cannot report the service state.
    fn state(&self) -> Result<ServiceState>;
}
