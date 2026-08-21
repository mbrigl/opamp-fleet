//! The `command` plugin: the example Custom Supervisor (ADR-0011). It brings a Foreign Agent —
//! any process started by a command-line invocation — under management: spawned as configured,
//! restarted when a remote configuration arrives (the files land in the Supervisor's
//! `config/` directory for the process to re-read), health derived from the outside.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::debug;

use crate::supervisor::ports::{Plugin, ProcessCommand, SupervisorContext};
use crate::supervisor::process::{Preflight, ProcessSpec, Runner, VersionProbe};

/// The block's plugin-specific keys, parsed strictly — a typo fails startup, per ADR-0008.
///
/// `command` is not among them: the core takes it out and resolves it (ADR-0021), and what
/// arrives here is [`SupervisorContext::program`].
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandSettings {
    /// Its arguments, verbatim.
    #[serde(default)]
    args: Vec<String>,
    /// Additional environment for the process.
    #[serde(default)]
    env: BTreeMap<String, String>,
    /// The working directory to start in.
    #[serde(default)]
    working_dir: Option<PathBuf>,
    /// Arguments that make the command print its version (e.g. `["--version"]`). When set, the
    /// command is invoked once with exactly these arguments and the first Semantic Versioning
    /// 2.0.0 version in its output becomes the Agent's `service.version`. A Foreign Agent's
    /// version flag is its own convention — hence opt-in, unlike the Collector's.
    ///
    /// They are also this kind's **preflight** (ADR-0068): a package's staged program is run with
    /// them before the running one is stopped, and a non-zero exit refuses the package with the
    /// program's own message. Same arguments, same contract — a check that is cheap and touches
    /// no state — asked where a refusal costs nothing rather than after the swap.
    #[serde(default)]
    version_args: Option<Vec<String>>,
    /// The signal that makes this process re-read its configuration in place (e.g. `"HUP"`),
    /// applied instead of the restart (ADR-0060); a reload that fails still falls back to the
    /// restart. Whether a daemon reloads on a signal is its own convention — hence opt-in, and
    /// unix-only: a set key is refused on Windows at parse time, not at the first apply.
    #[serde(default)]
    reload_signal: Option<String>,
}

/// Maps a declared reload signal (ADR-0060) to its number. Only the signals daemons
/// conventionally re-read configuration on — a stop signal here would turn every apply into a
/// kill, so anything unknown is refused loudly (ADR-0008), with or without a `SIG` prefix.
#[cfg(unix)]
fn reload_signal(name: &str, value: &str) -> Result<i32, String> {
    match value.strip_prefix("SIG").unwrap_or(value) {
        "HUP" => Ok(libc::SIGHUP),
        "USR1" => Ok(libc::SIGUSR1),
        "USR2" => Ok(libc::SIGUSR2),
        _ => Err(format!(
            "supervisor {name:?}: unknown `reload_signal` {value:?} (known: HUP, USR1, USR2)"
        )),
    }
}

/// Windows has no signal a process can reload on, so a set key is a configuration error — the
/// operator learns it at startup, not from a Supervisor that silently restarts instead.
#[cfg(not(unix))]
fn reload_signal(name: &str, value: &str) -> Result<i32, String> {
    let _ = value;
    Err(format!(
        "supervisor {name:?}: `reload_signal` is unix-only — this platform has no signal a \
         process can reload on"
    ))
}

pub struct CommandPlugin;

impl Plugin for CommandPlugin {
    fn kind(&self) -> &'static str {
        "command"
    }

    fn program_key(&self) -> &'static str {
        "command"
    }

    fn start(&self, mut ctx: SupervisorContext) -> Result<mpsc::Sender<ProcessCommand>, String> {
        // Taken out rather than consumed with `ctx`, because the placeholder expansion below is a
        // method on the context and needs it whole.
        let settings: CommandSettings = std::mem::take(&mut ctx.settings)
            .try_into()
            .map_err(|e| format!("supervisor {:?}: {e}", ctx.name))?;
        // Resolved before anything runs, so a bad signal name fails startup (ADR-0060).
        let reload = settings
            .reload_signal
            .as_deref()
            .map(|value| reload_signal(&ctx.name, value))
            .transpose()?;
        // Everything the operator wrote about *where* things are goes through the placeholders
        // (ADR-0022) — the program itself deliberately does not.
        let args: Vec<String> = settings.args.iter().map(|a| ctx.expand(a)).collect();
        let env: Vec<(String, String)> = settings
            .env
            .iter()
            .map(|(k, v)| (k.clone(), ctx.expand(v)))
            .collect();
        let working_dir = settings
            .working_dir
            .as_ref()
            .map(|d| PathBuf::from(ctx.expand(&d.to_string_lossy())));
        let command = ctx.program;
        let install = ctx.install;
        let (commands, command_rx) = mpsc::channel(16);
        // Asked at startup and again after every package swap, so a Foreign Agent the Server
        // updated describes the version it now runs rather than the one it replaced.
        let version_probe = settings.version_args.clone().map(|args| VersionProbe {
            program: command.clone(),
            args,
            // A Foreign Agent's version flag is its own convention, and so is its banner: the
            // strict SemVer read stays the default here (ADR-0068).
            parse: None,
        });
        // The same arguments, asked of the *staged* program before the running one is stopped
        // (ADR-0068). This kind knows no argument of its own to be safe to run — but an operator
        // who set `version_args` has named one: the contract on that key is that the command may
        // be invoked with exactly these and will print its version, which is precisely a check
        // that is cheap and touches no state. Nothing new is asked of anyone; the arguments that
        // already run after every swap now also run before one, where a refusal is free.
        //
        // Unset, this stays `None` and the kind behaves exactly as it did. No environment either,
        // for the reason the probe has none: these arguments have always been invoked bare, so any
        // that need one to succeed report no version today.
        let preflight = settings.version_args.clone().map(|args| Preflight {
            args,
            env: Vec::new(),
        });
        // What this Foreign Agent will actually be invoked with, after the placeholders were
        // expanded (ADR-0022). The spawn line names the program; the arguments are where a
        // placeholder that did not resolve — or a working directory that is not the one the
        // operator meant — becomes visible, and the process itself usually reports neither.
        //
        // **The environment is logged by key, never by value.** Both are the operator's, and a
        // Foreign Agent's environment is exactly where a token or a password is handed to it
        // (ADR-0013's reasoning, applied to a Managed Process). Which variables are set answers
        // "did my configuration reach it"; their contents answer nothing this line is for.
        debug!(
            supervisor = %ctx.name,
            program = %command.display(),
            args = ?args,
            working_dir = ?working_dir,
            env = ?env.iter().map(|(key, _)| key.as_str()).collect::<Vec<_>>(),
            "foreign agent invocation"
        );
        let runner = Runner {
            name: ctx.name,
            stop_timeout: ctx.stop_timeout,
            apply_grace: ctx.apply_grace,
            retain_previous: ctx.retain_previous,
            // A package (ADR-0015) swaps this command's program — one file, or a whole tree.
            install: Some(install),
            archive_key: ctx.archive_key.clone(),
            version_probe,
            preflight,
            reload_signal: reload,
            events: ctx.events,
            commands: command_rx,
            // A Foreign Agent has its own configuration until told otherwise: it always runs.
            build: Box::new(move || {
                Some(ProcessSpec {
                    program: command.clone(),
                    args: args.clone(),
                    env: env.clone(),
                    working_dir: working_dir.clone(),
                    // Whatever the operator points this at is supervised as one process.
                    own_process_group: false,
                })
            }),
        };
        tokio::spawn(runner.run(ctx.shutdown));
        Ok(commands)
    }

    fn check(&self, name: &str, settings: toml::Table) -> Result<(), String> {
        let settings: CommandSettings = settings
            .try_into()
            .map_err(|e| format!("supervisor {name:?}: {e}"))?;
        // The signal name is part of the strict read (ADR-0060): an offered set naming an
        // unknown one — or any on Windows — is refused before a running process is touched.
        if let Some(value) = settings.reload_signal.as_deref() {
            reload_signal(name, value)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `command` is gone from these settings — the core resolves it (ADR-0021) — so a block that
    /// still carries it here would be an unknown key, which is exactly what must fail.
    #[test]
    fn settings_parse_strictly() {
        let table: toml::Table = toml::from_str(
            r#"
            args = ["--a"]
            working_dir = "/tmp"
            version_args = ["--version"]
            [env]
            K = "v"
            "#,
        )
        .expect("table");
        let settings: CommandSettings = table.try_into().expect("settings");
        assert_eq!(settings.working_dir, Some(PathBuf::from("/tmp")));
        assert_eq!(settings.env.get("K").map(String::as_str), Some("v"));
        assert_eq!(settings.version_args, Some(vec!["--version".to_string()]));

        let typo: toml::Table = toml::from_str("comand = \"/x\"").expect("table");
        assert!(typo.try_into::<CommandSettings>().is_err());
    }

    /// The reload signal is read strictly (ADR-0060): the conventional reload signals only, with
    /// or without the `SIG` prefix — never a stop signal, whose acceptance would turn every
    /// apply into a kill.
    #[cfg(unix)]
    #[test]
    fn the_reload_signal_maps_conventional_names_and_refuses_the_rest() {
        assert_eq!(reload_signal("s", "HUP"), Ok(libc::SIGHUP));
        assert_eq!(reload_signal("s", "SIGHUP"), Ok(libc::SIGHUP));
        assert_eq!(reload_signal("s", "USR1"), Ok(libc::SIGUSR1));
        assert_eq!(reload_signal("s", "USR2"), Ok(libc::SIGUSR2));
        for refused in ["TERM", "KILL", "hup", "1", ""] {
            let err = reload_signal("s", refused).expect_err(refused);
            assert!(err.contains("unknown `reload_signal`"), "{err}");
        }
    }

    /// Windows has no signal a process can reload on; the key itself is the error there.
    #[cfg(windows)]
    #[test]
    fn a_reload_signal_is_refused_on_windows() {
        let err = reload_signal("s", "HUP").expect_err("windows has no signals");
        assert!(err.contains("unix-only"), "{err}");
    }

    /// `check` reads the signal name exactly as `start` would (ADR-0056), so an offered set
    /// naming a bad one is refused before any running process is touched.
    #[test]
    fn check_refuses_a_bad_reload_signal() {
        let table: toml::Table = toml::from_str("reload_signal = \"NOPE\"").expect("table");
        let err = CommandPlugin.check("agent", table).expect_err("refused");
        assert!(err.contains("reload_signal"), "{err}");
    }
}
