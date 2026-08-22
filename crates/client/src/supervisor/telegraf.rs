//! The `telegraf` plugin (ADR-0094): Telegraf, delivered as a package and run out of this
//! Supervisor's own directory.
//!
//! There is nothing to decide about how Telegraf is invoked. It is a single-file program, it takes
//! its configuration from `--config`, it prints a Semantic Versioning version for `--version`, and
//! it re-reads its configuration on `SIGHUP`. All four are Telegraf's own properties, and as long
//! as they were block keys every host repeated them — one of them, the reload signal, in a form no
//! mixed fleet could write at all: the key is refused on Windows at parse time, so a Supervisor set
//! carrying it was rejected by every Windows host in the fleet. Here the reload is the kind's, and
//! platform-correct by construction: the signal where there are signals, and the Runner's restart
//! where there are none.

use tokio::sync::mpsc;

use crate::supervisor::ports::{
    KindDefaults, KindTiming, Plugin, ProcessCommand, SupervisorContext,
};
use crate::supervisor::process::{Preflight, ProcessSpec, Runner, VersionProbe};

/// InfluxData's archive holds one program of this name, and the installer finds the member by it
/// (ADR-0094) — which is why the path *inside* the archive does not matter and there is no
/// `program_path` here.
#[cfg(windows)]
const PROGRAM: &str = "telegraf.exe";
#[cfg(not(windows))]
const PROGRAM: &str = "telegraf";

/// The Agent type every Telegraf Configuration is aimed at (ADR-0033), and the name
/// `opamp-package-fetch` uploads its default Configuration under. The second is the first plus
/// `-conf`, and both are properties of the packing side rather than of a host.
const SERVICE_NAME: &str = "telegraf";
const CONFIG_ENTRY: &str = "telegraf-conf";

/// How Telegraf is asked for its version — and, run against a *staged* program before the running
/// one is stopped, this kind's preflight (ADR-0068). The same arguments serve both, because what
/// makes them a version probe is what makes them a safe check: cheap, and touching no state.
const VERSION_ARGS: &[&str] = &["--version"];

/// The keys this kind used to take as a `command` recipe and now supplies itself (ADR-0094), each
/// with what answers it now — refused by name rather than met with serde's "unknown field", so an
/// operator rewriting the old block is told where each value went.
const RETIRED: &[(&str, &str)] = &[
    ("args", "the kind points Telegraf at its delivered configuration"),
    (
        "version_args",
        "Telegraf prints a Semantic Versioning version for `--version`, which the kind asks for",
    ),
    (
        "reload_signal",
        "Telegraf re-reads its configuration on SIGHUP, which the kind applies where signals exist \
         and replaces with a restart where they do not",
    ),
    (
        "env",
        "a single-file program published as-is needs no environment of ours",
    ),
    (
        "working_dir",
        "a Managed Process starts in the directory its program lives in",
    ),
];

/// Refuses a retired key by name, before the strict parse turns it into "unknown field".
fn refuse_retired(name: &str, settings: &toml::Table) -> Result<(), String> {
    for (key, answer) in RETIRED {
        if settings.contains_key(*key) {
            return Err(format!(
                "supervisor {name:?}: `{key}` is no longer a supervisor key for type \"telegraf\" \
                 — {answer}; remove the line"
            ));
        }
    }
    Ok(())
}

/// `SIGHUP` is Telegraf's documented way of re-reading its configuration — the same signal
/// `systemctl reload` sends it. Windows has no signal at all, and the Runner falls back to the
/// restart there, which is the whole reason this is a property of the kind rather than a key: the
/// one block then serves both platforms.
#[cfg(unix)]
fn reload_signal() -> Option<i32> {
    Some(libc::SIGHUP)
}

#[cfg(not(unix))]
fn reload_signal() -> Option<i32> {
    None
}

/// The arguments the daemon is started with — derived whole, so the test below is the statement of
/// what a host runs.
fn daemon_args(ctx: &SupervisorContext) -> Vec<String> {
    vec![
        "--config".to_string(),
        ctx.config_dir
            .join(CONFIG_ENTRY)
            .to_string_lossy()
            .into_owned(),
    ]
}

pub struct TelegrafPlugin;

impl Plugin for TelegrafPlugin {
    fn kind(&self) -> &'static str {
        "telegraf"
    }

    fn program_key(&self) -> &'static str {
        "command"
    }

    /// A single file, so there is no tree and no `program_path`: the installer finds the member
    /// whose file name matches, and installs the archive as InfluxData published it (ADR-0094).
    fn defaults(&self) -> KindDefaults {
        KindDefaults {
            program: Some(PROGRAM),
            program_path: None,
            service_name: Some(SERVICE_NAME),
            // Wrapped, with nothing to correct: the fleet's `[supervisors]`/`[updates]` policy
            // stands, and the block says nothing about it (ADR-0091).
            timing: Some(KindTiming::default()),
            // Telegraf speaks no OpAMP to us; its Endpoint is bound and nothing connects to it.
            endpoint_port: false,
        }
    }

    fn start(&self, mut ctx: SupervisorContext) -> Result<mpsc::Sender<ProcessCommand>, String> {
        let raw = std::mem::take(&mut ctx.settings);
        refuse_retired(&ctx.name, &raw)?;
        // Strictly empty: a kind that knows its agent has no escape hatch (ADR-0091), so anything
        // left here is a key nobody supplies.
        let _: TelegrafSettings = raw
            .try_into()
            .map_err(|e| format!("supervisor {:?}: {e}", ctx.name))?;
        let args = daemon_args(&ctx);
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
                // Telegraf's banner carries a plain Semantic Versioning version, so the strict
                // default read finds it and no parser of its own is needed.
                parse: None,
            }),
            preflight: Some(Preflight {
                args: version_args,
                env: Vec::new(),
            }),
            reload_signal: reload_signal(),
            events: ctx.events,
            commands: command_rx,
            build: Box::new(move || {
                // Telegraf starts whether or not the fleet has delivered anything: with no
                // configuration it exits and the Runner reports that, which is the honest state —
                // exactly what the `command` recipe did before this kind existed.
                Some(ProcessSpec {
                    program: program.clone(),
                    args: args.clone(),
                    env: Vec::new(),
                    // Its own program's directory (ADR-0091).
                    working_dir: None,
                    // One process, no worker of its own.
                    own_process_group: false,
                    // Telegraf writes to its outputs, not to a directory of its own.
                    ensure_dirs: Vec::new(),
                })
            }),
        };
        tokio::spawn(runner.run(ctx.shutdown));
        Ok(commands)
    }

    fn check(&self, name: &str, settings: toml::Table) -> Result<(), String> {
        refuse_retired(name, &settings)?;
        let _: TelegrafSettings = settings
            .try_into()
            .map_err(|e| format!("supervisor {name:?}: {e}"))?;
        Ok(())
    }
}

/// This kind has no settings at all — the strict parse accepts an empty table and refuses every
/// key (ADR-0094). Written as a type rather than a length check so the refusal reads the same as
/// every other plugin's.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TelegrafSettings {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole block is `type` and `name`, so anything else is a key nobody supplies.
    #[test]
    fn the_block_has_no_settings() {
        let empty: toml::Table = toml::Table::new();
        TelegrafPlugin.check("telegraf", empty).expect("empty");

        let extra: toml::Table = toml::from_str("interval = \"10s\"").expect("table");
        let err = TelegrafPlugin
            .check("telegraf", extra)
            .expect_err("refused");
        assert!(err.contains("interval"), "{err}");
    }

    /// Each key the old `command` recipe carried is refused by name with what supplies it now —
    /// including through `check`, so a Supervisor set the Server offers is refused before any
    /// running process is touched (ADR-0056).
    #[test]
    fn the_recipes_keys_are_refused_by_name() {
        for (key, line) in [
            ("args", "args = [\"--config\", \"/x\"]"),
            ("version_args", "version_args = [\"--version\"]"),
            ("reload_signal", "reload_signal = \"HUP\""),
            ("working_dir", "working_dir = \"/x\""),
        ] {
            let table: toml::Table = toml::from_str(line).expect("table");
            let err = TelegrafPlugin.check("telegraf", table).expect_err(key);
            assert!(err.contains(key), "{err}");
            assert!(err.contains("no longer a supervisor key"), "{err}");
        }
    }

    /// The whole invocation: Telegraf is pointed at the one Configuration the fleet delivers for
    /// it, in this Supervisor's own `config/` directory.
    #[test]
    fn the_invocation_points_at_the_delivered_configuration() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_tx, shutdown) = crate::service::runtime::shutdown_channel();
        let (events, _rx) = tokio::sync::mpsc::channel(1);
        let ctx = SupervisorContext {
            name: "telegraf".to_string(),
            supervisor_dir: dir.path().to_path_buf(),
            config_dir: dir.path().join("config"),
            program: dir.path().join("program").join(PROGRAM),
            install: crate::supervisor::process::InstallTarget::Binary(
                dir.path().join("program").join(PROGRAM),
            ),
            archive_key: None,
            settings: toml::Table::new(),
            stop_timeout: std::time::Duration::from_secs(1),
            apply_grace: std::time::Duration::from_secs(1),
            retain_previous: std::time::Duration::ZERO,
            events: crate::supervisor::ports::EventSender::new(0, events),
            shutdown,
        };
        assert_eq!(
            daemon_args(&ctx),
            vec![
                "--config".to_string(),
                dir.path()
                    .join("config")
                    .join(CONFIG_ENTRY)
                    .to_string_lossy()
                    .into_owned(),
            ]
        );
    }

    /// What the kind supplies is what `opamp-package-fetch` packs and uploads — the client half
    /// of `docs/artifacts/telegraf.md`, whose packing half is
    /// `telegraf_urls_carry_upstreams_spelling_and_the_platform_this_fleet_names`. An upstream
    /// that renames the program turns this red rather than a rollout.
    #[test]
    fn the_defaults_are_the_artifacts() {
        let defaults = TelegrafPlugin.defaults();
        assert_eq!(defaults.service_name, Some("telegraf"));
        assert_eq!(
            defaults.program_path, None,
            "a single-file package has no tree"
        );
        assert!(!defaults.endpoint_port);
        #[cfg(windows)]
        assert_eq!(defaults.program, Some("telegraf.exe"));
        #[cfg(not(windows))]
        assert_eq!(defaults.program, Some("telegraf"));
    }
}
