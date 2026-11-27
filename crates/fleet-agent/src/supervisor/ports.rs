//! The Managed-Process-facing ports of the supervision core: the commands it sends a Supervisor's
//! adapter task and the events it receives back, the registry of compiled-in kinds, and the host
//! facts. A Plugin is no part of them: it implements the port of
//! [`port`](crate::supervisor::port) alone, and the core starts it through
//! [`PluginRegistry::start`] (CLT-wrappers-act-through-the-core §1).
//!
//! The adapter task is a message pair, not a trait with async methods: commands flow to it,
//! events flow back, which keeps the domain core free of process handles.

use std::path::PathBuf;
use std::time::Duration;

use opamp::proto::{
    AgentDescription, AgentRemoteConfig, AvailableComponents, ComponentHealth, EffectiveConfig,
};
use opamp::uid::InstanceUid;
use tokio::sync::mpsc;

use crate::shutdown::Shutdown;
pub use crate::supervisor::port::{parse_settings, KindDefaults, KindTiming, Plugin};

/// The health status a Supervisor reports while the second service cannot be reached (CLT-accounts-service-runs-under §17).
pub const UNREACHABLE: &str = "the second service cannot be reached";

/// What a Supervisor reports while the second service is reached and refused: another account
/// serves its socket, it is another build, or it refuses the Client (CLT-accounts-service-runs-under §19 and §20).
/// Not an outage that ends by itself, so it is told apart from [`UNREACHABLE`].
pub const REFUSED: &str =
    "the second service is refused: another account, another build, or it refuses this Client";

/// Why `health` says that the second service is not reached — [`UNREACHABLE`] or [`REFUSED`] —
/// or `None` where it says nothing of the kind. While it says so, a configuration that is
/// `APPLYING` and a package that is `Installing` wait for the service, and the Agent names it as the
/// reason (CLT-accounts-service-runs-under §17).
#[must_use]
pub fn waiting_reason(health: &ComponentHealth) -> Option<&str> {
    let waits = !health.healthy && (health.status == UNREACHABLE || health.status == REFUSED);
    waits.then_some(health.status.as_str())
}

/// What the supervision core asks of a Managed Process.
#[derive(Debug)]
pub enum ProcessCommand {
    /// A remote configuration was received and persisted — the entry files are already written
    /// to the adapter's [`config_dir`](SupervisorContext::config_dir). Apply it, which for a
    /// process means restarting on the new files, and answer with
    /// [`ProcessEvent::ConfigApplied`] — where the start does not reach the second service, once
    /// a start does (CLT-accounts-service-runs-under §17).
    ApplyConfig {
        config: AgentRemoteConfig,
        /// The span of the apply this command is one half of (CLT-what-an-agent-reports). The core opens it when the
        /// configuration is handed over and the adapter's phases hang off it, so one trace covers
        /// the restart and its health gate rather than ending where the message does.
        span: tracing::Span,
    },
    /// A package was downloaded and verified (content hash and signature; PKG-signed-packages-and-delivery): swap its
    /// bytes over the Managed Process's binary, restart, and health-gate exactly as `ApplyConfig`
    /// does — a binary that will not stay up is rolled back to the previous one. Answered with
    /// [`ProcessEvent::PackageApplied`]. `staged` is the path of the verified artifact — a file,
    /// not its bytes, since a program is too big to carry through the core; `hash` is the package
    /// hash the status refers to; `version` is what the Agent then reports it has. While the
    /// second service cannot be reached, the package is staged and kept, never swapped in, and
    /// the answer follows its swap once a start reaches the service; a newer package replaces it
    /// unanswered (CLT-accounts-service-runs-under §17).
    ApplyPackage {
        staged: PathBuf,
        version: String,
        hash: Vec<u8>,
        /// The span of the install (CLT-what-an-agent-reports), opened where the download started. Carried rather
        /// than reopened here: staging, preflight, swap, gate and rollback happen in the adapter's
        /// task, and a trace that ended at the hand-over would stop one phase before the failures
        /// worth tracing.
        span: tracing::Span,
    },
    /// The Server commanded a restart (`AcceptsRestartCommand`): stop and respawn on the
    /// *current* files. No configuration changed, so no [`ProcessEvent::ConfigApplied`] follows —
    /// the health events of the stop/spawn cycle are the visible outcome.
    Restart,
    /// Stop the Managed Process gracefully.
    Shutdown,
    /// The Supervisor is retired for good (CLT-wrappers-act-through-the-core): stop the Managed Process, undo whatever
    /// installing it left *outside* the Supervisor's directory — the generic implementation has
    /// nothing there, so its uninstall is exactly the graceful stop — answer with
    /// [`ProcessEvent::Uninstalled`], and exit. The directory itself is the core's to purge
    /// (CLT-wrappers-act-through-the-core), after the answer.
    Uninstall,
}

/// What a Managed-Process adapter reports back to the core.
#[derive(Debug)]
pub enum ProcessEvent {
    /// The process's own description (reported through the Supervisor Endpoint), folded into
    /// the Agent's — its identity (`service.instance.id`) stays the Supervisor's.
    Description(AgentDescription),
    /// The pid of the running Managed Process, or `None` once it is gone (CLT-what-an-agent-reports). It is what
    /// lets this Client sample the process's own CPU and memory from the outside, which is the
    /// only honest reading of "own telemetry" for a process whose configuration it must not touch
    /// (CLT-wrappers-act-through-the-core).
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
    /// version, `Err` reports `InstallFailed` with the error after rolling back (PKG-signed-packages-and-delivery).
    PackageApplied {
        hash: Vec<u8>,
        result: Result<String, String>,
    },
    /// Outcome of a [`ProcessCommand::Uninstall`] (CLT-wrappers-act-through-the-core), the adapter's last event. The
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

/// What a package replaces on disk.
///
/// The two shapes share their whole lifecycle — set the old one aside, install the new one, prove
/// it starts, put the old one back if it does not — and differ only in what "it" is. Keeping that
/// difference in this type rather than inside the process adapter's swap is what lets the health
/// gate and the rollback stay one piece of code for both (PKG-signed-packages-and-delivery). How each shape is set aside,
/// installed and put back is the adapter's, in [`process`](super::process).
#[derive(Debug, Clone)]
pub enum InstallTarget {
    /// One file: the artifact is the program, or an archive holding it as one member.
    Binary {
        /// Where the program lives.
        path: PathBuf,
        /// Where its archive holds it, where its kind knows the place
        /// (PKG-signed-packages-and-delivery §55): the one member whose trailing
        /// path components are this, `/`-separated and without the archive's wrapping directory
        /// (`usr/bin/agent`), so a release that carries further files of the program's name
        /// installs the program and not one of those. `None` finds the member by the program's
        /// file name.
        member: Option<&'static str>,
    },
    /// A whole directory tree, unpacked beside the running one and swapped by renaming
    /// directories — the same move the single-file case makes, one level up.
    Tree {
        /// This Supervisor's `program/` directory, which holds the live tree and the rolled-back
        /// one under fixed names.
        root: PathBuf,
        /// Where the program sits inside the tree, as written in the configuration.
        program_path: PathBuf,
        /// The absolute links the kind's tree keeps, the only links the install creates
        /// (PKG-signed-packages-and-delivery §57).
        links: &'static [fleet_util::tree::KeptLink],
    },
}

impl InstallTarget {
    /// One file found in its archive by the program's file name.
    #[must_use]
    pub fn binary(path: PathBuf) -> Self {
        Self::Binary { path, member: None }
    }
}

/// The process account's primary group, where a process account is registered (CLT-accounts-service-runs-under §21).
/// What the Client shares with the Managed Processes belongs to this group, and nothing else does.
///
/// Resolved from the `--process-uid` of the Client's command line when it starts, never from the
/// configuration file: the registration that names the process account is the only place it is
/// stated (CLT-accounts-service-runs-under §6).
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessAccountGroup(pub u32);

/// On Windows the process account itself, by its SID, from the `--process-sid` of the Client's
/// command line: what the Client shares with the Managed Processes is granted to it by ACL, where
/// Unix gives it to the account's group (CLT-accounts-service-runs-under §21). Made once per run.
#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessAccountGroup(pub &'static str);

/// The package this Supervisor's Managed Process currently runs (PKG-signed-packages-and-delivery), persisted so a
/// restarted Client reports the version it has and is not re-offered it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InstalledPackage {
    pub name: String,
    pub version: String,
    pub hash_hex: String,
}

/// The highest package version this Client has installed for each Agent type — its **high-water
/// mark** (PKG-versions-high-water-mark-floor §8). Shared by every Supervisor of the Client and kept where no purge of
/// a Supervisor's directory reaches it. The filesystem adapter is
/// [`HighWaterFile`](crate::storage::HighWaterFile).
pub trait HighWaterMarks: Send + Sync {
    /// The mark of `agent_type`: `Ok(None)` when this Client has installed nothing of the type,
    /// and `Err` with a reason naming the file when the marks cannot be read — which refuses every
    /// Supervisor package (PKG-versions-high-water-mark-floor §11).
    ///
    /// # Errors
    /// Returns the reason the marks cannot be read.
    fn mark(&self, agent_type: &str) -> Result<Option<String>, String>;

    /// Raises the mark of `agent_type` to `version` when that is SemVer and greater; never lowers
    /// it. A write that fails is logged, and the raised mark is kept in memory.
    fn raise(&self, agent_type: &str, version: &str);
}

/// The floor of each Agent type this Client keeps (PKG-versions-high-water-mark-floor §17): the highest version below
/// which the operator signed that nothing of the type is taken. Shared by every Agent of the
/// Client and kept where no purge reaches it. The filesystem adapter, which also fetches them, is
/// [`FloorFile`](crate::floors::FloorFile).
pub trait PackageFloors: Send + Sync {
    /// The floor of `agent_type`: `Ok(None)` when none is kept, and `Err` with a reason naming the
    /// file when the floors cannot be read — which refuses every package offer, the Client's own
    /// included (PKG-versions-high-water-mark-floor §17).
    ///
    /// # Errors
    /// Returns the reason the floors cannot be read.
    fn floor(&self, agent_type: &str) -> Result<Option<String>, String>;
}

/// What an Agent persists across restarts (DEV-ports-and-adapters): its identity, the remote configuration it
/// last applied, and the package its Managed Process runs (PKG-signed-packages-and-delivery). The filesystem adapter is
/// [`Storage`](crate::storage::Storage).
pub trait AgentStorage: Send + Sync {
    /// The persisted `instance_uid`, or a new one, persisted, when there is none.
    ///
    /// # Errors
    /// Returns an error when the identity cannot be read or written.
    fn load_or_create_uid(&self) -> std::io::Result<InstanceUid>;

    /// Persists a reassigned `instance_uid` (`RequestInstanceUid`).
    ///
    /// # Errors
    /// Returns an error when the identity cannot be written.
    fn save_uid(&self, uid: &InstanceUid) -> std::io::Result<()>;

    /// The remote configuration last applied, if any.
    fn load_remote_config(&self) -> Option<AgentRemoteConfig>;

    /// Persists an applied remote configuration, with one entry file per config-map entry.
    ///
    /// # Errors
    /// Returns an error when the configuration cannot be written.
    fn store_remote_config(&self, config: &AgentRemoteConfig) -> std::io::Result<()>;

    /// The package the Managed Process runs, if one was installed.
    fn load_package(&self) -> Option<InstalledPackage>;

    /// Persists the package the Managed Process now runs.
    ///
    /// # Errors
    /// Returns an error when the record cannot be written.
    fn store_package(&self, package: &InstalledPackage) -> std::io::Result<()>;

    /// Forgets the installed package.
    ///
    /// # Errors
    /// Returns an error when the record cannot be removed.
    fn forget_package(&self) -> std::io::Result<()>;
}

/// What the operating system says about itself — the Baseline's `os.*`. Read **once** per process
/// and as one answer: two of the three platforms have to be asked by running a program, and asking
/// them once per attribute would start three processes to learn what one printout already holds.
/// A field the platform does not answer stays `None` and is then not reported at all.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct OsInfo {
    /// `os.description` — the human-readable line: "Ubuntu 24.04.2 LTS".
    pub description: Option<String>,
    /// `os.name` — the system's own name, without a version: "Ubuntu", "macOS", "Windows".
    pub name: Option<String>,
    /// `os.version` — the version that name is at: "24.04", "15.5", "10.0.26100.2033".
    pub version: Option<String>,
    /// `os.build_id` — the build behind that version, where the platform stamps one: os-release's
    /// `BUILD_ID`, `sw_vers`' BuildVersion ("24F74"), the build components of Windows' version
    /// line ("26100.2033").
    pub build_id: Option<String>,
}

/// What the host says about itself — the Baseline's `os.*` and `host.*` (CLT-what-an-agent-reports) — and the
/// time by its clock. Each fact is best effort: what the platform cannot answer is `None` and is
/// then not reported at all. The platform adapter is [`SystemHost`](crate::host::SystemHost).
pub trait HostFacts: Send + Sync {
    /// The operating system's own description of itself.
    fn os(&self) -> &OsInfo;
    /// `host.name`.
    fn host_name(&self) -> Option<&str>;
    /// `host.id` — what still names the machine after it has been renamed.
    fn host_id(&self) -> Option<&str>;
    /// `host.cpu.model.name`.
    fn cpu_model(&self) -> Option<&str>;
    /// `host.ip` and `host.mac`, excluding loopback, read live so a DHCP move is reported.
    fn addresses(&self) -> (Vec<String>, Vec<String>);
    /// The host's wall clock, in nanoseconds since the Unix epoch — what OpAMP's
    /// `*_time_unix_nano` fields carry.
    fn now_ns(&self) -> u64;
    /// The id of this process on the host.
    fn process_id(&self) -> u32;
}

/// Everything the core starts a Supervisor's adapter task with. The core's own record: its host
/// adapter reads it, and no Plugin sees it (CLT-wrappers-act-through-the-core §2).
pub struct SupervisorContext {
    /// The Supervisor's name (the TOML `name`; the Agent's `service.name`).
    pub name: String,
    /// What the Supervisor Endpoint asks the Managed Process to present (CLT-modes-and-gateway); handed to the
    /// process as [`ENDPOINT_TOKEN_ENV`](crate::supervisor::process::ENDPOINT_TOKEN_ENV). Wiped
    /// when it is dropped (SEC-secrets-wiped-on-drop).
    pub endpoint_token: zeroize::Zeroizing<String>,
    /// Everything this Supervisor owns: its state, its `program/`, its package staging
    /// (CLT-wrappers-act-through-the-core). Placed by `supervisor_dir`, so nothing may assume where it is.
    pub supervisor_dir: PathBuf,
    /// Where the received remote configuration's entry files are written — what the Managed
    /// Process is pointed at.
    pub config_dir: PathBuf,
    /// The Managed Process itself, already resolved inside this Supervisor's own `program/`
    /// directory (CLT-wrappers-act-through-the-core §27). The core starts this rather than a
    /// Plugin reading its own `binary`/`command` key, so the path rule lives in one place.
    pub program: PathBuf,
    /// What an offered package replaces (PKG-signed-packages-and-delivery) — resolved beside `program` and for
    /// the same reason: a plugin that decided this for itself could disagree with where the core
    /// put the program.
    pub install: InstallTarget,
    /// Graceful-stop budget before the Managed Process is killed.
    pub stop_timeout: Duration,
    /// How long a freshly (re)started process must survive before `ApplyConfig` is acknowledged
    /// `Ok` — the health-gated acknowledgement (CLT-wrappers-act-through-the-core). Zero acknowledges on start.
    pub apply_grace: Duration,
    /// How long the version a successful update supersedes is kept before deletion (PKG-signed-packages-and-delivery),
    /// resolved from the per-Supervisor override or the global `[updates]` default. Zero deletes on
    /// success.
    pub retain_previous: Duration,
    /// The key that opens an encrypted `.7z` package artifact (PKG-signed-packages-and-delivery); `None` when none is
    /// configured. Client-wide, like the package verification key.
    pub archive_key: Option<zeroize::Zeroizing<String>>,
    /// The process account's primary group, where a process account is registered: what the
    /// Supervisor shares with its Managed Process belongs to it (CLT-accounts-service-runs-under §21).
    pub process_account_group: Option<ProcessAccountGroup>,
    /// The plugin-specific keys of the block, for the strict second-stage parse.
    pub settings: toml::Table,
    /// Where the adapter reports events.
    pub events: EventSender,
    /// The Client's shutdown signal; the adapter stops its process and exits when it fires.
    pub shutdown: Shutdown,
}

/// What [`PluginRegistry::purge`] answers, once for each Supervisor it was asked about.
pub type Purged<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Vec<Result<(), String>>> + Send + 'a>>;

/// The plugins a run starts its Supervisors with (CLT-wrappers-act-through-the-core §14), as the composition root
/// made them: every one starts its Managed Process the way the run does (CLT-accounts-service-runs-under §18). The
/// Engine keeps it, so a Supervisor that a Supervisor-set apply starts later is started the same
/// way as those started with the Client.
pub trait PluginRegistry: Send + Sync {
    /// Every compiled-in plugin.
    fn plugins(&self) -> Vec<Box<dyn Plugin>>;

    /// Starts the Supervisor `ctx` describes on `plugin`, returning the command side of its
    /// adapter task. The core's host carries out what the Plugin asks
    /// (CLT-wrappers-act-through-the-core §4); the Plugin never sees `ctx`.
    ///
    /// # Errors
    /// Returns an error when the settings do not parse — startup fails loudly, nothing spawns.
    fn start(
        &self,
        plugin: &dyn Plugin,
        ctx: SupervisorContext,
    ) -> Result<mpsc::Sender<ProcessCommand>, String>;

    /// Removes what the Managed Processes wrote in each of `removed` — a Supervisor's name and its
    /// directory — that the Client cannot remove itself, before the purge deletes the rest
    /// (PKG-versions-high-water-mark-floor §13, CLT-accounts-service-runs-under §20), all of them within one wait (CLT-accounts-service-runs-under §17).
    /// Nothing where the Client starts its programs itself.
    ///
    /// # Errors
    /// Each answer, in the order of `removed`, is why something is left: the purge then leaves an
    /// orphan, with a warning (CLT-server-manages-the-supervisor-set §9).
    fn purge<'a>(&'a self, removed: &'a [(String, std::path::PathBuf)]) -> Purged<'a> {
        Box::pin(std::future::ready(removed.iter().map(|_| Ok(())).collect()))
    }
}
