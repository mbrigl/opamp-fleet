//! The supervision domain (ADR-0015): builds the Agents the [`Engine`](crate::engine) carries.
//!
//! With `[[supervisor]]` blocks configured, each becomes one Supervisor-backed Agent — everything
//! it owns under `<supervisor_dir>/<name>/` (ADR-0022), its Managed Process driven by the plugin
//! the block's `type` selects. Without any, the Client presents itself as the single self-Agent —
//! the same state machine with no Managed Process behind it.

pub mod agent;
pub mod block;
pub mod collector;
pub mod command;
pub mod endpoint;
pub mod glpi;
pub mod icinga2;
pub mod ports;
pub mod process;
pub mod restart;
pub mod telegraf;

use std::collections::BTreeMap;

use tokio::sync::{mpsc, watch};
use tracing::{info, warn};

use crate::config::{ClientConfig, SupervisorBlock};
use crate::engine::{Engine, EngineAgent};
use crate::shutdown::{shutdown_channel, Shutdown};
use crate::storage::{DroppedRemoteConfig, Storage};

use agent::AgentState;
use block::{find_plugin, resolve, take_program, Resolved};
use ports::{EventSender, Plugin, ProcessEvent, SupervisorContext};

pub use crate::engine::SELF_AGENT_INDEX;

/// What a Supervisor's block position must be shifted by to reach its Engine index — and, read
/// the other way, what an Engine index is shifted back by to find the block it came from.
pub const SELF_AGENT_OFFSET: usize = SELF_AGENT_INDEX + 1;

/// The compiled-in plugin registry (ADR-0015). A new process kind is a new module and one line
/// here — the supervision core stays untouched (goal 8).
fn registry() -> Vec<Box<dyn Plugin>> {
    vec![
        Box::new(collector::CollectorPlugin),
        Box::new(command::CommandPlugin),
        Box::new(icinga2::Icinga2Plugin),
        Box::new(glpi::GlpiPlugin),
        Box::new(telegraf::TelegrafPlugin),
    ]
}

/// The kinds this Client was compiled with, as attributes of its own Agent (ADR-0015 clause 18).
///
/// Wrapping created a fact the fleet did not have to know before: a `type` is something a Client
/// either carries or does not, and a Server rolling a `glpi` set at a Client too old to have that
/// plugin used to learn it from a `FAILED` afterwards rather than by not aiming there.
///
/// **One key per kind**, not one list, because of how matching works here: a Selector is equality
/// over string values (`configs.rs::matches`), so a list could only be matched by spelling the
/// whole list — and the question one wants to ask is about *one member* of it. `AvailableComponents`
/// is the Baseline's own home for this and is deliberately not used yet: it is marked *Development*
/// in the schema this project pins, and Selectors resolve over the description, so reporting kinds
/// there would tell the fleet something it could not act on.
///
/// An operator's own attribute of the same name is left alone — configured values are the host's
/// statement about itself, and this function only fills in what nothing else said.
fn kind_attributes(mut attributes: BTreeMap<String, String>) -> BTreeMap<String, String> {
    for plugin in registry() {
        attributes
            .entry(format!("supervisor.kind.{}", plugin.kind()))
            .or_insert_with(|| "true".to_string());
    }
    attributes
}

/// Build the Engine from the configuration, starting one adapter task per Supervisor.
///
/// # Errors
/// Returns an error when an Agent's state cannot be restored, a `[[supervisor]]` block names an
/// unknown plugin, or a plugin rejects its settings — startup fails loudly, nothing runs half.
pub fn build_engine(config: &ClientConfig, shutdown: &Shutdown) -> Result<Engine, String> {
    report_orphaned_supervisor_dirs(config);
    for notice in remote_config_disabled_notices(config) {
        warn!("{notice}");
    }
    let (event_tx, events) = mpsc::channel(64);
    let mut agents = Vec::with_capacity(config.supervisors.len() + 1);

    // The Client is always its own Agent (ADR-0021), whether or not it supervises anything. It
    // used to exist only when nothing else did, which left the Client invisible on exactly the
    // hosts that manage something — and left the Server with nobody to offer the Client's own
    // package to. It is index 0 so the Supervisors that follow keep a stable, obvious offset.
    let storage = Storage::new(config.state_dir.clone())
        .map_err(|e| format!("cannot prepare {}: {e}", config.state_dir.display()))?;
    let mut self_state = declare_heartbeat(
        config,
        AgentState::new(config.name.clone(), storage, crate::host::SystemHost)
            .map_err(|e| format!("cannot restore the agent state: {e}"))?
            .with_attributes(kind_attributes(config.agent_attributes(None)))
            .with_namespace(config.service_namespace.clone()),
    );
    // Consenting to be updated names the package it will take — anything else is refused rather
    // than written over this binary (ADR-0021). Since ADR-0021 the consent stands unless the file
    // withdraws it, so this is the ordinary path rather than the opted-into one.
    // And only from a signed package: without a verification key the consent is kept, but nothing
    // is declared (ADR-0044) — the startup notice names the key.
    if let (Some(package), Some(_)) = (config.self_update_package(), config.package_key()) {
        self_state.accept_packages_named(package.to_string());
    }
    // The self-Agent's effective configuration is its own file — `supervisor.toml` is what this
    // Client runs (a file that fails to load fails startup), so the fleet view can finally answer
    // it. The text was redacted at load; without it, echoing a stored offer would say nothing
    // about this Client. No file means the defaults run, and there is nothing truthful to show.
    if let Some(source) = &config.source {
        self_state.set_process_effective_config(opamp::proto::EffectiveConfig {
            config_map: Some(opamp::proto::AgentConfigMap {
                config_map: std::collections::HashMap::from([(
                    "supervisor.toml".to_string(),
                    opamp::proto::AgentConfigObject {
                        role: String::new(),
                        body: source.clone().into_bytes(),
                        content_type: String::new(),
                    },
                )]),
            }),
        });
    }
    agents.push(EngineAgent {
        state: self_state,
        commands: None,
        stop: None,
        block_name: None,
    });

    for (block_index, block) in config.supervisors.iter().enumerate() {
        // The event channel is keyed by position in `agents`, and the self-Agent holds 0.
        let index = block_index + SELF_AGENT_OFFSET;
        agents.push(start_supervisor(config, block, index, &event_tx, shutdown)?);
    }
    Ok(Engine::with_processes(agents, events, event_tx))
}

/// A directory under the Supervisor root that no `[[supervisor]]` block names is reported, never
/// reaped (ADR-0022): it may be a purge a crash or an error cut short — or an operator's
/// deliberate hand edit, a temporarily commented-out block whose identity and program are not the
/// Client's to delete. The log line makes the leftover visible; removing it stays the operator's
/// call.
fn report_orphaned_supervisor_dirs(config: &ClientConfig) {
    let root = config.supervisors_root();
    let Ok(entries) = std::fs::read_dir(&root) else {
        // No root yet — nothing was ever supervised here, so there is nothing to be orphaned.
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let name = entry.file_name();
        if config
            .supervisors
            .iter()
            .any(|block| name == std::ffi::OsStr::new(&block.name))
        {
            continue;
        }
        warn!(
            path = %entry.path().display(),
            "no [[supervisor]] block names this directory; leaving it untouched — remove it by \
             hand if its supervisor is gone for good"
        );
    }
}

/// Heartbeats are a Client-wide choice: enabled (interval > 0) every Agent declares the
/// capability; disabled none does — an undeclared capability must never be exercised.
fn declare_heartbeat(config: &ClientConfig, mut state: AgentState) -> AgentState {
    if config.heartbeat_interval_secs > 0 {
        state.declare_capability(opamp::proto::AgentCapabilities::ReportsHeartbeat);
    }
    state
}

/// Validates one `[[supervisor]]` block exactly as [`start_supervisor`] would read it — plugin
/// known, program key present and well-shaped, plugin settings parsing strictly — without
/// touching the filesystem or starting anything (ADR-0022). What an offered Supervisor set is
/// checked against before any running process is stopped.
///
/// # Errors
/// Returns the same error `start_supervisor` would fail with, naming the block.
pub fn validate_block(config: &ClientConfig, block: &SupervisorBlock) -> Result<(), String> {
    let plugins = registry();
    let resolved = resolve(config, block, &plugins)?;
    resolved.plugin.check(&block.name, resolved.settings)
}

/// What a Server-delivered block may not bring (ADR-0051 clauses 18, 19; for a Supervisor whose
/// remote configuration is switched off, ADR-0067 clause 7), checked against `running` — the
/// configuration in force, whose `[supervisors]` section the Server cannot change and whose block
/// of the same name the delivered one may repeat.
///
/// # Errors
/// Returns the reason the delivered block is refused, naming the block and what it brings.
pub fn check_delivered_block(
    running: &ClientConfig,
    block: &SupervisorBlock,
) -> Result<(), String> {
    let plugins = registry();
    let plugin = find_plugin(&plugins, block)?;
    let current = running
        .supervisors
        .iter()
        .find(|existing| existing.name == block.name && existing.kind == block.kind)
        .map(|existing| &existing.settings);
    if running.remote_config_disabled(&block.name) {
        // A listed Supervisor's block is the operator's whole: repeated as it runs, or added
        // naming its program and nothing else (ADR-0067 clause 7).
        let named = running
            .supervisors
            .iter()
            .find(|existing| existing.name == block.name);
        check_listed_block(block, named, plugin)?;
    } else {
        let policy = &running.supervisor_defaults;
        check_delivered_env(block, current, &policy.delivered_env)?;
        if !policy.delivered_args {
            for key in ["args", "version_args"] {
                if block.settings.get(key) != current.and_then(|settings| settings.get(key)) {
                    return Err(format!(
                        "supervisor {:?}: a delivered block may not set {key} — allow it with \
                         [supervisors] delivered_args = true in this Client's supervisor.toml",
                        block.name
                    ));
                }
            }
        }
    }
    plugin
        .check_delivered(&block.settings, current)
        .map_err(|e| format!("supervisor {:?}: {e}", block.name))
}

/// A delivered block for a Supervisor whose remote configuration is switched off configures
/// nothing, whatever `delivered_args` and `delivered_env` allow: any key it may change would be a
/// configuration by another route (ADR-0067 clause 7). With a block of that name running, the
/// delivered one equals it whole; added, it carries `type`, `name` and — where the kind does not
/// name its own program — the kind's program key, and nothing else.
fn check_listed_block(
    block: &SupervisorBlock,
    current: Option<&SupervisorBlock>,
    plugin: &dyn Plugin,
) -> Result<(), String> {
    let refuse = |key: &str| {
        Err(format!(
            "supervisor {:?}: a delivered block may not set {key} — remote configuration is \
             switched off for it in [supervisors] remote_config_disabled in this Client's \
             supervisor.toml, so the block must stay as the operator wrote it",
            block.name
        ))
    };
    // The keys the core takes out of a block, spelled for comparison; an unset one is `None`.
    let core = |b: &SupervisorBlock| {
        let secs = |value: Option<u64>| value.map(|secs| secs.to_string());
        [
            ("type", Some(b.kind.clone())),
            ("service_name", b.service_name.clone()),
            (
                "endpoint_port",
                (b.endpoint_port != 0).then(|| b.endpoint_port.to_string()),
            ),
            ("stop_timeout_secs", secs(b.stop_timeout_secs)),
            ("apply_grace_secs", secs(b.apply_grace_secs)),
            ("retain_previous_secs", secs(b.retain_previous_secs)),
            (
                "program_path",
                b.program_path
                    .as_ref()
                    .map(|path| path.display().to_string()),
            ),
        ]
    };
    match current {
        Some(current) => {
            if block == current {
                return Ok(());
            }
            for ((key, given), (_, running)) in core(block).into_iter().zip(core(current)) {
                if given != running {
                    return refuse(key);
                }
            }
            let keys = block.settings.keys().chain(current.settings.keys());
            for key in keys {
                if block.settings.get(key) != current.settings.get(key) {
                    return refuse(key);
                }
            }
            // Equal key by key yet unequal as a whole cannot happen; refuse rather than assume.
            refuse("a key")
        }
        None => {
            for (key, given) in core(block) {
                if key != "type" && given.is_some() {
                    return refuse(key);
                }
            }
            let program_key = plugin
                .defaults()
                .program
                .is_none()
                .then(|| plugin.program_key());
            match block
                .settings
                .keys()
                .find(|key| Some(key.as_str()) != program_key)
            {
                Some(key) => refuse(key),
                None => Ok(()),
            }
        }
    }
}

/// Variables that steer which code a program loads — the dynamic loader's, `PATH`, the hooks of
/// common runtimes — refused in a delivered block whatever `delivered_env` allows (ADR-0051 clause
/// 18). Compared without regard to case, as Windows compares environment names.
const LOADING_NAMES: &[&str] = &[
    "PATH",
    "GCONV_PATH",
    "GLIBC_TUNABLES",
    "OPENSSL_CONF",
    "OPENSSL_ENGINES",
    "DOTNET_STARTUP_HOOKS",
    "NODE_OPTIONS",
    "JAVA_TOOL_OPTIONS",
    "_JAVA_OPTIONS",
    "JDK_JAVA_OPTIONS",
    "PYTHONPATH",
    "PYTHONSTARTUP",
    "PERL5LIB",
    "PERL5OPT",
    "RUBYOPT",
    "BASH_ENV",
    "ENV",
];

/// Prefixes of the same kind: the loaders' own and the .NET profilers'.
const LOADING_PREFIXES: &[&str] = &["LD_", "DYLD_", "COR_PROFILER", "CORECLR_PROFILER"];

fn steers_loading(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    LOADING_NAMES.contains(&upper.as_str())
        || LOADING_PREFIXES
            .iter()
            .any(|prefix| upper.starts_with(prefix))
}

/// A delivered `env` entry is kept from the running block, or allowed by name — never a variable
/// that steers loading, and never a value pointing into the Supervisor's own directories, where
/// the Server delivers files no one signed.
fn check_delivered_env(
    block: &SupervisorBlock,
    current: Option<&toml::Table>,
    allowed: &[String],
) -> Result<(), String> {
    let Some(env) = block.settings.get("env").and_then(toml::Value::as_table) else {
        return Ok(());
    };
    let running = current
        .and_then(|settings| settings.get("env"))
        .and_then(toml::Value::as_table);
    for (name, value) in env {
        if running.and_then(|running| running.get(name)) == Some(value) {
            continue;
        }
        if steers_loading(name) {
            return Err(format!(
                "supervisor {:?}: a delivered block may not set {name} — it would steer which \
                 code the program loads",
                block.name
            ));
        }
        if value.as_str().is_some_and(|value| {
            value.contains("${config_dir}") || value.contains("${supervisor_dir}")
        }) {
            return Err(format!(
                "supervisor {:?}: a delivered block may not point {name} into its own \
                 directories — the Server delivers files there that no one signed",
                block.name
            ));
        }
        let upper = name.to_ascii_uppercase();
        let permitted = allowed
            .iter()
            .map(|pattern| pattern.to_ascii_uppercase())
            .any(|pattern| match pattern.strip_suffix('*') {
                Some(prefix) => upper.starts_with(prefix),
                None => pattern == upper,
            });
        if !permitted {
            return Err(format!(
                "supervisor {:?}: a delivered block may not set {name} — allow it in \
                 [supervisors] delivered_env in this Client's supervisor.toml",
                block.name
            ));
        }
    }
    Ok(())
}

/// The program a block resolves to, for callers that must inspect ownership rather than just
/// spawn it. The Supervisor-set apply uses it to keep a Server-delivered block to a Client-owned
/// program (ADR-0022).
pub fn resolve_block_program(
    config: &ClientConfig,
    block: &SupervisorBlock,
) -> Result<crate::config::Program, String> {
    let plugins = registry();
    let plugin = find_plugin(&plugins, block)?;
    let (_, program, _) = take_program(config, block, plugin)?;
    Ok(program)
}

/// Start one Supervisor at `index`: its state restored, its Endpoint bound, its adapter task
/// running. Used at startup for every configured block and at runtime for a block an applied
/// Supervisor set added or changed (ADR-0022).
///
/// # Errors
/// Returns an error when the block's state cannot be restored, its Endpoint port cannot be
/// bound, or its plugin rejects the settings.
pub fn start_supervisor(
    config: &ClientConfig,
    block: &SupervisorBlock,
    index: usize,
    event_tx: &mpsc::Sender<(usize, ProcessEvent)>,
    shutdown: &Shutdown,
) -> Result<EngineAgent, String> {
    let plugins = registry();
    let Resolved {
        plugin,
        settings,
        program,
        program_path,
        service_name,
        timing,
    } = resolve(config, block, &plugins)?;

    let supervisor_dir = config.supervisor_dir(&block.name);
    let storage = Storage::new(supervisor_dir.clone())
        .map_err(|e| format!("cannot prepare {}: {e}", supervisor_dir.display()))?;
    let config_dir = storage.config_dir();

    // What a package replaces: one file, or — when the block says where the program sits
    // inside the package — the whole tree under this Supervisor's `program/` (ADR-0019).
    let install = match program_path {
        Some(program_path) => crate::supervisor::ports::InstallTarget::Tree {
            root: supervisor_dir.join(crate::config::PROGRAM_DIR),
            program_path,
        },
        None => crate::supervisor::ports::InstallTarget::Binary(program.path.clone()),
    };

    // Switched off, the Server's last configuration leaves before the kind starts, and what the
    // operator placed in `config/` stays (ADR-0067 clause 5).
    let remote_config = !config.remote_config_disabled(&block.name);
    if !remote_config {
        drop_stored_remote_config(&block.name, &storage)?;
    }
    let state = if remote_config {
        AgentState::supervised(
            block.name.clone(),
            service_name,
            storage,
            crate::host::SystemHost,
        )
    } else {
        AgentState::supervised_without_remote_config(
            block.name.clone(),
            service_name,
            storage,
            crate::host::SystemHost,
        )
    };
    let mut state = declare_heartbeat(
        config,
        state
            .map_err(|e| format!("cannot restore the state of {:?}: {e}", block.name))?
            .with_attributes(config.agent_attributes(Some(block)))
            .with_namespace(config.service_namespace.clone()),
    );
    // Every Managed Process is the fleet's (ADR-0022), so every Supervisor takes whichever
    // top-level package the Server selects for it (ADR-0019, ADR-0020). There is no second branch:
    // a block naming a program on the machine no longer parses, so the consent ADR-0022 derived
    // from the path is discharged by the type system rather than by a rule. The log line stays and
    // loses its "declined" half — it now says *where* the program is, which is the thing an
    // operator reading a startup log actually wants.
    //
    // What the target itself needs — for a tree that is its root and nothing below it, since the
    // live tree arrives by renaming a directory over that name (ADR-0019).
    install.prepare()?;
    // Only a Client holding the operator's verification key takes packages: there is no unsigned
    // posture (ADR-0042). Without it the program stays as installed, and the startup notice says
    // why.
    if config.package_key().is_some() {
        state.accept_packages();
        info!(
            supervisor = %block.name,
            program = %program.path.display(),
            "packages accepted: the program is this supervisor's own"
        );
    }

    // Each Supervisor stops on its own channel (ADR-0022): the Client-wide shutdown is forwarded
    // into it, and retiring the Supervisor fires it alone — its Endpoint releases the port and
    // its adapter stops the Managed Process while the rest of the Client runs on.
    let (stop_tx, stop) = shutdown_channel();
    forward_shutdown(shutdown.clone(), stop_tx.clone());

    // The Supervisor Endpoint is intrinsic to every Supervisor (ADR-0009): bound
    // unconditionally, before the process starts — a taken port fails startup, not later.
    // Only the Managed Process may report through it (ADR-0053): a token fresh for every start,
    // handed to the process in its environment and asked of every connection.
    let endpoint_token = endpoint::new_token()?;
    endpoint::start(
        block.name.clone(),
        block.endpoint_port,
        EventSender::new(index, event_tx.clone()),
        stop.clone(),
        config.max_message_size_bytes,
        endpoint_token.clone(),
    )?;

    let commands = plugin.start(SupervisorContext {
        endpoint_token,
        name: block.name.clone(),
        supervisor_dir,
        config_dir,
        program: program.path,
        install,
        stop_timeout: timing.stop_timeout,
        apply_grace: timing.apply_grace,
        retain_previous: timing.retain_previous,
        archive_key: config.packages.as_ref().and_then(|p| p.archive_key.clone()),
        settings,
        events: EventSender::new(index, event_tx.clone()),
        shutdown: stop,
    })?;
    Ok(EngineAgent {
        state,
        commands: Some(commands),
        stop: Some(stop_tx),
        block_name: Some(block.name.clone()),
    })
}

/// Removes what a remote configuration stored for the Supervisor `name` before its remote
/// configuration was switched off, and says what it did (ADR-0067 clause 5).
///
/// # Errors
/// Returns an error when a file that has to go cannot be deleted — the Supervisor must not start
/// on the Server's configuration under a switch that says it does not.
fn drop_stored_remote_config(name: &str, storage: &Storage) -> Result<(), String> {
    match storage.drop_remote_config().map_err(|e| {
        format!("supervisor {name:?}: cannot remove the stored remote configuration: {e}")
    })? {
        None => {}
        Some(DroppedRemoteConfig::Removed { hash, kept }) => warn!(
            supervisor = %name,
            hash = %hex::encode(hash),
            kept = ?kept,
            "remote configuration is switched off: removed the stored remote configuration and \
             the files it wrote; kept the files whose content had changed"
        ),
        Some(DroppedRemoteConfig::Undecodable) => warn!(
            supervisor = %name,
            "remote configuration is switched off: removed a stored remote configuration that \
             does not decode; config/ is left as it is and may still hold files the Server wrote"
        ),
    }
    Ok(())
}

/// The startup notices `[supervisors] remote_config_disabled` earns (ADR-0067 clause 2): a listed
/// name no `[[supervisor]]` block carries, since the set may arrive later, and one that is the
/// Client's own name, whose Agent the switch does not cover.
#[must_use]
pub fn remote_config_disabled_notices(config: &ClientConfig) -> Vec<String> {
    config
        .supervisor_defaults
        .remote_config_disabled
        .iter()
        .filter(|name| config.supervisors.iter().all(|block| &block.name != *name))
        .map(|name| {
            if *name == config.name {
                format!(
                    "[supervisors] remote_config_disabled names {name:?}, which is this Client's \
                     own name: the Client's own Agent is not covered and keeps taking its \
                     supervisor set"
                )
            } else {
                format!(
                    "[supervisors] remote_config_disabled names {name:?}, which no \
                     [[supervisor]] block carries yet; it takes effect when one does"
                )
            }
        })
        .collect()
}

/// Forwards the Client-wide shutdown into one Supervisor's own channel, so its adapter and
/// Endpoint stop on whichever fires first — the operator stopping the Client, or the Supervisor
/// being retired (ADR-0022).
fn forward_shutdown(mut global: Shutdown, stop_tx: watch::Sender<bool>) {
    tokio::spawn(async move {
        global.requested().await;
        let _ = stop_tx.send(true);
    });
}

#[cfg(test)]
mod tests {
    use super::block::{check_endpoint_port, effective_service_name, effective_timing};
    use super::*;
    use crate::shutdown::shutdown_channel;
    use opamp::proto::AgentCapabilities;
    use std::path::PathBuf;
    use std::time::Duration;

    fn config(root: &std::path::Path, program: &str, supervisor_dir: Option<PathBuf>) -> String {
        format!(
            "endpoint = \"ws://127.0.0.1:1/v1/opamp\"\nstate_dir = {state:?}\n{moved}\n\
             [[supervisor]]\ntype = \"command\"\nname = \"agent\"\ncommand = {program:?}\n",
            state = root.join("state").to_string_lossy(),
            moved = supervisor_dir
                .map(|d| format!("supervisor_dir = {:?}\n", d.to_string_lossy()))
                .unwrap_or_default(),
        )
    }

    /// A configuration as `ClientConfig::load` leaves it when `[packages] verification_key` is set:
    /// the decoded key is what decides whether anything takes packages (ADR-0042).
    fn keyed(mut config: ClientConfig) -> ClientConfig {
        config.package_key = Some(vec![7u8; 32]);
        config
    }

    /// A block of a wrapped kind, as ADR-0015 means one to be written.
    fn wrapped(root: &std::path::Path, extra: &str) -> ClientConfig {
        toml::from_str(&format!(
            "endpoint = \"ws://127.0.0.1:1/v1/opamp\"\nstate_dir = {state:?}\n\
             [[supervisor]]\ntype = \"icinga2\"\nname = \"icinga2\"\n{extra}",
            state = root.join("state").to_string_lossy(),
        ))
        .expect("parse")
    }

    /// The point of ADR-0015, at the seam: a wrapped block names its agent and nothing about how
    /// that agent is built. What the kind supplies has to reach the program path and the Agent
    /// type without the block saying either.
    #[test]
    fn a_wrapped_block_needs_neither_a_program_nor_a_type() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = wrapped(dir.path(), "");
        let block = &config.supervisors[0];
        let plugins = registry();
        let plugin = find_plugin(&plugins, block).expect("the kind is known");

        let (_, program, _) =
            take_program(&config, block, plugin).expect("the kind names its program");
        let expected = config
            .supervisor_dir("icinga2")
            .join(crate::config::PROGRAM_DIR)
            .join(crate::config::TREE_DIR)
            .join(if cfg!(windows) {
                "sbin/icinga2.exe"
            } else {
                "sbin/icinga2"
            });
        assert_eq!(
            program.path, expected,
            "the tree's own layout, per platform"
        );
        assert_eq!(
            effective_service_name(block, plugin, &program.path).expect("a type"),
            "icinga2"
        );
    }

    /// And a block that states one anyway is refused, naming what supplies it now — the pattern
    /// `package` and `accepts_packages` already run (ADR-0015 clause 13). Silently preferring one
    /// of the two is how a host quietly differs from what the fleet believes.
    // Verifies: ADR-0015
    #[test]
    fn a_wrapped_block_that_restates_a_derived_value_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        for (line, expected) in [
            (
                "binary = \"icinga2\"\n",
                "installs and names its own program",
            ),
            (
                "program_path = \"sbin/icinga2\"\n",
                "where its program sits",
            ),
            ("service_name = \"icinga2\"\n", "the Agent type it presents"),
            ("endpoint_port = 4321\n", "says nothing for type"),
            ("stop_timeout_secs = 60\n", "how long an agent needs"),
            ("apply_grace_secs = 30\n", "how long an agent needs"),
            ("retain_previous_secs = 60\n", "how long an agent needs"),
        ] {
            let config = wrapped(dir.path(), line);
            let error = validate_block(&config, &config.supervisors[0])
                .expect_err(&format!("{line:?} is refused"));
            assert!(error.contains(expected), "{line:?} -> {error}");
        }
    }

    /// The claim of ADR-0015 in one assertion: every wrapped kind's block is `type` and `name`, and
    /// it validates whole — the program resolves inside this Supervisor's own directory, the Agent
    /// type is stated, the timing comes from the fleet, and the kind's own strict parse accepts an
    /// empty table. Icinga adds only its enrolment, and stands here without it as a standalone
    /// node.
    // Verifies: ADR-0015
    #[test]
    fn every_wrapped_kinds_block_is_two_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        for kind in ["icinga2", "glpi", "telegraf"] {
            let config: ClientConfig = toml::from_str(&format!(
                "endpoint = \"ws://127.0.0.1:1/v1/opamp\"\nstate_dir = {state:?}\n\
                 [[supervisor]]\ntype = {kind:?}\nname = {kind:?}\n",
                state = dir.path().join("state").to_string_lossy(),
            ))
            .unwrap_or_else(|e| panic!("{kind}: {e}"));
            validate_block(&config, &config.supervisors[0])
                .unwrap_or_else(|e| panic!("{kind} needs more than two lines: {e}"));
        }
    }

    /// A Client says which kinds it carries, one key per kind (ADR-0015 clause 18), so a Selector
    /// can aim a Supervisor set at the Clients that can actually run it — rather than the Server
    /// learning from a `FAILED` that it aimed at a Client too old to have the plugin.
    // Verifies: ADR-0015
    #[test]
    fn a_client_reports_the_kinds_it_was_compiled_with() {
        let reported = kind_attributes(BTreeMap::new());
        for plugin in registry() {
            assert_eq!(
                reported
                    .get(&format!("supervisor.kind.{}", plugin.kind()))
                    .map(String::as_str),
                Some("true"),
                "{} is compiled in and unreported",
                plugin.kind()
            );
        }
        assert!(reported.contains_key("supervisor.kind.glpi"));
        assert!(reported.contains_key("supervisor.kind.telegraf"));

        // An operator's own value under the same key is left alone: a configured attribute is the
        // host's statement about itself, and this only fills in what nothing else said.
        let stated = kind_attributes(
            [("supervisor.kind.glpi".to_string(), "no".to_string())]
                .into_iter()
                .collect(),
        );
        assert_eq!(
            stated.get("supervisor.kind.glpi").map(String::as_str),
            Some("no")
        );
    }

    /// Timing is the fleet's, then the kind's correction of it, and nothing below that
    /// (ADR-0015 clause 17). Icinga is the correction that exists: its shutdown drains checks and
    /// closes cluster connections, so the fleet's ten seconds would kill it mid-drain — a property
    /// of Icinga, which is why the kind holds it rather than every host repeating it.
    #[test]
    fn a_wrapped_kind_corrects_the_fleets_timing_and_the_block_says_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = wrapped(dir.path(), "");
        config.supervisor_defaults.stop_timeout_secs = 10;
        config.supervisor_defaults.apply_grace_secs = 3;
        config.updates.retain_previous_secs = 1234;
        let plugins = registry();
        let block = &config.supervisors[0];
        let timing = effective_timing(&config, block, find_plugin(&plugins, block).expect("kind"))
            .expect("resolved");
        assert_eq!(timing.stop_timeout, Duration::from_secs(60), "Icinga's own");
        assert_eq!(timing.apply_grace, Duration::from_secs(30), "Icinga's own");
        assert_eq!(
            timing.retain_previous,
            Duration::from_secs(1234),
            "nothing to correct here, so the fleet's number stands"
        );
    }

    /// And where a kind states no correction, the fleet's policy reaches the Supervisor unchanged
    /// — including for an unwrapped kind, whose block may still override it because no kind exists
    /// there to hold the value.
    #[test]
    fn the_fleets_timing_reaches_a_supervisor_that_says_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let plugins = registry();
        let mut config: ClientConfig =
            toml::from_str(&config(dir.path(), "managed-agent", None)).expect("parse");
        config.supervisor_defaults.stop_timeout_secs = 45;
        config.supervisor_defaults.apply_grace_secs = 7;
        let block = &config.supervisors[0];
        let timing = effective_timing(&config, block, find_plugin(&plugins, block).expect("kind"))
            .expect("resolved");
        assert_eq!(timing.stop_timeout, Duration::from_secs(45));
        assert_eq!(timing.apply_grace, Duration::from_secs(7));

        let stated: ClientConfig = toml::from_str(&format!(
            "endpoint = \"ws://127.0.0.1:1/v1/opamp\"\nstate_dir = {state:?}\n\
             [supervisors]\nstop_timeout_secs = 45\n\
             [[supervisor]]\ntype = \"command\"\nname = \"agent\"\ncommand = \"x\"\n\
             stop_timeout_secs = 90\n",
            state = dir.path().join("state").to_string_lossy(),
        ))
        .expect("parse");
        let block = &stated.supervisors[0];
        let timing = effective_timing(&stated, block, find_plugin(&plugins, block).expect("kind"))
            .expect("resolved");
        assert_eq!(
            timing.stop_timeout,
            Duration::from_secs(90),
            "an unwrapped kind's block still answers"
        );
    }

    /// The unwrapped kinds are untouched: `command` knows nothing, so its block still says
    /// everything — and a Collector may still pin the port something actually connects to.
    #[test]
    fn an_unwrapped_kind_still_says_everything_itself() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config: ClientConfig =
            toml::from_str(&config(dir.path(), "managed-agent", None)).expect("parse");
        let block = &config.supervisors[0];
        let plugins = registry();
        let plugin = find_plugin(&plugins, block).expect("the kind is known");
        let (_, program, _) = take_program(&config, block, plugin).expect("the block names it");
        assert!(program.path.ends_with("managed-agent"));
        assert_eq!(
            effective_service_name(block, plugin, &program.path).expect("a type"),
            "managed-agent",
            "the program's file name is what the operator already wrote"
        );

        let collector: ClientConfig = toml::from_str(&format!(
            "endpoint = \"ws://127.0.0.1:1/v1/opamp\"\nstate_dir = {state:?}\n\
             [[supervisor]]\ntype = \"collector\"\nname = \"otelcol\"\n\
             binary = \"otelcol\"\nendpoint_port = 4321\n",
            state = dir.path().join("state").to_string_lossy(),
        ))
        .expect("parse");
        assert!(
            check_endpoint_port(
                &collector.supervisors[0],
                find_plugin(&plugins, &collector.supervisors[0]).expect("kind")
            )
            .is_ok(),
            "the opampextension connects to it, so pinning it is a decision"
        );
    }

    fn accepts_packages(engine: &mut Engine) -> bool {
        let reports = engine.poll_reports();
        let supervisor = &reports[SELF_AGENT_OFFSET];
        supervisor.capabilities & AgentCapabilities::AcceptsPackages as u64 != 0
    }

    /// ADR-0022 where it becomes visible to the Server: **every** Supervisor declares
    /// `AcceptsPackages`, because every Managed Process is one this Client installed. The
    /// capability is a constant of this Client now, not a function of a path — which is why the
    /// second half of this test is a startup refusal rather than a second capability.
    ///
    /// The `program/` directory is created either way, before the first package: the swap renames
    /// inside it, so it has to exist beforehand rather than after.
    /// Verifies: ADR-0051, ADR-0042
    #[tokio::test]
    async fn every_supervisor_declares_package_acceptance() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_tx, shutdown) = shutdown_channel();

        let owned: ClientConfig =
            keyed(toml::from_str(&config(dir.path(), "managed-agent", None)).expect("parse"));
        let mut engine = build_engine(&owned, &shutdown).expect("build");
        assert!(
            accepts_packages(&mut engine),
            "the program is in this Client's own directory, which is what makes it updatable"
        );
        assert!(
            dir.path().join("state/supervisors/agent/program").is_dir(),
            "the directory the swap renames inside exists before any package arrives"
        );

        // The shape that used to declare nothing now does not start at all (ADR-0022).
        let foreign = dir.path().join("elsewhere/managed-agent");
        let machines: ClientConfig = toml::from_str(&config(
            dir.path(),
            &foreign.to_string_lossy(),
            Some(dir.path().join("other")),
        ))
        .expect("parse");
        let Err(err) = build_engine(&machines, &shutdown) else {
            panic!("a program on the machine must be refused at startup");
        };
        assert!(err.contains("only programs it installs"), "{err}");
    }

    /// Without the operator's verification key, no Agent of this Client takes packages — neither a
    /// Supervisor nor the Client's own Agent, whose self-update consent stands — so nothing can be
    /// installed unsigned (ADR-0042, ADR-0044).
    /// Verifies: ADR-0042, ADR-0044, Q-1
    #[tokio::test]
    async fn without_a_verification_key_no_agent_takes_packages() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_tx, shutdown) = shutdown_channel();
        let unkeyed: ClientConfig =
            toml::from_str(&config(dir.path(), "managed-agent", None)).expect("parse");
        let mut engine = build_engine(&unkeyed, &shutdown).expect("build");
        assert!(
            !engine.installs_packages(),
            "something takes packages without a key"
        );
        for report in engine.poll_reports() {
            assert_eq!(
                report.capabilities & AgentCapabilities::AcceptsPackages as u64,
                0,
                "an Agent declares AcceptsPackages without a key"
            );
        }
    }

    /// The side-effect-free `installs_packages()` that the startup signature-posture warning reads
    /// (ADR-0019) agrees with the `AcceptsPackages` capability an Agent actually declares.
    #[tokio::test]
    async fn installs_packages_reflects_declared_package_acceptance() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_tx, shutdown) = shutdown_channel();

        let owned: ClientConfig =
            keyed(toml::from_str(&config(dir.path(), "managed-agent", None)).expect("parse"));
        let engine = build_engine(&owned, &shutdown).expect("build");
        assert!(
            engine.installs_packages(),
            "the program is package-updatable, so the Client installs packages"
        );

        // Since ADR-0022 every Supervisor is package-updatable, so the only way for an Engine to
        // answer *no* is to have no Supervisor and a withdrawn self-update consent. That is worth
        // keeping green: the startup check this feeds warns about an unconfigured verification
        // key, and a Client that installs nothing has nothing for that key to protect.
        let alone: ClientConfig = toml::from_str(
            "endpoint = \"ws://127.0.0.1:1/v1/opamp\"\n[self_update]\nenabled = false\n",
        )
        .expect("parse");
        let engine = build_engine(&alone, &shutdown).expect("build");
        assert!(
            !engine.installs_packages(),
            "no Supervisor and no self-update consent means nothing here takes a package"
        );

        // The Client's own Agent consents by default (ADR-0021), so a Client with no Supervisor at
        // all still installs packages — its own.
        let bare: ClientConfig =
            keyed(toml::from_str("endpoint = \"ws://127.0.0.1:1/v1/opamp\"\n").expect("parse"));
        let engine = build_engine(&bare, &shutdown).expect("build");
        assert!(
            engine.installs_packages(),
            "the Client's own Agent consents by default"
        );
    }

    /// A tree Supervisor owns its `program/` directory and *nothing inside it* (ADR-0019). The
    /// live tree arrives by renaming a staging directory over `program/tree`, and a rename cannot
    /// replace a directory something else created and filled — so preparing the program's parent,
    /// which is right for a single file, would make every first install of a tree fail.
    #[tokio::test]
    async fn a_tree_supervisor_prepares_its_root_and_leaves_the_tree_to_the_package() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_tx, shutdown) = shutdown_channel();
        let config = format!(
            "endpoint = \"ws://127.0.0.1:1/v1/opamp\"\nstate_dir = {state:?}\n\n\
             [[supervisor]]\ntype = \"command\"\nname = \"agent\"\ncommand = \"fluent-bit\"\n\
             program_path = \"bin/fluent-bit\"\n",
            state = dir.path().join("state").to_string_lossy(),
        );
        let parsed: ClientConfig = keyed(toml::from_str(&config).expect("parse"));
        let mut engine = build_engine(&parsed, &shutdown).expect("build");

        assert!(
            accepts_packages(&mut engine),
            "a bare name is the consent whether the package is one file or a tree"
        );
        let program_dir = dir.path().join("state/supervisors/agent/program");
        assert!(
            program_dir.is_dir(),
            "the root the tree is renamed into exists"
        );
        assert!(
            !program_dir.join("tree").exists(),
            "nothing occupies the name the first install has to rename onto"
        );
    }

    /// The third case of the rule: refused at startup, not resolved against something.
    #[tokio::test]
    async fn a_program_path_that_is_neither_fails_the_build() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_tx, shutdown) = shutdown_channel();
        let config: ClientConfig =
            toml::from_str(&config(dir.path(), "./managed-agent", None)).expect("parse");
        let Err(err) = build_engine(&config, &shutdown) else {
            panic!("a path that is neither must not start");
        };
        assert!(err.contains("bare file name"), "{err}");
    }

    /// A block that names no program at all is a startup error too — the key moved out of the
    /// plugin's strict parse, and that must not turn a missing one into a default.
    #[tokio::test]
    async fn a_block_without_a_program_fails_the_build() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_tx, shutdown) = shutdown_channel();
        let config: ClientConfig = toml::from_str(&format!(
            "endpoint = \"ws://127.0.0.1:1/v1/opamp\"\nstate_dir = {state:?}\n\
             [[supervisor]]\ntype = \"command\"\nname = \"agent\"\n",
            state = dir.path().join("state").to_string_lossy(),
        ))
        .expect("parse");
        let Err(err) = build_engine(&config, &shutdown) else {
            panic!("a block without a program must not start");
        };
        assert!(err.contains("needs a `command`"), "{err}");
    }

    /// ADR-0022 point 17: a directory no block names survives startup — reported, never reaped.
    /// Startup cannot tell a purge a crash cut short from an operator's deliberate hand edit, and
    /// the destructive reading of that ambiguity would delete an identity and a program that were
    /// not meant to go.
    #[tokio::test]
    async fn an_orphaned_supervisor_directory_is_not_reaped_at_startup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_tx, shutdown) = shutdown_channel();
        let orphan = dir.path().join("state/supervisors/orphan");
        std::fs::create_dir_all(&orphan).expect("create");
        std::fs::write(orphan.join("instance-uid"), "uid").expect("write");

        let parsed: ClientConfig =
            toml::from_str(&config(dir.path(), "managed-agent", None)).expect("parse");
        build_engine(&parsed, &shutdown).expect("build");

        assert!(
            orphan.join("instance-uid").is_file(),
            "an orphaned directory is reported, not deleted"
        );
    }

    /// The self-Agent's effective configuration is its own file, not an echo of a stored offer:
    /// the first report carries `supervisor.toml`'s (redacted) text, which is what fills the fleet
    /// view's empty column for every Client.
    #[tokio::test]
    async fn the_self_agent_reports_its_file_as_the_effective_configuration() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_tx, shutdown) = shutdown_channel();
        let path = dir.path().join("supervisor.toml");
        std::fs::write(
            &path,
            format!(
                "# written by the operator\nendpoint = \"ws://127.0.0.1:1/v1/opamp\"\n\
                 state_dir = {state:?}\n[auth]\nbearer_token = \"s3cret\"\n",
                state = dir.path().join("state").to_string_lossy(),
            ),
        )
        .expect("write");
        let config = ClientConfig::load(&path).expect("load");
        let mut engine = build_engine(&config, &shutdown).expect("build");

        let reports = engine.poll_reports();
        let effective = reports[SELF_AGENT_INDEX]
            .effective_config
            .as_ref()
            .expect("the first report is a full one and carries the effective configuration");
        let map = &effective.config_map.as_ref().expect("map").config_map;
        let body = String::from_utf8(map["supervisor.toml"].body.clone()).expect("utf-8");
        assert!(body.contains("# written by the operator"), "{body}");
        assert!(body.contains("endpoint = \"ws://127.0.0.1:1/v1/opamp\""));
        assert!(
            !body.contains("s3cret"),
            "a credential must never leave the host: {body}"
        );
    }

    /// A stored configuration as a remote configuration left it: `fleet` and the roled `ruleset`
    /// as entry files, `.supplementary` naming the role, and the `.pb` beside them.
    fn store_offer(supervisor_dir: &std::path::Path) -> std::path::PathBuf {
        use crate::supervisor::ports::AgentStorage as _;
        let storage = Storage::new(supervisor_dir.to_path_buf()).expect("storage");
        let entry = |body: &str, role: &str| opamp::proto::AgentConfigObject {
            body: body.as_bytes().to_vec(),
            role: role.to_string(),
            content_type: String::new(),
        };
        storage
            .store_remote_config(&opamp::proto::AgentRemoteConfig {
                config: Some(opamp::proto::AgentConfigMap {
                    config_map: std::collections::HashMap::from([
                        ("fleet".to_string(), entry("receivers: {}\n", "")),
                        ("ruleset".to_string(), entry("rules: []\n", "rules")),
                        ("edited".to_string(), entry("server: 1\n", "")),
                    ]),
                }),
                config_hash: b"stored".to_vec(),
            })
            .expect("store");
        storage.config_dir()
    }

    /// One `command` Supervisor named `agent`, with `names` in `[supervisors]
    /// remote_config_disabled` and, when given, the Client's own `name`.
    fn listed_config(root: &std::path::Path, names: &str, name: Option<&str>) -> ClientConfig {
        toml::from_str(&format!(
            "endpoint = \"ws://127.0.0.1:1/v1/opamp\"\nstate_dir = {state:?}\n{own}\
             [supervisors]\nremote_config_disabled = {names}\n\
             [[supervisor]]\ntype = \"command\"\nname = \"agent\"\ncommand = \"managed-agent\"\n",
            state = root.join("state").to_string_lossy(),
            own = name.map(|n| format!("name = {n:?}\n")).unwrap_or_default(),
        ))
        .expect("parse")
    }

    /// A listed name no block carries starts the Client and earns one notice naming it; one equal
    /// to the Client's own name says that Agent is not covered (ADR-0067 clause 2).
    /// Verifies: ADR-0067
    #[tokio::test]
    async fn a_listed_name_without_a_block_is_a_notice_not_a_refusal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_tx, shutdown) = shutdown_channel();
        let config = listed_config(dir.path(), "[\"agent\", \"later\"]", None);
        build_engine(&config, &shutdown).expect("a name without a block starts");
        let notices = remote_config_disabled_notices(&config);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].contains("\"later\""), "{notices:?}");
        assert!(
            notices[0].contains("no [[supervisor]] block"),
            "{notices:?}"
        );
    }

    /// Switching off takes the Server's last configuration out of force before the kind starts:
    /// the entry files it wrote and `.supplementary` go while their bytes are still the stored
    /// ones, an overwritten entry and an operator's own file stay, and the `.pb` goes (ADR-0067
    /// clause 5).
    /// Verifies: ADR-0067
    #[tokio::test]
    async fn a_listed_supervisor_drops_the_stored_remote_config_and_keeps_the_operators_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_tx, shutdown) = shutdown_channel();
        let config = listed_config(dir.path(), "[\"agent\"]", None);
        let supervisor_dir = config.supervisor_dir("agent");
        let config_dir = store_offer(&supervisor_dir);
        assert!(config_dir
            .join(crate::storage::SUPPLEMENTARY_FILE)
            .is_file());
        std::fs::write(config_dir.join("edited"), "operator: 1\n").expect("overwrite");
        std::fs::write(config_dir.join("local.yaml"), "mine: 1\n").expect("operator's file");

        build_engine(&config, &shutdown).expect("build");

        assert!(!supervisor_dir.join("remote-config.pb").exists());
        assert!(!config_dir.join("fleet").exists());
        assert!(!config_dir.join("ruleset").exists());
        assert!(!config_dir.join(crate::storage::SUPPLEMENTARY_FILE).exists());
        assert_eq!(
            std::fs::read_to_string(config_dir.join("edited")).expect("kept"),
            "operator: 1\n"
        );
        assert_eq!(
            std::fs::read_to_string(config_dir.join("local.yaml")).expect("kept"),
            "mine: 1\n"
        );
    }

    /// A stored configuration whose files cannot be removed stops the Supervisor before its kind
    /// starts on them, and at startup that fails the whole Client: it fails closed rather than run
    /// the Server's configuration under a switch that says it does not (ADR-0067 clause 5).
    /// Verifies: ADR-0067
    #[tokio::test]
    async fn a_stored_remote_config_that_cannot_be_removed_fails_startup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_tx, shutdown) = shutdown_channel();
        let config = listed_config(dir.path(), "[\"agent\"]", None);
        let supervisor_dir = config.supervisor_dir("agent");
        let config_dir = store_offer(&supervisor_dir);
        // An entry the stored map names that cannot be read, and so cannot be compared or removed.
        std::fs::remove_file(config_dir.join("fleet")).expect("remove");
        std::fs::create_dir(config_dir.join("fleet")).expect("in the way");

        let Err(err) = build_engine(&config, &shutdown) else {
            panic!("started over a stored configuration it could not remove");
        };
        assert!(
            err.contains("\"agent\"")
                && err.contains("cannot remove the stored remote configuration"),
            "{err}"
        );
        assert!(supervisor_dir.join("remote-config.pb").is_file());
    }

    /// A stored `.pb` that does not decode is deleted, and `config/` is left exactly as it is,
    /// since nothing says which of its files the Server wrote (ADR-0067 clause 5).
    /// Verifies: ADR-0067
    #[tokio::test]
    async fn an_undecodable_stored_remote_config_is_deleted_and_config_is_left_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_tx, shutdown) = shutdown_channel();
        let config = listed_config(dir.path(), "[\"agent\"]", None);
        let supervisor_dir = config.supervisor_dir("agent");
        let config_dir = store_offer(&supervisor_dir);
        std::fs::write(supervisor_dir.join("remote-config.pb"), [0xff; 7]).expect("garble");

        build_engine(&config, &shutdown).expect("build");

        assert!(!supervisor_dir.join("remote-config.pb").exists());
        for kept in [
            "fleet",
            "ruleset",
            "edited",
            crate::storage::SUPPLEMENTARY_FILE,
        ] {
            assert!(config_dir.join(kept).is_file(), "{kept} was touched");
        }
    }

    /// A listed Supervisor restarted over a stored configuration reports no status and no hash,
    /// and declares neither remote-configuration capability (ADR-0067 clauses 3 and 5).
    /// Verifies: ADR-0067
    #[tokio::test]
    async fn a_listed_supervisor_reports_no_remote_config_status_after_a_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_tx, shutdown) = shutdown_channel();
        let config = listed_config(dir.path(), "[\"agent\"]", None);
        store_offer(&config.supervisor_dir("agent"));

        let mut engine = build_engine(&config, &shutdown).expect("build");
        let report = &engine.poll_reports()[SELF_AGENT_OFFSET];
        assert!(report.remote_config_status.is_none(), "{report:?}");
        assert_eq!(
            report.capabilities
                & (AgentCapabilities::AcceptsRemoteConfig as u64
                    | AgentCapabilities::ReportsRemoteConfig as u64),
            0
        );

        // Unlisted, the same stored configuration is restored as applied, as before.
        let (_tx, shutdown) = shutdown_channel();
        let other = tempfile::tempdir().expect("tempdir");
        let unlisted = listed_config(other.path(), "[]", None);
        store_offer(&unlisted.supervisor_dir("agent"));
        let mut engine = build_engine(&unlisted, &shutdown).expect("build");
        let status = engine.poll_reports()[SELF_AGENT_OFFSET]
            .remote_config_status
            .clone()
            .expect("restored");
        assert_eq!(status.last_remote_config_hash, b"stored");
    }

    /// The switch covers Supervisors only: the Client's own name in the list earns a notice, and
    /// its Agent still declares `AcceptsRemoteConfig` for its Supervisor set (ADR-0067 clause 2
    /// and out of scope).
    /// Verifies: ADR-0067
    #[tokio::test]
    async fn the_clients_own_agent_keeps_accepting_its_supervisor_set_when_its_name_is_listed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_tx, shutdown) = shutdown_channel();
        let config = listed_config(dir.path(), "[\"edge-1\"]", Some("edge-1"));
        let notices = remote_config_disabled_notices(&config);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(
            notices[0].contains("\"edge-1\"") && notices[0].contains("own Agent is not covered"),
            "{notices:?}"
        );
        let mut engine = build_engine(&config, &shutdown).expect("build");
        let reports = engine.poll_reports();
        let own = reports[SELF_AGENT_INDEX].capabilities;
        assert_ne!(own & AgentCapabilities::AcceptsRemoteConfig as u64, 0);
        assert_ne!(own & AgentCapabilities::ReportsRemoteConfig as u64, 0);
        assert_ne!(
            reports[SELF_AGENT_OFFSET].capabilities & AgentCapabilities::AcceptsRemoteConfig as u64,
            0,
            "an unlisted Supervisor keeps it too"
        );
    }
}
