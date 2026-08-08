//! The `collector` plugin: the Collector Supervisor (ADR-0011). It owns an OpenTelemetry
//! Collector: spawns the configured binary with one `--config` flag per written config-map
//! entry — the Collector merges multiple configs itself, so no YAML is touched here — and
//! restarts it when a new remote configuration arrives. Until a configuration exists nothing
//! runs and the Agent reports "awaiting configuration".
//!
//! A Collector carrying the `opampextension` additionally reports its own description, health,
//! and effective configuration to the Supervisor Endpoint; one without it is observed from the
//! outside. Either way it is the same plugin (goal 16 versus plain supervision).

use serde::Deserialize;
use tokio::sync::mpsc;

use crate::supervisor::ports::{Plugin, ProcessCommand, SupervisorContext};
use crate::supervisor::process::{probe_version, ProcessSpec, Runner};

/// The block's plugin-specific keys, parsed strictly — a typo fails startup, per ADR-0008.
///
/// `binary` is not among them: the core takes it out and resolves it (ADR-0021), and what arrives
/// here is [`SupervisorContext::program`].
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CollectorSettings {
    /// Extra arguments, appended after the `--config` flags.
    #[serde(default)]
    args: Vec<String>,
}

pub struct CollectorPlugin;

impl Plugin for CollectorPlugin {
    fn kind(&self) -> &'static str {
        "collector"
    }

    fn program_key(&self) -> &'static str {
        "binary"
    }

    fn start(&self, ctx: SupervisorContext) -> Result<mpsc::Sender<ProcessCommand>, String> {
        let settings: CollectorSettings = ctx
            .settings
            .try_into()
            .map_err(|e| format!("supervisor {:?}: {e}", ctx.name))?;
        let config_dir = ctx.config_dir;
        let binary = ctx.program;
        let install = ctx.install;
        let (commands, command_rx) = mpsc::channel(16);
        // The Collector states its version on `--version` — probe it once, so even a Collector
        // without the opampextension (which never self-reports) shows its own version, not
        // none. An extension's later self-report replaces the probed value.
        tokio::spawn(probe_version(
            binary.clone(),
            vec!["--version".to_string()],
            ctx.events.clone(),
        ));
        let runner = Runner {
            name: ctx.name,
            stop_timeout: ctx.stop_timeout,
            apply_grace: ctx.apply_grace,
            // A package (ADR-0015) swaps this Collector's program — one file, or a whole tree.
            install: Some(install),
            archive_key: ctx.archive_key.clone(),
            events: ctx.events,
            commands: command_rx,
            build: Box::new(move || {
                // Only the entries that *are* configuration: supplementary content (ADR-0016)
                // sits in the same directory for the Collector to read by path, and handing it
                // over as `--config` is exactly what the role exists to prevent.
                let entries = crate::storage::config_entries(&config_dir);
                if entries.is_empty() {
                    // No configuration yet — the Collector does not run on nothing.
                    return None;
                }
                let mut args = Vec::with_capacity(entries.len() * 2 + settings.args.len());
                for entry in entries {
                    args.push("--config".to_string());
                    args.push(entry.to_string_lossy().into_owned());
                }
                args.extend(settings.args.iter().cloned());
                Some(ProcessSpec {
                    program: binary.clone(),
                    args,
                    env: Vec::new(),
                    working_dir: None,
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

    /// `binary` is gone from these settings — the core resolves it (ADR-0021) — so a block that
    /// still carries it here would be an unknown key, which is exactly what must fail.
    #[test]
    fn settings_parse_strictly() {
        let table: toml::Table = toml::from_str("args = [\"--feature-gates=x\"]\n").expect("table");
        let settings: CollectorSettings = table.try_into().expect("settings");
        assert_eq!(settings.args, vec!["--feature-gates=x".to_string()]);

        let typo: toml::Table = toml::from_str("arg = [\"--x\"]").expect("table");
        assert!(typo.try_into::<CollectorSettings>().is_err());
    }
}
