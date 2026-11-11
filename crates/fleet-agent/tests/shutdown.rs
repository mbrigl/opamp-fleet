//! Regression test for ADR-0020's graceful shutdown: a service manager stops the Client with
//! `SIGTERM` (systemd, launchd) — the process must exit cleanly (code 0, goodbye path) instead of
//! dying on the default signal disposition.

#![cfg(unix)]

mod common;

use std::process::Command;
use std::time::{Duration, Instant};

/// Verifies: ADR-0020
#[test]
fn sigterm_shuts_the_client_down_cleanly() {
    let dir = tempfile::tempdir().expect("create a tempdir");
    let config = dir.path().join("supervisor.toml");
    // Port 9 (discard) refuses immediately: the client sits in its poll backoff when the signal
    // arrives — exactly where a service stop usually catches it.
    std::fs::write(
        &config,
        format!(
            "endpoint = \"http://127.0.0.1:9\"\nstate_dir = \"{}\"\n{}",
            dir.path().join("state").display(),
            common::client_identity(dir.path())
        ),
    )
    .expect("write the config");

    let mut child = Command::new(env!("CARGO_BIN_EXE_supervisor"))
        .arg("--config")
        .arg(&config)
        .stdout(std::process::Stdio::piped())
        // The wait below reads an `info` line, whatever the environment asks to be logged.
        .env("RUST_LOG", "info")
        .spawn()
        .expect("spawn the client");

    // The signal handlers are in place before the Client logs anything; its transport loop says
    // it polls once it runs. Its log is drained to the end, so the Client never blocks on it.
    let (lines, logged) = std::sync::mpsc::channel();
    let stdout = child.stdout.take().expect("the client's stdout");
    std::thread::spawn(move || {
        use std::io::BufRead as _;
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            let _ = lines.send(line);
        }
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match logged.recv_timeout(left) {
            Ok(line) if line.contains("polling") => break,
            Ok(_) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let status = child.wait().expect("the client's status");
                panic!("the client exited with {status} before it entered its transport loop");
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                panic!("the client never entered its transport loop");
            }
        }
    }
    let kill = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .expect("send SIGTERM");
    assert!(kill.success(), "kill -TERM failed");

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().expect("poll the child") {
            assert!(status.success(), "the client exited with {status}");
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the client did not exit within 10s of SIGTERM"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The second service is a daemon too: `SIGHUP` leaves it serving (ADR-0020 clause 18), where the
/// default disposition would end it and leave every program it started running without it.
/// `SIGTERM` still stops it, and it removes its socket on the way out.
/// Verifies: ADR-0021
#[test]
fn sighup_leaves_the_agents_service_running() {
    let dir = tempfile::tempdir().expect("create a tempdir");
    let socket = dir.path().join("agents.sock");
    // Any uid but this one: the service refuses its own as the Client's.
    // SAFETY: getuid(2) cannot fail and touches no memory.
    let client_uid = unsafe { libc::getuid() }.wrapping_add(1).max(1);
    let mut child = Command::new(env!("CARGO_BIN_EXE_supervisor"))
        .arg("run")
        .arg("--agents")
        .arg(&socket)
        .arg("--client-uid")
        .arg(client_uid.to_string())
        .spawn()
        .expect("spawn the second service");

    // The socket is bound once the signal handlers are in place.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        assert!(
            child.try_wait().expect("poll the service").is_none(),
            "the second service exited before it listened"
        );
        assert!(Instant::now() < deadline, "the socket never appeared");
        std::thread::sleep(Duration::from_millis(20));
    }

    send(&child, "-HUP");
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        child.try_wait().expect("poll the service").is_none(),
        "SIGHUP ended the second service"
    );

    send(&child, "-TERM");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().expect("poll the service") {
            assert!(status.success(), "the second service exited with {status}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the second service did not exit within 10s of SIGTERM"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!socket.exists(), "the socket outlived the service");
}

fn send(child: &std::process::Child, signal: &str) {
    let sent = Command::new("kill")
        .args([signal, &child.id().to_string()])
        .status()
        .expect("send the signal");
    assert!(sent.success(), "kill {signal} failed");
}
