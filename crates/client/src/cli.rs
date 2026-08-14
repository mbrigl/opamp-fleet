//! The command-line surface (ADR-0010).
//!
//! A `clap` subcommand CLI that stays deliberately thin: it only parses arguments and hands off.
//! A bare invocation with no subcommand defaults to `run`, so today's `client --config <path>`
//! keeps working unchanged. The Client is file-configured (ADR-0008) — there are no environment
//! fallbacks; the flags only say where the file is and which instance is meant.

use std::ffi::OsString;
use std::path::PathBuf;

use clap::parser::ValueSource;
use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser, Subcommand};

/// The OpAMP Fleet Client command-line interface.
#[derive(Debug, Parser)]
#[command(
    name = "opamp-fleet-client",
    // The git-derived version baked in at build time (ADR-0009) — never clap's default, which
    // would silently report the static crate version.
    version = opamp::version::current(),
    about = "OpAMP Fleet Client — runs standalone or as a native OS service"
)]
pub struct Cli {
    // ADR-0008: the file is the whole configuration; the flag only says where it is.
    /// Path to the TOML configuration file; defaults apply if it does not exist.
    #[arg(long, global = true, default_value = "client.toml")]
    pub config: PathBuf,
    /// Instance name: selects the service identity (`opamp-fleet-client-<instance>`) and the
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

/// A parsed command line, plus the one thing the parsed struct cannot say: whether `--config`
/// carries a path the operator named or the default that stands in for one (ADR-0027).
///
/// The distinction is load-bearing exactly once. `service install` bakes an absolute config path
/// into the service unit, and when nobody named a path, the right one is not the default resolved
/// against a working directory the service manager will not have — it is the install root, which
/// is derived per platform, scope, and instance.
#[derive(Debug)]
pub struct Parsed {
    /// The command line as declared.
    pub cli: Cli,
    /// `true` when `--config` appeared on the command line, at any level.
    pub config_named: bool,
}

/// Parse the process arguments, exiting with clap's own message and exit code on a bad one.
#[must_use]
pub fn parse() -> Parsed {
    match parse_from(std::env::args_os()) {
        Ok(parsed) => parsed,
        Err(e) => e.exit(),
    }
}

/// Parse an explicit argument list — what [`parse`] does, minus the exit, so tests can reach it.
///
/// # Errors
/// Returns clap's error for an argument list it refuses (including `--help` and `--version`,
/// which clap reports the same way).
pub fn parse_from<I, T>(args: I) -> Result<Parsed, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let matches = Cli::command().try_get_matches_from(args)?;
    let config_named = config_named(&matches);
    Ok(Parsed {
        cli: Cli::from_arg_matches(&matches)?,
        config_named,
    })
}

/// Whether `--config` was given on the command line. A global argument may be parsed at the level
/// it was written on, so both `client --config x service install` and
/// `client service install --config x` have to count — hence the walk down the subcommands.
fn config_named(matches: &ArgMatches) -> bool {
    if matches.value_source("config") == Some(ValueSource::CommandLine) {
        return true;
    }
    matches
        .subcommand()
        .is_some_and(|(_, sub)| config_named(sub))
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
    // ADR-0020.
    /// Prove that this executable is an OpAMP Fleet Client and say which version.
    ///
    /// Run as a child process on a freshly staged binary before the `current` pointer moves to
    /// it. Two things are being asked at once: *does it run at all* on this host — the failure
    /// class no post-restart mechanism can catch, because a binary that cannot exec never gets
    /// far enough to notice anything — and *is it this program*, since a package offered under
    /// the configured name is still only a name until something answers for it. Hidden, because
    /// it is a machine-to-machine handshake and not an operator command.
    #[command(hide = true)]
    SelfCheck,
}

/// Arguments for `run`.
#[derive(Debug, clap::Args)]
pub struct RunArgs {
    /// Set by `service install` on every platform. On Windows it routes into the SCM dispatcher;
    /// everywhere it says the machine's service manager started this process and no terminal is
    /// watching, which is what turns on the log file (ADR-0041).
    #[arg(long, hide = true)]
    pub service: bool,
}

/// Service-lifecycle actions (`service install|uninstall|start|stop|status`).
#[derive(Debug, Subcommand)]
pub enum ServiceAction {
    // ADR-0010 decides the layout this lays out.
    /// Register this instance as a system (or `--user`) service and lay out the versioned
    /// install.
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
    // ADR-0027.
    /// Ask for the settings a fresh host cannot guess and write the configuration file before
    /// registering the service.
    ///
    /// Off by default, because `install` is the command a provisioning run invokes and it must
    /// never block on a question. An existing file is kept, never overwritten. With no terminal
    /// on stdin this fails rather than waiting for an answer that cannot come.
    #[arg(long)]
    pub interactive: bool,
    // ADR-0046.
    /// Write the configuration file with this Server endpoint instead of asking for it.
    ///
    /// The non-interactive half of `--interactive`, for an install driven by a packaged installer:
    /// the MSI's endpoint dialog and a `.deb`/`.rpm` post-install both have an answer and no
    /// terminal. An existing file is kept, never overwritten, exactly as with `--interactive`.
    ///
    /// Only the endpoint. A credential is not accepted here — it would stand in the process list
    /// and in the installer log; write it into the file afterwards, or use `--interactive`.
    #[arg(long, value_name = "URL", conflicts_with = "interactive")]
    pub endpoint: Option<String>,
    // ADR-0062.
    /// Run the service as this account instead of root/`LocalSystem`, and hand the instance's
    /// files — configuration, state, and the executable layout — over to it.
    ///
    /// System scope only: a `--user` service already runs as its user. On Linux and macOS the
    /// account must exist. On Windows only passwordless account forms are accepted — the
    /// service's own virtual account (`NT SERVICE\<service name>`), a gMSA (`name$`), or
    /// `NT AUTHORITY\LocalService`/`NetworkService`; a password is never taken here, for the
    /// same reason no credential is (ADR-0046).
    #[arg(long, value_name = "ACCOUNT", conflicts_with = "user")]
    pub run_as: Option<String>,
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

/// The only way to build an [`InstanceName`]: validated against the grammar above. `pub` because
/// the service smoke test names the instance it installs, and there is nothing else to construct
/// one with (ADR-0024 widens visibility by need).
///
/// # Errors
/// Returns an error naming the rule the value breaks.
pub fn parse_instance_name(raw: &str) -> Result<InstanceName, String> {
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

    /// ADR-0027: interactivity is something the operator asks for. Every invocation that existed
    /// before this flag keeps meaning what it meant.
    #[test]
    fn install_is_not_interactive_unless_asked() {
        let quiet = parse(&["client", "service", "install"]);
        let Some(Command::Service {
            action: ServiceAction::Install(args),
        }) = quiet.command
        else {
            panic!("expected service install");
        };
        assert!(!args.interactive);

        let asked = parse(&["client", "service", "install", "--interactive"]);
        let Some(Command::Service {
            action: ServiceAction::Install(args),
        }) = asked.command
        else {
            panic!("expected service install");
        };
        assert!(args.interactive);
        assert_eq!(args.endpoint, None);
    }

    /// ADR-0046 clause 7: a packaged install passes the answer it collected. The endpoint is not
    /// validated here — the loader's own rule does that, once, in `config_init`.
    #[test]
    fn install_takes_an_endpoint_without_a_terminal() {
        let told = parse(&[
            "client",
            "service",
            "install",
            "--endpoint",
            "wss://fleet.example.com/v1/opamp",
        ]);
        let Some(Command::Service {
            action: ServiceAction::Install(args),
        }) = told.command
        else {
            panic!("expected service install");
        };
        assert!(!args.interactive);
        assert_eq!(
            args.endpoint.as_deref(),
            Some("wss://fleet.example.com/v1/opamp")
        );
    }

    /// The two ways of writing the first configuration cannot both be asked for: one blocks on a
    /// terminal and the other exists precisely because there is none.
    #[test]
    fn an_endpoint_and_interactive_are_refused_together() {
        let err = Cli::try_parse_from([
            "client",
            "service",
            "install",
            "--interactive",
            "--endpoint",
            "wss://fleet.example.com/v1/opamp",
        ])
        .expect_err("mutually exclusive");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    /// ADR-0062: the account is named at install time and nowhere else. No password parameter
    /// exists beside it — the accepted Windows forms are all passwordless.
    #[test]
    fn install_takes_a_run_as_account() {
        let cli = parse(&["client", "service", "install", "--run-as", "opamp-fleet"]);
        let Some(Command::Service {
            action: ServiceAction::Install(args),
        }) = cli.command
        else {
            panic!("expected service install");
        };
        assert_eq!(args.run_as.as_deref(), Some("opamp-fleet"));

        let none = parse(&["client", "service", "install"]);
        let Some(Command::Service {
            action: ServiceAction::Install(args),
        }) = none.command
        else {
            panic!("expected service install");
        };
        assert_eq!(args.run_as, None, "absent flag means today's behaviour");
    }

    /// ADR-0062: `--run-as` is system scope only — a `--user` service already runs as its user,
    /// so naming an account beside it could only contradict it.
    #[test]
    fn run_as_and_user_scope_are_refused_together() {
        let err = Cli::try_parse_from([
            "client",
            "service",
            "install",
            "--user",
            "--run-as",
            "opamp-fleet",
        ])
        .expect_err("mutually exclusive");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    /// The default value of `--config` must not be mistaken for a path someone chose: it decides
    /// whether `install` writes into the install root or where the operator pointed (ADR-0027).
    #[test]
    fn a_named_config_is_told_apart_from_the_default() {
        let default = parse_from(["client", "service", "install"]).expect("parse");
        assert!(!default.config_named);
        assert_eq!(default.cli.config, PathBuf::from("client.toml"));

        // A global argument counts from either side of the subcommand.
        for args in [
            [
                "client",
                "--config",
                "/etc/opamp/client.toml",
                "service",
                "install",
            ],
            [
                "client",
                "service",
                "install",
                "--config",
                "/etc/opamp/client.toml",
            ],
        ] {
            let named = parse_from(args).expect("parse");
            assert!(named.config_named, "{args:?}");
            assert_eq!(named.cli.config, PathBuf::from("/etc/opamp/client.toml"));
        }

        // Even spelled with the same value as the default: what counts is that it was written.
        let same =
            parse_from(["client", "service", "install", "--config", "client.toml"]).expect("parse");
        assert!(same.config_named);
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
        assert!(err.to_string().contains(opamp::version::current()));
    }
}
