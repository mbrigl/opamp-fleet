//! The heads of the audit chains signed with the audit key on a token
//! (ADR-draft-the-heads-of-the-audit-chains-are-signed-with-a-key-on-the-token), on the
//! filesystem store: the writer signs through the token, the Server binary signs at its start and
//! its shutdown, and `server audit-verify --audit-key` checks the record against the key
//! `server pki status` prints. The tests that need a token skip without
//! `FLEET_TEST_PKCS11_MODULE`.
#![cfg(unix)]

mod ports;

#[path = "support/token.rs"]
mod token;

use std::io::{BufRead as _, BufReader};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::Arc;
use std::time::Duration;

use fleet_server::audit::Audit as _;
use fleet_server::audit_log::{AuditLog, Limits, Signing};
use fleet_server::checkpoint::{AuditKey, Check, CheckpointSigner as _, OneChain};
use token::TestToken;

/// Kills the server on drop, so a failing assertion never leaks the process.
struct ServerUnderTest(Child);

impl Drop for ServerUnderTest {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Waits until `done` holds, for ten seconds at most.
fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(std::time::Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Every line of the record in `dir`.
fn lines(dir: &Path) -> Vec<String> {
    fleet_server::fs::audit_lines(&fleet_server::fs::audit_files(dir).expect("files"))
        .expect("lines")
        .into_iter()
        .map(|(_, line)| line)
        .collect()
}

fn checkpoints(dir: &Path) -> Vec<String> {
    lines(dir)
        .into_iter()
        .filter(|line| line.contains("\"event\":\"audit.checkpoint\""))
        .collect()
}

/// With the audit key on a token, a checkpoint is written within an interval naming the chain's
/// head, and one at the close; the record verifies against the key's public half, and fails
/// against another key, and where the head a checkpoint names was changed.
/// Verifies: ADR-draft-the-heads-of-the-audit-chains-are-signed-with-a-key-on-the-token
#[test]
fn the_heads_are_signed_on_the_token_and_verify_against_its_public_half() {
    let Some(test) = TestToken::named() else {
        return;
    };
    test.generate("audit");
    let token = test.open();
    let key = token
        .key("[audit] key_label", "audit")
        .expect("the audit key");
    let spki = key.public_key_der();
    let dir = tempfile::tempdir().expect("dir");
    let audit = AuditLog::start_signing(
        Box::new(fleet_server::fs::FsAuditStore::open(dir.path()).expect("store")),
        Limits::default(),
        Arc::new(fleet_server::clock::SystemClock),
        None,
        None,
        Some(Signing {
            interval: Duration::from_millis(300),
            ..Signing::new(Arc::new(key), Box::new(OneChain::new(None)))
        }),
    )
    .expect("start");
    let record = dir.path().join(fleet_server::fs::AUDIT);
    audit
        .record(fleet_server::audit::Entry::new("host.admitted", "admitted"))
        .expect("record");
    until("a checkpoint within an interval", || {
        checkpoints(&record).len() == 1
    });
    audit
        .record(fleet_server::audit::Entry::new("host.admitted", "admitted"))
        .expect("record");
    assert!(audit.close(Duration::from_secs(5)));
    drop(audit);
    let all = lines(&record);
    let last = all.last().expect("a line");
    assert!(
        last.contains("\"event\":\"audit.checkpoint\""),
        "the close's: {last}"
    );
    let parsed = fleet_server::checkpoint::parse(last).expect("checkpoint");
    assert_eq!(parsed.heads.len(), 1);
    assert_eq!(parsed.heads[0].chain, "fs");
    assert_eq!(
        parsed.heads[0].hash,
        fleet_server::audit_log::line_hash(&all[all.len() - 2])
    );
    assert_eq!(
        parsed.key_sha256,
        fleet_server::checkpoint::key_sha256(&spki)
    );
    assert!(
        all.iter().all(|line| !line.contains("pkcs11.signed")),
        "a checkpoint is its own signature's entry"
    );

    let check = || Check::new(AuditKey::from_der(&spki).expect("key"), None);
    let report = fleet_server::fs::check_audit_checkpoints(&record, check()).expect("check");
    assert!(!report.failed, "{:?}", report.lines);
    assert_eq!(report.newest.as_deref(), Some(last.as_str()));

    let other = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("key");
    let other =
        AuditKey::from_der(&rcgen::PublicKeyData::subject_public_key_info(&other)).expect("key");
    let report =
        fleet_server::fs::check_audit_checkpoints(&record, Check::new(other, None)).expect("check");
    assert!(report.failed);
    assert!(
        report
            .lines
            .iter()
            .any(|line| line.contains("names the key")),
        "{:?}",
        report.lines
    );

    // The head the newest checkpoint names, changed.
    let file = fleet_server::fs::audit_files(&record)
        .expect("files")
        .pop()
        .expect("a file");
    let text = std::fs::read_to_string(&file).expect("read");
    let head = &all[all.len() - 2];
    std::fs::write(
        &file,
        text.replace(head, &head.replace("\"seq\"", "\"seq\" ")),
    )
    .expect("write");
    let report = fleet_server::fs::check_audit_checkpoints(&record, check()).expect("check");
    assert!(report.failed);
    assert!(
        report
            .lines
            .iter()
            .any(|line| line.starts_with("chain fs, entry") && line.contains("rewritten")),
        "{:?}",
        report.lines
    );
}

/// A `server.toml` for the Server binary in `dir`, with `extra` appended.
fn server_toml(dir: &Path, extra: &str) -> std::path::PathBuf {
    let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()]).expect("cert");
    std::fs::write(dir.join("cert.pem"), cert.cert.pem()).expect("cert");
    std::fs::write(dir.join("key.pem"), cert.signing_key.serialize_pem()).expect("key");
    let (agent, operator) = (ports::spare_port(), ports::spare_port());
    let config = dir.join("server.toml");
    std::fs::write(
        &config,
        format!(
            "listen = \"127.0.0.1:{agent}\"\ndata_dir = {data:?}\n\
             [tls]\ncert_file = {cert:?}\nkey_file = {key:?}\nclient_ca_file = {cert:?}\n\
             [rest]\nlisten = \"127.0.0.1:{operator}\"\n{extra}",
            data = dir.join("data"),
            cert = dir.join("cert.pem"),
            key = dir.join("key.pem"),
        ),
    )
    .expect("server.toml");
    config
}

/// Starts the Server on `config`, waits until it serves, stops it in order, and answers its log.
fn run_and_stop(config: &Path) -> Vec<String> {
    let mut server = ServerUnderTest(
        Command::new(env!("CARGO_BIN_EXE_server"))
            .arg("--config")
            .arg(config)
            .env("RUST_LOG", "info")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start the server"),
    );
    let stderr = server.0.stderr.take().expect("stderr");
    let (lines, logged) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let mut seen = Vec::new();
    loop {
        match logged.recv_timeout(Duration::from_secs(30)) {
            Ok(line) if line.contains("serving the REST API") => break,
            Ok(line) => seen.push(line),
            Err(_) => panic!("the server never came up:\n{}", seen.join("\n")),
        }
    }
    // SAFETY: kill(2) on the pid of a child this test started and has not yet reaped.
    let sent = unsafe { libc::kill(server.0.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(sent, 0, "send SIGTERM");
    let status = server.0.wait().expect("the server exits");
    assert!(status.success(), "{status:?}");
    seen.extend(logged.try_iter());
    seen
}

fn server(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_server"))
        .args(args)
        .output()
        .expect("run the server")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The Server binary with `[audit] key_label` signs a checkpoint at its start and its shutdown;
/// `server pki status` prints the key's public half, against which `server audit-verify
/// --audit-key` holds and prints the newest checkpoint, which holds again as `--since`.
/// Verifies: ADR-draft-the-heads-of-the-audit-chains-are-signed-with-a-key-on-the-token
#[test]
fn the_server_signs_its_heads_and_audit_verify_checks_them_against_pki_status() {
    let Some(test) = TestToken::named() else {
        return;
    };
    test.generate("audit");
    let dir = tempfile::tempdir().expect("dir");
    let config = server_toml(
        dir.path(),
        &format!("{}[audit]\nkey_label = \"audit\"\n", test.section()),
    );
    let log = run_and_stop(&config);
    assert!(
        !log.iter().any(|line| line.contains("are not signed")),
        "{log:?}"
    );
    let record = dir.path().join("data").join(fleet_server::fs::AUDIT);
    // One at the start; the shutdown's is skipped where nothing grew since.
    let all = lines(&record);
    assert!(
        all.last()
            .is_some_and(|line| line.contains("\"event\":\"audit.checkpoint\"")),
        "the record ends in a checkpoint: {all:?}"
    );

    let status = server(&[
        "pki".as_ref(),
        "status".as_ref(),
        "--config".as_ref(),
        config.as_os_str(),
    ]);
    let printed = text(&status.stdout);
    let begin = printed.find("-----BEGIN PUBLIC KEY-----").expect(&printed);
    let end = printed.find("-----END PUBLIC KEY-----").expect(&printed);
    let pem = format!("{}-----END PUBLIC KEY-----\n", &printed[begin..end]);
    let key = AuditKey::from_pem(pem.as_bytes()).expect("the printed key");
    assert!(printed.contains(key.sha256()), "{printed}");
    assert!(printed.contains("audit key \"audit\""), "{printed}");
    let pem_file = dir.path().join("audit.pem");
    std::fs::write(&pem_file, &pem).expect("pem");

    let verified = server(&[
        "audit-verify".as_ref(),
        record.as_os_str(),
        "--audit-key".as_ref(),
        pem_file.as_os_str(),
    ]);
    let out = text(&verified.stdout);
    assert!(verified.status.success(), "{out}{}", text(&verified.stderr));
    let newest = out
        .lines()
        .find_map(|line| line.strip_prefix("checkpoint: "))
        .expect(&out)
        .to_string();
    assert_eq!(Some(&newest), checkpoints(&record).last());
    let saved = dir.path().join("saved");
    std::fs::write(&saved, format!("{newest}\n")).expect("saved");
    let again = server(&[
        "audit-verify".as_ref(),
        record.as_os_str(),
        "--audit-key".as_ref(),
        pem_file.as_os_str(),
        "--since".as_ref(),
        saved.as_os_str(),
    ]);
    let out = text(&again.stdout);
    assert!(again.status.success(), "{out}{}", text(&again.stderr));
    assert!(out.contains("holds the saved checkpoint"), "{out}");
}

/// Without `[audit] key_label` the Server says at startup that the heads are not signed, writes no
/// checkpoint, and `server audit-verify --audit-key` fails naming the missing checkpoints; without
/// `--audit-key` it says it checked no signature.
/// Verifies: ADR-draft-the-heads-of-the-audit-chains-are-signed-with-a-key-on-the-token
#[test]
fn without_an_audit_key_no_checkpoint_is_written_and_the_server_says_so() {
    let dir = tempfile::tempdir().expect("dir");
    let config = server_toml(dir.path(), "");
    let log = run_and_stop(&config);
    assert!(
        log.iter()
            .any(|line| line.contains("the heads of the audit record are not signed")),
        "{log:?}"
    );
    let record = dir.path().join("data").join(fleet_server::fs::AUDIT);
    assert!(checkpoints(&record).is_empty());
    let unchecked = server(&["audit-verify".as_ref(), record.as_os_str()]);
    assert!(unchecked.status.success());
    assert!(text(&unchecked.stdout).contains("no signature was checked"));
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("key");
    let pem_file = dir.path().join("audit.pem");
    std::fs::write(&pem_file, key.public_key_pem()).expect("pem");
    let verified = server(&[
        "audit-verify".as_ref(),
        record.as_os_str(),
        "--audit-key".as_ref(),
        pem_file.as_os_str(),
    ]);
    assert!(!verified.status.success());
    assert!(
        text(&verified.stdout).contains("the record holds no checkpoint"),
        "{}",
        text(&verified.stdout)
    );
}

/// On the files store, a rotation a checkpoint covers is honoured; the oldest file deleted with an
/// `audit.rotated` forged after the newest checkpoint to explain it is reported and fails.
/// Verifies: ADR-draft-the-heads-of-the-audit-chains-are-signed-with-a-key-on-the-token
#[test]
fn a_rotation_counts_once_signed_and_a_forged_one_does_not() {
    let Some(test) = TestToken::named() else {
        return;
    };
    test.generate("audit");
    let token = test.open();
    let key = token
        .key("[audit] key_label", "audit")
        .expect("the audit key");
    let spki = key.public_key_der();
    let dir = tempfile::tempdir().expect("dir");
    let audit = AuditLog::start_signing(
        Box::new(fleet_server::fs::FsAuditStore::open(dir.path()).expect("store")),
        Limits {
            max_file_bytes: 4096,
            keep_files: 3,
        },
        Arc::new(fleet_server::clock::SystemClock),
        None,
        None,
        Some(Signing {
            interval: Duration::from_millis(100),
            ..Signing::new(Arc::new(key), Box::new(OneChain::new(None)))
        }),
    )
    .expect("start");
    let record = dir.path().join(fleet_server::fs::AUDIT);
    let rotated_then_signed = || {
        let all = lines(&record);
        all.iter()
            .position(|line| line.contains("\"event\":\"audit.rotated\""))
            .is_some_and(|at| {
                all[at..]
                    .iter()
                    .any(|line| line.contains("\"event\":\"audit.checkpoint\""))
            })
    };
    let mut n = 0u64;
    until("a rotation a checkpoint covers", || {
        n += 1;
        audit
            .record(fleet_server::audit::Entry::new("host.admitted", "admitted").with("n", n))
            .expect("record");
        std::thread::sleep(Duration::from_millis(10));
        rotated_then_signed()
    });
    assert!(audit.close(Duration::from_secs(5)));
    drop(audit);
    let check = || Check::new(AuditKey::from_der(&spki).expect("key"), None);
    let report = fleet_server::fs::check_audit_checkpoints(&record, check()).expect("check");
    assert!(!report.failed, "{:?}", report.lines);

    // The oldest file deleted, and an audit.rotated naming the first line left's prev appended.
    let files = fleet_server::fs::audit_files(&record).expect("files");
    std::fs::remove_file(&files[0]).expect("delete");
    let first_left = std::fs::read_to_string(&files[1]).expect("read");
    let first_left: serde_json::Value =
        serde_json::from_str(first_left.lines().next().expect("a line")).expect("json");
    let last = lines(&record).pop().expect("a line");
    let last_value: serde_json::Value = serde_json::from_str(&last).expect("json");
    let entry = fleet_server::audit::Entry::new("audit.rotated", "recorded")
        .with("deleted", "audit-1.jsonl")
        .with(
            "deleted_last_hash",
            first_left["prev"].as_str().expect("prev").to_string(),
        );
    let now = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("now")
            .as_millis(),
    )
    .expect("ms");
    let forged = fleet_server::audit_log::render(
        last_value["seq"].as_u64().expect("seq") + 1,
        now,
        &entry,
        &fleet_server::audit_log::line_hash(&last),
    );
    let newest = files.last().expect("a file");
    let mut text = std::fs::read_to_string(newest).expect("read");
    text.push_str(&forged);
    text.push('\n');
    std::fs::write(newest, text).expect("write");
    let report = fleet_server::fs::check_audit_checkpoints(&record, check()).expect("check");
    assert!(report.failed, "{:?}", report.lines);
    assert!(
        report
            .lines
            .iter()
            .any(|line| line.contains("the entries before it are gone")),
        "{:?}",
        report.lines
    );
    assert!(
        report.lines.iter().any(|line| line
            .contains("audit.rotated lies after every signed head the record still holds")),
        "{:?}",
        report.lines
    );
}
