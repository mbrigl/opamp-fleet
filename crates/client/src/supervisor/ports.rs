//! The Managed-Process-facing Port (ADR-0011): the boundary the supervision domain defines and
//! depends on. A Plugin is an adapter behind it — a factory that validates its block's settings
//! and starts a task driving one Managed Process.
//!
//! The Port is a message pair, not a trait with async methods: commands flow to the adapter,
//! events flow back. That keeps the [`Plugin`] trait object-safe without an `async-trait`
//! dependency, makes every adapter a plain tokio task, and keeps the domain core free of
//! process handles.

use std::path::PathBuf;
use std::time::Duration;

use opamp::proto::{
    AgentDescription, AgentRemoteConfig, AvailableComponents, ComponentHealth, EffectiveConfig,
};
use tokio::sync::mpsc;

use crate::service::runtime::Shutdown;

/// What the supervision core asks of a Managed Process.
#[derive(Debug)]
pub enum ProcessCommand {
    /// A remote configuration was received and persisted — the entry files are already written
    /// to the adapter's [`config_dir`](SupervisorContext::config_dir). Apply it, which for a
    /// process means restarting on the new files, and answer with
    /// [`ProcessEvent::ConfigApplied`].
    ApplyConfig { config: AgentRemoteConfig },
    /// A package was downloaded and verified (content hash and signature; ADR-0015): swap its
    /// bytes over the Managed Process's binary, restart, and health-gate exactly as `ApplyConfig`
    /// does — a binary that will not stay up is rolled back to the previous one. Answered with
    /// [`ProcessEvent::PackageApplied`]. `staged` is the path of the verified artifact — a file,
    /// not its bytes, since a program is too big to carry through the core; `hash` is the package
    /// hash the status refers to; `version` is what the Agent then reports it has.
    ApplyPackage {
        staged: PathBuf,
        version: String,
        hash: Vec<u8>,
    },
    /// The Server commanded a restart (`AcceptsRestartCommand`): stop and respawn on the
    /// *current* files. No configuration changed, so no [`ProcessEvent::ConfigApplied`] follows —
    /// the health events of the stop/spawn cycle are the visible outcome.
    Restart,
    /// Stop the Managed Process gracefully.
    Shutdown,
    /// The Supervisor is retired for good (ADR-0060): stop the Managed Process, undo whatever
    /// installing it left *outside* the Supervisor's directory — the generic implementation has
    /// nothing there, so its uninstall is exactly the graceful stop — answer with
    /// [`ProcessEvent::Uninstalled`], and exit. The directory itself is the core's to purge
    /// (ADR-0059), after the answer.
    Uninstall,
}

/// What a Managed-Process adapter reports back to the core.
#[derive(Debug)]
pub enum ProcessEvent {
    /// The process's own description (reported through the Supervisor Endpoint), folded into
    /// the Agent's — its identity (`service.instance.id`) stays the Supervisor's.
    Description(AgentDescription),
    /// The pid of the running Managed Process, or `None` once it is gone (ADR-0036). It is what
    /// lets this Client sample the process's own CPU and memory from the outside, which is the
    /// only honest reading of "own telemetry" for a process whose configuration it must not touch
    /// (ADR-0011).
    Pid(Option<u32>),
    /// Health — derived from the outside (spawned, exited, spawn failed) or self-reported.
    Health(ComponentHealth),
    /// The process's self-reported effective configuration; replaces the written-files echo.
    EffectiveConfig(EffectiveConfig),
    /// The process's available components (reported through the Supervisor Endpoint by the
    /// Collector's `opampextension`), relayed upstream under the owning Agent.
    AvailableComponents(AvailableComponents),
    /// Outcome of an [`ProcessCommand::ApplyConfig`]: `Ok` acknowledges `APPLIED`, `Err`
    /// reports `FAILED` with the error — a rejected configuration is a report, not a silence.
    ConfigApplied {
        hash: Vec<u8>,
        result: Result<(), String>,
    },
    /// Outcome of an [`ProcessCommand::ApplyPackage`]: `Ok(version)` reports `Installed` at that
    /// version, `Err` reports `InstallFailed` with the error after rolling back (ADR-0015).
    PackageApplied {
        hash: Vec<u8>,
        result: Result<String, String>,
    },
    /// Outcome of a [`ProcessCommand::Uninstall`] (ADR-0060), the adapter's last event. The
    /// Agent's goodbye carries no status, so the outcome is a log line — but an `Err` names what
    /// the retired kind could not undo, which the operator otherwise learns from nothing.
    Uninstalled { result: Result<(), String> },
}

/// The adapter's way back into the core: events tagged with the owning Agent's index on the
/// shared channel the [`Engine`](crate::engine::Engine) drains.
#[derive(Debug, Clone)]
pub struct EventSender {
    index: usize,
    tx: mpsc::Sender<(usize, ProcessEvent)>,
}

impl EventSender {
    #[must_use]
    pub fn new(index: usize, tx: mpsc::Sender<(usize, ProcessEvent)>) -> Self {
        EventSender { index, tx }
    }

    /// Sends one event; a closed channel means the Engine is gone and the event is moot.
    pub async fn send(&self, event: ProcessEvent) {
        let _ = self.tx.send((self.index, event)).await;
    }
}

/// Everything a plugin needs to start its adapter task.
pub struct SupervisorContext {
    /// The Supervisor's name (the TOML `name`; the Agent's `service.name`).
    pub name: String,
    /// Everything this Supervisor owns: its state, its `program/`, its package staging
    /// (ADR-0021). Placed by `supervisor_dir`, so nothing may assume where it is.
    pub supervisor_dir: PathBuf,
    /// Where the received remote configuration's entry files are written — what the Managed
    /// Process is pointed at.
    pub config_dir: PathBuf,
    /// The Managed Process itself, already resolved (ADR-0021): either inside this Supervisor's
    /// own `program/` directory, or the absolute path the block named. The plugin spawns this
    /// rather than reading its own `binary`/`command` key, so the path rule — and the package
    /// consent derived from it — lives in one place instead of once per plugin.
    pub program: PathBuf,
    /// What an offered package replaces (ADR-0015, ADR-0023) — resolved beside `program` and for
    /// the same reason: a plugin that decided this for itself could disagree with the Agent's
    /// declared consent.
    pub install: crate::supervisor::process::InstallTarget,
    /// Graceful-stop budget before the Managed Process is killed.
    pub stop_timeout: Duration,
    /// How long a freshly (re)started process must survive before `ApplyConfig` is acknowledged
    /// `Ok` — the health-gated acknowledgement (ADR-0011). Zero acknowledges on start.
    pub apply_grace: Duration,
    /// How long the version a successful update supersedes is kept before deletion (ADR-0058),
    /// resolved from the per-Supervisor override or the global `[updates]` default. Zero deletes on
    /// success.
    pub retain_previous: Duration,
    /// The key that opens an encrypted `.7z` package artifact (ADR-0018); `None` when none is
    /// configured. Client-wide, like the package verification key.
    pub archive_key: Option<String>,
    /// The plugin-specific keys of the block, for the strict second-stage parse.
    pub settings: toml::Table,
    /// Where the adapter reports events.
    pub events: EventSender,
    /// The Client's shutdown signal; the adapter stops its process and exits when it fires.
    pub shutdown: Shutdown,
}

impl SupervisorContext {
    /// Expands the placeholders naming this Supervisor's own directories (ADR-0022):
    /// `${supervisor_dir}` and `${config_dir}`.
    ///
    /// They exist because a Custom Supervisor is told where its configuration is *through its own
    /// command line*, and an absolute path written there drifts the moment `supervisor_dir` moves
    /// or the Supervisor is renamed — silently, since the process then starts happily on a file
    /// nobody writes to.
    ///
    /// An unrecognized `${…}` is **left exactly as written**, neither refused nor emptied. A
    /// Foreign Agent's own configuration language may use the same syntax — Fluent Bit's does —
    /// and eating those to catch a typo would break a working deployment. What this substitutes
    /// is the two names below; everything else is the process's business.
    ///
    /// Never applied to the program itself: under ADR-0021 the written shape of that path is what
    /// decides whether the Agent declares `AcceptsPackages`, and a substituted one would make a
    /// fleet-visible capability depend on something the file does not literally say.
    #[must_use]
    pub fn expand(&self, value: &str) -> String {
        value
            .replace("${supervisor_dir}", &self.supervisor_dir.to_string_lossy())
            .replace("${config_dir}", &self.config_dir.to_string_lossy())
    }
}

/// A compiled-in Supervisor Plugin (ADR-0011): the adapter factory on the Managed-Process side.
/// A new process kind is a new implementation and one line in
/// [`registry`](crate::supervisor::registry).
pub trait Plugin {
    /// The TOML `type` value this plugin serves.
    fn kind(&self) -> &'static str;

    /// The block key naming this plugin's Managed Process — `binary` for a Collector, `command`
    /// for the example Custom Supervisor. The core takes that key out of the settings, applies
    /// ADR-0021's path rule to it, and hands the result back as
    /// [`SupervisorContext::program`]; the plugin never sees the raw value.
    fn program_key(&self) -> &'static str;

    /// Validate the settings and start the adapter task, returning the command side of the Port.
    ///
    /// # Errors
    /// Returns an error when the settings do not parse — startup fails loudly, nothing spawns.
    fn start(&self, ctx: SupervisorContext) -> Result<mpsc::Sender<ProcessCommand>, String>;

    /// The strict settings parse [`start`](Self::start) performs, without the side effects
    /// (ADR-0056): what validates an offered Supervisor set *before* any running process is
    /// touched. `settings` is the block's table with the program key already taken out, exactly
    /// as `start` receives it.
    ///
    /// # Errors
    /// Returns an error when the settings do not parse.
    fn check(&self, name: &str, settings: toml::Table) -> Result<(), String>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::runtime::shutdown_channel;

    /// A per-Supervisor root that is absolute on *this* platform — on Windows that means naming a
    /// drive (ADR-0021), and it is why nothing below spells a path out with POSIX separators: what
    /// a placeholder expands to is a `PathBuf`, so its separators are the platform's own.
    #[cfg(windows)]
    fn root(place: &str) -> PathBuf {
        PathBuf::from(format!("C:\\{place}\\supervisors\\fluent-bit"))
    }

    #[cfg(not(windows))]
    fn root(place: &str) -> PathBuf {
        PathBuf::from(format!("/{place}/supervisors/fluent-bit"))
    }

    fn context(supervisor_dir: PathBuf) -> SupervisorContext {
        let (_tx, shutdown) = shutdown_channel();
        let (event_tx, _events) = mpsc::channel(1);
        SupervisorContext {
            name: "fluent-bit".to_string(),
            config_dir: supervisor_dir.join("config"),
            supervisor_dir,
            program: PathBuf::from("/opt/fluent-bit/bin/fluent-bit"),
            install: crate::supervisor::process::InstallTarget::Binary(PathBuf::from(
                "/opt/fluent-bit/bin/fluent-bit",
            )),
            stop_timeout: Duration::from_secs(1),
            apply_grace: Duration::from_secs(0),
            retain_previous: Duration::from_secs(0),
            archive_key: None,
            settings: toml::Table::new(),
            events: EventSender::new(0, event_tx),
            shutdown,
        }
    }

    /// The case ADR-0022 exists for: the argument that points a Foreign Agent at its configuration
    /// is derived from the same value the Client derives it from, so relocating `supervisor_dir`
    /// cannot leave the process reading a file nobody writes to.
    #[test]
    fn the_placeholders_name_this_supervisors_own_directories() {
        let ctx = context(root("opt"));
        // The placeholder becomes the directory the Client itself writes to; what the operator
        // wrote after it is a string and survives verbatim, separator included.
        assert_eq!(
            ctx.expand("${config_dir}/fluent-bit-conf"),
            format!("{}/fluent-bit-conf", ctx.config_dir.display())
        );
        // Two different directories, and the configuration's is the one inside.
        let supervisor = ctx.expand("${supervisor_dir}");
        let config = ctx.expand("${config_dir}");
        assert_ne!(supervisor, config);
        assert!(
            config.starts_with(&supervisor),
            "{config} must sit inside {supervisor}"
        );
        // Relocating the root moves the expansion with it — that is the whole point.
        let moved = context(root("var"));
        assert_ne!(
            moved.expand("${config_dir}/x"),
            ctx.expand("${config_dir}/x")
        );
        assert!(
            moved
                .expand("${config_dir}/x")
                .starts_with(&moved.supervisor_dir.display().to_string()),
            "the expansion follows the relocated root"
        );
    }

    /// Anything else is left exactly as written. Fluent Bit's own configuration language uses
    /// `${…}` too, and a Client that ate or refused those would break a working deployment to
    /// catch a typo — which is the trade ADR-0022 makes, deliberately and in this direction.
    #[test]
    fn an_unknown_placeholder_is_passed_through_untouched() {
        let ctx = context(root("opt"));
        for verbatim in [
            "${FLB_LOG_LEVEL}",
            "${config-dir}", // a typo: passed on, not refused
            "-c",
            "",
            "$config_dir",
            "${}",
        ] {
            assert_eq!(
                ctx.expand(verbatim),
                verbatim,
                "must pass through untouched"
            );
        }
        // And a known placeholder still expands when it sits beside an unknown one.
        assert_eq!(
            ctx.expand("${config_dir}/${FLB_ENV}.conf"),
            format!("{}/${{FLB_ENV}}.conf", ctx.config_dir.display())
        );
    }
}
