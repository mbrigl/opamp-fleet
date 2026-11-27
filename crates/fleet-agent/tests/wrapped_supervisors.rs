//! The harness every kind's integration tests share ([`wrapped`]), and the stub features they rely
//! on, held through the core's own `command` kind (CLT-wrappers-act-through-the-core): a kind that knows its agent states
//! the arguments itself, so the stub takes the flags a test needs from a file beside it, and a kind
//! that reloads by signal needs a stub that shows the signal arrived. A Plugin of this test tree,
//! on the port alone, shows what the core does for any kind: a refused call fails alone, and the
//! manifest's data directories and reload signal reach the process.

mod wrapped;

use fleet_agent::supervisor::port::{
    Call, Entry, Health, Host, Input, Instance, Manifest, Plugin, Probe, Process, Root, RootPath,
    Spec, Value,
};
use fleet_agent::supervisor::ports::ProcessEvent;

/// The harness starts a kind as the core does, the program where the install puts it, and the stub
/// adds the flags of its `<program>.args` side-file to its own: the marker it writes holds them.
/// The `command` kind reports the probed version and applies a Configuration by restarting.
/// Verifies: CLT-wrappers-act-through-the-core
#[tokio::test]
async fn the_harness_drives_a_kind_and_the_stub_reads_its_side_file() {
    let marker_dir = tempfile::tempdir().expect("tempdir");
    let marker = marker_dir.path().join("started");
    let mut settings = toml::Table::new();
    settings.insert(
        "version_args".to_string(),
        toml::Value::Array(vec![toml::Value::String("--version".to_string())]),
    );
    let mut harness = wrapped::start_with(
        &fleet_agent::supervisor::command::Command,
        "agent",
        settings,
        &["--touch".to_string(), marker.display().to_string()],
    );
    let (version, first) = wrapped::wait_for_running_version(&mut harness).await;
    assert_eq!(version, "9.9.9");
    wrapped::wait_for_file(&marker, &format!("pid={first}")).await;
    assert!(
        std::fs::read_to_string(&marker)
            .expect("the marker")
            .contains("--touch"),
        "the side-file's flags reached the stub"
    );

    let (result, spawned) = wrapped::apply_config(&mut harness).await;
    assert_eq!(result, Ok(()));
    assert_eq!(spawned.len(), 1, "the command kind applies by restarting");
    wrapped::stop(harness).await;
}

/// `--record-hup` keeps the stub running through `SIGHUP` and counts each one in a file, so a test
/// of a kind that reloads by signal sees that the signal reached the process.
/// Verifies: CLT-wrappers-act-through-the-core
#[cfg(unix)]
#[tokio::test]
async fn the_stub_records_each_sighup_and_survives_it() {
    let marker_dir = tempfile::tempdir().expect("tempdir");
    let (started, hups) = (
        marker_dir.path().join("started"),
        marker_dir.path().join("hups"),
    );
    let mut settings = toml::Table::new();
    settings.insert(
        "version_args".to_string(),
        toml::Value::Array(vec![toml::Value::String("--version".to_string())]),
    );
    let mut harness = wrapped::start_with(
        &fleet_agent::supervisor::command::Command,
        "agent",
        settings,
        &[
            "--touch".to_string(),
            started.display().to_string(),
            "--record-hup".to_string(),
            hups.display().to_string(),
        ],
    );
    let (_, pid) = wrapped::wait_for_running_version(&mut harness).await;
    // The handler is installed before the marker is written, so the signal cannot race it.
    wrapped::wait_for_file(&started, &format!("pid={pid}")).await;
    for count in 1..=2 {
        // SAFETY: sends SIGHUP to the stub this test started; no memory is touched.
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGHUP) }, 0);
        wrapped::wait_for_file(&hups, &format!("hups={count}")).await;
    }
    // SAFETY: signal 0 probes existence only; no signal is delivered, no memory is touched.
    assert_eq!(
        unsafe { libc::kill(pid as libc::pid_t, 0) },
        0,
        "the stub survived the signals"
    );
    wrapped::stop(harness).await;
}

/// A Plugin of the test tree, on the port alone. At its start it tries to write into its
/// `config` root, which the core refuses, reports the refusal as its health and starts its
/// program anyway; a restart it answers by writing into its `state` root.
struct Steady {
    reload_signal: Option<i32>,
}

impl Plugin for Steady {
    fn manifest(&self) -> Manifest {
        Manifest {
            reload_signal: self.reload_signal,
            data_dirs: &["log", "lib/state"],
            ..Manifest::of("steady", "binary")
        }
    }

    fn check(&self, _name: &str, _settings: toml::Table) -> Result<(), String> {
        Ok(())
    }

    fn instantiate(
        &self,
        _name: &str,
        _settings: toml::Table,
    ) -> Result<Box<dyn Instance>, String> {
        Ok(Box::new(SteadyInstance))
    }
}

struct SteadyInstance;

impl Instance for SteadyInstance {
    fn process(&self) -> Process {
        Process {
            probe: Some(Probe {
                args: vec![Value::Text("--version".to_string())],
                ..Probe::default()
            }),
            read_version: None,
        }
    }

    fn spec(&self, _entries: &[Entry]) -> Option<Spec> {
        Some(Spec::default())
    }

    fn handle<'a>(&'a mut self, input: Input, host: &'a dyn Host) -> Call<'a, ()> {
        Box::pin(async move {
            match input {
                Input::Start { .. } => {
                    let refused = host
                        .write(
                            RootPath {
                                root: Root::Config,
                                path: "x".to_string(),
                            },
                            b"x".to_vec(),
                        )
                        .await
                        .expect_err("the config root is read-only to a Plugin");
                    host.report_health(Health {
                        healthy: false,
                        status: format!("refused: {refused}"),
                        last_error: String::new(),
                    })
                    .await?;
                    host.start(Spec::default()).await
                }
                Input::Restart => {
                    host.write(
                        RootPath {
                            root: Root::State,
                            path: "note".to_string(),
                        },
                        b"kept".to_vec(),
                    )
                    .await?;
                    host.report_health(Health {
                        healthy: true,
                        status: "still handling".to_string(),
                        last_error: String::new(),
                    })
                    .await
                }
                other => {
                    let spec = self.spec(&[]);
                    fleet_agent::supervisor::port::default_handling(other, spec, host).await
                }
            }
        })
    }
}

/// The next health event whose status starts with `prefix`, skipping every other event.
async fn health_starting(harness: &mut wrapped::Harness, prefix: &str) -> String {
    // timed: the waiter's own deadline, which only a defect reaches
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let (_, event) = tokio::time::timeout_at(deadline, harness.events.recv())
            .await
            .expect("the health in time")
            .expect("an open channel");
        if let ProcessEvent::Health(health) = event {
            if health.status.starts_with(prefix) {
                return health.status;
            }
        }
    }
}

/// A Plugin of the test tree that names a file outside the roots it may write is refused that call
/// alone: it learns why, without a host path, and goes on to start its program and handle the
/// next input. The data directories of its manifest are there before its program runs.
/// Verifies: CLT-wrappers-act-through-the-core
#[tokio::test]
async fn a_refused_call_fails_alone_and_the_plugin_keeps_running() {
    let mut harness = wrapped::start(
        &Steady {
            reload_signal: None,
        },
        "steady",
        &[],
    );
    let refused = health_starting(&mut harness, "refused: ").await;
    assert!(refused.contains("config:x"), "{refused}");
    assert!(
        !refused.contains(&harness.dir.path().display().to_string()),
        "no host path reaches the Plugin: {refused}"
    );
    let (version, _) = wrapped::wait_for_running_version(&mut harness).await;
    assert_eq!(version, "9.9.9");
    for dir in ["log", "lib/state"] {
        assert!(harness.dir.path().join("data").join(dir).is_dir(), "{dir}");
    }
    harness
        .commands
        .send(fleet_agent::supervisor::ports::ProcessCommand::Restart)
        .await
        .expect("send");
    health_starting(&mut harness, "still handling").await;
    assert_eq!(
        std::fs::read(harness.dir.path().join("state").join("note")).expect("note"),
        b"kept"
    );
    wrapped::stop(harness).await;
}

/// The reload signal a manifest names applies a Configuration by signal, without a restart.
/// Verifies: CLT-wrappers-act-through-the-core
#[cfg(unix)]
#[tokio::test]
async fn the_manifests_reload_signal_applies_a_configuration() {
    let marker_dir = tempfile::tempdir().expect("tempdir");
    let hups = marker_dir.path().join("hups");
    let mut harness = wrapped::start(
        &Steady {
            reload_signal: Some(libc::SIGHUP),
        },
        "steady",
        &["--record-hup".to_string(), hups.display().to_string()],
    );
    wrapped::wait_for_running_version(&mut harness).await;
    let (result, spawned) = wrapped::apply_config(&mut harness).await;
    assert_eq!(result, Ok(()));
    assert!(spawned.is_empty(), "applied in place: {spawned:?}");
    wrapped::wait_for_file(&hups, "hups=1").await;
    wrapped::stop(harness).await;
}
