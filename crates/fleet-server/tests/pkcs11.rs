//! The CA keys on a PKCS#11 token, against SoftHSM2: the token signs, the Server builds and
//! audits every signature
//! (ADR-0043).
//! Each test makes a token of its own; without `FLEET_TEST_PKCS11_MODULE` it says so and skips.
#![cfg(unix)]

mod ports;

#[path = "support/token.rs"]
mod token;

use std::sync::Arc;

use fleet_server::bootstrap::BootstrapSigner as _;
use fleet_server::ca::{BootstrapCa, ClientCa, UseCase};
use fleet_server::config::{ClientCaConfig, EnrolmentConfig};
use fleet_server::revocation::SubjectValues;
use rcgen::{KeyPair, PublicKeyData as _, SanType, SigningKey as _};
use token::{Recorded, TestToken};
use x509_parser::prelude::{FromDer, X509Certificate};

/// A CSR for a fresh key, with no proof.
fn csr() -> String {
    rcgen::CertificateParams::new(Vec::<String>::new())
        .expect("params")
        .serialize_request(&KeyPair::generate().expect("key"))
        .expect("csr")
        .pem()
        .expect("pem")
}

/// A CSR for a fresh key that proves it renews `old_pem`, whose key is `old_key`.
fn renewing(old_pem: &str, old_key: &KeyPair) -> String {
    use base64::Engine as _;
    use sha2::{Digest, Sha256};
    let new_key = KeyPair::generate().expect("new key");
    let old_der = der(old_pem);
    let signature = old_key
        .sign(&fleet_core::renewal::statement(&Sha256::digest(
            new_key.subject_public_key_info(),
        )))
        .expect("sign");
    let proof = format!(
        "{}{}",
        fleet_core::renewal::URI_PREFIX,
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(fleet_core::renewal::encode(&old_der, &signature))
    );
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("params");
    params
        .subject_alt_names
        .push(SanType::URI(proof.try_into().expect("uri")));
    params
        .serialize_request(&new_key)
        .expect("csr")
        .pem()
        .expect("pem")
}

fn der(pem: &str) -> Vec<u8> {
    opamp::tls::certificates(pem.as_bytes()).expect("pem")[0]
        .as_ref()
        .to_vec()
}

/// Whether the certificate `pem` was signed by the key of the CA `ca_pem`.
fn signed_by(pem: &str, ca_pem: &str) -> bool {
    let (leaf, ca) = (der(pem), der(ca_pem));
    let (_, leaf) = X509Certificate::from_der(&leaf).expect("leaf");
    let (_, ca) = X509Certificate::from_der(&ca).expect("ca");
    leaf.verify_signature(Some(ca.public_key())).is_ok()
}

fn client_ca_on(
    dir: &std::path::Path,
    cert_file: std::path::PathBuf,
    label: &str,
) -> ClientCaConfig {
    let _ = dir;
    ClientCaConfig {
        cert_file,
        key_file: None,
        key_label: Some(label.to_string()),
        validity_days: 30,
    }
}

/// A host certificate signed through the token verifies against the CA certificate whose key is on
/// it, and the signature wrote `pkcs11.signed` naming the key, the issuer and serial, and the use
/// case; so does a bootstrap certificate.
/// Verifies: ADR-0043
#[test]
fn the_token_signs_and_every_signature_is_audited() {
    let Some(test) = TestToken::named() else {
        return;
    };
    let dir = tempfile::tempdir().expect("dir");
    let token = test.open();
    let (ca_pem, ca_file) = test.ca(&token, "client-ca", "OpAMP Fleet Client CA", dir.path());
    let (bootstrap_pem, bootstrap_file) = test.ca(
        &token,
        "bootstrap-ca",
        "OpAMP Fleet Bootstrap CA",
        dir.path(),
    );
    let recorded = Recorded::default();
    let entries = recorded.entries.clone();
    token.set_audit(Arc::new(recorded));
    let client_config = client_ca_on(dir.path(), ca_file, "client-ca");
    let ca = ClientCa::from_config_on(&client_config, Some(&token)).expect("client CA");
    assert_eq!(ca.key_label(), Some("client-ca"));

    // A host renews: the token signs, and the signature is audited.
    let first = ca
        .sign(
            &csr(),
            "host-1",
            &SubjectValues::default(),
            UseCase::MemberCsr,
        )
        .expect("signed");
    assert!(signed_by(&first.pem, &ca_pem), "{}", first.pem);
    let renewed = ca
        .sign(
            &csr(),
            "host-1",
            &SubjectValues::default(),
            UseCase::Renewal,
        )
        .expect("renewed");
    assert!(signed_by(&renewed.pem, &ca_pem));
    let entries_now = entries.lock().expect("entries").clone();
    assert_eq!(entries_now.len(), 2, "{entries_now:?}");
    let last = &entries_now[1];
    assert_eq!(last.event, "pkcs11.signed");
    for (key, value) in [
        ("key_label", "client-ca"),
        ("use_case", "renewal"),
        ("issuer", renewed.facts.id.issuer.as_str()),
        ("serial", renewed.facts.id.serial.as_str()),
    ] {
        assert_eq!(
            last.fields.get(key).map(|field| format!("{field:?}")),
            Some(format!(
                "{:?}",
                fleet_server::audit::Field::Text(value.to_string())
            )),
            "{key}: {last:?}"
        );
    }

    // A bootstrap certificate on an operator's request.
    let enrolment = EnrolmentConfig {
        bootstrap_ca_file: bootstrap_file,
        bootstrap_ca_key_file: None,
        bootstrap_ca_key_label: Some("bootstrap-ca".to_string()),
        max_pending_per_bootstrap: 1,
    };
    let bootstrap = BootstrapCa::from_config_on(&enrolment, Some(&client_config), Some(&token))
        .expect("bootstrap CA")
        .expect("one that issues");
    let now = u64::try_from(time::OffsetDateTime::now_utc().unix_timestamp()).expect("now") * 1000;
    let pair = bootstrap
        .sign(
            &[(
                fleet_server::bootstrap::Attribute::Organization,
                "Acme".to_string(),
            )],
            now,
            now + 86_400_000,
            "a password of sufficient length",
        )
        .expect("bootstrap certificate");
    assert!(signed_by(&pair.certificate_pem, &bootstrap_pem));
    let entries_now = entries.lock().expect("entries").clone();
    let last = entries_now.last().expect("an entry");
    assert_eq!(
        last.fields
            .get("use_case")
            .map(|field| format!("{field:?}")),
        Some(format!(
            "{:?}",
            fleet_server::audit::Field::Text("bootstrap_issue".to_string())
        ))
    );
    assert_eq!(
        last.fields
            .get("key_label")
            .map(|field| format!("{field:?}")),
        Some(format!(
            "{:?}",
            fleet_server::audit::Field::Text("bootstrap-ca".to_string())
        ))
    );
}

/// With no place in the record for its entry, nothing is signed with a token key.
/// Verifies: ADR-0043
#[test]
fn without_a_record_nothing_is_signed() {
    let Some(test) = TestToken::named() else {
        return;
    };
    let dir = tempfile::tempdir().expect("dir");
    let token = test.open();
    let (_, ca_file) = test.ca(&token, "client-ca", "OpAMP Fleet Client CA", dir.path());
    let ca = ClientCa::from_config_on(
        &client_ca_on(dir.path(), ca_file, "client-ca"),
        Some(&token),
    )
    .expect("client CA");
    // No record handed to the token yet, then one that refuses.
    let err = ca
        .sign(
            &csr(),
            "host-1",
            &SubjectValues::default(),
            UseCase::MemberCsr,
        )
        .expect_err("no record");
    assert!(err.contains("nothing is signed"), "{err}");
    token.set_audit(Arc::new(Recorded {
        refuse: true,
        ..Recorded::default()
    }));
    let err = ca
        .sign(
            &csr(),
            "host-1",
            &SubjectValues::default(),
            UseCase::MemberCsr,
        )
        .expect_err("a record that refuses");
    assert!(err.contains("nothing is signed"), "{err}");
}

/// A key file signs as before and writes no `pkcs11.signed`: the client CA moves to the token, a
/// renewal proof from the old CA in `client_ca_file` is accepted, and the token's key signs the
/// renewal.
/// Verifies: ADR-0043
#[test]
fn a_host_moves_to_the_token_key_at_its_ordinary_renewal() {
    let Some(test) = TestToken::named() else {
        return;
    };
    let dir = tempfile::tempdir().expect("dir");
    // The fleet's CA as it was: a key file.
    let old_key = KeyPair::generate().expect("old CA key");
    let mut params = rcgen::CertificateParams::default();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "OpAMP Fleet Client CA");
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let old_pem = params.self_signed(&old_key).expect("old CA").pem();
    std::fs::write(dir.path().join("old.pem"), &old_pem).expect("old CA file");
    std::fs::write(dir.path().join("old-key.pem"), old_key.serialize_pem()).expect("old key file");
    let old_ca = ClientCa::from_config(&ClientCaConfig {
        cert_file: dir.path().join("old.pem"),
        key_file: Some(dir.path().join("old-key.pem")),
        key_label: None,
        validity_days: 30,
    })
    .expect("old CA");
    let host_key = KeyPair::generate().expect("host key");
    let host_csr = rcgen::CertificateParams::new(Vec::<String>::new())
        .expect("params")
        .serialize_request(&host_key)
        .expect("csr")
        .pem()
        .expect("pem");
    let held = old_ca
        .sign(
            &host_csr,
            "host-9",
            &SubjectValues::default(),
            UseCase::MemberCsr,
        )
        .expect("a certificate of the old CA");

    // The new CA on the token; client_ca_file names both.
    let token = test.open();
    let recorded = Recorded::default();
    let entries = recorded.entries.clone();
    token.set_audit(Arc::new(recorded));
    let (new_pem, new_file) = test.ca(&token, "client-ca", "OpAMP Fleet Client CA", dir.path());
    let new_ca = ClientCa::from_config_on(
        &client_ca_on(dir.path(), new_file, "client-ca"),
        Some(&token),
    )
    .expect("new CA");
    let request = renewing(&held.pem, &host_key);
    assert!(
        new_ca.renewal_proof(&request).is_err(),
        "without the old CA beside it, the proof is not the new CA's"
    );
    let new_ca = new_ca.trusting(vec![der(&old_pem), der(&new_pem)]);
    let predecessor = new_ca
        .renewal_proof(&request)
        .expect("the old CA's proof holds")
        .expect("a proof");
    assert_eq!(predecessor.host.as_deref(), Some("host-9"));
    let renewed = new_ca
        .sign(
            &request,
            "host-9",
            &predecessor.subject_values,
            UseCase::Renewal,
        )
        .expect("renewed");
    assert!(
        signed_by(&renewed.pem, &new_pem),
        "the token's key signs the renewal"
    );
    assert!(!signed_by(&renewed.pem, &old_pem));
    let entries = entries.lock().expect("entries").clone();
    assert_eq!(
        entries.len(),
        1,
        "the key file's signature wrote none: {entries:?}"
    );
}

/// A PIN file that others may read fails startup naming it; a token that is not there and a key
/// that is not on it fail naming them.
/// Verifies: ADR-0043
#[test]
fn what_is_not_there_fails_startup_naming_it() {
    use std::os::unix::fs::PermissionsExt as _;
    let Some(test) = TestToken::named() else {
        return;
    };
    std::fs::set_permissions(
        &test.config.pin_file,
        std::fs::Permissions::from_mode(0o644),
    )
    .expect("mode");
    let err = fleet_server::pkcs11::Token::open(&test.config).expect_err("a PIN file others read");
    assert!(err.contains("[pkcs11] pin_file"), "{err}");
    std::fs::set_permissions(
        &test.config.pin_file,
        std::fs::Permissions::from_mode(0o640),
    )
    .expect("mode");
    let token = test.open();
    let err = token
        .key("[client_ca] key_label", "no-such-key")
        .expect_err("no key");
    assert!(
        err.contains("[client_ca] key_label \"no-such-key\""),
        "{err}"
    );
    let missing = fleet_server::config::Pkcs11Config {
        module: test.config.module.clone(),
        token_label: "no-such-token".to_string(),
        pin_file: test.config.pin_file.clone(),
    };
    let err = fleet_server::pkcs11::Token::open(&missing).expect_err("no token");
    assert!(err.contains("token_label \"no-such-token\""), "{err}");
    let wrong = test.config.pin_file.with_file_name("wrong-pin");
    std::fs::write(&wrong, "not-the-pin").expect("wrong PIN");
    std::fs::set_permissions(&wrong, std::fs::Permissions::from_mode(0o600)).expect("mode");
    // The user is logged in already through the open token, so a wrong PIN is tried on a token
    // with no session of this process: one made for it.
    drop(token);
    let err = fleet_server::pkcs11::Token::open(&fleet_server::config::Pkcs11Config {
        pin_file: wrong,
        ..missing_with(&test)
    })
    .expect_err("a PIN the token refuses");
    assert!(err.contains("refuses the PIN"), "{err}");
}

fn missing_with(test: &TestToken) -> fleet_server::config::Pkcs11Config {
    fleet_server::config::Pkcs11Config {
        module: test.config.module.clone(),
        token_label: test.config.token_label.clone(),
        pin_file: test.config.pin_file.clone(),
    }
}

/// A label two tokens of the module carry names no token: startup fails saying so, rather than
/// taking the first.
/// Verifies: ADR-0043
#[test]
fn a_label_two_tokens_carry_fails_startup() {
    let Some(test) = TestToken::named() else {
        return;
    };
    let context = fleet_core::pkcs11::context(&test.config.module).expect("module");
    let free = context
        .get_all_slots()
        .expect("slots")
        .into_iter()
        .find(|slot| {
            context
                .get_token_info(*slot)
                .is_ok_and(|info| !info.token_initialized())
        })
        .expect("a slot with a token not yet initialised");
    context
        .init_token(
            free,
            &cryptoki::types::AuthPin::from("the-twin-so-pin"),
            &test.config.token_label,
        )
        .expect("a second token with the label");
    let err =
        fleet_server::pkcs11::Token::open(&test.config).expect_err("two tokens carry the label");
    assert!(err.contains("2 tokens carry this label"), "{err}");
}

/// A module whose manufacturer is `SoftHSM` is a software token: the Server says so in its log
/// and writes `pkcs11.software_token` at every start.
/// Verifies: ADR-0043
#[test]
fn a_software_token_says_so() {
    let Some(test) = TestToken::named() else {
        return;
    };
    let token = test.open();
    assert!(token.is_software());
    let recorded = Recorded::default();
    token.warn_if_software(&recorded);
    let entries = recorded.entries.lock().expect("entries").clone();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].event, "pkcs11.software_token");
}

/// The bootstrap CA's key on the token is checked against `bootstrap_ca_file` and against the
/// client CA's key, as a key file is.
/// Verifies: ADR-0040
#[test]
fn a_bootstrap_key_on_the_token_is_its_own() {
    let Some(test) = TestToken::named() else {
        return;
    };
    let dir = tempfile::tempdir().expect("dir");
    let token = test.open();
    let (_, client_file) = test.ca(&token, "client-ca", "OpAMP Fleet Client CA", dir.path());
    let (_, bootstrap_file) = test.ca(
        &token,
        "bootstrap-ca",
        "OpAMP Fleet Bootstrap CA",
        dir.path(),
    );
    let client = client_ca_on(dir.path(), client_file, "client-ca");
    let named = |label: &str| EnrolmentConfig {
        bootstrap_ca_file: bootstrap_file.clone(),
        bootstrap_ca_key_file: None,
        bootstrap_ca_key_label: Some(label.to_string()),
        max_pending_per_bootstrap: 1,
    };
    let err = BootstrapCa::from_config_on(&named("client-ca"), Some(&client), Some(&token))
        .err()
        .expect("the client CA's key is not the bootstrap CA's");
    assert!(err.contains("is not the key of bootstrap_ca_file"), "{err}");
    assert!(
        BootstrapCa::from_config_on(&named("bootstrap-ca"), Some(&client), Some(&token))
            .expect("loads")
            .is_some()
    );
}

/// Kills the server on drop, so a failing assertion never leaks the process.
struct ServerUnderTest(std::process::Child);

impl Drop for ServerUnderTest {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The Server starts against a SoftHSM2 token whose configuration only `SOFTHSM2_CONF` names, with
/// the client CA's, the bootstrap CA's and the audit key named by their labels: `server.toml`
/// names no path of SoftHSM2's own beside `[pkcs11] module`. It warns that the keys have no
/// hardware protection and writes `pkcs11.software_token` into its record.
/// Verifies: ADR-0043
#[test]
fn the_server_starts_on_a_software_token_and_says_so() {
    use std::io::{BufRead as _, BufReader};
    use std::process::{Command, Stdio};
    use std::time::Duration;
    let Some(test) = TestToken::named() else {
        return;
    };
    assert!(
        std::env::var_os("SOFTHSM2_CONF").is_some(),
        "SoftHSM2 finds its configuration by SOFTHSM2_CONF"
    );
    let dir = tempfile::tempdir().expect("dir");
    let token = test.open();
    let (_, client_file) = test.ca(&token, "client-ca", "OpAMP Fleet Client CA", dir.path());
    let (_, bootstrap_file) = test.ca(
        &token,
        "bootstrap-ca",
        "OpAMP Fleet Bootstrap CA",
        dir.path(),
    );
    test.generate("audit");
    drop(token);
    let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()]).expect("cert");
    std::fs::write(dir.path().join("cert.pem"), cert.cert.pem()).expect("cert");
    std::fs::write(dir.path().join("key.pem"), cert.signing_key.serialize_pem()).expect("key");
    let (agent, operator) = (ports::spare_port(), ports::spare_port());
    let config = dir.path().join("server.toml");
    let text = format!(
        "listen = \"127.0.0.1:{agent}\"\ndata_dir = {data:?}\n\
         [tls]\ncert_file = {cert:?}\nkey_file = {key:?}\nclient_ca_file = {client:?}\n\
         [rest]\nlisten = \"127.0.0.1:{operator}\"\n\
         {pkcs11}\
         [client_ca]\ncert_file = {client:?}\nkey_label = \"client-ca\"\n\
         [enrolment]\nbootstrap_ca_file = {bootstrap:?}\nbootstrap_ca_key_label = \"bootstrap-ca\"\n\
         [audit]\nkey_label = \"audit\"\n",
        data = dir.path().join("data"),
        cert = dir.path().join("cert.pem"),
        key = dir.path().join("key.pem"),
        client = client_file,
        bootstrap = bootstrap_file,
        pkcs11 = test.section(),
    );
    assert!(!text.contains("softhsm2.conf") && !text.contains("tokendir"));
    std::fs::write(&config, text).expect("server.toml");
    let mut server = ServerUnderTest(
        Command::new(env!("CARGO_BIN_EXE_server"))
            .arg("--config")
            .arg(&config)
            .env("RUST_LOG", "info")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start the server"),
    );
    let (lines, logged) = std::sync::mpsc::channel();
    let stderr = server.0.stderr.take().expect("stderr");
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let mut seen = Vec::new();
    let mut saw = |what: &str| loop {
        match logged.recv_timeout(Duration::from_secs(30)) {
            Ok(line) if line.contains(what) => return Ok(()),
            Ok(line) => seen.push(line),
            Err(_) => return Err(seen.join("\n")),
        }
    };
    if let Err(log) = saw("no hardware protection") {
        panic!("no warning of a software token:\n{log}");
    }
    if let Err(log) = saw("serving the REST API") {
        panic!("the server never came up:\n{log}");
    }
    // An orderly shutdown, so the record is written out.
    // SAFETY: kill(2) on the pid of a child this test started and has not yet reaped.
    let sent = unsafe { libc::kill(server.0.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(sent, 0, "send SIGTERM");
    let status = server.0.wait().expect("the server exits");
    assert!(status.success(), "{status:?}");
    let audit = std::fs::read_dir(dir.path().join("data").join("audit"))
        .expect("the record")
        .map(|entry| std::fs::read_to_string(entry.expect("entry").path()).unwrap_or_default())
        .collect::<String>();
    assert!(audit.contains("pkcs11.software_token"), "{audit}");
}

/// Signatures from more threads than the pool has sessions all complete and all verify: each waits
/// for a session of the four, and no more than four are opened.
/// Verifies: ADR-0043
#[test]
fn more_signers_than_sessions_take_turns_on_the_pool() {
    const THREADS: usize = 12;
    const EACH: usize = 8;
    let Some(test) = TestToken::named() else {
        return;
    };
    let dir = tempfile::tempdir().expect("dir");
    let token = test.open();
    let (ca_pem, ca_file) = test.ca(&token, "client-ca", "OpAMP Fleet Client CA", dir.path());
    let recorded = Recorded::default();
    let entries = recorded.entries.clone();
    token.set_audit(Arc::new(recorded));
    let ca = ClientCa::from_config_on(
        &client_ca_on(dir.path(), ca_file, "client-ca"),
        Some(&token),
    )
    .expect("client CA");
    let signed: Vec<String> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..THREADS)
            .map(|_| {
                scope.spawn(|| {
                    (0..EACH)
                        .map(|_| {
                            ca.sign(
                                &csr(),
                                "host-1",
                                &SubjectValues::default(),
                                UseCase::MemberCsr,
                            )
                            .expect("signed")
                            .pem
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| worker.join().expect("a signer"))
            .collect()
    });
    assert_eq!(signed.len(), THREADS * EACH);
    assert!(signed.iter().all(|pem| signed_by(pem, &ca_pem)));
    assert_eq!(entries.lock().expect("entries").len(), THREADS * EACH);
    let open = token.open_sessions();
    assert!(
        (1..=fleet_server::pkcs11::SESSIONS).contains(&open),
        "{open} sessions open"
    );
}
