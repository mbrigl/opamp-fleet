//! `server pki` as an operator runs it (ADR-0029): the commands, what they print and how they
//! exit, and the running Server's warning about a certificate it depends on that is ending.
#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

fn server(args: &[&str], cwd: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_server"))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run the server binary")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Kills the server on drop, so a failing assertion never leaks the process.
struct ServerUnderTest(Child);

impl Drop for ServerUnderTest {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// Verifies: ADR-0029
#[test]
fn the_commands_make_inspect_and_refuse_as_an_operator_sees_them() {
    let dir = tempfile::tempdir().expect("tempdir");
    let init = [
        "pki",
        "init",
        "--server-dir",
        "srv",
        "--offline-dir",
        "off",
        "--name",
        "fleet.example.com",
    ];
    let made = server(&init, dir.path());
    assert!(made.status.success(), "{}", text(&made.stderr));
    let printed = text(&made.stdout);
    let serial = printed
        .lines()
        .find_map(|line| line.strip_prefix("bootstrap certificate serial: "))
        .expect("the bootstrap serial is printed");

    let again = server(&init, dir.path());
    assert_eq!(again.status.code(), Some(1));
    assert!(text(&again.stderr).contains("exists; nothing was written"));

    // A fresh set: nothing is ending, a fresh bootstrap certificate included.
    let status = server(&["pki", "status", "--offline-dir", "off"], dir.path());
    assert_eq!(status.status.code(), Some(0), "{}", text(&status.stdout));
    assert!(
        text(&status.stdout).contains(serial),
        "status lists the bootstrap certificate by its serial"
    );

    let renewed = server(
        &[
            "pki",
            "bootstrap-cert",
            "--offline-dir",
            "off",
            "--out",
            "next",
        ],
        dir.path(),
    );
    assert!(renewed.status.success(), "{}", text(&renewed.stderr));
    let gateway = server(
        &[
            "pki",
            "server-cert",
            "--offline-dir",
            "off",
            "--name",
            "gw.example.com",
            "--out",
            "gw",
        ],
        dir.path(),
    );
    assert!(gateway.status.success(), "{}", text(&gateway.stderr));

    let refused = server(&["pki", "init", "--server-dir", "a"], dir.path());
    assert_eq!(
        refused.status.code(),
        Some(2),
        "a missing flag is a usage error"
    );
}

/// A CA that lived long and ends in ten days, in place of the client CA `pki init` made.
fn ending_ca(file: &Path) {
    let key = rcgen::KeyPair::generate().expect("key");
    let mut params = rcgen::CertificateParams::default();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "opamp-fleet client CA");
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(3600);
    params.not_after = now + time::Duration::days(10);
    std::fs::remove_file(file).expect("remove");
    std::fs::write(file, params.self_signed(&key).expect("ca").pem()).expect("write");
}

// Verifies: ADR-0029
#[test]
fn the_server_warns_at_startup_about_a_ca_that_is_ending_and_serves_on() {
    let dir = tempfile::tempdir().expect("tempdir");
    let made = server(
        &[
            "pki",
            "init",
            "--server-dir",
            "srv",
            "--offline-dir",
            "off",
            "--name",
            "localhost",
        ],
        dir.path(),
    );
    assert!(made.status.success(), "{}", text(&made.stderr));
    ending_ca(&dir.path().join("srv/client-ca.pem"));

    let agent = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let operator = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let config = dir.path().join("server.toml");
    std::fs::write(
        &config,
        format!(
            "listen = \"127.0.0.1:{}\"\nconfig_dir = {:?}\npackages_dir = {:?}\n{}\n[rest]\nlisten = \"127.0.0.1:{}\"\n",
            agent.local_addr().expect("addr").port(),
            dir.path().join("configs").display().to_string(),
            dir.path().join("packages").display().to_string(),
            std::fs::read_to_string(dir.path().join("srv/server.toml.fragment")).expect("fragment"),
            operator.local_addr().expect("addr").port(),
        ),
    )
    .expect("write server.toml");
    drop((agent, operator));

    let mut running = ServerUnderTest(
        Command::new(env!("CARGO_BIN_EXE_server"))
            .arg("--config")
            .arg(&config)
            .env("RUST_LOG", "info")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start the server"),
    );
    let (lines, logged) = mpsc::channel();
    let stderr = running.0.stderr.take().expect("stderr");
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let mut warned = false;
    loop {
        match logged.recv_timeout(Duration::from_secs(30)) {
            Ok(line) if line.contains("ends soon") && line.contains("client-ca.pem") => {
                warned = true;
            }
            Ok(line) if line.contains("serving the REST API") => break,
            Ok(_) => {}
            Err(_) => panic!("the server never came up"),
        }
    }
    assert!(
        warned,
        "the ending client CA was not warned about before serving"
    );

    // The audit writer runs on its own; give it a moment to reach the file.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let audit = std::fs::read_dir(dir.path().join("configs/audit"))
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| std::fs::read_to_string(entry.path()).unwrap_or_default())
                    .collect::<String>()
            })
            .unwrap_or_default();
        if audit.contains("\"pki.expiring\"") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no pki.expiring entry in the audit: {audit}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
