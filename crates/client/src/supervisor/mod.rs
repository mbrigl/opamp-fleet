//! The supervision domain (ADR-0011): builds the Agents the [`Engine`](crate::engine) carries.
//!
//! With `[[supervisor]]` blocks configured, each becomes one Supervisor-backed Agent — everything
//! it owns under `<supervisor_dir>/<name>/` (ADR-0021), its Managed Process driven by the plugin
//! the block's `type` selects. Without any, the Client presents itself as the single self-Agent —
//! the same state machine with no Managed Process behind it.

pub mod agent;
pub mod collector;
pub mod command;
pub mod endpoint;
pub mod icinga2;
pub mod ports;
pub mod process;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tracing::{info, warn};

use crate::config::{ClientConfig, SupervisorBlock};
use crate::engine::{Engine, EngineAgent};
use crate::service::runtime::{shutdown_channel, Shutdown};
use crate::storage::Storage;

use agent::AgentState;
use ports::{EventSender, Plugin, ProcessEvent, SupervisorContext};

/// The Engine index of the Client's own Agent (ADR-0020). It is built first, so a Supervisor's
/// index is its block's position plus this.
pub const SELF_AGENT_INDEX: usize = 0;

/// What a Supervisor's block position must be shifted by to reach its Engine index — and, read
/// the other way, what an Engine index is shifted back by to find the block it came from.
pub const SELF_AGENT_OFFSET: usize = SELF_AGENT_INDEX + 1;

/// The compiled-in plugin registry (ADR-0011). A new process kind is a new module and one line
/// here — the supervision core stays untouched (goal 8).
fn registry() -> Vec<Box<dyn Plugin>> {
    vec![
        Box::new(collector::CollectorPlugin),
        Box::new(command::CommandPlugin),
        Box::new(icinga2::Icinga2Plugin),
    ]
}

/// The kinds this Client was compiled with, as attributes of its own Agent (ADR-0091 clause 7).
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
    let (event_tx, events) = mpsc::channel(64);
    let mut agents = Vec::with_capacity(config.supervisors.len() + 1);

    // The Client is always its own Agent (ADR-0020), whether or not it supervises anything. It
    // used to exist only when nothing else did, which left the Client invisible on exactly the
    // hosts that manage something — and left the Server with nobody to offer the Client's own
    // package to. It is index 0 so the Supervisors that follow keep a stable, obvious offset.
    let storage = Storage::new(config.state_dir.clone())
        .map_err(|e| format!("cannot prepare {}: {e}", config.state_dir.display()))?;
    let mut self_state = declare_heartbeat(
        config,
        AgentState::new(config.name.clone(), storage)
            .map_err(|e| format!("cannot restore the agent state: {e}"))?
            .with_attributes(kind_attributes(config.agent_attributes(None)))
            .with_namespace(config.service_namespace.clone()),
    );
    // Consenting to be updated names the package it will take — anything else is refused rather
    // than written over this binary (ADR-0020). Since ADR-0075 the consent stands unless the file
    // withdraws it, so this is the ordinary path rather than the opted-into one.
    if let Some(package) = config.self_update_package() {
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
/// reaped (ADR-0059): it may be a purge a crash or an error cut short — or an operator's
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
/// touching the filesystem or starting anything (ADR-0056). What an offered Supervisor set is
/// checked against before any running process is stopped.
///
/// # Errors
/// Returns the same error `start_supervisor` would fail with, naming the block.
pub fn validate_block(config: &ClientConfig, block: &SupervisorBlock) -> Result<(), String> {
    let plugins = registry();
    let plugin = find_plugin(&plugins, block)?;
    let (settings, program) = take_program(config, block, plugin)?;
    // The three the core resolves, checked here so a Server-delivered set carrying one is refused
    // before a running process is touched (ADR-0056), exactly as a bad plugin setting is.
    effective_service_name(block, plugin, &program.path)?;
    check_endpoint_port(block, plugin)?;
    effective_timing(config, block, plugin)?;
    plugin.check(&block.name, settings)
}

/// Pinning the Supervisor Endpoint's port is a decision only where something connects to it
/// (ADR-0091).
///
/// The Endpoint itself is bound for every Supervisor and stays that way (ADR-0003) — what is
/// refused is *naming* its port for a kind whose Managed Process speaks no OpAMP, where the value
/// would read as configuration and do nothing. `0` is not refused: it is the default written out,
/// and refusing a no-op teaches nobody anything.
fn check_endpoint_port(block: &SupervisorBlock, plugin: &dyn Plugin) -> Result<(), String> {
    if block.endpoint_port != 0 && !plugin.defaults().endpoint_port {
        return Err(format!(
            "supervisor {:?}: `endpoint_port` says nothing for type {:?} — the Supervisor Endpoint \
             is bound for every Supervisor, but only a Managed Process that speaks OpAMP connects \
             to one, and this kind's does not; remove the line",
            block.name,
            plugin.kind()
        ));
    }
    Ok(())
}

/// The program a block resolves to (path plus whether this Client owns its directory), for callers
/// that must inspect ownership rather than just spawn it. The Supervisor-set apply uses it to keep
/// a Server-delivered block to a Client-owned program (ADR-0057).
pub fn resolve_block_program(
    config: &ClientConfig,
    block: &SupervisorBlock,
) -> Result<crate::config::Program, String> {
    let plugins = registry();
    let plugin = find_plugin(&plugins, block)?;
    let (_, program) = take_program(config, block, plugin)?;
    Ok(program)
}

/// Start one Supervisor at `index`: its state restored, its Endpoint bound, its adapter task
/// running. Used at startup for every configured block and at runtime for a block an applied
/// Supervisor set added or changed (ADR-0056).
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
    let plugin = find_plugin(&plugins, block)?;

    let supervisor_dir = config.supervisor_dir(&block.name);
    let storage = Storage::new(supervisor_dir.clone())
        .map_err(|e| format!("cannot prepare {}: {e}", supervisor_dir.display()))?;
    let config_dir = storage.config_dir();

    let (settings, program) = take_program(config, block, plugin)?;
    // What a package replaces: one file, or — when the block says where the program sits
    // inside the package — the whole tree under this Supervisor's `program/` (ADR-0023).
    let install = match effective_program_path(block, plugin)? {
        Some(program_path) => crate::supervisor::process::InstallTarget::Tree {
            root: supervisor_dir.join(crate::config::PROGRAM_DIR),
            program_path,
        },
        None => crate::supervisor::process::InstallTarget::Binary(program.path.clone()),
    };

    let service_name = effective_service_name(block, plugin, &program.path)?;
    check_endpoint_port(block, plugin)?;
    let mut state = declare_heartbeat(
        config,
        AgentState::supervised(block.name.clone(), service_name, storage)
            .map_err(|e| format!("cannot restore the state of {:?}: {e}", block.name))?
            .with_attributes(config.agent_attributes(Some(block)))
            .with_namespace(config.service_namespace.clone()),
    );
    // Every Managed Process is the fleet's (ADR-0085), so every Supervisor takes whichever
    // top-level package the Server selects for it (ADR-0015, ADR-0017). There is no second branch:
    // a block naming a program on the machine no longer parses, so the consent ADR-0021 derived
    // from the path is discharged by the type system rather than by a rule. The log line stays and
    // loses its "declined" half — it now says *where* the program is, which is the thing an
    // operator reading a startup log actually wants.
    //
    // What the target itself needs — for a tree that is its root and nothing below it, since the
    // live tree arrives by renaming a directory over that name (ADR-0023).
    install.prepare()?;
    state.accept_packages();
    info!(
        supervisor = %block.name,
        program = %program.path.display(),
        "packages accepted: the program is this supervisor's own"
    );

    // Each Supervisor stops on its own channel (ADR-0056): the Client-wide shutdown is forwarded
    // into it, and retiring the Supervisor fires it alone — its Endpoint releases the port and
    // its adapter stops the Managed Process while the rest of the Client runs on.
    let (stop_tx, stop) = shutdown_channel();
    forward_shutdown(shutdown.clone(), stop_tx.clone());

    // The Supervisor Endpoint is intrinsic to every Supervisor (ADR-0003): bound
    // unconditionally, before the process starts — a taken port fails startup, not later.
    endpoint::start(
        block.name.clone(),
        block.endpoint_port,
        EventSender::new(index, event_tx.clone()),
        stop.clone(),
        config.max_message_size_bytes,
    )?;

    let timing = effective_timing(config, block, plugin)?;
    let commands = plugin.start(SupervisorContext {
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

/// Forwards the Client-wide shutdown into one Supervisor's own channel, so its adapter and
/// Endpoint stop on whichever fires first — the operator stopping the Client, or the Supervisor
/// being retired (ADR-0056).
fn forward_shutdown(mut global: Shutdown, stop_tx: watch::Sender<bool>) {
    tokio::spawn(async move {
        global.requested().await;
        let _ = stop_tx.send(true);
    });
}

fn find_plugin<'a>(
    plugins: &'a [Box<dyn Plugin>],
    block: &SupervisorBlock,
) -> Result<&'a dyn Plugin, String> {
    plugins
        .iter()
        .find(|p| p.kind() == block.kind)
        .map(|p| p.as_ref())
        .ok_or_else(|| {
            let known: Vec<&str> = plugins.iter().map(|p| p.kind()).collect();
            format!(
                "supervisor {:?}: unknown type {:?} (known: {})",
                block.name,
                block.kind,
                known.join(", ")
            )
        })
}

/// Takes the program key out of the block's settings and resolves it (ADR-0021) — the rule that
/// derives package consent belongs to the core, and a plugin that parsed its own key could
/// disagree with the Agent's declared capability. Returns the remaining plugin settings and the
/// resolved program.
fn take_program(
    config: &ClientConfig,
    block: &SupervisorBlock,
    plugin: &dyn Plugin,
) -> Result<(toml::Table, crate::config::Program), String> {
    let mut settings = block.settings.clone();
    let key = plugin.program_key();
    let named = settings.remove(key);
    let program_name = match (named, plugin.defaults().program) {
        // A wrapped kind knows its program, so writing it is naming a value this Client computes
        // (ADR-0091 clause 1) — refused with what supplies it now, never quietly overridden.
        (Some(_), Some(derived)) => {
            return Err(format!(
                "supervisor {:?}: `{key}` is no longer a supervisor key for type {:?} — the kind \
                 installs and names its own program ({derived}); remove the line",
                block.name,
                plugin.kind()
            ))
        }
        (Some(raw), None) => raw
            .as_str()
            .ok_or_else(|| {
                format!(
                    "supervisor {:?}: `{key}` must be a path, not {}",
                    block.name,
                    raw.type_str()
                )
            })?
            .to_string(),
        (None, Some(derived)) => derived.to_string(),
        (None, None) => return Err(format!("supervisor {:?}: needs a `{key}`", block.name)),
    };
    let program = crate::config::resolve_program(
        key,
        std::path::Path::new(&program_name),
        effective_program_path(block, plugin)?.as_deref(),
        &config.supervisor_dir(&block.name),
        &block.name,
    )?;
    Ok((settings, program))
}

/// Where the program sits inside a package tree (ADR-0023): the block's answer, or the one the kind
/// knows (ADR-0091). A kind that knows it refuses a block that states it, for the reason
/// [`take_program`] refuses a program name.
///
/// # Errors
/// Returns an error when a block states a `program_path` its kind already supplies.
fn effective_program_path(
    block: &SupervisorBlock,
    plugin: &dyn Plugin,
) -> Result<Option<PathBuf>, String> {
    match (&block.program_path, plugin.defaults().program_path) {
        (Some(_), Some(derived)) => Err(format!(
            "supervisor {:?}: `program_path` is no longer a supervisor key for type {:?} — the \
             kind knows where its program sits in the tree it delivers ({derived}); remove the line",
            block.name,
            plugin.kind()
        )),
        (Some(stated), None) => Ok(Some(stated.clone())),
        (None, derived) => Ok(derived.map(PathBuf::from)),
    }
}

/// The Agent type this Supervisor presents until — and unless — its Managed Process reports one of
/// its own (ADR-0033): the block's, the kind's, else the program's file name.
///
/// The file-name fallback is what the operator already wrote in this very block; it is read from
/// configuration and never parsed out of a program's output, where a name has no grammar to
/// recognise it by. A kind that states its type refuses a block that restates it (ADR-0091).
/// What this Supervisor's three timings are, and whether its block was allowed to say anything
/// about them (ADR-0091).
///
/// Three layers, outermost first: the fleet's policy in `[supervisors]` and `[updates]`, a wrapped
/// kind's correction of it, and — only where no kind exists to hold the value — the block. A block
/// of a wrapped kind naming one is refused with what supplies it now, on the same terms as every
/// other retired key, so an offered Supervisor set is refused before a process is touched.
fn effective_timing(
    config: &ClientConfig,
    block: &SupervisorBlock,
    plugin: &dyn Plugin,
) -> Result<Timing, String> {
    let fleet = Timing {
        stop_timeout: Duration::from_secs(config.supervisor_defaults.stop_timeout_secs),
        apply_grace: Duration::from_secs(config.supervisor_defaults.apply_grace_secs),
        retain_previous: Duration::from_secs(config.updates.retain_previous_secs),
    };
    let stated = [
        ("stop_timeout_secs", block.stop_timeout_secs),
        ("apply_grace_secs", block.apply_grace_secs),
        ("retain_previous_secs", block.retain_previous_secs),
    ];
    let Some(kind) = plugin.defaults().timing else {
        // An unwrapped kind: nothing here knows the agent, so the block still answers.
        return Ok(Timing {
            stop_timeout: block
                .stop_timeout_secs
                .map_or(fleet.stop_timeout, Duration::from_secs),
            apply_grace: block
                .apply_grace_secs
                .map_or(fleet.apply_grace, Duration::from_secs),
            retain_previous: block
                .retain_previous_secs
                .map_or(fleet.retain_previous, Duration::from_secs),
        });
    };
    for (key, value) in stated {
        if value.is_some() {
            return Err(format!(
                "supervisor {:?}: `{key}` is no longer a supervisor key for type {:?} — how long \
                 an agent needs is a property of that agent, which the kind states, over the \
                 fleet's own `[supervisors]`/`[updates]` policy; remove the line",
                block.name,
                plugin.kind()
            ));
        }
    }
    Ok(Timing {
        stop_timeout: kind.stop_timeout.unwrap_or(fleet.stop_timeout),
        apply_grace: kind.apply_grace.unwrap_or(fleet.apply_grace),
        retain_previous: kind.retain_previous.unwrap_or(fleet.retain_previous),
    })
}

/// The three resolved timings of one Supervisor.
struct Timing {
    stop_timeout: Duration,
    apply_grace: Duration,
    retain_previous: Duration,
}

fn effective_service_name(
    block: &SupervisorBlock,
    plugin: &dyn Plugin,
    program: &std::path::Path,
) -> Result<String, String> {
    match (&block.service_name, plugin.defaults().service_name) {
        (Some(_), Some(derived)) => Err(format!(
            "supervisor {:?}: `service_name` is no longer a supervisor key for type {:?} — the \
             kind states the Agent type it presents ({derived}); remove the line",
            block.name,
            plugin.kind()
        )),
        (Some(stated), None) => Ok(stated.clone()),
        (None, Some(derived)) => Ok(derived.to_string()),
        (None, None) => Ok(program
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::runtime::shutdown_channel;
    use opamp::proto::AgentCapabilities;
    use std::path::PathBuf;

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

    /// A Client says which kinds it carries, one key per kind (ADR-0091 clause 7), so a Selector
    /// can aim a Supervisor set at the Clients that can actually run it — rather than the Server
    /// learning from a `FAILED` that it aimed at a Client too old to have the plugin.
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

        // An operator's own value under the same key is left alone: a configured attribute is the
        // host's statement about itself, and this only fills in what nothing else said.
        let stated = kind_attributes(
            [("supervisor.kind.command".to_string(), "no".to_string())]
                .into_iter()
                .collect(),
        );
        assert_eq!(
            stated.get("supervisor.kind.command").map(String::as_str),
            Some("no")
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
        let (_, program) = take_program(&config, block, plugin).expect("the block names it");
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

    /// ADR-0085 where it becomes visible to the Server: **every** Supervisor declares
    /// `AcceptsPackages`, because every Managed Process is one this Client installed. The
    /// capability is a constant of this Client now, not a function of a path — which is why the
    /// second half of this test is a startup refusal rather than a second capability.
    ///
    /// The `program/` directory is created either way, before the first package: the swap renames
    /// inside it, so it has to exist beforehand rather than after.
    #[tokio::test]
    async fn every_supervisor_declares_package_acceptance() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_tx, shutdown) = shutdown_channel();

        let owned: ClientConfig =
            toml::from_str(&config(dir.path(), "managed-agent", None)).expect("parse");
        let mut engine = build_engine(&owned, &shutdown).expect("build");
        assert!(
            accepts_packages(&mut engine),
            "the program is in this Client's own directory, which is what makes it updatable"
        );
        assert!(
            dir.path().join("state/supervisors/agent/program").is_dir(),
            "the directory the swap renames inside exists before any package arrives"
        );

        // The shape that used to declare nothing now does not start at all (ADR-0085).
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

    /// The side-effect-free `installs_packages()` that the startup signature-posture warning reads
    /// (ADR-0015) agrees with the `AcceptsPackages` capability an Agent actually declares.
    #[tokio::test]
    async fn installs_packages_reflects_declared_package_acceptance() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_tx, shutdown) = shutdown_channel();

        let owned: ClientConfig =
            toml::from_str(&config(dir.path(), "managed-agent", None)).expect("parse");
        let engine = build_engine(&owned, &shutdown).expect("build");
        assert!(
            engine.installs_packages(),
            "the program is package-updatable, so the Client installs packages"
        );

        // Since ADR-0085 every Supervisor is package-updatable, so the only way for an Engine to
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

        // The Client's own Agent consents by default (ADR-0075), so a Client with no Supervisor at
        // all still installs packages — its own.
        let bare: ClientConfig =
            toml::from_str("endpoint = \"ws://127.0.0.1:1/v1/opamp\"\n").expect("parse");
        let engine = build_engine(&bare, &shutdown).expect("build");
        assert!(
            engine.installs_packages(),
            "the Client's own Agent consents by default"
        );
    }

    /// A tree Supervisor owns its `program/` directory and *nothing inside it* (ADR-0023). The
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
        let parsed: ClientConfig = toml::from_str(&config).expect("parse");
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

    /// ADR-0059 point 5: a directory no block names survives startup — reported, never reaped.
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
}
