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
            .with_attributes(config.agent_attributes(None))
            .with_namespace(config.service_namespace.clone()),
    );
    // Consenting to be updated is its own decision, made per Client, and it names the package it
    // will take — anything else is refused rather than written over this binary (ADR-0020).
    if let Some(self_update) = &config.self_update {
        self_state.accept_packages_named(self_update.package.clone());
    }
    // The self-Agent's effective configuration is its own file — `client.toml` is what this
    // Client runs (a file that fails to load fails startup), so the fleet view can finally answer
    // it. The text was redacted at load; without it, echoing a stored offer would say nothing
    // about this Client. No file means the defaults run, and there is nothing truthful to show.
    if let Some(source) = &config.source {
        self_state.set_process_effective_config(opamp::proto::EffectiveConfig {
            config_map: Some(opamp::proto::AgentConfigMap {
                config_map: std::collections::HashMap::from([(
                    "client.toml".to_string(),
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
    let (settings, _) = take_program(config, block, plugin)?;
    plugin.check(&block.name, settings)
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
    let install = match block.program_path.as_ref() {
        Some(program_path) => crate::supervisor::process::InstallTarget::Tree {
            root: supervisor_dir.join(crate::config::PROGRAM_DIR),
            program_path: program_path.clone(),
        },
        None => crate::supervisor::process::InstallTarget::Binary(program.path.clone()),
    };

    // The Agent type this Supervisor presents until — and unless — its Managed Process reports
    // one of its own (ADR-0033). The program's file name is the fallback because it is what
    // the operator already wrote in this very block: read from configuration, never parsed out
    // of a program's output, where a name has no grammar to recognise it by.
    let service_name = block.service_name.clone().unwrap_or_else(|| {
        program
            .path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    });
    let mut state = declare_heartbeat(
        config,
        AgentState::supervised(block.name.clone(), service_name, storage)
            .map_err(|e| format!("cannot restore the state of {:?}: {e}", block.name))?
            .with_attributes(config.agent_attributes(Some(block)))
            .with_namespace(config.service_namespace.clone()),
    );
    // Owning the directory the program sits in *is* the consent (ADR-0021): a Supervisor that
    // has it takes whichever top-level package the Server selects for it (ADR-0015, ADR-0017).
    // Logged either way — the consent is now derived rather than written, and an operator who
    // changes a path should not have to infer what it did to the fleet.
    if program.owned {
        // What the target itself needs — for a tree that is its root and nothing below it,
        // since the live tree arrives by renaming a directory over that name (ADR-0023).
        install.prepare()?;
        state.accept_packages();
        info!(
            supervisor = %block.name,
            program = %program.path.display(),
            "packages accepted: the program is this supervisor's own"
        );
    } else {
        info!(
            supervisor = %block.name,
            program = %program.path.display(),
            "packages declined: the program is named by an absolute path"
        );
    }

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

    let commands = plugin.start(SupervisorContext {
        name: block.name.clone(),
        supervisor_dir,
        config_dir,
        program: program.path,
        install,
        stop_timeout: Duration::from_secs(block.stop_timeout_secs),
        apply_grace: Duration::from_secs(block.apply_grace_secs),
        retain_previous: Duration::from_secs(
            block
                .retain_previous_secs
                .unwrap_or(config.updates.retain_previous_secs),
        ),
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
    let raw = settings
        .remove(key)
        .ok_or_else(|| format!("supervisor {:?}: needs a `{key}`", block.name))?;
    let raw = raw.as_str().ok_or_else(|| {
        format!(
            "supervisor {:?}: `{key}` must be a path, not {}",
            block.name,
            raw.type_str()
        )
    })?;
    let program = crate::config::resolve_program(
        key,
        std::path::Path::new(raw),
        block.program_path.as_deref(),
        &config.supervisor_dir(&block.name),
        &block.name,
    )?;
    Ok((settings, program))
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

    fn accepts_packages(engine: &mut Engine) -> bool {
        let reports = engine.poll_reports();
        let supervisor = &reports[SELF_AGENT_OFFSET];
        supervisor.capabilities & AgentCapabilities::AcceptsPackages as u64 != 0
    }

    /// ADR-0021's rule where it actually becomes visible to the Server: owning the directory the
    /// program sits in is the consent, so the capability follows the shape of the path and nothing
    /// else. The directory is created for the owned case — the swap renames inside it, so it has
    /// to exist before the first package rather than after it.
    #[tokio::test]
    async fn the_program_path_decides_the_declared_package_capability() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_tx, shutdown) = shutdown_channel();

        let owned: ClientConfig =
            toml::from_str(&config(dir.path(), "managed-agent", None)).expect("parse");
        let mut engine = build_engine(&owned, &shutdown).expect("build");
        assert!(
            accepts_packages(&mut engine),
            "a bare name puts the program in our own directory, which is the consent"
        );
        assert!(
            dir.path().join("state/supervisors/agent/program").is_dir(),
            "the directory the swap renames inside exists before any package arrives"
        );

        let foreign = dir.path().join("elsewhere/managed-agent");
        let machines: ClientConfig = toml::from_str(&config(
            dir.path(),
            &foreign.to_string_lossy(),
            Some(dir.path().join("other")),
        ))
        .expect("parse");
        let mut engine = build_engine(&machines, &shutdown).expect("build");
        assert!(
            !accepts_packages(&mut engine),
            "an absolute path is the machine's program; we declare nothing"
        );
        assert!(
            !dir.path().join("other/agent/program").exists(),
            "nothing is created for a program we do not own"
        );
        assert!(
            dir.path().join("other/agent/instance-uid").is_file(),
            "the relocated root is where the supervisor's state went"
        );
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
            "an owned program is package-updatable, so the Client installs packages"
        );

        let foreign = dir.path().join("elsewhere/managed-agent");
        let machines: ClientConfig = toml::from_str(&config(
            dir.path(),
            &foreign.to_string_lossy(),
            Some(dir.path().join("other")),
        ))
        .expect("parse");
        let engine = build_engine(&machines, &shutdown).expect("build");
        assert!(
            !engine.installs_packages(),
            "an absolute program is the machine's; the Client installs no packages"
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
    /// the first report carries `client.toml`'s (redacted) text, which is what fills the fleet
    /// view's empty column for every Client.
    #[tokio::test]
    async fn the_self_agent_reports_its_file_as_the_effective_configuration() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_tx, shutdown) = shutdown_channel();
        let path = dir.path().join("client.toml");
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
        let body = String::from_utf8(map["client.toml"].body.clone()).expect("utf-8");
        assert!(body.contains("# written by the operator"), "{body}");
        assert!(body.contains("endpoint = \"ws://127.0.0.1:1/v1/opamp\""));
        assert!(
            !body.contains("s3cret"),
            "a credential must never leave the host: {body}"
        );
    }
}
