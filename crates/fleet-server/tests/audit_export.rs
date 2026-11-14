//! The audit record's export
//! (ADR-0043
//! clause 9), seen from outside the Server binary: an OTLP/HTTP receiver in the test process, and a
//! decision on the Operator plane, whose `operator.act` entry the Server records before it runs.
#![cfg(unix)]

mod ports;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// What the log holds last once the server has closed its stderr.
const LOG_CLOSED: &str = "<stderr closed>";

/// Kills the server on drop, so a failing assertion never leaks the process.
struct ServerUnderTest(Child);

impl Drop for ServerUnderTest {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// What reached the receiver: one item per connection, then the path and body of each request.
enum Heard {
    Connected,
    Request {
        path: String,
        /// Each header as `name: value`, the name in lower case.
        headers: Vec<String>,
        body: Vec<u8>,
    },
}

/// An OTLP/HTTP receiver on a loopback port: it answers every request `200` with an empty
/// protobuf body, and tells what it heard.
fn receiver() -> (u16, mpsc::Receiver<Heard>) {
    receiver_over(None)
}

/// [`receiver`], behind TLS with `tls` where it is given: what is heard is what arrives through
/// a completed handshake.
fn receiver_over(tls: Option<Arc<rustls::ServerConfig>>) -> (u16, mpsc::Receiver<Heard>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind the receiver");
    let port = listener.local_addr().expect("address").port();
    let (heard, hearing) = mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            if heard.send(Heard::Connected).is_err() {
                break;
            }
            let (heard, tls) = (heard.clone(), tls.clone());
            std::thread::spawn(move || match tls {
                None => serve(stream, &heard),
                Some(tls) => {
                    let connection = rustls::ServerConnection::new(tls).expect("tls");
                    serve(rustls::StreamOwned::new(connection, stream), &heard);
                }
            });
        }
    });
    (port, hearing)
}

/// Reads the requests on one connection and answers each.
fn serve(stream: impl Read + Write, heard: &mpsc::Sender<Heard>) {
    let mut reader = BufReader::new(stream);
    loop {
        let mut request_line = String::new();
        if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
            return;
        }
        let path = request_line
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .to_string();
        let mut length = 0;
        let mut headers = Vec::new();
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).unwrap_or(0) == 0 {
                return;
            }
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap_or(0);
                }
                headers.push(format!("{}: {}", name.to_ascii_lowercase(), value.trim()));
            }
        }
        let mut body = vec![0; length];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        let _ = heard.send(Heard::Request {
            path,
            headers,
            body,
        });
        let answer = "HTTP/1.1 200 OK\r\ncontent-type: application/x-protobuf\r\n\
                      content-length: 0\r\n\r\n";
        let stream = reader.get_mut();
        if stream.write_all(answer.as_bytes()).is_err() || stream.flush().is_err() {
            return;
        }
    }
}

/// A receiver's TLS: a certificate for 127.0.0.1 signed by a CA of the test's own, offering
/// `versions` alone. Answers the configuration and the CA's PEM.
fn receiver_tls(
    versions: &[&'static rustls::SupportedProtocolVersion],
) -> (Arc<rustls::ServerConfig>, String) {
    use rcgen::{CertificateParams, IsCa, Issuer, KeyPair};
    let ca_key = KeyPair::generate().expect("ca key");
    let mut params = CertificateParams::new(vec!["audit export test ca".to_string()]).expect("ca");
    params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca = params.self_signed(&ca_key).expect("ca");
    let issuer = Issuer::from_ca_cert_pem(&ca.pem(), ca_key).expect("issuer");
    let key = KeyPair::generate().expect("key");
    let leaf = CertificateParams::new(vec!["127.0.0.1".to_string()])
        .expect("leaf")
        .signed_by(&key, &issuer)
        .expect("signed");
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(versions)
    .expect("versions")
    .with_no_client_auth()
    .with_single_cert(
        vec![leaf.der().clone()],
        rustls::pki_types::PrivateKeyDer::try_from(key.serialize_der()).expect("key der"),
    )
    .expect("server config");
    (Arc::new(config), ca.pem())
}

/// The Server binary on a fresh data directory, `audit` appended to its `server.toml`, with
/// `env` in its environment; answers it, the Operator plane's port, the PEM of its certificate,
/// and its log as it comes.
fn start(
    dir: &Path,
    audit: &str,
    env: &[(&str, String)],
) -> (ServerUnderTest, u16, String, Arc<Mutex<Vec<String>>>) {
    let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()])
        .expect("a self-signed certificate");
    std::fs::write(dir.join("cert.pem"), cert.cert.pem()).expect("write cert");
    std::fs::write(dir.join("key.pem"), cert.signing_key.serialize_pem()).expect("write key");
    let (agent, operator) = (ports::spare_port(), ports::spare_port());
    let config = dir.join("server.toml");
    std::fs::write(
        &config,
        format!(
            concat!(
                "listen = \"127.0.0.1:{agent}\"\n",
                "data_dir = {data:?}\n",
                "[tls]\n",
                "cert_file = {cert:?}\n",
                "key_file = {key:?}\n",
                "client_ca_file = {cert:?}\n",
                "[rest]\n",
                "listen = \"127.0.0.1:{operator}\"\n",
                "{audit}",
            ),
            agent = agent,
            operator = operator,
            data = dir.join("data"),
            cert = dir.join("cert.pem"),
            key = dir.join("key.pem"),
            audit = audit,
        ),
    )
    .expect("write server.toml");
    let mut command = Command::new(env!("CARGO_BIN_EXE_server"));
    command
        .arg("--config")
        .arg(&config)
        .env("RUST_LOG", "info")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for (name, value) in env {
        command.env(name, value);
    }
    let mut server = ServerUnderTest(command.spawn().expect("start the server"));
    let stderr = server.0.stderr.take().expect("stderr");
    let (lines, logged) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = lines.send(line);
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
    // The rest of the log is kept, and drained so the server never blocks on a full pipe.
    let log = Arc::new(Mutex::new(Vec::new()));
    let kept = log.clone();
    std::thread::spawn(move || {
        while let Ok(line) = logged.recv() {
            kept.lock().expect("log").push(line);
        }
        // The server closed its stderr: the log is whole.
        kept.lock().expect("log").push(LOG_CLOSED.to_string());
    });
    (server, operator, cert.cert.pem(), log)
}

/// A mutating request on the Operator plane, which the Server records as `operator.act` before
/// it runs.
fn act(operator: u16, cert: &str) {
    acts(operator, cert, 1);
}

/// `count` acts, one after the other on one connection; each must be answered, whatever its
/// status.
fn acts(operator: u16, cert: &str, count: usize) {
    opamp::tls::install_ring_provider();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let client = reqwest::Client::builder()
            .add_root_certificate(reqwest::Certificate::from_pem(cert.as_bytes()).expect("pem"))
            .build()
            .expect("client");
        for _ in 0..count {
            client
                .post(format!(
                    "https://127.0.0.1:{operator}/api/v1/agents/00000000000000000000000000000001/restart"
                ))
                .send()
                .await
                .expect("the Operator plane answers");
        }
    });
}

/// The `operator.act` line the record holds, once the store has accepted it.
fn stored_act(data: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let found = std::fs::read_dir(data.join("audit"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|file| std::fs::read_to_string(file.path()).ok())
            .flat_map(|text| text.lines().map(str::to_string).collect::<Vec<_>>())
            .find(|line| line.contains("\"event\":\"operator.act\""));
        if let Some(line) = found {
            return line;
        }
        assert!(Instant::now() < deadline, "the record never held the act");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Without `[audit.export]` the Server opens no export connection, the OpenTelemetry SDK's own
/// variables naming a receiver in its environment notwithstanding.
/// Verifies: ADR-0043, ADR-0012
#[test]
fn without_an_export_section_the_server_opens_no_export_connection() {
    let (port, hearing) = receiver();
    let dir = tempfile::tempdir().expect("tempdir");
    let named = format!("http://127.0.0.1:{port}");
    let (_server, operator, cert, _) = start(
        dir.path(),
        "",
        &[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", named.clone()),
            (
                "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
                format!("{named}/v1/logs"),
            ),
        ],
    );
    act(operator, &cert);
    stored_act(&dir.path().join("data"));
    // Longer than a batch takes to be sent once an entry is written.
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        hearing.try_recv().is_err(),
        "the server connected to a receiver no [audit.export] named"
    );
}

/// With `[audit.export]` naming a receiver, a decision recorded reaches it as an OTLP log record
/// whose body is the line the store holds, with its sequence number, time and previous hash, on
/// the path the endpoint names.
/// Verifies: ADR-0043, ADR-0012
#[test]
fn a_recorded_decision_reaches_the_export_receiver_as_the_stored_line() {
    let (port, hearing) = receiver();
    let dir = tempfile::tempdir().expect("tempdir");
    let (_server, operator, cert, _) = start(
        dir.path(),
        &format!("[audit.export]\nendpoint = \"http://127.0.0.1:{port}/v1/audit-logs\"\n"),
        &[],
    );
    act(operator, &cert);
    let line = stored_act(&dir.path().join("data"));
    assert!(
        line.contains("\"seq\":") && line.contains("\"prev\":"),
        "{line}"
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match hearing.recv_timeout(left) {
            Ok(Heard::Request { path, body, .. }) => {
                assert_eq!(path, "/v1/audit-logs");
                if body
                    .windows(line.len())
                    .any(|window| window == line.as_bytes())
                {
                    break;
                }
            }
            Ok(Heard::Connected) => {}
            Err(_) => panic!("the receiver never got the act's line {line}"),
        }
    }
}

/// The lines of the audit record under the data directory in `dir`.
fn record_of(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir.join("data").join("audit"))
        .expect("audit")
        .flatten()
        .filter_map(|file| std::fs::read_to_string(file.path()).ok())
        .flat_map(|text| text.lines().map(str::to_string).collect::<Vec<_>>())
        .collect()
}

/// With `[audit.export]` naming a receiver that takes connections and never answers, the Server
/// still answers every act, its export drops what its queue cannot hold, and the count of what it
/// dropped reaches the record as `audit.export_dropped` at shutdown.
/// Verifies: ADR-0043, ADR-0028
#[test]
fn a_silent_receiver_blocks_no_decision_and_the_dropped_count_reaches_the_record() {
    // Bound and never accepted from: a connection completes and then hears nothing.
    let silent = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = silent.local_addr().expect("address").port();
    let dir = tempfile::tempdir().expect("tempdir");
    let (server, operator, cert, log) = start(
        dir.path(),
        &format!("[audit.export]\nendpoint = \"http://127.0.0.1:{port}/v1/logs\"\n"),
        &[],
    );
    // Two entries each: more than the queue and the batch in flight hold together.
    let started = Instant::now();
    acts(operator, &cert, 1500);
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "the acts waited on the export"
    );

    // Waits until the record holds more than `after` of the acts' entries; only a defect takes a
    // minute.
    let holds_more_than = |after: usize| {
        let deadline = Instant::now() + Duration::from_secs(60); // timed: a deadline, not a wait
        loop {
            let written = record_of(dir.path())
                .iter()
                .filter(|line| line.contains("\"event\":\"operator.act\""))
                .count();
            if written > after {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the record holds {written} of the acts' entries, not more than {after}"
            );
            std::thread::sleep(Duration::from_millis(50)); // timed: the wait's poll
        }
    };
    // The shutdown counts what was dropped before it writes the record out: a line the writer had
    // not yet handed to the export then reaches it after the count, and is logged, not counted in
    // the record, when it is dropped. The writer hands a batch to the export only once the store
    // holds it, so a record that holds all 3000 entries may not yet have handed over the last
    // batch. It has once the record holds a line written after them: the writer writes the next
    // batch only after it handed over the one before. So the test acts once more, and stops the
    // server once the record holds that act's entry too.
    holds_more_than(2999);
    act(operator, &cert);
    holds_more_than(3000);

    // SAFETY: kill(2) on the pid of a child this test started and has not yet reaped.
    let sent = unsafe { libc::kill(server.0.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(sent, 0, "send SIGTERM");
    let mut server = server;
    let deadline = Instant::now() + Duration::from_secs(60);
    while server.0.try_wait().expect("poll").is_none() {
        assert!(Instant::now() < deadline, "the server did not exit");
        std::thread::sleep(Duration::from_millis(50));
    }
    let record = record_of(dir.path());
    let dropped: u64 = record
        .iter()
        .filter(|line| line.contains("\"event\":\"audit.export_dropped\""))
        .map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).expect("json");
            value["count"].as_u64().expect("count")
        })
        .sum();
    // At least what neither the queue nor a batch in flight holds of the 3000 entries.
    assert!(
        dropped >= 3000 - 2048 - 512,
        "{dropped} dropped in the record"
    );
    // Everything the server wrote before it exited, a panic of a thread at exit included.
    let deadline = Instant::now() + Duration::from_secs(30);
    while !log
        .lock()
        .expect("log")
        .iter()
        .any(|line| line == LOG_CLOSED)
    {
        assert!(
            Instant::now() < deadline,
            "the server's stderr never closed"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let log = log.lock().expect("log").clone();
    assert!(
        log.iter()
            .any(|line| line.contains("did not reach the export's receiver")
                && line.contains("waiting")),
        "the shutdown logged no count of what it left behind:\n{}",
        log.join("\n")
    );
    assert!(
        !log.iter()
            .any(|line| line.contains("panicked") || line.contains("timer error")),
        "{}",
        log.join("\n")
    );
    drop(silent);
}

/// With `[audit.export]` set, the `OTEL_*` variables in the Server's environment change nothing
/// the export sends: no header they name reaches the receiver, a credential included, nor the
/// exporter's own `User-Agent`; the compression they name is not taken up.
/// Verifies: ADR-0043, ADR-0012
#[test]
fn the_export_sends_no_header_the_environment_names() {
    let (port, hearing) = receiver();
    let dir = tempfile::tempdir().expect("tempdir");
    let (_server, operator, cert, _) = start(
        dir.path(),
        &format!("[audit.export]\nendpoint = \"http://127.0.0.1:{port}/v1/logs\"\n"),
        &[
            (
                "OTEL_EXPORTER_OTLP_HEADERS",
                "authorization=Bearer from-the-environment,x-from-env=1".to_string(),
            ),
            (
                "OTEL_EXPORTER_OTLP_LOGS_HEADERS",
                "authorization=Basic ZnJvbTplbnY=".to_string(),
            ),
            ("OTEL_EXPORTER_OTLP_COMPRESSION", "gzip".to_string()),
            ("OTEL_EXPORTER_OTLP_LOGS_PROTOCOL", "http/json".to_string()),
        ],
    );
    act(operator, &cert);
    let line = stored_act(&dir.path().join("data"));
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match hearing.recv_timeout(left) {
            Ok(Heard::Request { headers, body, .. }) => {
                for header in &headers {
                    let name = header.split(':').next().unwrap_or_default();
                    assert!(
                        ![
                            "authorization",
                            "x-from-env",
                            "user-agent",
                            "content-encoding"
                        ]
                        .contains(&name),
                        "the export sent {header}"
                    );
                }
                if body
                    .windows(line.len())
                    .any(|window| window == line.as_bytes())
                {
                    break;
                }
            }
            Ok(Heard::Connected) => {}
            Err(_) => panic!("the receiver never got the act's line {line}"),
        }
    }
}

/// Over `https://` the export trusts the root `ca_file` names: a TLS 1.3 receiver whose
/// certificate a CA of the test's own signed gets the line. A receiver that speaks TLS 1.2 alone
/// gets nothing, as one whose root is not named does not either.
/// Verifies: ADR-0043, ADR-0012
#[test]
fn over_https_the_export_trusts_ca_file_and_speaks_tls_1_3_alone() {
    let (tls13, ca13) = receiver_tls(&[&rustls::version::TLS13]);
    let (tls12, ca12) = receiver_tls(&[&rustls::version::TLS12]);
    let (unnamed, _) = receiver_tls(&[&rustls::version::TLS13]);
    for (tls, ca, reaches) in [
        (tls13, ca13, true),
        (tls12, ca12, false),
        (unnamed, receiver_tls(&[&rustls::version::TLS13]).1, false),
    ] {
        let (port, hearing) = receiver_over(Some(tls));
        let dir = tempfile::tempdir().expect("tempdir");
        let ca_file = dir.path().join("receiver-ca.pem");
        std::fs::write(&ca_file, ca).expect("write ca");
        let (_server, operator, cert, _) = start(
            dir.path(),
            &format!(
                "[audit.export]\nendpoint = \"https://127.0.0.1:{port}/v1/logs\"\nca_file = {ca_file:?}\n"
            ),
            &[],
        );
        act(operator, &cert);
        let line = stored_act(&dir.path().join("data"));
        let wait = if reaches { 30 } else { 3 };
        let deadline = Instant::now() + Duration::from_secs(wait);
        let (mut got, mut connected) = (false, 0);
        while let Ok(heard) =
            hearing.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            if matches!(heard, Heard::Connected) {
                connected += 1;
            }
            if let Heard::Request { body, .. } = heard {
                if body
                    .windows(line.len())
                    .any(|window| window == line.as_bytes())
                {
                    got = true;
                    break;
                }
            }
        }
        assert_eq!(
            got, reaches,
            "port {port}: the line reached the receiver: {got}"
        );
        // Refused at the handshake, not never tried.
        assert!(connected >= 1, "port {port}: the export never connected");
    }
}
