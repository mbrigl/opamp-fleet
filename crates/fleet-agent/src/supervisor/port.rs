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
//! `the_port_carries_no_host_resource` holds the rule.

use std::future::Future;
use std::pin::Pin;

use super::ports::KindDefaults;

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
    /// The process leads its own process group and is stopped as a group (§32).
    pub own_process_group: bool,
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
}

/// What one Supervisor's instance states about running its program, read once when it starts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Process {
    /// The arguments that make the program print its version, which also prove a staged
    /// package before the swap (CLT-wrappers-act-through-the-core §33). `None`: no probe, no
    /// preflight.
    pub version_args: Option<Vec<String>>,
    /// The signal the program re-reads its configuration on; `None` applies by restart (§7).
    pub reload_signal: Option<i32>,
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
}

/// A Supervisor Plugin: the kind, as compiled into the Client (CLT-wrappers-act-through-the-core §1).
pub trait Plugin: Send + Sync {
    /// What the kind states about itself.
    fn manifest(&self) -> Manifest;

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

#[cfg(test)]
mod tests {
    /// The names a port type must not hold: a host path, a file or other handle, a channel, a
    /// task, a span or a secret (CLT-wrappers-act-through-the-core §2).
    const HOST_RESOURCES: &[&str] = &[
        "PathBuf",
        "Path",
        "OsString",
        "OsStr",
        "File",
        "TcpStream",
        "Command",
        "Child",
        "mpsc",
        "oneshot",
        "watch",
        "Sender",
        "Receiver",
        "JoinHandle",
        "Span",
        "Instant",
        "SystemTime",
        "Zeroizing",
        "Shutdown",
        "Arc",
        "Mutex",
    ];

    /// The identifiers the shipped part of `source` names, with comments, strings and the tests
    /// cut away.
    fn identifiers(source: &str) -> Vec<String> {
        let shipped = source.split("#[cfg(test)]").next().unwrap_or_default();
        let mut names = Vec::new();
        for line in shipped.lines() {
            let code = line.split("//").next().unwrap_or_default();
            let mut quoted = false;
            let mut name = String::new();
            for c in code.chars() {
                if c == '"' {
                    quoted = !quoted;
                }
                if !quoted && (c.is_alphanumeric() || c == '_') {
                    name.push(c);
                } else if !name.is_empty() {
                    names.push(std::mem::take(&mut name));
                }
            }
            if !name.is_empty() {
                names.push(name);
            }
        }
        names
    }

    /// The port is plain data: no type of it names a host resource, so a WebAssembly component
    /// can implement it.
    /// Verifies: CLT-wrappers-act-through-the-core
    #[test]
    fn the_port_carries_no_host_resource() {
        let names = identifiers(include_str!("port.rs"));
        let found: Vec<&String> = names
            .iter()
            .filter(|n| HOST_RESOURCES.contains(&n.as_str()))
            .collect();
        assert!(found.is_empty(), "the port names host resources: {found:?}");
    }

    /// The reading above finds what it looks for, so its silence on the port means something.
    #[test]
    fn the_reading_finds_a_host_resource() {
        let names = identifiers("pub struct S { p: std::path::PathBuf } // Span\n");
        assert!(names.iter().any(|n| n == "PathBuf"));
        assert!(!names.iter().any(|n| n == "Span"), "a comment is no code");
    }
}
