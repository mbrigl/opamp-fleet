//! The command-line surface (ADR-0010).
//!
//! A `clap` subcommand CLI that stays deliberately thin: it only parses arguments and hands off.
//! A bare invocation with no subcommand defaults to `run`, so today's `client --config <path>`
//! keeps working unchanged. The Client is file-configured (ADR-0008) — there are no environment
//! fallbacks; the flags only say where the file is and which instance is meant.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// The OpAMP Fleet Client command-line interface.
#[derive(Debug, Parser)]
#[command(
    name = "client",
    // The git-derived version baked in at build time (ADR-0009) — never clap's default, which
    // would silently report the static crate version.
    version = crate::version::version(),
    about = "OpAMP Fleet Client — runs standalone or as a native OS service (ADR-0010)"
)]
pub struct Cli {
    /// Path to the TOML configuration file (ADR-0008); defaults apply if it does not exist.
    #[arg(long, global = true, default_value = "client.toml")]
    pub config: PathBuf,
    /// Instance name: selects the service identity (`io.opamp-fleet.client.<instance>`) and the
    /// default install root, so several differently-configured Clients coexist on one host.
    #[arg(long, global = true, default_value = "default", value_parser = parse_instance_name)]
    pub instance: InstanceName,
    /// Overrides the configuration file's state directory. `service install` bakes this into the
    /// unit so an installed service never depends on a relative path.
    #[arg(long, global = true)]
    pub state_dir: Option<PathBuf>,
    /// The subcommand to run. Absent means `run` (foreground daemon).
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the Client in the foreground (the default when no subcommand is given).
    Run(RunArgs),
    /// Install, control, or remove this Client instance as a native OS service.
    Service {
        /// The service lifecycle action to perform.
        #[command(subcommand)]
        action: ServiceAction,
    },
}

/// Arguments for `run`.
#[derive(Debug, clap::Args)]
pub struct RunArgs {
    /// Set by `service install`; routes into the Windows SCM dispatcher. Hidden, and ignored on
    /// non-Windows platforms (where the manager supervises a plain foreground process).
    #[arg(long, hide = true)]
    #[cfg_attr(not(windows), allow(dead_code))]
    pub service: bool,
}

/// Service-lifecycle actions (`service install|uninstall|start|stop|status`).
#[derive(Debug, Subcommand)]
pub enum ServiceAction {
    /// Register this instance as a system (or `--user`) service and lay out the versioned
    /// install (ADR-0010).
    Install(InstallArgs),
    /// Deregister the service (the install layout and state are never deleted).
    Uninstall(ScopeArgs),
    /// Start the installed service.
    Start(ScopeArgs),
    /// Stop the installed service.
    Stop(ScopeArgs),
    /// Report whether the service is installed and running.
    Status(ScopeArgs),
}

/// Arguments for `service install`.
#[derive(Debug, clap::Args)]
pub struct InstallArgs {
    /// System or `--user` scope.
    #[command(flatten)]
    pub scope: ScopeArgs,
    /// Install root holding `versions/`, `current`, and the default `state/` directory. Defaults
    /// to the platform data directory for the scope and instance — no path is ever fixed.
    #[arg(long)]
    pub root: Option<PathBuf>,
}

/// Whether an action targets the system service or the current user's service.
#[derive(Debug, Clone, Copy, clap::Args)]
pub struct ScopeArgs {
    /// Target a user-level service instead of the system service.
    #[arg(long)]
    pub user: bool,
}

/// A validated instance name — the intersection of the systemd-unit, launchd-label, Windows
/// service-name, and directory-name grammars (ADR-0010).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceName(String);

impl InstanceName {
    /// The validated name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for InstanceName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Windows reserved device names: legal under the grammar below, but invalid directory names on
/// Windows — an instance must be a directory everywhere.
const WINDOWS_RESERVED: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

pub(crate) fn parse_instance_name(raw: &str) -> Result<InstanceName, String> {
    if raw.is_empty() || raw.len() > 32 {
        return Err("must be 1–32 characters".to_string());
    }
    if !raw
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err("only lowercase letters, digits, and '-' are allowed".to_string());
    }
    if raw.starts_with('-') || raw.ends_with('-') {
        return Err("must not start or end with '-'".to_string());
    }
    if WINDOWS_RESERVED.contains(&raw) {
        return Err(format!("{raw:?} is a reserved device name on Windows"));
    }
    Ok(InstanceName(raw.to_string()))
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
        let cli = parse(&["client"]);
        assert!(cli.command.is_none());
        assert_eq!(cli.config, PathBuf::from("client.toml"));
        assert_eq!(cli.instance.as_str(), "default");
    }

    #[test]
    fn todays_invocation_still_parses() {
        // The pre-ADR-0010 command line: `client --config <path>`.
        let cli = parse(&["client", "--config", "config/client.toml"]);
        assert!(cli.command.is_none());
        assert_eq!(cli.config, PathBuf::from("config/client.toml"));
    }

    #[test]
    fn run_is_explicit_too_and_config_is_global() {
        // `--config` is a global flag: valid before and after the subcommand.
        let cli = parse(&["client", "run", "--config", "x.toml"]);
        assert!(matches!(cli.command, Some(Command::Run(_))));
        assert_eq!(cli.config, PathBuf::from("x.toml"));
    }

    #[test]
    fn the_installed_command_line_parses() {
        // What `service install` writes into the unit (ADR-0010).
        let cli = parse(&[
            "client",
            "run",
            "--service",
            "--config",
            "/etc/opamp/client.toml",
            "--instance",
            "prod",
            "--state-dir",
            "/var/lib/opamp-fleet/client/prod/state",
        ]);
        let Some(Command::Run(args)) = cli.command else {
            panic!("expected run");
        };
        assert!(args.service);
        assert_eq!(cli.instance.as_str(), "prod");
        assert!(cli.state_dir.is_some());
    }

    #[test]
    fn service_verbs_parse_with_scope_and_root() {
        let cli = parse(&["client", "service", "install", "--user", "--root", "/opt/x"]);
        let Some(Command::Service {
            action: ServiceAction::Install(args),
        }) = cli.command
        else {
            panic!("expected service install");
        };
        assert!(args.scope.user);
        assert_eq!(args.root, Some(PathBuf::from("/opt/x")));

        let cli = parse(&["client", "service", "status", "--instance", "staging"]);
        assert!(matches!(
            cli.command,
            Some(Command::Service {
                action: ServiceAction::Status(ScopeArgs { user: false })
            })
        ));
        assert_eq!(cli.instance.as_str(), "staging");
    }

    #[test]
    fn instance_names_are_validated() {
        for valid in ["default", "prod", "a", "web-1", "x2", &"a".repeat(32)] {
            assert!(parse_instance_name(valid).is_ok(), "{valid:?} should parse");
        }
        for invalid in [
            "",
            "Prod",
            "with space",
            "über",
            "-lead",
            "trail-",
            "dot.name",
            "path/name",
            "con",
            "com7",
            "lpt1",
            &"a".repeat(33),
        ] {
            assert!(
                parse_instance_name(invalid).is_err(),
                "{invalid:?} should be rejected"
            );
        }
    }

    #[test]
    fn state_dir_is_a_global_override() {
        let cli = parse(&["client", "run", "--state-dir", "/var/lib/x"]);
        assert_eq!(cli.state_dir, Some(PathBuf::from("/var/lib/x")));
        // Absent by default: the configuration file's value applies.
        assert_eq!(parse(&["client"]).state_dir, None);
    }

    #[test]
    fn the_version_flag_reports_the_baked_in_version() {
        let err = Cli::try_parse_from(["client", "--version"]).unwrap_err();
        assert!(err.to_string().contains(crate::version::version()));
    }
}
