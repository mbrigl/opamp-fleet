//! The `glpi` plugin (ADR-draft-glpi-agent clauses 1 to 4): the GLPI Agent, delivered as a
//! package tree and run out of this Supervisor's own directory.
//!
//! This is the agent that shows most plainly why a kind beats a recipe. Its two platform
//! invocations differ in nearly everything — the program's name and where it sits in the tree, the
//! working directory, and on Windows four Perl `-I` paths and the script named by path — and not
//! one of those differences is a decision anybody makes. They follow from `EXE_SUFFIX` and from
//! where the AppImage this project repacks puts its interpreter (ADR-draft-glpi-agent clause 2),
//! so they belong to the side that packs the artifact, not to every host's file.
//!
//! Two flags are supervision requirements rather than preferences, and are therefore not the
//! operator's to drop. Without `--daemon` the agent runs its tasks once and exits, and the watchdog
//! restarts it for ever. Without `--no-fork` it detaches, leaving the Supervisor holding a pid that
//! ends immediately while the real process runs on unsupervised.

use std::path::{Path, PathBuf};

use tokio::sync::mpsc;

use crate::supervisor::ports::{
    parse_settings, KindDefaults, KindTiming, Plugin, ProcessCommand, SupervisorContext,
};
use crate::supervisor::process::{Preflight, ProcessSpec, Runner, Spawner, VersionProbe, STAGED};

/// What this project's own packaging puts in the tree, per platform (ADR-draft-glpi-agent
/// clause 2, `docs/artifacts/glpi-agent.md`). On Linux the repacked AppImage's entry point is
/// `AppRun` at the tree root — it bundles several programs, and `--script=glpi-agent` picks this
/// one. On Windows upstream's zip is installed as published, with the program deep inside the
/// bundled Perl.
///
/// **Never `glpi-agent.bat`**: the batch file is a wrapper, so the supervised child would be
/// `cmd.exe` and the agent would outlive every stop.
#[cfg(windows)]
const PROGRAM: &str = "glpi-agent.exe";
#[cfg(windows)]
const PROGRAM_PATH: &str = "perl/bin/glpi-agent.exe";
#[cfg(not(windows))]
const PROGRAM: &str = "AppRun";
#[cfg(not(windows))]
const PROGRAM_PATH: &str = "AppRun";

/// The one absolute link the repacked Linux tree keeps (ADR-draft-glpi-agent clause 6): upstream's
/// default runtime reaches the host's own program loader through it, which is how `AppRun` runs
/// the agent on the host's C library. Public so the packing side packs exactly this link, and the
/// Client creates exactly this one.
pub const LINUX_TREE_LINKS: &[fleet_core::tree::KeptLink] = &[fleet_core::tree::KeptLink {
    path: "runtime/default/lib64/ld-linux-x86-64.so.2",
    target: "/lib64/ld-linux-x86-64.so.2",
}];

/// The Agent type every GLPI Configuration is aimed at (ADR-0016) — a property of the packing
/// side, which no host's block states.
const SERVICE_NAME: &str = "glpi-agent";

/// The Configuration `--conf-file` reads, by its name: the one `opamp-fleetctl package fetch`
/// uploads beside the package (ADR-draft-glpi-agent clause 10). Public so the packing side's test
/// holds both to one name — renamed on one side only, the agent would start against a file that
/// does not exist.
pub const CONFIG_ENTRY: &str = "glpi-agent-conf";

/// Where the agent keeps its own state. Deliberately **outside** `program/`: a package swap
/// replaces that directory whole and would take the agent's inventory history with it.
const STATE_DIR: &str = "agent-state";

/// The agent's own log, and the size in MiB it rotates at. A daemon with no console has nowhere
/// else to write, which is why this is not optional. It lies in a directory of its own beside
/// `program/`, which the kind names among the directories made before every spawn: where a process
/// account runs the agent, that is a directory it may write, and the Supervisor's own is not
/// (ADR-0037 clause 21).
const LOG_DIR: &str = "agent-log";
const LOG_FILE: &str = "glpi-agent.log";
const LOG_MAX_SIZE: &str = "16";

/// How the GLPI Agent is asked for its version — and, run against a *staged* program before the
/// running one is stopped, this kind's preflight (ADR-0017 clause 54). It answers the question a
/// repacked tree raises: does this host satisfy the interpreter we shipped it?
///
/// It is asked of the agent started exactly as the daemon is, after the same [`Lead`]: on Windows
/// the program is the bundled Perl, which answers a bare `--version` with its own banner
/// (`v5.38.2`), and on Linux `AppRun` without `--script` is the AppImage's installer, which
/// answers with *its* version line. Either would report a version that is not the agent's, and
/// prove at preflight only that the interpreter starts.
const VERSION_ARGS: &[&str] = &["--version"];

/// The version the GLPI Agent's banner names, written as SemVer by the rule a Package version is
/// written by (ADR-draft-glpi-agent clause 4, ADR-0028 clause 6). The banner's first line is
/// `GLPI Agent (1.19-1)` from the 1.19 AppImage: the number sits in parentheses, mostly has two
/// components, and may carry the package revision of upstream's build, so the strict SemVer read
/// of the version probe would find nothing in it. It is the upstream version, which a later
/// repack of the same release does not change; the Server decides by the
/// `supervisor.package.version` the Supervisor reports
/// (ADR-draft-deployments-and-rollouts clauses 14 and 25).
///
/// The read is strict in its own way. The first line has to be exactly that shape, and the number
/// has to be what a release tag is (ADR-draft-glpi-agent clause 7), two or three numeric parts,
/// followed at most by one package revision `-<digits>`, which is dropped: `1.19` and `1.19-1`
/// both read as `1.19.0`. Kept, the revision would be a SemVer pre-release and rank the build
/// *below* the release it was built from. Anything else reports none rather than a guess — a
/// development build's `1.22-dev`, a `1.19-1-gabcdef` or `1.19-1.2`, whose meaning upstream does
/// not document, and the banner of a program that is not the agent at all. The
/// `supervisor.package.version` then tracks the install alone. This is the one attribute the kind
/// reads from its agent, and a `service.*` key, which the description admits (ADR-0016 clause 8).
fn version_of_banner(text: &str) -> Option<String> {
    let line = text.trim_start().lines().next()?.trim_end();
    let number = line.strip_prefix("GLPI Agent (")?.strip_suffix(')')?;
    let numeric = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    let release = match number.split_once('-') {
        Some((release, revision)) => numeric(revision).then_some(release)?,
        None => number,
    };
    let parts: Vec<&str> = release.split('.').collect();
    ((2..=3).contains(&parts.len()) && parts.iter().all(|part| numeric(part)))
        .then(|| fleet_core::version::written_as_semver(release))
        .flatten()
}

/// The keys a `command` recipe carries and this kind supplies (ADR-draft-glpi-agent clause 1), each
/// with what answers it — refused by name, so an operator rewriting a `command` block is told where
/// each value went rather than meeting serde's "unknown field".
const RETIRED: &[(&str, &str)] = &[
    (
        "args",
        "the kind builds the agent's invocation whole, per platform",
    ),
    (
        "version_args",
        "the GLPI Agent prints its version for `--version`, which the kind asks for",
    ),
    (
        "working_dir",
        "a Managed Process starts in the directory its program lives in, and this kind names the \
         tree root where its program sits deeper than that",
    ),
    (
        "env",
        "the tree this kind delivers is self-contained and needs no environment of ours",
    ),
    (
        "reload_signal",
        "the GLPI Agent applies a configuration by restarting",
    ),
];

/// The root of the delivered tree — `program/tree` under this Supervisor's own directory
/// (ADR-0027 clause 56).
fn tree_root(ctx: &SupervisorContext) -> PathBuf {
    ctx.supervisor_dir
        .join(crate::config::PROGRAM_DIR)
        .join(crate::config::TREE_DIR)
}

/// What makes this platform's program the agent: the arguments in front of the agent's own, and
/// where it starts. The daemon, the version probe and the preflight all start with it, which is
/// what makes the version the probe reads the running agent's.
struct Lead {
    args: Vec<String>,
    working_dir: Option<PathBuf>,
}

/// The [`Lead`] for the tree rooted at `tree`, on Windows or not — a parameter rather than a
/// `cfg`, so both platforms' shape is asserted on every host.
///
/// On Linux the AppImage bundles several programs behind one entry point, and `--script` selects
/// the agent. It starts nowhere in particular: the program *is* the tree root's `AppRun`, so the
/// general derivation (ADR-0017 clause 14) already lands there.
///
/// On Windows the bundled Perl finds nothing on its own: four library roots, then the script named
/// by path rather than by `--script`. The program sits at `perl/bin/`, below what the bundled Perl
/// expects as its base, so the kind names the tree root, which is the directory upstream's own
/// portable `.bat` launcher moves to before invoking the agent.
fn lead(tree: &Path, windows: bool) -> Lead {
    if windows {
        let mut args: Vec<String> = ["perl/agent", "perl/site/lib", "perl/vendor/lib", "perl/lib"]
            .iter()
            .map(|lib| format!("-I{}", tree.join(lib).to_string_lossy()))
            .collect();
        args.push(
            tree.join("perl/bin/glpi-agent")
                .to_string_lossy()
                .into_owned(),
        );
        Lead {
            args,
            working_dir: Some(tree.to_path_buf()),
        }
    } else {
        Lead {
            args: vec!["--script=glpi-agent".to_string()],
            working_dir: None,
        }
    }
}

/// Where the process starts: the [`Lead`]'s, on this platform.
fn working_dir(ctx: &SupervisorContext) -> Option<PathBuf> {
    lead(&tree_root(ctx), cfg!(windows)).working_dir
}

/// How the running agent is asked for its version: the [`Lead`] of the live tree, then
/// [`VERSION_ARGS`].
fn version_probe(program: PathBuf, tree: &Path, windows: bool) -> VersionProbe {
    let Lead {
        mut args,
        working_dir,
    } = lead(tree, windows);
    args.extend(VERSION_ARGS.iter().map(|a| (*a).to_string()));
    VersionProbe {
        program,
        args,
        working_dir,
        parse: Some(version_of_banner),
        env: Vec::new(),
    }
}

/// The same question asked of a staged package: the [`Lead`] of the staged tree, whose root is
/// known only once it is staged and is therefore written as [`STAGED`]
/// (ADR-draft-supervisor-mode-and-its-kinds clause 56).
fn preflight(windows: bool) -> Preflight {
    let Lead {
        mut args,
        working_dir,
    } = lead(Path::new(STAGED), windows);
    args.extend(VERSION_ARGS.iter().map(|a| (*a).to_string()));
    Preflight {
        args,
        env: Vec::new(),
        working_dir: working_dir.map(|dir| dir.to_string_lossy().into_owned()),
    }
}

/// The arguments the agent is invoked with, per platform — derived whole, which is what the tests
/// below assert. The tail is common to both: where the configuration is, where state goes, and the
/// file logging a console-less daemon needs.
fn agent_args(ctx: &SupervisorContext) -> Vec<String> {
    let mut args = lead(&tree_root(ctx), cfg!(windows)).args;
    // Not defaults an operator may drop: see this module's header.
    args.push("--daemon".to_string());
    args.push("--no-fork".to_string());
    args.push(format!(
        "--conf-file={}",
        ctx.config_dir.join(CONFIG_ENTRY).to_string_lossy()
    ));
    args.push(format!(
        "--vardir={}",
        ctx.supervisor_dir.join(STATE_DIR).to_string_lossy()
    ));
    args.push("--logger=file".to_string());
    args.push(format!(
        "--logfile={}",
        ctx.supervisor_dir
            .join(LOG_DIR)
            .join(LOG_FILE)
            .to_string_lossy()
    ));
    args.push(format!("--logfile-maxsize={LOG_MAX_SIZE}"));
    args
}

/// The `glpi` kind. Its [`Spawner`] is the one the registry hands every plugin (ADR-0037
/// clause 18).
#[derive(Default)]
pub struct GlpiPlugin {
    pub spawner: Spawner,
}

impl Plugin for GlpiPlugin {
    fn kind(&self) -> &'static str {
        "glpi"
    }

    fn program_key(&self) -> &'static str {
        "command"
    }

    /// What `opamp-fleetctl package fetch --agent glpi-agent` packs decides, per platform
    /// (ADR-draft-glpi-agent clauses 2 and 4): the program's file name, where it sits inside the
    /// tree, and the Agent type it presents.
    fn defaults(&self) -> KindDefaults {
        KindDefaults {
            program: Some(PROGRAM),
            program_path: Some(PROGRAM_PATH),
            // A tree, so no single-file member to name.
            member: None,
            // Upstream's zip keeps no link; the repacked AppImage keeps the host loader's.
            tree_links: if cfg!(windows) { &[] } else { LINUX_TREE_LINKS },
            service_name: Some(SERVICE_NAME),
            // Wrapped, with nothing to correct: the fleet's `[supervisors]`/`[updates]` policy
            // stands, and the block says nothing about it (ADR-0017 clause 17).
            timing: Some(KindTiming::default()),
            // The GLPI Agent speaks no OpAMP to us; its Endpoint is bound and nothing connects.
            endpoint_port: false,
        }
    }

    fn start(&self, ctx: SupervisorContext) -> Result<mpsc::Sender<ProcessCommand>, String> {
        let (runner, commands, shutdown) = self.runner(ctx)?;
        tokio::spawn(runner.run(shutdown));
        Ok(commands)
    }

    fn check(&self, name: &str, settings: toml::Table) -> Result<(), String> {
        parse_settings::<GlpiSettings>(name, self.kind(), RETIRED, settings).map(|_| ())
    }
}

impl GlpiPlugin {
    /// The [`Runner`] this kind drives its agent with, the channel its commands arrive on, and the
    /// shutdown it ends on — everything [`Plugin::start`] does but spawn it.
    fn runner(
        &self,
        mut ctx: SupervisorContext,
    ) -> Result<
        (
            Runner,
            mpsc::Sender<ProcessCommand>,
            crate::shutdown::Shutdown,
        ),
        String,
    > {
        // Strictly empty: a wrapper that needed an escape hatch would be a wrapper that does not
        // know its agent (ADR-0017 clause 13).
        let _: GlpiSettings = parse_settings(
            &ctx.name,
            self.kind(),
            RETIRED,
            std::mem::take(&mut ctx.settings),
        )?;
        let args = agent_args(&ctx);
        let working_dir = working_dir(&ctx);
        let version_probe = version_probe(ctx.program.clone(), &tree_root(&ctx), cfg!(windows));
        let state_dir = ctx.supervisor_dir.join(STATE_DIR);
        let log_dir = ctx.supervisor_dir.join(LOG_DIR);
        let program = ctx.program;
        let (commands, command_rx) = mpsc::channel(16);
        let runner = Runner {
            // None. The GLPI Agent speaks no OpAMP and never connects to the Supervisor Endpoint,
            // so the token that admits a connection there would only sit in the environment of a
            // program whose Deploy task runs what the GLPI server sends it (ADR-draft-glpi-agent
            // clause 12). An empty token is one the Runner does not set
            // (ADR-draft-client-modes-and-the-gateway clause 28).
            endpoint_token: zeroize::Zeroizing::default(),
            name: ctx.name,
            stop_timeout: ctx.stop_timeout,
            apply_grace: ctx.apply_grace,
            retain_previous: ctx.retain_previous,
            install: Some(ctx.install),
            archive_key: ctx.archive_key,
            version_probe: Some(version_probe),
            preflight: Some(preflight(cfg!(windows))),
            // The GLPI Agent has no reload signal of its own, so a configuration applies by the
            // restart ADR-0017 clause 9 defines.
            reload_signal: None,
            // The Client itself, or the second service where a process account is registered
            // (ADR-0037 clause 18): the registry says which.
            spawner: self.spawner.clone(),
            process_account_group: ctx.process_account_group,
            supervisor_dir: ctx.supervisor_dir.clone(),
            events: ctx.events,
            commands: command_rx,
            build: Box::new(move || {
                Some(ProcessSpec {
                    program: program.clone(),
                    args: args.clone(),
                    env: Vec::new(),
                    working_dir: working_dir.clone(),
                    own_process_group: false,
                    // The agent exits when --vardir is missing and never creates it, so the fleet
                    // guarantees it here rather than asking every host to make it by hand. It sits
                    // outside program/, which a package swap replaces whole — which is also why
                    // the install cannot be the thing that creates it.
                    ensure_dirs: vec![state_dir.clone(), log_dir.clone()],
                })
            }),
        };
        Ok((runner, commands, ctx.shutdown))
    }
}

/// This kind has no settings at all (ADR-draft-glpi-agent clause 1): the block is `type` and
/// `name`.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GlpiSettings {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn context(root: &std::path::Path) -> SupervisorContext {
        let (_tx, shutdown) = crate::shutdown::shutdown_channel();
        let (events, _rx) = tokio::sync::mpsc::channel(1);
        SupervisorContext {
            endpoint_token: zeroize::Zeroizing::new(String::new()),
            name: "glpi".to_string(),
            supervisor_dir: root.to_path_buf(),
            config_dir: root.join("config"),
            program: root.join("program/tree").join(PROGRAM_PATH),
            install: crate::supervisor::ports::InstallTarget::Tree {
                root: root.join("program"),
                program_path: PathBuf::from(PROGRAM_PATH),
                links: GlpiPlugin::default().defaults().tree_links,
            },
            archive_key: None,
            process_account_group: None,
            settings: toml::Table::new(),
            stop_timeout: Duration::from_secs(1),
            apply_grace: Duration::from_secs(1),
            retain_previous: Duration::ZERO,
            events: crate::supervisor::ports::EventSender::new(0, events),
            shutdown,
        }
    }

    /// The block is two lines, so anything in it is a key nobody supplies.
    /// Verifies: ADR-draft-glpi-agent
    #[test]
    fn the_block_has_no_settings() {
        GlpiPlugin::default()
            .check("glpi", toml::Table::new())
            .expect("empty");
        let extra: toml::Table = toml::from_str("tag = \"edge\"").expect("table");
        let err = GlpiPlugin::default()
            .check("glpi", extra)
            .expect_err("refused");
        assert!(err.contains("tag"), "{err}");
    }

    /// Every key of a `command` recipe is refused by name with what supplies it — through `check`
    /// too, so an offered Supervisor set is refused before a running process is touched.
    /// Verifies: ADR-draft-glpi-agent
    #[test]
    fn the_recipes_keys_are_refused_by_name() {
        for (key, line) in [
            ("args", "args = [\"--daemon\"]"),
            ("version_args", "version_args = [\"--version\"]"),
            ("working_dir", "working_dir = \"/x\""),
            ("env", "env = { PERL5LIB = \"/x\" }"),
            ("reload_signal", "reload_signal = \"HUP\""),
        ] {
            let table: toml::Table = toml::from_str(line).expect("table");
            let err = GlpiPlugin::default().check("glpi", table).expect_err(key);
            assert!(err.contains(key), "{err}");
            assert!(err.contains("no longer a supervisor key"), "{err}");
        }
    }

    /// What this kind supplies is what `opamp-fleetctl package fetch` packs — the client half of
    /// `docs/artifacts/glpi-agent.md`, whose packing half is
    /// `glpi_finds_both_zip_spellings_and_repacks_only_linux`. An upstream release that moves the
    /// program should turn one of the two red rather than a rollout.
    /// Verifies: ADR-draft-glpi-agent
    #[test]
    fn the_defaults_are_the_artifacts() {
        let defaults = GlpiPlugin::default().defaults();
        assert_eq!(defaults.service_name, Some("glpi-agent"));
        assert!(!defaults.endpoint_port);
        // A tree package on both platforms, so the program is always named inside the tree; which
        // name that is, is asserted per platform by the two invocation tests below.
        assert!(defaults.program_path.is_some());
        // On Linux the one link the repacked tree keeps, the host's program loader, by its exact
        // path and target; on Windows none.
        #[cfg(not(windows))]
        assert_eq!(
            defaults.tree_links,
            [fleet_core::tree::KeptLink {
                path: "runtime/default/lib64/ld-linux-x86-64.so.2",
                target: "/lib64/ld-linux-x86-64.so.2",
            }]
        );
        #[cfg(windows)]
        assert!(defaults.tree_links.is_empty());
    }

    /// The two flags that are supervision requirements, the configuration the fleet delivers, and
    /// the state directory outside `program/` — the parts of the invocation whose loss would be a
    /// bug rather than a difference of taste.
    /// Verifies: ADR-draft-glpi-agent
    #[test]
    fn the_invocation_carries_what_supervision_requires() {
        let dir = tempfile::tempdir().expect("tempdir");
        let args = agent_args(&context(dir.path()));
        assert!(args.contains(&"--daemon".to_string()), "{args:?}");
        assert!(args.contains(&"--no-fork".to_string()), "{args:?}");
        let conf = format!(
            "--conf-file={}",
            dir.path()
                .join("config")
                .join(CONFIG_ENTRY)
                .to_string_lossy()
        );
        assert!(args.contains(&conf), "{args:?}");
        let state = dir.path().join(STATE_DIR);
        assert!(
            args.contains(&format!("--vardir={}", state.to_string_lossy())),
            "{args:?}"
        );
        // Beside `program/`, never inside it: a package swap replaces that directory whole and
        // would take the agent's inventory history with it.
        assert_eq!(state.parent(), Some(dir.path()));
        assert!(!state.starts_with(dir.path().join(crate::config::PROGRAM_DIR)));
    }

    /// The agent logs to a file of its own, in `agent-log/` beside `program/`, and both that
    /// directory and the state directory are made before every spawn, so a process account may
    /// write them and the agent never meets them missing (ADR-0037 clause 21).
    /// Verifies: ADR-draft-glpi-agent
    #[test]
    fn the_agent_logs_to_a_file_in_a_directory_made_for_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let args = agent_args(&context(dir.path()));
        let log_dir = dir.path().join("agent-log");
        for flag in [
            "--logger=file".to_string(),
            format!(
                "--logfile={}",
                log_dir.join("glpi-agent.log").to_string_lossy()
            ),
            "--logfile-maxsize=16".to_string(),
        ] {
            assert!(args.contains(&flag), "{flag}: {args:?}");
        }
        assert!(!log_dir.starts_with(dir.path().join(crate::config::PROGRAM_DIR)));

        let (runner, _commands, _shutdown) = GlpiPlugin::default()
            .runner(context(dir.path()))
            .expect("a runner");
        let spec = (runner.build)().expect("the agent always runs");
        assert_eq!(
            spec.ensure_dirs,
            vec![dir.path().join("agent-state"), log_dir]
        );
        assert_eq!(spec.args, args);
    }

    /// `--conf-file` reads the Configuration by the name `opamp-fleetctl package fetch` uploads
    /// it under; the packing side's half of this is
    /// `the_glpi_configuration_is_the_file_the_kind_points_its_agent_at`.
    /// Verifies: ADR-draft-glpi-agent
    #[test]
    fn the_agent_reads_the_configuration_by_its_contracted_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let args = agent_args(&context(dir.path()));
        let conf = format!(
            "--conf-file={}",
            dir.path()
                .join("config")
                .join("glpi-agent-conf")
                .to_string_lossy()
        );
        assert_eq!(CONFIG_ENTRY, "glpi-agent-conf");
        assert!(args.contains(&conf), "{args:?}");
    }

    /// The version probe and the preflight ask the agent started as the daemon is — after the same
    /// lead arguments and in the same working directory, the preflight's for the staged tree — on
    /// both platforms, so what they read is the agent's answer and not its interpreter's: a bare
    /// `--version` reaches Perl's own banner on Windows and the AppImage installer's on Linux.
    /// Verifies: ADR-draft-glpi-agent
    #[test]
    fn the_version_is_asked_of_the_agent_the_daemon_runs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tree = dir.path().join("program/tree");
        for windows in [false, true] {
            let lead = lead(&tree, windows);
            let probe = version_probe(tree.join(PROGRAM_PATH), &tree, windows);
            let mut asked = lead.args.clone();
            asked.push("--version".to_string());
            assert_eq!(probe.args, asked, "windows: {windows}");
            assert_eq!(probe.working_dir, lead.working_dir, "windows: {windows}");

            let staged = preflight(windows);
            let root = tree.to_string_lossy();
            let resolved: Vec<String> = staged
                .args
                .iter()
                .map(|arg| arg.replace(STAGED, &root))
                .collect();
            assert_eq!(resolved, asked, "windows: {windows}");
            assert_eq!(
                staged
                    .working_dir
                    .map(|dir| PathBuf::from(dir.replace(STAGED, &root))),
                lead.working_dir,
                "windows: {windows}"
            );
            assert!(staged.env.is_empty());
        }
        // The two leads themselves, as the artifact document states them.
        assert_eq!(lead(&tree, false).args, vec!["--script=glpi-agent"]);
        assert_eq!(lead(&tree, false).working_dir, None);
        let windows = lead(&tree, true);
        assert_eq!(windows.args.len(), 5);
        assert!(windows.args[..4].iter().all(|arg| arg.starts_with("-I")));
        assert_eq!(windows.working_dir.as_deref(), Some(tree.as_path()));
    }

    /// The agent is handed no Supervisor Endpoint token: it never connects to the Endpoint, and a
    /// token in its environment would reach whatever its Deploy task runs.
    /// Verifies: ADR-draft-glpi-agent
    #[test]
    fn the_agent_is_handed_no_endpoint_token() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut ctx = context(dir.path());
        ctx.endpoint_token = zeroize::Zeroizing::new("the token".to_string());
        let (runner, _commands, _shutdown) = GlpiPlugin::default().runner(ctx).expect("a runner");
        assert!(runner.endpoint_token.is_empty());
    }

    /// The banner `--version` prints names the version in parentheses, mostly in two parts and
    /// with the package revision of upstream's build, as the 1.19 AppImage's `GLPI Agent (1.19-1)`;
    /// it is reported as the upstream version without the revision, written as SemVer, which is
    /// the Package version of the release's first repack. A later repack keeps the banner, so
    /// after one the two differ, and the Server decides by `supervisor.package.version`.
    /// Verifies: ADR-draft-glpi-agent, ADR-0028
    #[test]
    fn the_version_banner_reads_as_the_package_version() {
        assert_eq!(
            version_of_banner("GLPI Agent (1.19-1)\n").as_deref(),
            Some("1.19.0")
        );
        assert_eq!(
            version_of_banner("GLPI Agent (1.19)\n").as_deref(),
            Some("1.19.0")
        );
        assert_eq!(
            version_of_banner("GLPI Agent (1.7.1-12)").as_deref(),
            Some("1.7.1")
        );
        assert_eq!(
            version_of_banner("GLPI Agent (1.7.1)").as_deref(),
            Some("1.7.1")
        );
        // What the probe hands over is stdout, then stderr; the comments upstream prints below
        // the banner, and Windows line ends, change nothing.
        assert_eq!(
            version_of_banner("GLPI Agent (1.19-1)\r\nProvided by GLPI\r\n ").as_deref(),
            Some("1.19.0")
        );
        assert_eq!(
            version_of_banner("GLPI Agent (01.019)").as_deref(),
            Some("1.19.0")
        );
    }

    /// Only a banner of the agent's own shape, carrying a number a release tag could carry and at
    /// most a numeric package revision, reports a version: a later token is never read in its
    /// place, and no other suffix is guessed at — taken for a pre-release, it would rank below the
    /// release and hold every update back.
    /// Verifies: ADR-draft-glpi-agent
    #[test]
    fn a_banner_that_is_not_a_release_reports_no_version() {
        for none in [
            "GLPI Agent",
            "GLPI Agent ()",
            "GLPI Agent (dev)",
            "GLPI Agent (1)",
            "GLPI Agent (1-1)",
            "GLPI Agent (1.2.3.4)",
            "GLPI Agent (1.2.3.4-1)",
            "GLPI Agent (1.19-)",
            "GLPI Agent (1.19-dev)",
            "GLPI Agent (1.19-1-gabcdef)",
            "GLPI Agent (1.19-1.2)",
            "GLPI Agent (1.19+1)",
            "GLPI Agent (1.22-dev)",
            "GLPI Agent (v1.19)",
            "GLPI Agent (1.19) built 2.0.0",
            "Agent 2",
            // What the programs that share the agent's file print for a bare `--version`.
            "This is perl 5, version 38, subversion 2 (v5.38.2) built for MSWin32-x64-multi-thread",
            "GLPI Agent AppImage installer v1.19",
            // A version on a later line is not the banner's.
            "Can't locate setup.pm in @INC\nGLPI Agent (1.19)",
            "stub_agent version 9.9.9 (test build)",
        ] {
            assert_eq!(version_of_banner(none), None, "{none}");
        }
    }

    /// The Linux invocation: the AppImage's entry point picks the agent by `--script`, and the
    /// program is the tree root itself, so the general working-directory derivation is right and
    /// the kind names none (`docs/artifacts/glpi-agent.md`).
    /// Verifies: ADR-draft-glpi-agent
    #[cfg(not(windows))]
    #[test]
    fn linux_runs_the_apprun_entry_point() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = context(dir.path());
        assert_eq!(
            agent_args(&ctx).first().map(String::as_str),
            Some("--script=glpi-agent")
        );
        assert_eq!(working_dir(&ctx), None);
        assert_eq!(GlpiPlugin::default().defaults().program, Some("AppRun"));
        assert_eq!(
            GlpiPlugin::default().defaults().program_path,
            Some("AppRun")
        );
    }

    /// The Windows invocation: upstream's zip as published, so the program sits inside the bundled
    /// Perl and the four library roots plus the script path are what the portable launcher sets —
    /// including the working directory, which the general derivation cannot supply here because the
    /// program is not at the tree root (`docs/artifacts/glpi-agent.md`).
    /// Verifies: ADR-draft-glpi-agent
    #[cfg(windows)]
    #[test]
    fn windows_runs_the_bundled_perl() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = context(dir.path());
        let tree = tree_root(&ctx);
        let args = agent_args(&ctx);
        for lib in ["perl/agent", "perl/site/lib", "perl/vendor/lib", "perl/lib"] {
            let flag = format!("-I{}", tree.join(lib).to_string_lossy());
            assert!(args.contains(&flag), "{args:?}");
        }
        assert!(
            args.contains(
                &tree
                    .join("perl/bin/glpi-agent")
                    .to_string_lossy()
                    .into_owned()
            ),
            "{args:?}"
        );
        assert_eq!(working_dir(&ctx), Some(tree));
        assert_eq!(
            GlpiPlugin::default().defaults().program,
            Some("glpi-agent.exe")
        );
        assert_eq!(
            GlpiPlugin::default().defaults().program_path,
            Some("perl/bin/glpi-agent.exe")
        );
    }
}
