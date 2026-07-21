//! The command-line surface (ADR-0006).
//!
//! `supervisor-host` is a `clap` subcommand CLI. It stays deliberately thin — it only parses
//! arguments (with environment fallbacks that preserve the first version's env-only configuration)
//! and hands off to the `service` module. A bare invocation with no subcommand defaults to `run`,
//! preserving the original "just run it" contract.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// The Supervisor Host command-line interface.
#[derive(Debug, Parser)]
#[command(
    name = "supervisor-host",
    // The release version baked in at build time (ADR-0008), falling back to the crate version.
    version = supervisor::version(),
    about = "OpAMP Fleet Supervisor Host — runs standalone or as a native OS service (ADR-0006)"
)]
pub struct Cli {
    /// The subcommand to run. Absent means `run` (foreground daemon).
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the Supervisor Host in the foreground (the default when no subcommand is given).
    Run(RunArgs),
    /// Install, control, or remove the Supervisor Host as a native OS service.
    Service {
        /// The service lifecycle action to perform.
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// Apply a new binary as a self-update (ADR-0007): stage, verify, stop, switch, restart, and
    /// roll back on failure.
    Update(UpdateArgs),
}

/// Arguments for `update`.
#[derive(Debug, clap::Args)]
pub struct UpdateArgs {
    /// Path to the new binary to apply.
    #[arg(long)]
    pub new_binary: PathBuf,
    /// Expected SHA-256 (hex) of the new binary; verified before applying.
    #[arg(long)]
    pub hash: Option<String>,
    /// Seconds to wait for the new version to prove healthy before rolling back.
    #[arg(long, default_value_t = 60)]
    pub settle_seconds: u64,
    /// Target the user-level service instead of the system service.
    #[command(flatten)]
    pub scope: ScopeArgs,
    /// Runtime configuration (state directory + endpoint) locating the install layout.
    #[command(flatten)]
    pub config: ConfigArgs,
}

/// Service-lifecycle actions (`service install|uninstall|start|stop|status`).
#[derive(Debug, Subcommand)]
pub enum ServiceAction {
    /// Register the Supervisor Host as a system (or `--user`) service.
    Install(InstallArgs),
    /// Deregister the service.
    Uninstall(ScopeArgs),
    /// Start the installed service.
    Start(ScopeArgs),
    /// Stop the installed service.
    Stop(ScopeArgs),
    /// Report whether the service is installed and running.
    Status(ScopeArgs),
}

/// The daemon's configuration, shared by `run` and captured by `service install`. Each flag falls
/// back to its environment variable and then to the first version's default.
#[derive(Debug, Clone, clap::Args)]
pub struct ConfigArgs {
    /// The Server's full OpAMP endpoint URL (default: the local Server).
    #[arg(long, env = "OPAMP_SERVER_URL")]
    pub server_url: Option<String>,
    /// Directory holding the Instance UID and effective configuration.
    #[arg(long, env = "OPAMP_STATE_DIR", default_value = "./supervisor-state")]
    pub state_dir: PathBuf,
    /// Seconds between status reports (default: 10).
    #[arg(long, env = "OPAMP_POLL_SECONDS")]
    pub poll_seconds: Option<u64>,
}

impl ConfigArgs {
    /// Resolve configuration from environment variables and defaults alone, used when the binary is
    /// invoked with no subcommand (which defaults to `run`).
    #[must_use]
    pub fn from_env() -> Self {
        #[derive(Parser)]
        struct EnvOnly {
            #[command(flatten)]
            config: ConfigArgs,
        }
        EnvOnly::parse_from(["supervisor-host"]).config
    }
}

/// Arguments for `run`.
#[derive(Debug, clap::Args)]
pub struct RunArgs {
    /// Runtime configuration (with env fallbacks).
    #[command(flatten)]
    pub config: ConfigArgs,
    /// Entered by the Windows Service Control Manager; routes into the SCM dispatcher. Hidden, and
    /// ignored on non-Windows platforms (where the manager supervises a plain foreground process).
    #[arg(long, hide = true)]
    #[cfg_attr(not(windows), allow(dead_code))]
    pub service: bool,
}

/// Whether an action targets the system service or the current user's service.
#[derive(Debug, Clone, clap::Args)]
pub struct ScopeArgs {
    /// Target a user-level service instead of the system service.
    #[arg(long)]
    pub user: bool,
}

/// Arguments for `service install`: the scope plus the configuration to bake into the unit.
#[derive(Debug, clap::Args)]
pub struct InstallArgs {
    /// System or `--user` scope.
    #[command(flatten)]
    pub scope: ScopeArgs,
    /// Configuration captured into the installed service so it is self-contained.
    #[command(flatten)]
    pub config: ConfigArgs,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("valid CLI arguments")
    }

    #[test]
    fn bare_invocation_has_no_subcommand() {
        // No subcommand → the caller (main) defaults to `run`.
        let cli = parse(&["supervisor-host"]);
        assert!(cli.command.is_none());
    }

    #[test]
    fn run_flags_override_and_parse() {
        let cli = parse(&[
            "supervisor-host",
            "run",
            "--server-url",
            "http://example:9999/v1/opamp",
            "--poll-seconds",
            "5",
        ]);
        let Some(Command::Run(args)) = cli.command else {
            panic!("expected run");
        };
        assert_eq!(
            args.config.server_url.as_deref(),
            Some("http://example:9999/v1/opamp")
        );
        assert_eq!(args.config.poll_seconds, Some(5));
        assert!(!args.service);
    }

    #[test]
    fn service_install_defaults_to_system_scope() {
        let cli = parse(&["supervisor-host", "service", "install"]);
        let Some(Command::Service {
            action: ServiceAction::Install(args),
        }) = cli.command
        else {
            panic!("expected service install");
        };
        assert!(!args.scope.user);
    }

    #[test]
    fn service_start_accepts_user_scope() {
        let cli = parse(&["supervisor-host", "service", "start", "--user"]);
        let Some(Command::Service {
            action: ServiceAction::Start(scope),
        }) = cli.command
        else {
            panic!("expected service start");
        };
        assert!(scope.user);
    }
}
