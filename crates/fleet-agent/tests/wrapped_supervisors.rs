//! The harness every kind's integration tests share ([`wrapped`]), and the stub features they rely
//! on, held through the core's own `command` kind (ADR-0017): a kind that knows its agent states
//! the arguments itself, so the stub takes the flags a test needs from a file beside it, and a kind
//! that reloads by signal needs a stub that shows the signal arrived.

mod wrapped;

/// The harness starts a kind as the core does, the program where the install puts it, and the stub
/// adds the flags of its `<program>.args` side-file to its own: the marker it writes holds them.
/// The `command` kind reports the probed version and applies a Configuration by restarting.
/// Verifies: ADR-0017
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
        &fleet_agent::supervisor::command::CommandPlugin::default(),
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
/// Verifies: ADR-0017
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
        &fleet_agent::supervisor::command::CommandPlugin::default(),
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
