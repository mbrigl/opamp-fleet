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
    self, Call, Entry, EntryKind, Facts, Host, Input, Instance, Ran, Root, RootPath, Run, Spec,
};
use crate::supervisor::ports::{ProcessCommand, SupervisorContext};
use crate::supervisor::process::{Launch, Preflight, ProcessSpec, Runner, Spawner, VersionProbe};

use files::Roots;
use timers::{Fired, Timers};

pub use files::{DATA_DIR, READ_LIMIT, STATE_DIR};

/// Starts the Supervisor `ctx` describes on `plugin`: makes its instance, the `Runner` of its
/// Managed Process with programs started through `spawner` (CLT-accounts-service-runs-under §18),
/// and the tasks that feed the instance its inputs and carry out its host functions.
///
/// # Errors
/// Returns an error when the block's settings do not parse.
pub fn start(
    plugin: &dyn port::Plugin,
    spawner: &Spawner,
    mut ctx: SupervisorContext,
) -> Result<mpsc::Sender<ProcessCommand>, String> {
    let instance = plugin.instantiate(&ctx.name, std::mem::take(&mut ctx.settings))?;
    let process = instance.process();
    let roots = Roots::of(&ctx);
    let first = Input::Start {
        entries: entries(&roots.config),
    };
    let spec = Arc::new(Mutex::new(None::<ProcessSpec>));
    let (to_runner, runner_commands) = mpsc::channel(16);
    let version_probe = process.version_args.clone().map(|args| VersionProbe {
        program: ctx.program.clone(),
        args,
        working_dir: None,
        parse: None,
        env: Vec::new(),
    });
    let preflight = process.version_args.map(|args| Preflight {
        args,
        env: Vec::new(),
        working_dir: None,
    });
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
        reload_signal: process.reload_signal,
        spawner: spawner.clone(),
        process_account_group: ctx.process_account_group,
        supervisor_dir: ctx.supervisor_dir,
        events: ctx.events,
        commands: runner_commands,
        build: Box::new(move || built.lock().ok()?.clone()),
    };
    let (timers, fired) = Timers::new();
    let host = Arc::new(CoreHost {
        name: ctx.name,
        roots,
        program: ctx.program,
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
        first,
        move || {
            started.running.store(true, Ordering::SeqCst);
            tokio::spawn(runner.run(runner_shutdown));
        },
        Fed {
            inputs,
            timers,
            fired,
        },
        ctx.shutdown,
    ));
    Ok(commands)
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
        let args = spec
            .args
            .iter()
            .map(|a| self.roots.value(a))
            .collect::<Result<Vec<_>, _>>()?;
        let env = spec
            .env
            .iter()
            .map(|(k, v)| Ok((k.clone(), self.roots.value(v)?)))
            .collect::<Result<Vec<_>, String>>()?;
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
            own_process_group: spec.own_process_group,
            ensure_dirs: Vec::new(),
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

    fn apply_config(&self, hash: Vec<u8>, spec: Option<Spec>) -> Call<'_, ()> {
        Box::pin(async move {
            let command = Self::take(&self.configs, &hash, "configuration")?;
            let resolved = spec.as_ref().map(|spec| self.resolve(spec)).transpose()?;
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
            let args = run
                .args
                .iter()
                .map(|a| self.roots.value(a))
                .collect::<Result<Vec<_>, _>>()?;
            let env = run
                .env
                .iter()
                .map(|(k, v)| Ok((k.clone(), self.roots.value(v)?)))
                .collect::<Result<Vec<_>, String>>()?;
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
                    entries: entries(&host.roots.config),
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

/// Feeds `instance` its inputs, one at a time: the start first, then `started`, then each input
/// once the call before it has returned (CLT-wrappers-act-through-the-core §1). A shutdown cancels
/// the call in progress; the `Runner` stops the Managed Process on the same signal.
/// What feeds an instance after its start: the inputs of the core's commands and the timers it
/// set.
struct Fed {
    inputs: mpsc::Receiver<Input>,
    timers: Arc<Timers>,
    fired: mpsc::UnboundedReceiver<Fired>,
}

async fn drive(
    mut instance: Box<dyn Instance>,
    host: Arc<dyn Host>,
    first: Input,
    started: impl FnOnce() + Send,
    fed: Fed,
    mut shutdown: Shutdown,
) {
    let Fed {
        mut inputs,
        timers,
        mut fired,
    } = fed;
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
            outcome = instance.handle(input, &*host) => outcome,
            () = shutdown.requested() => return,
        };
        if let Err(e) = outcome {
            warn!(error = %e, "the Plugin's host function failed");
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
            Input::Start {
                entries: Vec::new(),
            },
            || {},
            fed(inputs),
            shutdown,
        ));
        to_instance.send(Input::Restart).await.expect("send");
        to_instance.send(Input::Uninstall).await.expect("send");
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
            Input::Start {
                entries: Vec::new(),
            },
            || {},
            fed(inputs),
            shutdown,
        ));
        to_instance.send(Input::Restart).await.expect("send");
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
        let (runner, _gone) = mpsc::channel(1);
        let (timers, _fired) = Timers::new();
        CoreHost {
            name: "agent".to_string(),
            program: roots.program.join("agent"),
            roots,
            spec: Arc::new(Mutex::new(None)),
            runner,
            running: AtomicBool::new(false),
            configs: Mutex::new(HashMap::new()),
            packages: Mutex::new(HashMap::new()),
            spawner,
            timers,
            account: tokio::sync::OnceCell::new(),
        }
    }

    /// The second service, run in this process on a socket in `dir` under this test's own uid,
    /// and the spawner that starts programs through it.
    #[cfg(unix)]
    async fn second_service(dir: &Path) -> (Spawner, tokio::task::AbortHandle) {
        use crate::supervisor::channel::{bind, serve, Channel, ServiceProcess};
        let socket = dir.join("agents.sock");
        let listener = bind(&socket).expect("bind");
        // SAFETY: getuid(2) cannot fail and touches no memory.
        let uid = unsafe { libc::getuid() };
        let service = tokio::spawn(serve(listener, uid, || false, std::future::pending()));
        let channel = Channel::connect(
            &socket,
            uid,
            ServiceProcess::named(|| Ok(Some(std::process::id()))),
        )
        .await
        .expect("connect");
        (Spawner::Channel(channel), service.abort_handle())
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
    /// service, as the Managed Process does (CLT-accounts-service-runs-under §18).
    /// Verifies: CLT-wrappers-act-through-the-core
    #[cfg(unix)]
    #[tokio::test]
    async fn a_program_a_plugin_runs_goes_through_the_second_service() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (spawner, service) = second_service(dir.path()).await;
        a_program_runs_once(spawner, dir.path()).await;
        service.abort();
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
        let (spawner, service) = second_service(dir.path()).await;
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
            Input::Start {
                entries: Vec::new(),
            },
            || {},
            Fed {
                inputs,
                timers,
                fired,
            },
            shutdown,
        ));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(*log.lock().expect("log"), vec![1]);
        driven.abort();
    }
}
