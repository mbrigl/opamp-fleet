//! The core's side of the Plugin port (CLT-wrappers-act-through-the-core §1, §4 and §5): it feeds
//! a Plugin instance its inputs, one at a time, and carries out the host functions the instance
//! calls over the [`Runner`], which keeps the whole lifecycle of the Managed Process.
//!
//! [`Hosted`] serves a [`port::Plugin`] behind the older factory port of [`ports::Plugin`], so
//! the registry, the block parse and the Supervisor-set apply reach both kinds of Plugin the same
//! way while the kinds move onto the new port one by one.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::shutdown::Shutdown;
use crate::supervisor::port::{self, Call, Host, Input, Instance, Root, RootPath, Spec, Value};
use crate::supervisor::ports::{
    self, InstallTarget, KindDefaults, ProcessCommand, SupervisorContext,
};
use crate::supervisor::process::{Preflight, ProcessSpec, Runner, Spawner, VersionProbe};

/// A [`port::Plugin`] as the core starts it: with the [`Spawner`] the run chose, which starts its
/// programs (CLT-accounts-service-runs-under §18).
#[derive(Default)]
pub struct Hosted<P> {
    pub plugin: P,
    pub spawner: Spawner,
}

impl<P> Hosted<P> {
    #[must_use]
    pub fn new(plugin: P, spawner: Spawner) -> Self {
        Hosted { plugin, spawner }
    }
}

impl<P: port::Plugin + 'static> ports::Plugin for Hosted<P> {
    fn kind(&self) -> &'static str {
        self.plugin.manifest().kind
    }

    fn defaults(&self) -> KindDefaults {
        self.plugin.manifest().defaults
    }

    fn program_key(&self) -> &'static str {
        self.plugin.manifest().program_key
    }

    fn check(&self, name: &str, settings: toml::Table) -> Result<(), String> {
        self.plugin.check(name, settings)
    }

    fn check_delivered(
        &self,
        settings: &toml::Table,
        running: Option<&toml::Table>,
    ) -> Result<(), String> {
        self.plugin.check_delivered(settings, running)
    }

    fn start(&self, mut ctx: SupervisorContext) -> Result<mpsc::Sender<ProcessCommand>, String> {
        let instance = self
            .plugin
            .instantiate(&ctx.name, std::mem::take(&mut ctx.settings))?;
        let process = instance.process();
        let roots = Roots::of(&ctx);
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
            spawner: self.spawner.clone(),
            process_account_group: ctx.process_account_group,
            supervisor_dir: ctx.supervisor_dir,
            events: ctx.events,
            commands: runner_commands,
            build: Box::new(move || built.lock().ok()?.clone()),
        };
        let host = Arc::new(CoreHost {
            name: ctx.name,
            roots,
            program: ctx.program,
            spec,
            runner: to_runner,
            running: AtomicBool::new(false),
            configs: Mutex::new(HashMap::new()),
            packages: Mutex::new(HashMap::new()),
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
            inputs,
            ctx.shutdown,
        ));
        Ok(commands)
    }
}

/// The Supervisor's own directories, as the roots of CLT-wrappers-act-through-the-core §3 name
/// them.
struct Roots {
    supervisor_dir: PathBuf,
    config: PathBuf,
    program: PathBuf,
}

impl Roots {
    fn of(ctx: &SupervisorContext) -> Self {
        let program = match &ctx.install {
            InstallTarget::Binary { path, .. } => path
                .parent()
                .map_or_else(|| ctx.supervisor_dir.clone(), Path::to_path_buf),
            InstallTarget::Tree { root, .. } => root.join(crate::config::TREE_DIR),
        };
        Roots {
            supervisor_dir: ctx.supervisor_dir.clone(),
            config: ctx.config_dir.clone(),
            program,
        }
    }

    fn root(&self, root: Root) -> &Path {
        match root {
            Root::Config => &self.config,
            Root::Program => &self.program,
        }
    }

    /// The absolute path of `file`, refused where it would leave its root
    /// (CLT-wrappers-act-through-the-core §3).
    fn path(&self, file: &RootPath) -> Result<PathBuf, String> {
        let relative = Path::new(&file.path);
        let inside = relative
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
        if !inside || file.path.contains('\\') {
            return Err(format!(
                "the path {:?} leaves its root: a Plugin names a file by a relative path without `..`",
                file.path
            ));
        }
        Ok(self.root(file.root).join(relative))
    }

    /// The placeholders naming the Supervisor's own directories, expanded in an operator's text
    /// (CLT-wrappers-act-through-the-core §31); anything else in it stays as written.
    fn expand(&self, text: &str) -> String {
        text.replace("${supervisor_dir}", &self.supervisor_dir.to_string_lossy())
            .replace("${config_dir}", &self.config.to_string_lossy())
    }

    fn value(&self, value: &Value) -> Result<String, String> {
        match value {
            Value::Text(text) => Ok(text.clone()),
            Value::Template(text) => Ok(self.expand(text)),
            Value::Rooted(file) => Ok(self.path(file)?.to_string_lossy().into_owned()),
        }
    }
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
            let resolved = self.resolve(&spec)?;
            *self
                .spec
                .lock()
                .map_err(|_| "the process specification is lost".to_string())? = Some(resolved);
            if self.running.load(Ordering::SeqCst) {
                self.send(ProcessCommand::Restart).await?;
            }
            Ok(())
        })
    }

    fn apply_config(&self, hash: Vec<u8>) -> Call<'_, ()> {
        Box::pin(async move {
            let command = Self::take(&self.configs, &hash, "configuration")?;
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
                Input::ApplyConfig { hash }
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
async fn drive(
    mut instance: Box<dyn Instance>,
    host: Arc<dyn Host>,
    started: impl FnOnce() + Send,
    mut inputs: mpsc::Receiver<Input>,
    mut shutdown: Shutdown,
) {
    let outcome = tokio::select! {
        outcome = instance.handle(Input::Start, &*host) => outcome,
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
        fn apply_config(&self, _hash: Vec<u8>) -> Call<'_, ()> {
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
        fn spec(&self) -> Spec {
            Spec::default()
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
                    other => push(&format!("{other:?}")),
                }
                Ok(())
            })
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
            inputs,
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
            || {},
            inputs,
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

    fn roots() -> Roots {
        Roots {
            supervisor_dir: PathBuf::from("/s/agent"),
            config: PathBuf::from("/s/agent/config"),
            program: PathBuf::from("/s/agent/program"),
        }
    }

    /// A file a Plugin names resolves inside its root, and a path that would leave the root fails.
    /// Verifies: CLT-wrappers-act-through-the-core
    #[test]
    fn a_plugin_path_stays_inside_its_root() {
        let roots = roots();
        let file = |root, path: &str| RootPath {
            root,
            path: path.to_string(),
        };
        assert_eq!(
            roots.path(&file(Root::Config, "a/b.yaml")),
            Ok(PathBuf::from("/s/agent/config/a/b.yaml"))
        );
        assert_eq!(
            roots.path(&file(Root::Program, "")),
            Ok(PathBuf::from("/s/agent/program"))
        );
        for escape in ["../x", "a/../../x", "/etc/passwd", "a\\..\\x"] {
            assert!(
                roots.path(&file(Root::Config, escape)).is_err(),
                "{escape} leaves its root"
            );
        }
    }

    /// An operator's template expands the two placeholders and leaves any other `${…}` as written.
    #[test]
    fn a_template_expands_the_supervisors_own_directories() {
        let roots = roots();
        assert_eq!(
            roots.value(&Value::Template("-c ${config_dir}/x ${env:Y}".to_string())),
            Ok("-c /s/agent/config/x ${env:Y}".to_string())
        );
        assert_eq!(
            roots.value(&Value::Text("${config_dir}".to_string())),
            Ok("${config_dir}".to_string()),
            "a text is passed as written"
        );
    }
}
