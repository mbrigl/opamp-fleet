//! The `command` plugin: the example Custom Supervisor (ADR-0017). It brings a Foreign Agent —
//! any process started by a command-line invocation — under management: spawned as configured,
//! restarted when a remote configuration arrives (the files land in the Supervisor's
//! `config/` directory for the process to re-read), health derived from the outside.

use std::collections::BTreeMap;

use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::debug;

use crate::supervisor::ports::{Plugin, ProcessCommand, SupervisorContext};

/// The block's plugin-specific keys, parsed strictly — a typo fails startup, per ADR-0009.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandSettings {
    /// Its arguments, verbatim.
    #[serde(default)]
    args: Vec<String>,
    /// Additional environment for the process.
    #[serde(default)]
    env: BTreeMap<String, String>,
    /// Arguments that make the command print its version (e.g. `["--version"]`). When set, the
    /// command is invoked once with exactly these arguments and the first Semantic Versioning
    /// 2.0.0 version in its output becomes the Agent's `service.version`. A Foreign Agent's
    /// version flag is its own convention — hence opt-in, unlike the Collector's.
    #[serde(default)]
    version_args: Option<Vec<String>>,
}

/// The keys this kind used to take and no longer does (ADR-0017), each with what answers it now.
/// Refused by name rather than met with serde's "unknown field", for the reason `icinga2` refuses
/// its own: a block carrying one was written against a Client that needed it, and the operator
/// deleting the line deserves to be told where the value went.
const RETIRED: &[(&str, &str)] = &[
    (
        "working_dir",
        "a Managed Process starts in the directory its program lives in",
    ),
    (
        "reload_signal",
        "whether a program re-reads its configuration on a signal is the program's own convention          and belongs in a kind that knows it — an unwrapped agent applies by restarting",
    ),
];

/// Refuses a retired key by name, before the strict parse turns it into "unknown field".
fn refuse_retired(name: &str, settings: &toml::Table) -> Result<(), String> {
    for (key, answer) in RETIRED {
        if settings.contains_key(*key) {
            return Err(format!(
                "supervisor {name:?}: `{key}` is no longer a supervisor key for type \"command\" \
                 — {answer}; remove the line"
            ));
        }
    }
    Ok(())
}

pub struct CommandPlugin;

impl Plugin for CommandPlugin {
    fn kind(&self) -> &'static str {
        "command"
    }

    /// Nothing at all. This is the kind for an agent nobody has written a wrapper for, so every
    /// value is the operator's to state (ADR-0017).
    fn defaults(&self) -> crate::supervisor::ports::KindDefaults {
        crate::supervisor::ports::KindDefaults::none()
    }

        let raw = std::mem::take(&mut ctx.settings);
        refuse_retired(&ctx.name, &raw)?;
        let settings: CommandSettings = raw
            .try_into()
            .map_err(|e| format!("supervisor {:?}: {e}", ctx.name))?;
        let install = ctx.install;
        let (commands, command_rx) = mpsc::channel(16);
        // Asked at startup and again after every package swap, so a Foreign Agent the Server
        // updated describes the version it now runs rather than the one it replaced.
        let version_probe = settings.version_args.clone().map(|args| VersionProbe {
            program: command.clone(),
            args,
        });
        // What this Foreign Agent will actually be invoked with, after the placeholders were
        // expanded (ADR-0017). The spawn line names the program; the arguments are where a
        // placeholder that did not resolve — or a working directory that is not the one the
        // operator meant — becomes visible, and the process itself usually reports neither.
        //
        // **The environment is logged by key, never by value.** Both are the operator's, and a
        // Foreign Agent's environment is exactly where a token or a password is handed to it
        // (ADR-0022's reasoning, applied to a Managed Process). Which variables are set answers
        // "did my configuration reach it"; their contents answer nothing this line is for.
        debug!(
            supervisor = %ctx.name,
            program = %command.display(),
            args = ?args,
            env = ?env.iter().map(|(key, _)| key.as_str()).collect::<Vec<_>>(),
            "foreign agent invocation"
        );
        let runner = Runner {
            name: ctx.name,
            stop_timeout: ctx.stop_timeout,
            apply_grace: ctx.apply_grace,
            retain_previous: ctx.retain_previous,
            // A package (ADR-0028) swaps this command's program — one file, or a whole tree.
            install: Some(install),
            archive_key: ctx.archive_key.clone(),
            version_probe,
            // Not this kind's to know (ADR-0017): an agent nobody wrote a wrapper for applies a
            // configuration by restarting, which is ADR-0017's generic behaviour.
            reload_signal: None,
            events: ctx.events,
            commands: command_rx,
            // A Foreign Agent has its own configuration until told otherwise: it always runs.
            build: Box::new(move || {
                Some(ProcessSpec {
                    // The program's own directory (ADR-0017), resolved at the spawn.
                    working_dir: None,
                    // Nothing: this kind knows no agent, so it knows no directory an
                    // agent of it would write into. An operator whose Foreign Agent needs one
                    // states the path in its own configuration, where the agent can make it.
                    ensure_dirs: Vec::new(),
                })
            }),
        };
        tokio::spawn(runner.run(ctx.shutdown));
        Ok(commands)
    }
        refuse_retired(name, &settings)?;
        let _: CommandSettings = settings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_parse_strictly() {
        let table: toml::Table = toml::from_str(
            r#"
            args = ["--a"]
            version_args = ["--version"]
            [env]
            K = "v"
            "#,
        )
        .expect("table");
        let settings: CommandSettings = table.try_into().expect("settings");
        assert_eq!(settings.env.get("K").map(String::as_str), Some("v"));
        assert_eq!(settings.version_args, Some(vec!["--version".to_string()]));

        let typo: toml::Table = toml::from_str("comand = \"/x\"").expect("table");
        assert!(typo.try_into::<CommandSettings>().is_err());
    }

    /// The two keys ADR-0017 retires are refused by name, on both sides of the seam: at startup,
    /// and in an offered Supervisor set before any running process is touched (ADR-0017). Each
    /// message says what supplies the value now, because a block carrying one was written against
    /// a Client that took it.
    #[test]
    fn the_retired_keys_are_refused_by_name() {
        for (key, line) in [
            ("working_dir", "working_dir = \"/tmp\""),
            ("reload_signal", "reload_signal = \"HUP\""),
        ] {
            let table: toml::Table = toml::from_str(line).expect("table");
            let err = CommandPlugin.check("agent", table).expect_err(key);
            assert!(err.contains(key), "{err}");
            assert!(err.contains("no longer a supervisor key"), "{err}");
        }
    }
}
