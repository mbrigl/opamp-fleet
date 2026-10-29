//! The `telegraf` kind, whose whole block is `type` and `name` (ADR-0017), driven through
//! `Plugin::start` the way the core drives it ([`telegraf`](ADR-draft-telegraf)): the derived
//! program is where the install puts it, the version probe reaches it, a delivered Configuration
//! lands where the kind points `--config`, and the kind applies it the way its platform allows — by
//! signal where there are signals, by restart where there are none.
#![cfg(feature = "telegraf")]

mod wrapped;

/// Telegraf's whole block: `type` and `name`. The program is a single file under its own name,
/// the version comes from the kind's own `--version` as the only thing the kind reports of its
/// process, and the Configuration the fleet delivers is the one the kind points `--config` at.
/// Verifies: G-7, ADR-draft-telegraf, ADR-0016
#[tokio::test]
async fn a_two_line_telegraf_block_runs_reports_and_applies() {
    let mut harness = wrapped::start(
        &fleet_agent::supervisor::telegraf::TelegrafPlugin::default(),
        "telegraf",
        &[],
    );
    assert_eq!(
        wrapped::wait_for_running_version(&mut harness).await.0,
        "9.9.9",
        "the kind asked the program for its version without the block naming how"
    );

    // The entry the fleet delivers, under the name `opamp-fleetctl package fetch` uploads — which
    // is the file name `--config` was built with.
    std::fs::write(harness.config_dir().join("telegraf-conf"), "[agent]\n").expect("the entry");
    assert_eq!(wrapped::apply_config(&mut harness).await.0, Ok(()));
    wrapped::stop(harness).await;
}

/// On Unix the kind applies a Configuration with `SIGHUP`, Telegraf's own way of re-reading it
/// (ADR-draft-telegraf clause 2): the signal reaches the running process, the apply is
/// acknowledged, and nothing is spawned — the process that was running is still the one running.
/// Verifies: ADR-draft-telegraf
#[cfg(unix)]
#[tokio::test]
async fn telegraf_applies_a_configuration_by_sighup_in_place() {
    let dir = tempfile::tempdir().expect("tempdir");
    let started = dir.path().join("started");
    let hups = dir.path().join("hups");
    let mut harness = wrapped::start(
        &fleet_agent::supervisor::telegraf::TelegrafPlugin::default(),
        "telegraf",
        &[
            "--touch".to_string(),
            started.display().to_string(),
            "--record-hup".to_string(),
            hups.display().to_string(),
        ],
    );
    let (_, pid) = wrapped::wait_for_running_version(&mut harness).await;
    // The handler is installed before the marker is written, so the signal cannot race it.
    wrapped::wait_for_file(&started, "pid=").await;

    let (result, spawned) = wrapped::apply_config(&mut harness).await;
    assert_eq!(result, Ok(()));
    assert!(
        spawned.is_empty(),
        "a reload spawns nothing; a fresh pid would mean a restart: {spawned:?}"
    );
    wrapped::wait_for_file(&hups, "hups=1").await;
    // SAFETY: signal 0 probes existence only; no signal is delivered, no memory is touched.
    assert_eq!(
        unsafe { libc::kill(pid as libc::pid_t, 0) },
        0,
        "the process that was signalled is still the one running"
    );
    wrapped::stop(harness).await;
}

/// On Windows there is no signal, and the kind applies a Configuration by the Runner's restart
/// (ADR-draft-telegraf clause 2): the apply is acknowledged from a fresh process, with the same
/// block.
/// Verifies: ADR-draft-telegraf
#[cfg(windows)]
#[tokio::test]
async fn telegraf_applies_a_configuration_by_restarting_on_windows() {
    let mut harness = wrapped::start(
        &fleet_agent::supervisor::telegraf::TelegrafPlugin::default(),
        "telegraf",
        &[],
    );
    let (_, first) = wrapped::wait_for_running_version(&mut harness).await;

    let (result, spawned) = wrapped::apply_config(&mut harness).await;
    assert_eq!(result, Ok(()));
    assert_eq!(spawned.len(), 1, "exactly one respawn carries the apply");
    assert_ne!(
        spawned[0], first,
        "a fresh process replaced the running one"
    );
    wrapped::stop(harness).await;
}
