//! The command-line surface (ADR-0021).
//!
//! A `clap` subcommand CLI that stays deliberately thin: it only parses arguments and hands off.
//! A bare invocation with no subcommand defaults to `run`, so `supervisor --config <path>` keeps
//! working unchanged. The Client is file-configured (ADR-0009) — there are no environment
//! fallbacks; the flags only say where the file and the state directory are.
//!
//! There is no `--instance` (ADR-0021 clause 6). One build installs one service under the
//! product's name, so the service verbs have nothing to look up and take no name at all; a second
//! installation is a second build. The *grammar* that flag used survives as
//! [`parse_instance_name`], because `[[supervisor]]` block names still need it.

use std::ffi::OsString;
use std::path::PathBuf;

use clap::parser::ValueSource;
use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser, Subcommand};

/// The OpAMP Fleet Client command-line interface.
#[derive(Debug, Parser)]
#[command(
    // The git-derived version baked in at build time (ADR-0011) — never clap's default, which
    // would silently report the static crate version.
    version = opamp::version::current(),
    about = "OpAMP Fleet Client — runs standalone or as a native OS service"
)]
pub struct Cli {
    // ADR-0009: the file is the whole configuration; the flag only says where it is.
    /// Path to the TOML configuration file; defaults apply if it does not exist.
    pub config: PathBuf,
    /// Overrides the configuration file's state directory. `service install` bakes this into the
    /// unit so an installed service never depends on a relative path.
    #[arg(long, global = true)]
    pub state_dir: Option<PathBuf>,
    /// The subcommand to run. Absent means `run` (foreground daemon).
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// A parsed command line, plus the one thing the parsed struct cannot say: whether `--config`
/// carries a path the operator named or the default that stands in for one (ADR-0021).
///
/// The distinction is load-bearing exactly once. `service install` bakes an absolute config path
/// into the service unit, and when nobody named a path, the right one is not the default resolved
/// against a working directory the service manager will not have — it is the data root, which is
/// derived per platform and scope.
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
}

/// Arguments for `run`.
#[derive(Debug, clap::Args)]
pub struct RunArgs {
    /// Set by `service install` on every platform. On Windows it routes into the SCM dispatcher;
    /// everywhere it says the machine's service manager started this process and no terminal is
    /// watching, which is what turns on the log file (ADR-0021).
    #[arg(long, hide = true)]
    pub service: bool,
}

/// Service-lifecycle actions (`service install|uninstall|start|stop|status`).
#[derive(Debug, Subcommand)]
pub enum ServiceAction {
    // ADR-0021 decides the layout this lays out.
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
    /// The layout root, holding `versions/` and the `current` pointer. Defaults to
    /// `<base>/<PRODUCT_NAME>` for the scope — no path is ever fixed (ADR-0021 clause 7).
    ///
    /// Given **alone** it collapses layout and data into the one directory it names, exactly as
    /// ADR-0021 defined it, and the labeling and permissions of that directory are then yours to
    /// manage.
    #[arg(long)]
    pub root: Option<PathBuf>,
    // ADR-0021 clause 8.
    /// The data root, holding `supervisor.toml` and the state directory — everything a reinstall
    /// cannot recreate.
    ///
    /// It exists because Linux at system scope must not execute from `/var/lib`: the layout goes
    /// to `/opt/<PRODUCT_NAME>` and the data stays behind. Naming it beside `--root` is how an
    /// operator keeps the two halves apart anywhere else. Left alone, it follows `--root` when
    /// that is given and the platform default otherwise.
    #[arg(long)]
    pub data_root: Option<PathBuf>,
    // ADR-0021.
    /// Ask for the settings a fresh host cannot guess and write the configuration file before
    /// registering the service.
    ///
    /// Off by default, because `install` is the command a provisioning run invokes and it must
    /// never block on a question. An existing file is kept, never overwritten. With no terminal
    /// on stdin this fails rather than waiting for an answer that cannot come.
    #[arg(long)]
    pub interactive: bool,
    // ADR-0021.
    /// Run the service as this account instead of root/`LocalSystem`, and hand this
    /// installation's files — both roots — over to it.
    ///
    /// System scope only: a `--user` service already runs as its user. On Linux and macOS the
    /// account must exist. On Windows only passwordless account forms are accepted — the
    /// service's own virtual account (`NT SERVICE\<service name>`), a gMSA (`name$`), or
    /// `NT AUTHORITY\LocalService`/`NetworkService`; a password is never taken here, for the
    /// same reason no credential is (ADR-0021).
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

/// A name validated against the intersection of the systemd-unit, launchd-label, Windows
/// service-name, and directory-name grammars (ADR-0021).
///
/// Since ADR-0021 removed `--instance`, this no longer names an instance: it governs
/// `[[supervisor]]` block names, and `build.rs` holds a second copy of the same rules for
/// `PRODUCT_NAME` — which cannot borrow this one, because a build script cannot depend on the
/// crate it builds.
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
/// callers outside this module need to construct one and there is nothing else to do it with
/// (ADR-0009 widens visibility by need).
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
    }

    /// ADR-0021 clause 6: removed, not hidden and not accepted-and-ignored. A unit written by an
    /// older install would carry it, and this is what makes that fail loudly instead of running a
    /// Client whose paths silently mean something else.
    #[test]
    fn instance_is_not_a_flag_any_more() {
        assert!(
            Cli::try_parse_from(["supervisor", "--instance", "prod"]).is_err(),
            "--instance must be rejected outright"
        );
        for verb in ["uninstall", "start", "stop", "status"] {
            assert!(
                Cli::try_parse_from(["supervisor", "service", verb, "--instance", "prod"]).is_err(),
                "`service {verb}` takes no instance name — there is nothing to look up"
            );
        }
    }

    #[test]
    fn todays_invocation_still_parses() {
        // The pre-ADR-0021 command line: `client --config <path>`.
        assert!(cli.command.is_none());
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
        // What `service install` writes into the unit (ADR-0021).
        let cli = parse(&[
            "client",
            "run",
            "--service",
            "--config",
            "/var/lib/opamp-fleet/supervisor.toml",
            "--state-dir",
            "/var/lib/opamp-fleet/state",
        ]);
        let Some(Command::Run(args)) = cli.command else {
            panic!("expected run");
        };
        assert!(args.service);
        assert_eq!(
            cli.config,
            PathBuf::from("/var/lib/opamp-fleet/supervisor.toml")
        );
        assert_eq!(
            cli.state_dir,
            Some(PathBuf::from("/var/lib/opamp-fleet/state"))
        );
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
        assert_eq!(args.data_root, None, "it follows --root unless named");

        let cli = parse(&["client", "service", "status"]);
        assert!(matches!(
            cli.command,
            Some(Command::Service {
                action: ServiceAction::Status(ScopeArgs { user: false })
            })
        ));
    }

    /// ADR-0021 clause 8: the two halves can be named apart, which is what the Linux system-scope
    /// split needs and what the manual tells an operator to do on a host that wants them apart.
    #[test]
    fn both_roots_can_be_named() {
        let args = install(&[
            "--root",
            "/opt/opamp-fleet",
            "--data-root",
            "/var/lib/opamp-fleet",
        ]);
        assert_eq!(args.root, Some(PathBuf::from("/opt/opamp-fleet")));
        assert_eq!(args.data_root, Some(PathBuf::from("/var/lib/opamp-fleet")));
    }

    /// ADR-0021: interactivity is something the operator asks for. Every invocation that existed
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
                "client",
                "service",
                "install",
                "client",
                "service",
                "install",
                "client",
                "service",
                "install",
        let Some(Command::Service {
            action: ServiceAction::Install(args),
        else {
            panic!("expected service install");
        };
        assert!(!args.interactive);
    }

    /// ADR-0021: the account is named at install time and nowhere else. No password parameter
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

    /// ADR-0021: `--run-as` is system scope only — a `--user` service already runs as its user,
    /// so naming an account beside it could only contradict it.
    #[test]
    fn run_as_and_user_scope_are_refused_together() {
            "--user",
            "--run-as",
            "opamp-fleet",
    }

    /// The default value of `--config` must not be mistaken for a path someone chose: it decides
    /// whether `install` writes into the install root or where the operator pointed (ADR-0021).
    #[test]
    fn a_named_config_is_told_apart_from_the_default() {
        let default = parse_from(["client", "service", "install"]).expect("parse");
        assert!(!default.config_named);

        // A global argument counts from either side of the subcommand.
        for args in [
            [
                "client",
                "--config",
                "service",
                "install",
            ],
            [
                "client",
                "service",
                "install",
                "--config",
            ],
        ] {
            let named = parse_from(args).expect("parse");
            assert!(named.config_named, "{args:?}");
        }

        // Even spelled with the same value as the default: what counts is that it was written.
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
