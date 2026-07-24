//! The `collector` plugin: the Collector Supervisor (ADR-0011). It owns an OpenTelemetry
//! Collector: spawns the configured binary with one `--config` flag per written config-map
//! entry — the Collector merges multiple configs itself, so no YAML is touched here — and
//! restarts it when a new remote configuration arrives. Until a configuration exists nothing
//! runs and the Agent reports "awaiting configuration".
//!
//! A Collector carrying the `opampextension` additionally reports its own description, health,
//! and effective configuration to the Supervisor Endpoint; one without it is observed from the
//! outside. Either way it is the same plugin (goal 16 versus plain supervision).

use std::path::PathBuf;

use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::warn;

use crate::supervisor::ports::{Plugin, ProcessCommand, SupervisorContext};
use crate::supervisor::process::{probe_version, ProcessSpec, Runner};

/// The block's plugin-specific keys, parsed strictly — a typo fails startup, per ADR-0008.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CollectorSettings {
    /// The Collector binary to run.
    binary: PathBuf,
    /// Extra arguments, appended after the `--config` flags.
    #[serde(default)]
    args: Vec<String>,
}

pub struct CollectorPlugin;

impl Plugin for CollectorPlugin {
    fn kind(&self) -> &'static str {
        "collector"
    }

    fn start(&self, ctx: SupervisorContext) -> Result<mpsc::Sender<ProcessCommand>, String> {
        let settings: CollectorSettings = ctx
            .settings
            .try_into()
            .map_err(|e| format!("supervisor {:?}: {e}", ctx.name))?;
        let config_dir = ctx.config_dir;
        let (commands, command_rx) = mpsc::channel(16);
        // The Collector states its version on `--version` — probe it once, so even a Collector
        // without the opampextension (which never self-reports) shows its own version, not
        // none. An extension's later self-report replaces the probed value.
        tokio::spawn(probe_version(
            settings.binary.clone(),
            vec!["--version".to_string()],
            ctx.events.clone(),
        ));
        let runner = Runner {
            name: ctx.name,
            stop_timeout: ctx.stop_timeout,
            events: ctx.events,
            commands: command_rx,
            build: Box::new(move || {
                let entries = config_entries(&config_dir);
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
                    program: settings.binary.clone(),
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

/// The written config-map entry files, in deterministic (sorted) order — the Collector's own
/// merge semantics are order-dependent.
fn config_entries(config_dir: &std::path::Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(config_dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    warn!(dir = %config_dir.display(), error = %e, "unreadable config entry");
                    return None;
                }
            };
            entry
                .file_type()
                .ok()
                .filter(std::fs::FileType::is_file)
                .map(|_| entry.path())
        })
        .collect();
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_parse_strictly() {
        let table: toml::Table =
            toml::from_str("binary = \"/usr/local/bin/otelcol\"\nargs = [\"--feature-gates=x\"]\n")
                .expect("table");
        let settings: CollectorSettings = table.try_into().expect("settings");
        assert_eq!(settings.binary, PathBuf::from("/usr/local/bin/otelcol"));

        let typo: toml::Table = toml::from_str("binry = \"/x\"").expect("table");
        assert!(typo.try_into::<CollectorSettings>().is_err());
    }

    #[test]
    fn config_entries_are_files_only_and_sorted() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(config_entries(dir.path()).is_empty());
        std::fs::write(dir.path().join("b.yaml"), "b").expect("write");
        std::fs::write(dir.path().join("a.yaml"), "a").expect("write");
        std::fs::create_dir(dir.path().join("subdir")).expect("mkdir");
        let names: Vec<String> = config_entries(dir.path())
            .into_iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        assert_eq!(names, vec!["a.yaml", "b.yaml"]);
    }
}
