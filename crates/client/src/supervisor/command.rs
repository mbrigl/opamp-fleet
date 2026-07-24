//! The `command` plugin: the example Custom Supervisor (ADR-0011). It brings a Foreign Agent —
//! any process started by a command-line invocation — under management: spawned as configured,
//! restarted when a remote configuration arrives (the files land in the Supervisor's
//! `config/` directory for the process to re-read), health derived from the outside.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;
use tokio::sync::mpsc;

use crate::supervisor::ports::{Plugin, ProcessCommand, SupervisorContext};
use crate::supervisor::process::{probe_version, ProcessSpec, Runner};

/// The block's plugin-specific keys, parsed strictly — a typo fails startup, per ADR-0008.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandSettings {
    /// The command to run.
    command: PathBuf,
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
    #[serde(default)]
    version_args: Option<Vec<String>>,
}

pub struct CommandPlugin;

impl Plugin for CommandPlugin {
    fn kind(&self) -> &'static str {
        "command"
    }

    fn start(&self, ctx: SupervisorContext) -> Result<mpsc::Sender<ProcessCommand>, String> {
        let settings: CommandSettings = ctx
            .settings
            .try_into()
            .map_err(|e| format!("supervisor {:?}: {e}", ctx.name))?;
        let (commands, command_rx) = mpsc::channel(16);
        if let Some(version_args) = settings.version_args.clone() {
            tokio::spawn(probe_version(
                settings.command.clone(),
                version_args,
                ctx.events.clone(),
            ));
        }
        let runner = Runner {
            name: ctx.name,
            stop_timeout: ctx.stop_timeout,
            apply_grace: ctx.apply_grace,
            events: ctx.events,
            commands: command_rx,
            // A Foreign Agent has its own configuration until told otherwise: it always runs.
            build: Box::new(move || {
                Some(ProcessSpec {
                    program: settings.command.clone(),
                    args: settings.args.clone(),
                    env: settings
                        .env
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                    working_dir: settings.working_dir.clone(),
                })
            }),
        };
        tokio::spawn(runner.run(ctx.shutdown));
        Ok(commands)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_parse_strictly() {
        let table: toml::Table = toml::from_str(
            r#"
            command = "/usr/bin/thing"
            args = ["--a"]
            working_dir = "/tmp"
            version_args = ["--version"]
            [env]
            K = "v"
            "#,
        )
        .expect("table");
        let settings: CommandSettings = table.try_into().expect("settings");
        assert_eq!(settings.command, PathBuf::from("/usr/bin/thing"));
        assert_eq!(settings.env.get("K").map(String::as_str), Some("v"));
        assert_eq!(settings.version_args, Some(vec!["--version".to_string()]));

        let typo: toml::Table = toml::from_str("comand = \"/x\"").expect("table");
        assert!(typo.try_into::<CommandSettings>().is_err());
    }
}
