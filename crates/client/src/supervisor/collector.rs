//! The `collector` plugin: the Collector Supervisor (ADR-0011). It owns an OpenTelemetry
//! Collector: spawns the configured binary with one `--config` flag per written config-map
//! entry — the Collector merges multiple configs itself, so no YAML is touched here — and
//! restarts it when a new remote configuration arrives. Until a configuration exists nothing
//! runs and the Agent reports "awaiting configuration".
//!
//! A Collector carrying the `opampextension` additionally reports its own description, health,
//! and effective configuration to the Supervisor Endpoint; one without it is observed from the
//! outside. Either way it is the same plugin (goal 16 versus plain supervision).

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use tokio::sync::mpsc;

use crate::supervisor::ports::{Plugin, ProcessCommand, SupervisorContext};
use crate::supervisor::process::{ProcessSpec, Runner, VersionProbe};

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
    /// Additional environment for the Collector process. Environment is not a `command`-only
    /// feature — a Collector's config reads `${env:VAR}` too — so it is honoured here just as the
    /// command plugin honours it, values expanded through the same placeholders (ADR-0022).
    #[serde(default)]
    env: BTreeMap<String, String>,
}

/// The Collector's process spec: one `--config` per written entry, then the operator's extra args,
/// with its environment. `None` until a configuration exists — the Collector does not run on
/// nothing. Extracted from the build closure so the environment threading has a regression test.
fn collector_spec(
    program: &Path,
    config_dir: &Path,
    extra_args: &[String],
    env: &[(String, String)],
) -> Option<ProcessSpec> {
    // Only the entries that *are* configuration: supplementary content (ADR-0016) sits in the same
    // directory for the Collector to read by path, and handing it over as `--config` is exactly
    // what the role exists to prevent.
    let entries = crate::storage::config_entries(config_dir);
    if entries.is_empty() {
        return None;
    }
    let mut args = Vec::with_capacity(entries.len() * 2 + extra_args.len());
    for entry in entries {
        args.push("--config".to_string());
        args.push(entry.to_string_lossy().into_owned());
    }
    args.extend(extra_args.iter().cloned());
    Some(ProcessSpec {
        program: program.to_path_buf(),
        args,
        env: env.to_vec(),
        working_dir: None,
    })
}

pub struct CollectorPlugin;

impl Plugin for CollectorPlugin {
    fn kind(&self) -> &'static str {
        "collector"
    }

    fn program_key(&self) -> &'static str {
        "binary"
    }

    fn start(&self, mut ctx: SupervisorContext) -> Result<mpsc::Sender<ProcessCommand>, String> {
        // Taken out rather than consumed with `ctx`, because the placeholder expansion below is a
        // method on the context and needs it whole.
        let settings: CollectorSettings = std::mem::take(&mut ctx.settings)
            .try_into()
            .map_err(|e| format!("supervisor {:?}: {e}", ctx.name))?;
        // Everything the operator wrote about *where* things are goes through the placeholders
        // (ADR-0022) — the same for the Collector's extra args and its environment as for a command.
        let extra_args: Vec<String> = settings.args.iter().map(|a| ctx.expand(a)).collect();
        let env: Vec<(String, String)> = settings
            .env
            .iter()
            .map(|(k, v)| (k.clone(), ctx.expand(v)))
            .collect();
        let config_dir = ctx.config_dir;
        let binary = ctx.program;
        let install = ctx.install;
        let (commands, command_rx) = mpsc::channel(16);
        let runner = Runner {
            name: ctx.name,
            stop_timeout: ctx.stop_timeout,
            apply_grace: ctx.apply_grace,
            // A package (ADR-0015) swaps this Collector's program — one file, or a whole tree.
            install: Some(install),
            archive_key: ctx.archive_key.clone(),
            // The Collector states its version on `--version`, so even one without the
            // opampextension (which never self-reports) shows its own version rather than none.
            // The Runner asks at startup and again after every swap; an extension's self-report
            // overwrites the probed value.
            version_probe: Some(VersionProbe {
                program: binary.clone(),
                args: vec!["--version".to_string()],
            }),
            events: ctx.events,
            commands: command_rx,
            build: Box::new(move || collector_spec(&binary, &config_dir, &extra_args, &env)),
        };
        tokio::spawn(runner.run(ctx.shutdown));
        Ok(commands)
    }

    fn check(&self, name: &str, settings: toml::Table) -> Result<(), String> {
        let _: CollectorSettings = settings
            .try_into()
            .map_err(|e| format!("supervisor {name:?}: {e}"))?;
        Ok(())
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

    /// `[supervisor.env]` is accepted on a collector block — environment is not a `command`-only
    /// feature.
    #[test]
    fn env_parses() {
        let table: toml::Table =
            toml::from_str("[env]\nOTLP_HTTP_ENDPOINT = \"127.0.0.1:4318\"\n").expect("table");
        let settings: CollectorSettings = table.try_into().expect("settings");
        assert_eq!(
            settings.env.get("OTLP_HTTP_ENDPOINT").map(String::as_str),
            Some("127.0.0.1:4318")
        );
    }

    /// The regression for the bug this fixes: the built spec must carry the environment, not the
    /// empty vector the collector plugin used to hardcode. A configuration entry has to exist first,
    /// since the Collector does not run on nothing.
    #[test]
    fn the_spec_carries_the_environment() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Empty config directory: no spec at all.
        assert!(collector_spec(Path::new("otelcol"), dir.path(), &[], &[]).is_none());

        // One written configuration entry (a plain, non-dot file), and an environment to pass.
        std::fs::write(dir.path().join("fleet"), b"service: {}\n").expect("write entry");
        let env = vec![(
            "OTLP_HTTP_ENDPOINT".to_string(),
            "127.0.0.1:4318".to_string(),
        )];
        let spec = collector_spec(
            Path::new("otelcol"),
            dir.path(),
            &["--feature-gates=x".to_string()],
            &env,
        )
        .expect("a spec once a configuration exists");

        assert_eq!(
            spec.env, env,
            "the operator's environment reaches the process"
        );
        assert!(
            spec.args.iter().any(|a| a == "--config"),
            "the written entry is passed as --config"
        );
        assert!(
            spec.args.iter().any(|a| a == "--feature-gates=x"),
            "extra args follow the --config flags"
        );
    }
}
