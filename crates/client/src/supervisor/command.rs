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
    #[serde(default)]
    version_args: Option<Vec<String>>,
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
        let (commands, command_rx) = mpsc::channel(16);
        if let Some(version_args) = settings.version_args.clone() {
            tokio::spawn(probe_version(
                command.clone(),
                version_args,
                ctx.events.clone(),
            ));
        }
        let runner = Runner {
            name: ctx.name,
            stop_timeout: ctx.stop_timeout,
            apply_grace: ctx.apply_grace,
            // A package (ADR-0015) swaps this command's binary.
            binary: Some(command.clone()),
            archive_key: ctx.archive_key.clone(),
            events: ctx.events,
            commands: command_rx,
            // A Foreign Agent has its own configuration until told otherwise: it always runs.
            build: Box::new(move || {
                Some(ProcessSpec {
                    program: command.clone(),
                    args: args.clone(),
                    env: env.clone(),
                    working_dir: working_dir.clone(),
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
}
