//! The core's side of the Plugin port (CLT-wrappers-act-through-the-core §1, §4 and §5): it feeds
//! a Plugin instance its inputs, one at a time, and carries out the host functions the instance
//! calls over the [`Runner`], which keeps the whole lifecycle of the Managed Process.
//!
//! [`start`] is how the core starts a Supervisor on its Plugin; the Plugin sees only the port.

mod files;
mod timers;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::shutdown::Shutdown;
use crate::supervisor::port::{
    self, Call, Entry, EntryKind, Facts, Health, Host, Input, Instance, Probe, Ran, Root, RootPath,
    Run, Spec, Value,
};
use crate::supervisor::ports::{EventSender, ProcessCommand, ProcessEvent, SupervisorContext};
use crate::supervisor::process::{
    Launch, Preflight, ProcessSpec, Runner, Spawner, VersionProbe, STAGED,
};

use files::Roots;
use timers::{Fired, Timers};

pub use files::{DATA_DIR, READ_LIMIT, STATE_DIR};

/// Starts the Supervisor `ctx` describes on `plugin`: makes its instance, the `Runner` of its
/// Managed Process with programs started through `spawner` (CLT-accounts-service-runs-under §18),
/// and the tasks that feed the instance its inputs and carry out its host functions.
///
/// # Errors
/// Returns an error when the block's settings do not parse, or the kind names a path that would
/// leave its root.
pub fn start(
    plugin: &dyn port::Plugin,
    spawner: &Spawner,
    mut ctx: SupervisorContext,
) -> Result<mpsc::Sender<ProcessCommand>, String> {
    let manifest = plugin.manifest();
    let instance = plugin.instantiate(&ctx.name, std::mem::take(&mut ctx.settings))?;
    let process = instance.process();
    let roots = Roots::of(&ctx);
    let ensure_dirs = data_dirs(&roots, manifest.data_dirs)?;
    let (version_probe, preflight) = match &process.probe {
        Some(probe) => {
            let (args, env, working_dir) = probe_parts(&roots, probe)?;
            let staged = roots.with_program(PathBuf::from(STAGED));
            let (pre_args, pre_env, pre_dir) = probe_parts(&staged, probe)?;
            (
                Some(VersionProbe {
                    program: ctx.program.clone(),
                    args,
                    working_dir,
                    parse: process.read_version.map(Arc::from),
                    env,
                }),
                Some(Preflight {
                    args: pre_args,
                    env: pre_env,
                    working_dir: pre_dir.map(|dir| dir.to_string_lossy().into_owned()),
                }),
            )
        }
        None => (None, None),
    };
    let spec = Arc::new(Mutex::new(None::<ProcessSpec>));
    let (to_runner, runner_commands) = mpsc::channel(16);
    let built = Arc::clone(&spec);
    let runner = Runner {
        endpoint_token: ctx.endpoint_token.clone(),
        name: ctx.name.clone(),
        stop_timeout: ctx.stop_timeout,
        apply_grace: ctx.apply_grace,
        retain_previous: ctx.retain_previous,
        install: Some(ctx.install),
        archive_key: ctx.archive_key,
        version_probe,
        preflight,
        reload_signal: manifest.reload_signal,
        spawner: spawner.clone(),
        process_account_group: ctx.process_account_group,
        supervisor_dir: ctx.supervisor_dir,
        events: ctx.events.clone(),
        commands: runner_commands,
        build: Box::new(move || built.lock().ok()?.clone()),
    };
    let (timers, fired) = Timers::new();
    let config_dir = roots.config.clone();
    let host = Arc::new(CoreHost {
        name: ctx.name,
        roots,
        program: ctx.program,
        own_process_group: manifest.own_process_group,
        ensure_dirs,
        events: ctx.events,
        uninstalled: AtomicBool::new(false),
        spec,
        runner: to_runner,
        running: AtomicBool::new(false),
        configs: Mutex::new(HashMap::new()),
        packages: Mutex::new(HashMap::new()),
        spawner: spawner.clone(),
        timers: Arc::clone(&timers),
        account: tokio::sync::OnceCell::new(),
    });
    let (commands, command_rx) = mpsc::channel(16);
    let (to_instance, inputs) = mpsc::channel(16);
    tokio::spawn(front(command_rx, Arc::clone(&host), to_instance));
    let runner_shutdown = ctx.shutdown.clone();
    let started = Arc::clone(&host);
    tokio::spawn(drive(
        instance,
        host,
        move || {
            started.running.store(true, Ordering::SeqCst);
            tokio::spawn(runner.run(runner_shutdown));
        },
        Fed {
            config: config_dir,
            inputs,
            timers,
            fired,
        },
        ctx.shutdown,
    ));
    Ok(commands)
}

/// The directories a kind's agent writes into, beneath the `data` root
/// (CLT-wrappers-act-through-the-core §6). The `Runner` makes each with every directory on the way.
fn data_dirs(roots: &Roots, named: &[&str]) -> Result<Vec<PathBuf>, String> {
    named
        .iter()
        .map(|dir| {
            roots.path(&RootPath {
                root: Root::Data,
                path: (*dir).to_string(),
            })
        })
        .collect()
}

/// A probe's arguments, environment and working directory, as the `Runner` takes them.
type ProbeParts = (Vec<String>, Vec<(String, String)>, Option<PathBuf>);

/// A probe's arguments, environment and working directory, the paths in the `program` root
/// expanded in `roots`.
fn probe_parts(roots: &Roots, probe: &Probe) -> Result<ProbeParts, String> {
    let args = values(roots, &probe.args)?;
    let env = environment(roots, &probe.env)?;
    let working_dir = probe
        .working_dir
        .as_ref()
        .map(|dir| roots.path(dir))
        .transpose()?;
    Ok((args, env, working_dir))
}

fn values(roots: &Roots, values: &[Value]) -> Result<Vec<String>, String> {
    values.iter().map(|v| roots.value(v)).collect()
}

fn environment(roots: &Roots, env: &[(String, Value)]) -> Result<Vec<(String, String)>, String> {
    env.iter()
        .map(|(k, v)| Ok((k.clone(), roots.value(v)?)))
        .collect()
}

/// The configuration entries written in `config_dir`, read off the runtime's threads.
async fn entries_of(config_dir: PathBuf) -> Vec<Entry> {
    tokio::task::spawn_blocking(move || entries(&config_dir))
        .await
        .unwrap_or_default()
}

/// The configuration entries written in `config_dir`, by name, each with its role and digest
/// (CLT-wrappers-act-through-the-core §10). An unreadable directory holds none.
fn entries(config_dir: &Path) -> Vec<Entry> {
    let roles = crate::storage::entry_roles(config_dir);
    let configuration = crate::storage::config_entries(config_dir)
        .into_iter()
        .filter_map(|path| Some((path.file_name()?.to_string_lossy().into_owned(), None)));
    let supplementary = roles
        .into_iter()
        .filter(|(name, _)| {
            std::fs::symlink_metadata(config_dir.join(name)).is_ok_and(|m| m.is_file())
        })
        .map(|(name, role)| (name, Some(role)));
    let mut entries: Vec<Entry> = configuration
        .chain(supplementary)
        .map(|(name, role)| Entry {
            digest: std::fs::read(config_dir.join(&name))
                .map(|content| {
                    ring::digest::digest(&ring::digest::SHA256, &content)
                        .as_ref()
                        .to_vec()
                })
                .unwrap_or_default(),
            name,
            role,
        })
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

/// The host functions of one Supervisor, carried out over its [`Runner`].
struct CoreHost {
    name: String,
    roots: Roots,
    program: PathBuf,
    /// The kind leads its own process group (CLT-wrappers-act-through-the-core §32).
    own_process_group: bool,
    /// What the core creates before every spawn (§6).
    ensure_dirs: Vec<PathBuf>,
    /// Where a reported health and the answers the core gives itself go.
    events: EventSender,
    /// Whether the instance asked for the uninstall, so that the core stops the process itself
    /// where it did not (§9).
    uninstalled: AtomicBool,
    /// What the `Runner` starts the Managed Process on; `None` until the instance starts it.
    spec: Arc<Mutex<Option<ProcessSpec>>>,
    runner: mpsc::Sender<ProcessCommand>,
    /// Whether the `Runner` runs, so that a later start restarts the process on its new spec.
    running: AtomicBool,
    /// The commands an input stands for, by hash, until the instance asks for them: the
    /// configuration and the span of an apply, and the staged file of a package stay in the core
    /// (CLT-wrappers-act-through-the-core §2).
    configs: Mutex<HashMap<Vec<u8>, ProcessCommand>>,
    packages: Mutex<HashMap<Vec<u8>, ProcessCommand>>,
    /// What starts a program the instance runs once: the same as starts the Managed Process
    /// (CLT-accounts-service-runs-under §18).
    spawner: Spawner,
    timers: Arc<Timers>,
    /// The account the Supervisor's programs run under, asked once.
    account: tokio::sync::OnceCell<Option<String>>,
}

impl CoreHost {
    fn resolve(&self, spec: &Spec) -> Result<ProcessSpec, String> {
        let args = values(&self.roots, &spec.args)?;
        let env = environment(&self.roots, &spec.env)?;
        let working_dir = spec
            .working_dir
            .as_ref()
            .map(|d| self.roots.path(d))
            .transpose()?;
        debug!(
            supervisor = %self.name,
            program = %self.program.display(),
            args = ?args,
            env = ?env.iter().map(|(key, _)| key.as_str()).collect::<Vec<_>>(),
            "managed process invocation"
        );
        Ok(ProcessSpec {
            program: self.program.clone(),
            args,
            env,
            working_dir,
            own_process_group: self.own_process_group,
            ensure_dirs: self.ensure_dirs.clone(),
        })
    }

    /// What the `Runner` starts the Managed Process on from its next start on.
    fn set(&self, spec: Option<ProcessSpec>) -> Result<(), String> {
        *self
            .spec
            .lock()
            .map_err(|_| "the process specification is lost".to_string())? = spec;
        Ok(())
    }

    /// Carries out a file operation off the runtime's threads.
    async fn files<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&Roots) -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let roots = self.roots.clone();
        tokio::task::spawn_blocking(move || operation(&roots))
            .await
            .map_err(|_| "the file operation was cut short".to_string())?
    }

    /// Runs `launch` once through the spawner and answers how it ended.
    async fn run_once(&self, launch: Launch, what: &str) -> Result<Ran, String> {
        match self.spawner.output(&self.name, &launch).await {
            Some(Ok(output)) => Ok(Ran {
                status: output.status.code(),
                stdout: output.stdout,
                stderr: output.stderr,
            }),
            Some(Err(e)) => Err(format!("cannot run {what}: {}", e.kind())),
            None => Err(format!("{what} did not end in time")),
        }
    }

    /// The account a program of this Supervisor runs under, as that program itself states it.
    async fn ask_account(&self) -> Option<String> {
        #[cfg(unix)]
        let (program, args) = (PathBuf::from("/usr/bin/id"), vec!["-un".to_string()]);
        #[cfg(windows)]
        let (program, args) = (
            PathBuf::from(std::env::var_os("SystemRoot")?).join(r"System32\whoami.exe"),
            Vec::new(),
        );
        let launch = Launch {
            program,
            args,
            env: Vec::new(),
            endpoint_token: None,
            working_dir: None,
            own_process_group: false,
        };
        let ran = self.run_once(launch, "the account query").await.ok()?;
        let name = String::from_utf8_lossy(&ran.stdout).trim().to_string();
        (ran.status == Some(0) && !name.is_empty()).then_some(name)
    }

    async fn send(&self, command: ProcessCommand) -> Result<(), String> {
        self.runner
            .send(command)
            .await
            .map_err(|_| "the process runner is gone".to_string())
    }

    fn take(
        pending: &Mutex<HashMap<Vec<u8>, ProcessCommand>>,
        hash: &[u8],
        what: &str,
    ) -> Result<ProcessCommand, String> {
        pending
            .lock()
            .map_err(|_| format!("the pending {what} are lost"))?
            .remove(hash)
            .ok_or_else(|| format!("no {what} of hash {} waits", hex::encode(hash)))
    }
}

impl Host for CoreHost {
    fn start(&self, spec: Spec) -> Call<'_, ()> {
        Box::pin(async move {
            self.set(Some(self.resolve(&spec)?))?;
            if self.running.load(Ordering::SeqCst) {
                self.send(ProcessCommand::Restart).await?;
            }
            Ok(())
        })
    }

    /// A specification the core refuses leaves the apply waiting, and the core answers it with the
    /// reason once the handler has returned ([`Settles::settle`]).
    fn apply_config(&self, hash: Vec<u8>, spec: Option<Spec>) -> Call<'_, ()> {
        Box::pin(async move {
            let resolved = spec.as_ref().map(|spec| self.resolve(spec)).transpose()?;
            let command = Self::take(&self.configs, &hash, "configuration")?;
            self.set(resolved)?;
            self.send(command).await
        })
    }

    fn install(&self, hash: Vec<u8>) -> Call<'_, ()> {
        Box::pin(async move {
            let command = Self::take(&self.packages, &hash, "package")?;
            self.send(command).await
        })
    }

    fn restart(&self) -> Call<'_, ()> {
        Box::pin(self.send(ProcessCommand::Restart))
    }

    fn uninstall(&self) -> Call<'_, ()> {
        self.uninstalled.store(true, Ordering::SeqCst);
        Box::pin(self.send(ProcessCommand::Uninstall))
    }

    fn read(&self, file: RootPath) -> Call<'_, Vec<u8>> {
        Box::pin(self.files(move |roots| roots.read(&file)))
    }

    fn write(&self, file: RootPath, contents: Vec<u8>) -> Call<'_, ()> {
        Box::pin(self.files(move |roots| roots.write(&file, &contents)))
    }

    fn rename(&self, from: RootPath, to: RootPath) -> Call<'_, ()> {
        Box::pin(self.files(move |roots| roots.rename(&from, &to)))
    }

    fn remove(&self, file: RootPath) -> Call<'_, ()> {
        Box::pin(self.files(move |roots| roots.remove(&file)))
    }

    fn list(&self, dir: RootPath) -> Call<'_, Vec<(String, EntryKind)>> {
        Box::pin(self.files(move |roots| roots.list(&dir)))
    }

    fn create_dir(&self, dir: RootPath) -> Call<'_, ()> {
        Box::pin(self.files(move |roots| roots.create_dir(&dir)))
    }

    /// The program from the `program` root, through the spawner the Managed Process starts
    /// through: the second service where a process account is registered
    /// (CLT-accounts-service-runs-under §18). It is handed no Endpoint token.
    fn run(&self, run: Run) -> Call<'_, Ran> {
        Box::pin(async move {
            let program = self.roots.path(&RootPath {
                root: Root::Program,
                path: run.program.clone(),
            })?;
            let args = values(&self.roots, &run.args)?;
            let env = environment(&self.roots, &run.env)?;
            let working_dir = match &run.working_dir {
                Some(dir) => self.roots.path(dir)?,
                None => self.roots.program.clone(),
            };
            let launch = Launch {
                program,
                args,
                env,
                endpoint_token: None,
                working_dir: Some(working_dir),
                own_process_group: false,
            };
            self.run_once(launch, &format!("program:{}", run.program))
                .await
        })
    }

    fn set_timer(&self, id: u64, after_ms: u64) -> Call<'_, ()> {
        self.timers.set(id, Duration::from_millis(after_ms));
        Box::pin(async { Ok(()) })
    }

    fn cancel_timer(&self, id: u64) -> Call<'_, ()> {
        self.timers.cancel(id);
        Box::pin(async { Ok(()) })
    }

    fn facts(&self) -> Call<'_, Facts> {
        Box::pin(async move {
            let host_name = tokio::task::spawn_blocking(crate::host::qualified_host_name)
                .await
                .ok()
                .flatten();
            let account = self
                .account
                .get_or_init(|| self.ask_account())
                .await
                .clone();
            Ok(Facts { host_name, account })
        })
    }

    fn report_health(&self, health: Health) -> Call<'_, ()> {
        Box::pin(async move {
            self.events
                .send(ProcessEvent::Health(opamp::proto::ComponentHealth {
                    healthy: health.healthy,
                    status: health.status,
                    last_error: health.last_error,
                    status_time_unix_nano: crate::supervisor::process::now_ns(),
                    ..Default::default()
                }))
                .await;
            Ok(())
        })
    }
}

/// What the core does once the handler has returned from an input: it answers what the instance
/// left unanswered (CLT-wrappers-act-through-the-core §5).
trait Settles: Host {
    fn settle<'a>(&'a self, _input: &'a Input, _outcome: &'a Result<(), String>) -> Call<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}

impl Settles for CoreHost {
    /// An apply or an install the instance did not hand on is answered as failed, with the
    /// handler's reason, so the Agent leaves `APPLYING`. An uninstall the instance did not ask for
    /// is the generic one, the bounded stop (§9).
    fn settle<'a>(&'a self, input: &'a Input, outcome: &'a Result<(), String>) -> Call<'a, ()> {
        Box::pin(async move {
            let reason = || match outcome {
                Err(e) => e.clone(),
                Ok(()) => "the Plugin left it unanswered".to_string(),
            };
            match input {
                Input::ApplyConfig { hash, .. } => {
                    if Self::take(&self.configs, hash, "configuration").is_ok() {
                        self.events
                            .send(ProcessEvent::ConfigApplied {
                                hash: hash.clone(),
                                result: Err(reason()),
                            })
                            .await;
                    }
                }
                Input::ApplyPackage { hash, .. } => {
                    if Self::take(&self.packages, hash, "package").is_ok() {
                        self.events
                            .send(ProcessEvent::PackageApplied {
                                hash: hash.clone(),
                                result: Err(reason()),
                            })
                            .await;
                    }
                }
                Input::Uninstall if !self.uninstalled.load(Ordering::SeqCst) => {
                    self.send(ProcessCommand::Uninstall).await?;
                }
                _ => {}
            }
            Ok(())
        })
    }
}

/// Turns the core's commands into the instance's inputs, keeping what an input stands for in the
/// host. A shutdown goes to the `Runner` directly: stopping is the core's, not a Plugin's.
///
/// It holds the core's command channel until the `Runner` has exited, stopped, shut down or
/// uninstalled, or was never started: the core reads the channel closing as the adapter's exit,
/// after the uninstall's answer.
async fn front(
    mut commands: mpsc::Receiver<ProcessCommand>,
    host: Arc<CoreHost>,
    inputs: mpsc::Sender<Input>,
) {
    loop {
        let command = tokio::select! {
            command = commands.recv() => match command {
                Some(command) => command,
                None => return,
            },
            () = host.runner.closed() => return,
        };
        let input = match command {
            ProcessCommand::ApplyConfig { config, span } => {
                let hash = config.config_hash.clone();
                if let Ok(mut pending) = host.configs.lock() {
                    pending.insert(hash.clone(), ProcessCommand::ApplyConfig { config, span });
                }
                Input::ApplyConfig {
                    hash,
                    entries: entries_of(host.roots.config.clone()).await,
                }
            }
            ProcessCommand::ApplyPackage {
                staged,
                version,
                hash,
                span,
            } => {
                if let Ok(mut pending) = host.packages.lock() {
                    pending.insert(
                        hash.clone(),
                        ProcessCommand::ApplyPackage {
                            staged,
                            version: version.clone(),
                            hash: hash.clone(),
                            span,
                        },
                    );
                }
                Input::ApplyPackage { version, hash }
            }
            ProcessCommand::Restart => Input::Restart,
            ProcessCommand::Uninstall => Input::Uninstall,
            ProcessCommand::Shutdown => {
                let _ = host.runner.send(ProcessCommand::Shutdown).await;
                host.runner.closed().await;
                return;
            }
        };
        if inputs.send(input).await.is_err() {
            return;
        }
    }
}

/// What feeds an instance after its start: the inputs of the core's commands and the timers it
/// set.
struct Fed {
    /// The `config` root, whose entries the start carries.
    config: PathBuf,
    inputs: mpsc::Receiver<Input>,
    timers: Arc<Timers>,
    fired: mpsc::UnboundedReceiver<Fired>,
}

/// Feeds `instance` its inputs, one at a time: the start first, on the entries written, then
/// `started`, then each input once the call before it has returned and the core has settled it
/// (CLT-wrappers-act-through-the-core §1). A shutdown cancels the call in progress; the `Runner`
/// stops the Managed Process on the same signal.
async fn drive<H: Settles + 'static>(
    mut instance: Box<dyn Instance>,
    host: Arc<H>,
    started: impl FnOnce() + Send,
    fed: Fed,
    mut shutdown: Shutdown,
) {
    let Fed {
        config,
        mut inputs,
        timers,
        mut fired,
    } = fed;
    let first = Input::Start {
        entries: entries_of(config).await,
    };
    let outcome = tokio::select! {
        outcome = instance.handle(first, &*host) => outcome,
        () = shutdown.requested() => return,
    };
    if let Err(e) = outcome {
        warn!(error = %e, "the Plugin could not start its Managed Process");
    }
    started();
    loop {
        let input = tokio::select! {
            input = inputs.recv() => match input {
                Some(input) => input,
                None => return,
            },
            Some(timer) = fired.recv() => {
                if !timers.take(timer) {
                    continue;
                }
                Input::Timer { id: timer.0 }
            }
            () = shutdown.requested() => return,
        };
        let last = input == Input::Uninstall;
        let outcome = tokio::select! {
            outcome = instance.handle(input.clone(), &*host) => outcome,
            () = shutdown.requested() => return,
        };
        if let Err(e) = &outcome {
            warn!(error = %e, "the Plugin's host function failed");
        }
        if let Err(e) = host.settle(&input, &outcome).await {
            warn!(error = %e, "the core could not settle an input the Plugin left");
        }
        if last {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shutdown::shutdown_channel;
    use crate::supervisor::port::Process;
    use std::time::Duration;
    use tokio::sync::Notify;

    /// A host that does nothing and answers every call.
    struct Idle;

    impl Host for Idle {
        fn start(&self, _spec: Spec) -> Call<'_, ()> {
            Box::pin(async { Ok(()) })
        }
        fn apply_config(&self, _hash: Vec<u8>, _spec: Option<Spec>) -> Call<'_, ()> {
            Box::pin(async { Ok(()) })
        }
        fn install(&self, _hash: Vec<u8>) -> Call<'_, ()> {
            Box::pin(async { Ok(()) })
        }
        fn restart(&self) -> Call<'_, ()> {
            Box::pin(async { Ok(()) })
        }
        fn uninstall(&self) -> Call<'_, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    impl Settles for Idle {}

    /// An instance that records each input it handles, and holds a restart until `gate` opens.
    struct Recording {
        log: Arc<Mutex<Vec<String>>>,
        gate: Arc<Notify>,
    }

    impl Instance for Recording {
        fn process(&self) -> Process {
            Process::default()
        }
        fn spec(&self, _entries: &[Entry]) -> Option<Spec> {
            None
        }
        fn handle<'a>(&'a mut self, input: Input, _host: &'a dyn Host) -> Call<'a, ()> {
            Box::pin(async move {
                let push = |s: &str| self.log.lock().expect("log").push(s.to_string());
                match input {
                    Input::Restart => {
                        push("restart begins");
                        self.gate.notified().await;
                        push("restart ends");
                    }
                    Input::Start { .. } => push("Start"),
                    other => push(&format!("{other:?}")),
                }
                Ok(())
            })
        }
    }

    fn fed(inputs: mpsc::Receiver<Input>) -> Fed {
        let (timers, fired) = Timers::new();
        Fed {
            config: PathBuf::from("no-config-here"),
            inputs,
            timers,
            fired,
        }
    }

    fn recording() -> (Recording, Arc<Mutex<Vec<String>>>, Arc<Notify>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new(Notify::new());
        (
            Recording {
                log: Arc::clone(&log),
                gate: Arc::clone(&gate),
            },
            log,
            gate,
        )
    }

    /// The core calls the handler with one input at a time: an input that arrives during a call
    /// waits until that call has returned.
    /// Verifies: CLT-wrappers-act-through-the-core
    #[tokio::test]
    async fn the_handler_is_never_called_twice_at_once() {
        let (instance, log, gate) = recording();
        let (_stop, shutdown) = shutdown_channel();
        let (to_instance, inputs) = mpsc::channel(4);
        let driven = tokio::spawn(drive(
            Box::new(instance),
            Arc::new(Idle),
            || {},
            fed(inputs),
            shutdown,
        ));
        to_instance.send(Input::Restart).await.expect("send");
        to_instance.send(Input::Uninstall).await.expect("send");
        // timed: the uninstall must not be handled; nothing signals what does not happen.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            *log.lock().expect("log"),
            vec!["Start".to_string(), "restart begins".to_string()],
            "the uninstall waits while the restart is handled"
        );
        gate.notify_one();
        tokio::time::timeout(Duration::from_secs(5), driven)
            .await
            .expect("the uninstall ends the instance")
            .expect("the task");
        assert_eq!(
            *log.lock().expect("log"),
            vec!["Start", "restart begins", "restart ends", "Uninstall"]
        );
    }

    /// A shutdown cancels the call in progress, so a handler that never returns does not hold up
    /// the Client's stop.
    /// Verifies: CLT-wrappers-act-through-the-core
    #[tokio::test]
    async fn a_shutdown_cancels_the_handler_in_progress() {
        let (instance, log, _gate) = recording();
        let (stop, shutdown) = shutdown_channel();
        let (to_instance, inputs) = mpsc::channel(4);
        let driven = tokio::spawn(drive(
            Box::new(instance),
            Arc::new(Idle),
            || {},
            fed(inputs),
            shutdown,
        ));
        to_instance.send(Input::Restart).await.expect("send");
        // timed: lets the restart's call begin; the assertion below sees whether it did.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = stop.send(true);
        tokio::time::timeout(Duration::from_secs(5), driven)
            .await
            .expect("the shutdown cancels the handler")
            .expect("the task");
        assert_eq!(
            *log.lock().expect("log"),
            vec!["Start".to_string(), "restart begins".to_string()]
        );
    }

    /// The core hands an instance every written entry, by name, with its role and digest, and
    /// none of its own bookkeeping.
    #[test]
    fn the_entries_carry_name_role_and_digest() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(entries(dir.path()).is_empty(), "no configuration yet");
        std::fs::write(dir.path().join("b"), b"service: {}\n").expect("write");
        std::fs::write(dir.path().join("a"), b"x").expect("write");
        std::fs::write(dir.path().join("ca"), b"pem").expect("write");
        std::fs::write(
            dir.path().join(crate::storage::SUPPLEMENTARY_FILE),
            "ca\tcertificate\n",
        )
        .expect("write");
        let found = entries(dir.path());
        let named: Vec<(&str, Option<&str>)> = found
            .iter()
            .map(|e| (e.name.as_str(), e.role.as_deref()))
            .collect();
        assert_eq!(
            named,
            vec![("a", None), ("b", None), ("ca", Some("certificate"))]
        );
        assert_eq!(
            found[0].digest,
            ring::digest::digest(&ring::digest::SHA256, b"x")
                .as_ref()
                .to_vec()
        );
    }

    /// A Supervisor's roots in a temporary directory, its `config/` and `program/` made.
    #[cfg(unix)]
    fn made_roots(dir: &Path) -> Roots {
        let supervisor_dir = dir.join("agent");
        std::fs::create_dir_all(supervisor_dir.join("config")).expect("config");
        std::fs::create_dir_all(supervisor_dir.join("program")).expect("program");
        Roots {
            config: supervisor_dir.join("config"),
            program: supervisor_dir.join("program"),
            data: supervisor_dir.join(DATA_DIR),
            state: supervisor_dir.join(STATE_DIR),
            supervisor_dir,
            group: None,
        }
    }

    #[cfg(unix)]
    fn core_host(roots: Roots, spawner: Spawner) -> CoreHost {
        wired(roots, spawner).0
    }

    /// A core host whose commands to the `Runner` and whose events the test reads.
    fn wired(
        roots: Roots,
        spawner: Spawner,
    ) -> (
        CoreHost,
        mpsc::Receiver<ProcessCommand>,
        mpsc::Receiver<(usize, ProcessEvent)>,
    ) {
        let (runner, commands) = mpsc::channel(4);
        let (timers, _fired) = Timers::new();
        let (events, reported) = mpsc::channel(4);
        let host = CoreHost {
            name: "agent".to_string(),
            program: roots.program.join("agent"),
            roots,
            own_process_group: false,
            ensure_dirs: Vec::new(),
            events: EventSender::new(0, events),
            uninstalled: AtomicBool::new(false),
            spec: Arc::new(Mutex::new(None)),
            runner,
            running: AtomicBool::new(false),
            configs: Mutex::new(HashMap::new()),
            packages: Mutex::new(HashMap::new()),
            spawner,
            timers,
            account: tokio::sync::OnceCell::new(),
        };
        (host, commands, reported)
    }

    /// Roots that name directories nobody made, for what touches no file.
    fn unmade() -> Roots {
        let supervisor_dir = PathBuf::from("no-such-supervisor");
        Roots {
            config: supervisor_dir.join("config"),
            program: supervisor_dir.join("program"),
            data: supervisor_dir.join(DATA_DIR),
            state: supervisor_dir.join(STATE_DIR),
            supervisor_dir,
            group: None,
        }
    }

    fn apply(hash: &[u8]) -> ProcessCommand {
        ProcessCommand::ApplyConfig {
            config: opamp::proto::AgentRemoteConfig {
                config_hash: hash.to_vec(),
                ..Default::default()
            },
            span: tracing::Span::none(),
        }
    }

    /// An apply the instance leaves unanswered, or answers with a specification the core refuses,
    /// is answered as failed by the core, so the Agent does not stay `APPLYING`; one the instance
    /// handed on is not answered twice.
    /// Verifies: CLT-wrappers-act-through-the-core
    #[tokio::test]
    async fn the_core_answers_an_apply_the_plugin_left() {
        let (host, mut commands, mut reported) = wired(unmade(), Spawner::Local);
        for hash in [[1u8], [2], [3]] {
            host.configs
                .lock()
                .expect("configs")
                .insert(hash.to_vec(), apply(&hash));
        }
        let left = Input::ApplyConfig {
            hash: vec![1],
            entries: Vec::new(),
        };
        host.settle(&left, &Ok(())).await.expect("settled");
        let refused = host
            .apply_config(
                vec![2],
                Some(Spec {
                    args: vec![Value::Rooted(RootPath {
                        root: Root::Config,
                        path: "../x".to_string(),
                    })],
                    ..Spec::default()
                }),
            )
            .await;
        assert!(refused.is_err(), "the path leaves its root");
        let input = Input::ApplyConfig {
            hash: vec![2],
            entries: Vec::new(),
        };
        host.settle(&input, &refused).await.expect("settled");
        host.apply_config(vec![3], None).await.expect("handed on");
        let input = Input::ApplyConfig {
            hash: vec![3],
            entries: Vec::new(),
        };
        host.settle(&input, &Ok(())).await.expect("settled");
        let mut answers = Vec::new();
        while let Ok((_, event)) = reported.try_recv() {
            if let ProcessEvent::ConfigApplied { hash, result } = event {
                answers.push((hash, result.is_err()));
            }
        }
        assert_eq!(answers, vec![(vec![1], true), (vec![2], true)]);
        assert!(
            matches!(commands.try_recv(), Ok(ProcessCommand::ApplyConfig { .. })),
            "the apply handed on reaches the Runner"
        );
    }

    /// An uninstall the instance did not ask for is the generic one: the core stops the process.
    /// Verifies: CLT-wrappers-act-through-the-core
    #[tokio::test]
    async fn the_core_stops_the_process_on_an_uninstall_the_plugin_left() {
        let (host, mut commands, _reported) = wired(unmade(), Spawner::Local);
        host.settle(&Input::Uninstall, &Err("failed early".to_string()))
            .await
            .expect("settled");
        assert!(matches!(commands.try_recv(), Ok(ProcessCommand::Uninstall)));
        host.uninstall().await.expect("asked");
        host.settle(&Input::Uninstall, &Ok(()))
            .await
            .expect("settled");
        assert!(matches!(commands.try_recv(), Ok(ProcessCommand::Uninstall)));
        assert!(commands.try_recv().is_err(), "not stopped twice");
    }

    /// A health a Plugin reports is the Supervisor's Agent's health.
    /// Verifies: CLT-wrappers-act-through-the-core
    #[tokio::test]
    async fn a_reported_health_reaches_the_agent() {
        let (host, _commands, mut reported) = wired(unmade(), Spawner::Local);
        host.report_health(Health {
            healthy: false,
            status: "awaiting the certificate".to_string(),
            last_error: "the parent is down".to_string(),
        })
        .await
        .expect("reported");
        let Ok((_, ProcessEvent::Health(health))) = reported.try_recv() else {
            panic!("a health event");
        };
        assert!(!health.healthy);
        assert_eq!(health.status, "awaiting the certificate");
        assert_eq!(health.last_error, "the parent is down");
    }

    /// The manifest's process group and data directories reach every specification the core
    /// starts; a data directory that would leave its root fails the start.
    /// Verifies: CLT-wrappers-act-through-the-core
    #[test]
    fn the_manifest_reaches_the_process_specification() {
        let roots = unmade();
        let dirs = data_dirs(&roots, &["log", "lib/state"]).expect("dirs");
        assert_eq!(
            dirs,
            vec![roots.data.join("log"), roots.data.join("lib").join("state"),]
        );
        assert_eq!(
            data_dirs(&roots, &[]),
            Ok(Vec::new()),
            "none named, none made"
        );
        assert!(data_dirs(&roots, &["../x"]).is_err());
        let (mut host, _commands, _reported) = wired(roots, Spawner::Local);
        host.own_process_group = true;
        host.ensure_dirs.clone_from(&dirs);
        let spec = host.resolve(&Spec::default()).expect("resolved");
        assert!(spec.own_process_group);
        assert_eq!(spec.ensure_dirs, dirs);
    }

    /// A probe's path in the `program` root names the live program for the version probe and the
    /// staged one for the preflight (CLT-wrappers-act-through-the-core §34).
    /// Verifies: CLT-wrappers-act-through-the-core
    #[test]
    fn a_probe_asks_the_live_program_and_the_preflight_the_staged_one() {
        let roots = unmade();
        let probe = Probe {
            args: vec![
                Value::Rooted(RootPath {
                    root: Root::Program,
                    path: "lib".to_string(),
                }),
                Value::Text("--version".to_string()),
            ],
            env: vec![(
                "LD_LIBRARY_PATH".to_string(),
                Value::Rooted(RootPath {
                    root: Root::Program,
                    path: "lib".to_string(),
                }),
            )],
            working_dir: Some(RootPath {
                root: Root::Program,
                path: String::new(),
            }),
        };
        let live = roots.program.join("lib").to_string_lossy().into_owned();
        assert_eq!(
            probe_parts(&roots, &probe),
            Ok((
                vec![live.clone(), "--version".to_string()],
                vec![("LD_LIBRARY_PATH".to_string(), live)],
                Some(roots.program.clone()),
            ))
        );
        let staged = PathBuf::from(STAGED);
        let in_staged = staged.join("lib").to_string_lossy().into_owned();
        assert_eq!(
            probe_parts(&roots.with_program(staged.clone()), &probe),
            Ok((
                vec![in_staged.clone(), "--version".to_string()],
                vec![("LD_LIBRARY_PATH".to_string(), in_staged)],
                Some(staged),
            ))
        );
    }

    /// The second service, run in this process on a socket in `dir` under this test's own uid,
    /// and the spawner that starts programs through it.
    #[cfg(unix)]
    async fn second_service(
        dir: &Path,
    ) -> (
        Spawner,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<crate::supervisor::channel::Served>,
    ) {
        use crate::supervisor::channel::{bind, serve, Channel, ServiceProcess};
        let socket = dir.join("agents.sock");
        let listener = bind(&socket).expect("bind");
        // SAFETY: getuid(2) cannot fail and touches no memory.
        let uid = unsafe { libc::getuid() };
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let service = tokio::spawn(serve(listener, uid, || false, async {
            let _ = stopped.await;
        }));
        let channel = Channel::connect(
            &socket,
            uid,
            ServiceProcess::named(|| Ok(Some(std::process::id()))),
        )
        .await
        .expect("connect");
        (Spawner::Channel(channel), stop, service)
    }

    /// A program a Plugin runs once comes from its `program` root, starts in that root, and
    /// answers with its exit status and output, the same through the Client and through the
    /// second service; a program outside the root is refused.
    #[cfg(unix)]
    async fn a_program_runs_once(spawner: Spawner, dir: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        let roots = made_roots(dir);
        let tool = roots.program.join("tool");
        std::fs::write(
            &tool,
            "#!/bin/sh\necho \"ran $1 in $(pwd)\"\necho err >&2\nexit 3\n",
        )
        .expect("tool");
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).expect("mode");
        let root = std::fs::canonicalize(&roots.program).expect("root");
        let host = core_host(roots, spawner);
        let ran = host
            .run(Run {
                program: "tool".to_string(),
                args: vec![port::Value::Text("x".to_string())],
                ..Run::default()
            })
            .await
            .expect("ran");
        assert_eq!(ran.status, Some(3));
        assert_eq!(
            String::from_utf8_lossy(&ran.stdout),
            format!("ran x in {}\n", root.display())
        );
        assert_eq!(String::from_utf8_lossy(&ran.stderr), "err\n");
        let outside = host
            .run(Run {
                program: "../../bin/sh".to_string(),
                ..Run::default()
            })
            .await;
        assert!(outside.is_err(), "a program outside its root is refused");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_program_runs_once_through_the_client() {
        let dir = tempfile::tempdir().expect("tempdir");
        a_program_runs_once(Spawner::Local, dir.path()).await;
    }

    /// Where a process account is registered, a program a Plugin runs goes through the second
    /// service, as the Managed Process does (CLT-accounts-service-runs-under §18): once the
    /// service is gone, the program does not run, where the Client could start it itself.
    /// Verifies: CLT-wrappers-act-through-the-core
    #[cfg(unix)]
    #[tokio::test]
    async fn a_program_a_plugin_runs_goes_through_the_second_service() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (spawner, stop, service) = second_service(dir.path()).await;
        a_program_runs_once(spawner.clone(), dir.path()).await;
        let _ = stop.send(());
        tokio::time::timeout(Duration::from_secs(10), service)
            .await
            .expect("the service stops")
            .expect("the service task");
        let host = core_host(made_roots(dir.path()), spawner);
        let ran = host
            .run(Run {
                program: "tool".to_string(),
                ..Run::default()
            })
            .await;
        assert!(
            ran.is_err(),
            "without the second service nothing runs: {ran:?}"
        );
    }

    /// The host facts name the host and the account a program runs under, as that program states
    /// it.
    /// Verifies: CLT-wrappers-act-through-the-core
    #[cfg(unix)]
    #[tokio::test]
    async fn the_facts_name_the_host_and_the_account() {
        let dir = tempfile::tempdir().expect("tempdir");
        let expected = std::process::Command::new("/usr/bin/id")
            .arg("-un")
            .output()
            .expect("id");
        let expected = String::from_utf8_lossy(&expected.stdout).trim().to_string();
        let (spawner, _stop, service) = second_service(dir.path()).await;
        for spawner in [Spawner::Local, spawner] {
            let host = core_host(made_roots(dir.path()), spawner);
            let facts = host.facts().await.expect("facts");
            assert!(facts.host_name.is_some_and(|n| !n.is_empty()));
            assert_eq!(facts.account.as_deref(), Some(expected.as_str()));
        }
        service.abort();
    }

    /// A host whose timers are the instance's, and which answers every other call.
    struct Timed(Arc<Timers>);

    impl Host for Timed {
        fn start(&self, _spec: Spec) -> Call<'_, ()> {
            Box::pin(async { Ok(()) })
        }
        fn apply_config(&self, _hash: Vec<u8>, _spec: Option<Spec>) -> Call<'_, ()> {
            Box::pin(async { Ok(()) })
        }
        fn install(&self, _hash: Vec<u8>) -> Call<'_, ()> {
            Box::pin(async { Ok(()) })
        }
        fn restart(&self) -> Call<'_, ()> {
            Box::pin(async { Ok(()) })
        }
        fn uninstall(&self) -> Call<'_, ()> {
            Box::pin(async { Ok(()) })
        }
        fn set_timer(&self, id: u64, after_ms: u64) -> Call<'_, ()> {
            self.0.set(id, Duration::from_millis(after_ms));
            Box::pin(async { Ok(()) })
        }
        fn cancel_timer(&self, id: u64) -> Call<'_, ()> {
            self.0.cancel(id);
            Box::pin(async { Ok(()) })
        }
    }

    impl Settles for Timed {}

    /// An instance that sets three timers at its start and cancels two of them, one after it has
    /// fired, and records each timer that arrives.
    struct Timing(Arc<Mutex<Vec<u64>>>);

    impl Instance for Timing {
        fn process(&self) -> Process {
            Process::default()
        }
        fn spec(&self, _entries: &[Entry]) -> Option<Spec> {
            None
        }
        fn handle<'a>(&'a mut self, input: Input, host: &'a dyn Host) -> Call<'a, ()> {
            Box::pin(async move {
                match input {
                    Input::Start { .. } => {
                        host.set_timer(1, 20).await?;
                        host.set_timer(2, 20).await?;
                        host.set_timer(3, 0).await?;
                        host.cancel_timer(2).await?;
                        // Timer 3 fires while this call runs, and waits for it to return.
                        // timed: a call that outlasts timer 3's firing, which nothing signals.
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        host.cancel_timer(3).await?;
                    }
                    Input::Timer { id } => self.0.lock().expect("log").push(id),
                    _ => {}
                }
                Ok(())
            })
        }
    }

    /// A timer arrives as an input; a cancelled one never does, even one that fired already.
    /// Verifies: CLT-wrappers-act-through-the-core
    #[tokio::test]
    async fn a_timer_arrives_as_an_input_and_a_cancelled_one_never() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let (timers, fired) = Timers::new();
        let (_stop, shutdown) = shutdown_channel();
        let (_to_instance, inputs) = mpsc::channel(4);
        let driven = tokio::spawn(drive(
            Box::new(Timing(Arc::clone(&log))),
            Arc::new(Timed(Arc::clone(&timers))),
            || {},
            Fed {
                config: PathBuf::from("no-config-here"),
                inputs,
                timers,
                fired,
            },
            shutdown,
        ));
        // Timer 1 fires 20 ms after the start's 50 ms call, and a cancelled one would as well.
        // timed: nothing signals that a cancelled timer stays away; 300 ms lets each arrive.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(*log.lock().expect("log"), vec![1]);
        driven.abort();
    }
}
