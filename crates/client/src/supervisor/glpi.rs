//! The `glpi` plugin (ADR-0093): the GLPI Agent, delivered as a package tree and run out of this
//! Supervisor's own directory.
//!
//! This is the agent that shows most plainly why a kind beats a recipe. Its two platform
//! invocations differ in nearly everything — the program's name and where it sits in the tree, the
//! working directory, and on Windows four Perl `-I` paths and the script named by path — and not
//! one of those differences is a decision anybody makes. They follow from `EXE_SUFFIX` and from
//! where the AppImage this project repacks puts its interpreter (ADR-0064), so they belong to the
//! side that packs the artifact, not to every host's file.
//!
//! Two flags are supervision requirements rather than preferences, and are therefore not the
//! operator's to drop. Without `--daemon` the agent runs its tasks once and exits, and the watchdog
//! restarts it for ever. Without `--no-fork` it detaches, leaving the Supervisor holding a pid that
//! ends immediately while the real process runs on unsupervised.

use std::path::PathBuf;

use tokio::sync::mpsc;

use crate::supervisor::ports::{
    KindDefaults, KindTiming, Plugin, ProcessCommand, SupervisorContext,
};
use crate::supervisor::process::{Preflight, ProcessSpec, Runner, VersionProbe};

/// What this project's own packaging puts in the tree, per platform (ADR-0064,
/// `docs/artifacts/glpi-agent.md`). On Linux the repacked AppImage's entry point is `AppRun` at the
/// tree root — it bundles several programs, and `--script=glpi-agent` picks this one. On Windows
/// upstream's zip is installed as published, with the program deep inside the bundled Perl.
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

/// The Agent type every GLPI Configuration is aimed at (ADR-0033), and the Configuration name
/// `opamp-package-fetch` uploads — both properties of the packing side, written on every host until
/// now.
const SERVICE_NAME: &str = "glpi-agent";
const CONFIG_ENTRY: &str = "glpi-agent-conf";

/// Where the agent keeps its own state. Deliberately **outside** `program/`: a package swap
/// replaces that directory whole and would take the agent's inventory history with it.
const STATE_DIR: &str = "agent-state";

/// The agent's own log file, and the size in MiB it rotates at. A daemon with no console has
/// nowhere else to write, which is why this is not optional.
const LOG_FILE: &str = "glpi-agent.log";
const LOG_MAX_SIZE: &str = "16";

/// How the GLPI Agent is asked for its version — and, run against a *staged* program before the
/// running one is stopped, this kind's preflight (ADR-0068). It answers the question a repacked
/// tree raises: does this host satisfy the interpreter we shipped it?
const VERSION_ARGS: &[&str] = &["--version"];

/// The keys the `command` recipe carried and this kind now supplies (ADR-0093), each with what
/// answers it now — refused by name, so an operator rewriting the old block is told where each
/// value went rather than meeting serde's "unknown field".
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

/// Refuses a retired key by name, before the strict parse turns it into "unknown field".
fn refuse_retired(name: &str, settings: &toml::Table) -> Result<(), String> {
    for (key, answer) in RETIRED {
        if settings.contains_key(*key) {
            return Err(format!(
                "supervisor {name:?}: `{key}` is no longer a supervisor key for type \"glpi\" — \
                 {answer}; remove the line"
            ));
        }
    }
    Ok(())
}

/// The root of the delivered tree — `program/tree` under this Supervisor's own directory
/// (ADR-0023).
fn tree_root(ctx: &SupervisorContext) -> PathBuf {
    ctx.supervisor_dir
        .join(crate::config::PROGRAM_DIR)
        .join(crate::config::TREE_DIR)
}

/// Where the process starts.
///
/// On Linux this is `None`: the program *is* the tree root's `AppRun`, so the general derivation
/// (ADR-0091) already lands there. On Windows the program sits at `perl/bin/`, four levels from
/// what the bundled Perl expects as its base — so the kind names the tree root, which is exactly
/// what upstream's own portable `.bat` launcher does before invoking the agent.
fn working_dir(ctx: &SupervisorContext) -> Option<PathBuf> {
    if cfg!(windows) {
        Some(tree_root(ctx))
    } else {
        None
    }
}

/// The arguments the agent is invoked with, per platform — derived whole, which is what the tests
/// below assert. The tail is common to both: where the configuration is, where state goes, and the
/// file logging a console-less daemon needs.
fn agent_args(ctx: &SupervisorContext) -> Vec<String> {
    let tree = tree_root(ctx);
    let mut args: Vec<String> = Vec::new();
    if cfg!(windows) {
        // The bundled Perl finds nothing on its own: these are the four library roots upstream's
        // launcher sets, followed by the script named by path rather than by `--script`.
        for lib in ["perl/agent", "perl/site/lib", "perl/vendor/lib", "perl/lib"] {
            args.push(format!("-I{}", tree.join(lib).to_string_lossy()));
        }
        args.push(
            tree.join("perl/bin/glpi-agent")
                .to_string_lossy()
                .into_owned(),
        );
    } else {
        // The AppImage bundles several programs behind one entry point; this selects the agent.
        args.push("--script=glpi-agent".to_string());
    }
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
        ctx.supervisor_dir.join(LOG_FILE).to_string_lossy()
    ));
    args.push(format!("--logfile-maxsize={LOG_MAX_SIZE}"));
    args
}

pub struct GlpiPlugin;

impl Plugin for GlpiPlugin {
    fn kind(&self) -> &'static str {
        "glpi"
    }

    fn program_key(&self) -> &'static str {
        "command"
    }

    /// What `opamp-package-fetch --agent glpi` packs decides, per platform (ADR-0093): the
    /// program's file name, where it sits inside the tree, and the Agent type it presents.
    fn defaults(&self) -> KindDefaults {
        KindDefaults {
            program: Some(PROGRAM),
            program_path: Some(PROGRAM_PATH),
            service_name: Some(SERVICE_NAME),
            // Wrapped, with nothing to correct: the fleet's `[supervisors]`/`[updates]` policy
            // stands, and the block says nothing about it (ADR-0091).
            timing: Some(KindTiming::default()),
            // The GLPI Agent speaks no OpAMP to us; its Endpoint is bound and nothing connects.
            endpoint_port: false,
        }
    }

    fn start(&self, mut ctx: SupervisorContext) -> Result<mpsc::Sender<ProcessCommand>, String> {
        let raw = std::mem::take(&mut ctx.settings);
        refuse_retired(&ctx.name, &raw)?;
        // Strictly empty: a wrapper that needed an escape hatch would be a wrapper that does not
        // know its agent (ADR-0091).
        let _: GlpiSettings = raw
            .try_into()
            .map_err(|e| format!("supervisor {:?}: {e}", ctx.name))?;
        let args = agent_args(&ctx);
        let working_dir = working_dir(&ctx);
        let state_dir = ctx.supervisor_dir.join(STATE_DIR);
        let program = ctx.program.clone();
        let (commands, command_rx) = mpsc::channel(16);
        let version_args: Vec<String> = VERSION_ARGS.iter().map(|a| (*a).to_string()).collect();
        let runner = Runner {
            name: ctx.name,
            stop_timeout: ctx.stop_timeout,
            apply_grace: ctx.apply_grace,
            retain_previous: ctx.retain_previous,
            install: Some(ctx.install),
            archive_key: ctx.archive_key.clone(),
            version_probe: Some(VersionProbe {
                program: program.clone(),
                args: version_args.clone(),
                parse: None,
            }),
            preflight: Some(Preflight {
                args: version_args,
                env: Vec::new(),
            }),
            // The GLPI Agent has no reload signal of its own, so a configuration applies by the
            // restart ADR-0060 defines.
            reload_signal: None,
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
                    ensure_dirs: vec![state_dir.clone()],
                })
            }),
        };
        tokio::spawn(runner.run(ctx.shutdown));
        Ok(commands)
    }

    fn check(&self, name: &str, settings: toml::Table) -> Result<(), String> {
        refuse_retired(name, &settings)?;
        let _: GlpiSettings = settings
            .try_into()
            .map_err(|e| format!("supervisor {name:?}: {e}"))?;
        Ok(())
    }
}

/// This kind has no settings at all (ADR-0093): the block is `type` and `name`.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GlpiSettings {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn context(root: &std::path::Path) -> SupervisorContext {
        let (_tx, shutdown) = crate::service::runtime::shutdown_channel();
        let (events, _rx) = tokio::sync::mpsc::channel(1);
        SupervisorContext {
            name: "glpi".to_string(),
            supervisor_dir: root.to_path_buf(),
            config_dir: root.join("config"),
            program: root.join("program/tree").join(PROGRAM_PATH),
            install: crate::supervisor::process::InstallTarget::Tree {
                root: root.join("program"),
                program_path: PathBuf::from(PROGRAM_PATH),
            },
            archive_key: None,
            settings: toml::Table::new(),
            stop_timeout: Duration::from_secs(1),
            apply_grace: Duration::from_secs(1),
            retain_previous: Duration::ZERO,
            events: crate::supervisor::ports::EventSender::new(0, events),
            shutdown,
        }
    }

    /// The block is two lines, so anything in it is a key nobody supplies.
    #[test]
    fn the_block_has_no_settings() {
        GlpiPlugin.check("glpi", toml::Table::new()).expect("empty");
        let extra: toml::Table = toml::from_str("tag = \"edge\"").expect("table");
        let err = GlpiPlugin.check("glpi", extra).expect_err("refused");
        assert!(err.contains("tag"), "{err}");
    }

    /// Every key of the old recipe is refused by name with what supplies it now — through `check`
    /// too, so an offered Supervisor set is refused before a running process is touched.
    #[test]
    fn the_recipes_keys_are_refused_by_name() {
        for (key, line) in [
            ("args", "args = [\"--daemon\"]"),
            ("version_args", "version_args = [\"--version\"]"),
            ("working_dir", "working_dir = \"/x\""),
            ("reload_signal", "reload_signal = \"HUP\""),
        ] {
            let table: toml::Table = toml::from_str(line).expect("table");
            let err = GlpiPlugin.check("glpi", table).expect_err(key);
            assert!(err.contains(key), "{err}");
            assert!(err.contains("no longer a supervisor key"), "{err}");
        }
    }

    /// What this kind supplies is what `opamp-package-fetch` packs — the client half of
    /// `docs/artifacts/glpi-agent.md`, whose packing half is
    /// `glpi_finds_both_zip_spellings_and_repacks_only_linux`. An upstream release that moves the
    /// program should turn one of the two red rather than a rollout.
    #[test]
    fn the_defaults_are_the_artifacts() {
        let defaults = GlpiPlugin.defaults();
        assert_eq!(defaults.service_name, Some("glpi-agent"));
        assert!(!defaults.endpoint_port);
        // A tree package on both platforms, so the program is always named inside the tree; which
        // name that is, is asserted per platform by the two invocation tests below.
        assert!(defaults.program_path.is_some());
    }

    /// The two flags that are supervision requirements, the configuration the fleet delivers, and
    /// the state directory outside `program/` — the parts of the invocation whose loss would be a
    /// bug rather than a difference of taste.
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

    /// The Linux invocation: the AppImage's entry point picks the agent by `--script`, and the
    /// program is the tree root itself, so the general working-directory derivation is right and
    /// the kind names none (`docs/artifacts/glpi-agent.md`).
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
        assert_eq!(GlpiPlugin.defaults().program, Some("AppRun"));
        assert_eq!(GlpiPlugin.defaults().program_path, Some("AppRun"));
    }

    /// The Windows invocation: upstream's zip as published, so the program sits inside the bundled
    /// Perl and the four library roots plus the script path are what the portable launcher sets —
    /// including the working directory, which the general derivation cannot supply here because the
    /// program is not at the tree root (`docs/artifacts/glpi-agent.md`).
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
        assert_eq!(GlpiPlugin.defaults().program, Some("glpi-agent.exe"));
        assert_eq!(
            GlpiPlugin.defaults().program_path,
            Some("perl/bin/glpi-agent.exe")
        );
    }
}
