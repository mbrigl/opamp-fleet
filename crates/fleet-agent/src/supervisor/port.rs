//! The Plugin port of CLT-wrappers-act-through-the-core: what a Supervisor Plugin is, as plain
//! data and asynchronous calls, and nothing of the host it runs on.
//!
//! A Plugin states a [`Manifest`], checks a block's settings, and makes an [`Instance`] per
//! Supervisor. The instance is an event handler: the core feeds it one [`Input`] at a time and the
//! instance acts only through the [`Host`] it is handed (CLT-wrappers-act-through-the-core §1). It
//! performs no I/O of its own and names files by [`Root`] and relative path (§3).
//!
//! Every type here is plain data, so that a WebAssembly component can implement the port later
//! (§2, §12): no host path, no handle, no channel, no span and no secret. The futures are boxed
//! by hand, as the trait objects of the core need them, without a macro the core does not use.
//! `the_port_carries_no_host_resource` in `crates/fleet-core/tests/dependency_direction.rs` holds
//! the rule.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

/// A future a [`Host`] function or an [`Instance`] handler returns.
pub type Call<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

/// The directories a Plugin may name a file in (CLT-wrappers-act-through-the-core §3). The core
/// resolves each to the Supervisor's own directory; a Plugin never learns where that is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Root {
    /// The Supervisor's `config/` directory, where the written configuration entries lie.
    Config,
    /// The directory the live program lives in: the program's own directory for a single file,
    /// the tree's root for a tree.
    Program,
    /// The Supervisor's `data/` directory, which the Managed Process writes into, shared with the
    /// process account's group.
    Data,
    /// The Supervisor's `state/` directory: the Plugin's own, which the Client alone reads and
    /// writes.
    State,
}

impl Root {
    /// Whether a Plugin may write in this root (CLT-wrappers-act-through-the-core §3).
    #[must_use]
    pub fn writable(self) -> bool {
        matches!(self, Root::Data | Root::State)
    }
}

/// A file named by its root and a path relative to it, `/`-separated, with no `..`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootPath {
    pub root: Root,
    pub path: String,
}

/// One argument, environment value or working directory of a process specification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// Passed as written.
    Text(String),
    /// Written by the operator: the core expands the placeholders naming the Supervisor's own
    /// directories in it, `${supervisor_dir}` and `${config_dir}`, and passes everything else as
    /// written (CLT-wrappers-act-through-the-core §31).
    Template(String),
    /// A file, which the core expands to its absolute path when it starts the program.
    Rooted(RootPath),
}

/// How the Managed Process is started: its program is the one the core resolved for the
/// Supervisor (CLT-wrappers-act-through-the-core §27), started with these arguments and this
/// environment.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Spec {
    pub args: Vec<Value>,
    pub env: Vec<(String, Value)>,
    /// `None` starts the program in the directory it lives in (CLT-wrappers-act-through-the-core §6).
    pub working_dir: Option<RootPath>,
}

/// A written configuration entry, as the core hands it to an instance
/// (CLT-wrappers-act-through-the-core §10). The file lies in the [`Root::Config`] root under
/// `name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// The role the configuration gave it; `None` for an entry that is configuration itself
    /// rather than supplementary content (SRV-configurations-compose-one-map).
    pub role: Option<String>,
    /// The SHA-256 of the entry's content.
    pub digest: Vec<u8>,
}

/// What an entry of a listed directory is itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A regular file.
    Regular,
    Directory,
    /// A link, which no host function follows.
    Link,
    Other,
}

/// A program a Plugin runs once from its [`Root::Program`] root, under the same account and
/// through the same path as the Managed Process (CLT-wrappers-act-through-the-core §4).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Run {
    /// The program's path in the `program` root.
    pub program: String,
    pub args: Vec<Value>,
    pub env: Vec<(String, Value)>,
    /// `None` runs it in the directory the Managed Process starts in.
    pub working_dir: Option<RootPath>,
}

/// How a program run once ended.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Ran {
    /// The exit code; `None` where a signal ended the program.
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// The host facts a Plugin may ask for (CLT-wrappers-act-through-the-core §4).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Facts {
    /// The host's fully qualified name, or its plain name where the host knows no domain.
    pub host_name: Option<String>,
    /// The account the Supervisor's programs run under.
    pub account: Option<String>,
}

/// What a kind states about itself, the same for every block of it
/// (CLT-wrappers-act-through-the-core §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Manifest {
    /// The TOML `type` value this Plugin serves.
    pub kind: &'static str,
    /// The block key naming the program, where the block names it (`collector` and `command`).
    pub program_key: &'static str,
    /// What the kind knows about its agent's artifact, name and timing.
    pub defaults: KindDefaults,
    /// The signal its program re-reads its configuration on; `None` applies by restart (§7).
    pub reload_signal: Option<i32>,
    /// Whether its Managed Process leads its own process group and is stopped as a group (§32).
    pub own_process_group: bool,
    /// The directories its agent writes into, relative to the [`Root::Data`] root: the core
    /// creates them before every spawn (§6).
    pub data_dirs: &'static [&'static str],
}

impl Manifest {
    /// The manifest of a kind that knows nothing beyond its `type` and program key.
    #[must_use]
    pub const fn of(kind: &'static str, program_key: &'static str) -> Self {
        Manifest {
            kind,
            program_key,
            defaults: KindDefaults::none(),
            reload_signal: None,
            own_process_group: false,
            data_dirs: &[],
        }
    }
}

/// How the program is asked for its version, and how a staged package is proved before the swap
/// (CLT-wrappers-act-through-the-core §33 and §34). A path in the [`Root::Program`] root names the
/// live program for the version probe and the staged one for the preflight.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Probe {
    pub args: Vec<Value>,
    pub env: Vec<(String, Value)>,
    /// `None` starts it where the probe's caller started, inheriting.
    pub working_dir: Option<RootPath>,
}

/// The reading of a version out of what the probe printed: `None` where it holds none
/// (CLT-wrappers-act-through-the-core §11). A function of the text alone, so the core calls it
/// while the instance handles an input.
pub type ReadVersion = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// What one Supervisor's instance states about running its program, read once when it starts.
#[derive(Default)]
pub struct Process {
    /// The version probe, which also proves a staged package before the swap. `None`: no probe,
    /// no preflight.
    pub probe: Option<Probe>,
    /// How the version is read out of the probe's output; `None` reads strict Semantic
    /// Versioning.
    pub read_version: Option<ReadVersion>,
}

/// A Managed Process's health as a Plugin reports it (CLT-wrappers-act-through-the-core §4).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Health {
    pub healthy: bool,
    /// What the process is doing, in a few words.
    pub status: String,
    /// Why it is not healthy; empty where it is.
    pub last_error: String,
}

/// What happened, as the core feeds it to an instance, one at a time
/// (CLT-wrappers-act-through-the-core §1 and §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// The Supervisor starts, on the configuration entries already written: the instance starts
    /// its Managed Process, where it has something to run on.
    Start { entries: Vec<Entry> },
    /// A remote configuration was received and its entries written: apply it.
    ApplyConfig { hash: Vec<u8>, entries: Vec<Entry> },
    /// A verified package waits to be installed.
    ApplyPackage { version: String, hash: Vec<u8> },
    /// The Server commanded a restart on the current files.
    Restart,
    /// The Supervisor is retired for good.
    Uninstall,
    /// The timer the instance set under `id` fired.
    Timer { id: u64 },
}

/// The host functions an instance acts through (CLT-wrappers-act-through-the-core §4). Each
/// answers with its outcome or with the reason the core refused it.
pub trait Host: Send + Sync {
    /// Starts the Managed Process on `spec`, or restarts it on `spec` where it runs.
    fn start(&self, spec: Spec) -> Call<'_, ()>;
    /// Applies the configuration of `hash` by restart or reload on `spec`, health-gated, and
    /// answers it. `None`: the new configuration leaves nothing to run, and the Supervisor awaits
    /// one.
    fn apply_config(&self, hash: Vec<u8>, spec: Option<Spec>) -> Call<'_, ()>;
    /// Installs the package of `hash` by the core's swap, health-gated, and answers it (§8).
    fn install(&self, hash: Vec<u8>) -> Call<'_, ()>;
    /// Restarts the Managed Process on the current files.
    fn restart(&self) -> Call<'_, ()>;
    /// Stops the Managed Process for good and answers the uninstall (§9).
    fn uninstall(&self) -> Call<'_, ()>;

    /// The content of `file`.
    fn read(&self, _file: RootPath) -> Call<'_, Vec<u8>> {
        refused("read")
    }
    /// Writes `contents` as `file`, whole, in a root a Plugin may write.
    fn write(&self, _file: RootPath, _contents: Vec<u8>) -> Call<'_, ()> {
        refused("write")
    }
    /// Renames `from` to `to`, both in one directory of a root a Plugin may write.
    fn rename(&self, _from: RootPath, _to: RootPath) -> Call<'_, ()> {
        refused("rename")
    }
    /// Removes the file or the empty directory `file`, in a root a Plugin may write.
    fn remove(&self, _file: RootPath) -> Call<'_, ()> {
        refused("remove")
    }
    /// The entries of the directory `dir`, by name.
    fn list(&self, _dir: RootPath) -> Call<'_, Vec<(String, EntryKind)>> {
        refused("list")
    }
    /// Creates the directory `dir` and what is missing above it, in a root a Plugin may write.
    fn create_dir(&self, _dir: RootPath) -> Call<'_, ()> {
        refused("create_dir")
    }
    /// Runs a program once and answers how it ended.
    fn run(&self, _run: Run) -> Call<'_, Ran> {
        refused("run")
    }
    /// Sets the timer `id` to fire once after `after_ms` milliseconds, as an [`Input::Timer`];
    /// a timer set again under the same `id` replaces it.
    fn set_timer(&self, _id: u64, _after_ms: u64) -> Call<'_, ()> {
        refused("set_timer")
    }
    /// Cancels the timer `id`: it never arrives, even where it fired already.
    fn cancel_timer(&self, _id: u64) -> Call<'_, ()> {
        refused("cancel_timer")
    }
    /// The host facts.
    fn facts(&self) -> Call<'_, Facts> {
        refused("facts")
    }
    /// Reports the Managed Process's health, as the Supervisor's Agent then reports it.
    fn report_health(&self, _health: Health) -> Call<'_, ()> {
        refused("report_health")
    }
}

/// The answer of a host that does not carry out `function`.
fn refused<'a, T: Send + 'a>(function: &'static str) -> Call<'a, T> {
    Box::pin(async move { Err(format!("the host carries out no `{function}`")) })
}

/// A Supervisor Plugin: the kind, as compiled into the Client (CLT-wrappers-act-through-the-core §1).
pub trait Plugin: Send + Sync {
    /// What the kind states about itself.
    fn manifest(&self) -> Manifest;

    /// The TOML `type` value this Plugin serves.
    fn kind(&self) -> &'static str {
        self.manifest().kind
    }

    /// What the kind knows about its agent.
    fn defaults(&self) -> KindDefaults {
        self.manifest().defaults
    }

    /// The block key naming the program.
    fn program_key(&self) -> &'static str {
        self.manifest().program_key
    }

    /// The strict parse of a block's settings, without side effects. `settings` is the block's
    /// table with the program key already taken out.
    ///
    /// # Errors
    /// Returns an error when the settings do not parse.
    fn check(&self, name: &str, settings: toml::Table) -> Result<(), String>;

    /// What a Server-delivered block of this kind may not say beyond the generic rule on `env`
    /// and `args` (CLT-server-manages-the-supervisor-set §11). By default a kind names no file.
    ///
    /// # Errors
    /// Returns the reason the delivered block is refused.
    fn check_delivered(
        &self,
        _settings: &toml::Table,
        _running: Option<&toml::Table>,
    ) -> Result<(), String> {
        Ok(())
    }

    /// The instance serving the Supervisor `name`, on its block's settings.
    ///
    /// # Errors
    /// Returns an error when the settings do not parse.
    fn instantiate(&self, name: &str, settings: toml::Table) -> Result<Box<dyn Instance>, String>;
}

/// One Supervisor's Plugin instance: the event handler (CLT-wrappers-act-through-the-core §1).
pub trait Instance: Send {
    /// How the program is run, read once before the first input.
    fn process(&self) -> Process;

    /// The process specification the default handling runs the Managed Process on, given the
    /// written configuration entries. `None`: nothing to run on yet.
    fn spec(&self, entries: &[Entry]) -> Option<Spec>;

    /// Handles one input through `host`. The default answers every input with the host
    /// function of the same name, the start and the apply on [`spec`](Self::spec), which is all a
    /// kind needs that only states how its program runs (CLT-wrappers-act-through-the-core §5).
    fn handle<'a>(&'a mut self, input: Input, host: &'a dyn Host) -> Call<'a, ()> {
        let spec = match &input {
            Input::Start { entries } | Input::ApplyConfig { entries, .. } => self.spec(entries),
            _ => None,
        };
        Box::pin(default_handling(input, spec, host))
    }
}

/// The host function each input asks for by default. A start without a `spec` starts nothing.
///
/// # Errors
/// Returns what the host function returns.
pub async fn default_handling(
    input: Input,
    spec: Option<Spec>,
    host: &dyn Host,
) -> Result<(), String> {
    match input {
        Input::Start { .. } => match spec {
            Some(spec) => host.start(spec).await,
            None => Ok(()),
        },
        Input::ApplyConfig { hash, .. } => host.apply_config(hash, spec).await,
        Input::ApplyPackage { hash, .. } => host.install(hash).await,
        Input::Restart => host.restart().await,
        Input::Uninstall => host.uninstall().await,
        Input::Timer { .. } => Ok(()),
    }
}

/// A host in memory that records each call, for the table tests of the kinds.
#[cfg(test)]
pub(crate) mod recorded {
    use super::{Call, Host, Spec};
    use std::sync::Mutex;

    #[derive(Default)]
    pub(crate) struct Recorded(pub(crate) Mutex<Vec<String>>);

    impl Recorded {
        fn push(&self, call: String) -> Call<'_, ()> {
            self.0.lock().expect("calls").push(call);
            Box::pin(async { Ok(()) })
        }

        pub(crate) fn calls(&self) -> Vec<String> {
            self.0.lock().expect("calls").clone()
        }
    }

    impl Host for Recorded {
        fn start(&self, spec: Spec) -> Call<'_, ()> {
            self.push(format!("start {spec:?}"))
        }
        fn apply_config(&self, hash: Vec<u8>, spec: Option<Spec>) -> Call<'_, ()> {
            self.push(format!("apply_config {hash:?} {spec:?}"))
        }
        fn install(&self, hash: Vec<u8>) -> Call<'_, ()> {
            self.push(format!("install {hash:?}"))
        }
        fn restart(&self) -> Call<'_, ()> {
            self.push("restart".to_string())
        }
        fn uninstall(&self) -> Call<'_, ()> {
            self.push("uninstall".to_string())
        }
    }
}

/// What a kind knows about its own agent, so a block does not have to say it (CLT-wrappers-act-through-the-core).
///
/// Every field is a `&'static str` resolved **per platform** at compile time, which is the point:
/// a constant can be asserted against the artifact this project packs, where a path written in a
/// manual can only be believed. A kind that knows nothing — `collector`, `command` — returns
/// [`KindDefaults::none`] and its blocks keep saying what they always said.
///
/// A value supplied here is not a default the block may override. It is the answer, and a block
/// naming the key is refused: two sources of truth for a value this Client computes is how a host
/// quietly differs from what the fleet believes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindDefaults {
    /// The program's file name in this Supervisor's own `program/` directory (CLT-wrappers-act-through-the-core).
    /// `None` leaves it to the block's program key.
    pub program: Option<&'static str>,
    /// Where the program sits inside an unpacked package tree (PKG-signed-packages-and-delivery); `None` is a single-file
    /// package, or a kind that leaves the question to the block.
    pub program_path: Option<&'static str>,
    /// Where a single-file package's archive holds the program, as trailing path components
    /// (`usr/bin/agent`), for an archive that carries more than one file of the program's name.
    /// `None` finds the member by its file name alone (PKG-signed-packages-and-delivery §55).
    pub member: Option<&'static str>,
    /// The absolute links a tree package of this kind keeps as links, each by its exact path in
    /// the tree and its exact target: the only links the tree's install creates, and the only
    /// links leading out that `opamp-fleetctl` packs
    /// (PKG-signed-packages-and-delivery §57). Empty for nearly every kind.
    pub tree_links: &'static [fleet_util::tree::KeptLink],
    /// The Agent *type* this kind presents (CLT-what-an-agent-reports). `None` falls back to the program's file
    /// name, which is what the block already said.
    pub service_name: Option<&'static str>,
    /// What this kind corrects about the fleet's timing policy, and whether its block may state
    /// any of it at all (CLT-wrappers-act-through-the-core). `None` — an unwrapped kind — leaves the three keys in the
    /// block, because no kind exists there to hold a value; `Some` takes them out of the block and
    /// states the kind's own corrections, each `None` meaning "the fleet's number is right".
    pub timing: Option<KindTiming>,
    /// Whether a block of this kind may pin the Supervisor Endpoint's port. The Endpoint is bound
    /// for every Supervisor (CLT-modes-and-gateway); pinning it only means something where a Managed Process
    /// connects to it, which in practice is a Collector carrying the `opampextension`.
    pub endpoint_port: bool,
}

/// What a wrapped kind says about timing, over the fleet's own policy (CLT-wrappers-act-through-the-core).
///
/// Every field is an *agent's* property rather than a host's: how long it needs to shut down, how
/// long a restart of it has to hold before the fleet may believe it, how long its superseded
/// version is worth keeping. A kind that has nothing to correct states three `None`s and still
/// takes the keys out of its block — the value is the fleet's, and there is no host-level answer
/// to a question about an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KindTiming {
    /// Overrides `[supervisors] stop_timeout_secs`.
    pub stop_timeout: Option<Duration>,
    /// Overrides `[supervisors] apply_grace_secs`.
    pub apply_grace: Option<Duration>,
    /// Overrides `[updates] retain_previous_secs` (PKG-signed-packages-and-delivery).
    pub retain_previous: Option<Duration>,
}

impl KindDefaults {
    /// A kind that knows nothing about its agent: the block says everything, as it always has.
    #[must_use]
    pub const fn none() -> Self {
        KindDefaults {
            program: None,
            program_path: None,
            member: None,
            tree_links: &[],
            service_name: None,
            timing: None,
            endpoint_port: false,
        }
    }
}

/// The strict second-stage parse of a block's plugin settings, shared by every plugin's
/// instance and its [`check`](Plugin::check) so the two cannot disagree (CLT-wrappers-act-through-the-core).
///
/// A key the kind has `retired` is refused by name first, with what answers it now — a block
/// carrying one was written against a Client that needed it, and the operator deleting the line
/// deserves to be told where the value went rather than meet serde's "unknown field".
///
/// # Errors
/// Returns an error naming the block when a retired key is present or the settings do not parse.
pub fn parse_settings<T: serde::de::DeserializeOwned>(
    name: &str,
    kind: &str,
    retired: &[(&str, &str)],
    settings: toml::Table,
) -> Result<T, String> {
    for (key, answer) in retired {
        if settings.contains_key(*key) {
            return Err(format!(
                "supervisor {name:?}: `{key}` is no longer a supervisor key for type {kind:?} \
                 — {answer}; remove the line"
            ));
        }
    }
    settings
        .try_into()
        .map_err(|e| format!("supervisor {name:?}: {e}"))
}
