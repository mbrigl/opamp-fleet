//! A fresh SoftHSM2 token for a test: initialised under a label of its own, its PIN in a file, with
//! the keys and CA certificates a test asks for made on it. `FLEET_TEST_PKCS11_MODULE` names the
//! module and `SOFTHSM2_CONF` the configuration whose token directory holds the tokens;
//! `scripts/check-rust.sh` and `scripts/check-store.sh` set both. Without the module the tests that
//! need a token say so and skip.
#![allow(dead_code)] // each integration-test binary uses a different subset of this scaffolding

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use cryptoki::mechanism::Mechanism;
use cryptoki::object::{Attribute, ObjectClass};
use cryptoki::session::UserType;
use cryptoki::types::AuthPin;
use fleet_server::audit::{Audit, Entry, Permit, Unavailable};
use fleet_server::config::Pkcs11Config;
use fleet_server::pkcs11::{self, Token, TokenKey};

/// What a test that needs a token says when none is named; `scripts/check-rust.sh` fails a run
/// that names one and still finds it.
pub const SKIPPED: &str = "skipped: no PKCS#11 token";

/// A token made for one test, and the directory of its PIN file, removed when it is dropped. It
/// holds the test's turn on the module: SoftHSM2 aborts when one thread lists the slots while
/// another initialises a token, so the tests of one process take turns.
pub struct TestToken {
    pub config: Pkcs11Config,
    dir: PathBuf,
    _turn: std::sync::MutexGuard<'static, ()>,
}

impl Drop for TestToken {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn random_hex(bytes: usize) -> String {
    let mut random = vec![0u8; bytes];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut random).expect("random");
    hex::encode(random)
}

impl TestToken {
    /// A token initialised on the module `FLEET_TEST_PKCS11_MODULE` names; `None`, said aloud,
    /// without one.
    pub fn named() -> Option<Self> {
        let Some(module) = std::env::var_os("FLEET_TEST_PKCS11_MODULE").filter(|m| !m.is_empty())
        else {
            eprintln!(
                "{SKIPPED} — set FLEET_TEST_PKCS11_MODULE to a PKCS#11 module, and with \
                 SoftHSM2 SOFTHSM2_CONF to its configuration, to run the tests of the keys on a \
                 token"
            );
            return None;
        };
        static TURN: Mutex<()> = Mutex::new(());
        let turn = TURN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let module = PathBuf::from(module);
        let context = pkcs11::context(&module).expect("module");
        let label = format!("test-{}", random_hex(6));
        let so_pin = AuthPin::from(random_hex(8).as_str());
        let user_pin = random_hex(8);
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
            .init_token(free, &so_pin, &label)
            .expect("init token");
        let slot = pkcs11::slot_of(&context, &label).expect("the new token");
        let session = context.open_rw_session(slot).expect("session");
        session
            .login(UserType::So, Some(&so_pin))
            .expect("SO login");
        session
            .init_pin(&AuthPin::from(user_pin.as_str()))
            .expect("user PIN");
        session.logout().expect("logout");
        let dir = std::env::temp_dir().join(format!("fleet-token-{label}"));
        std::fs::create_dir(&dir).expect("PIN directory");
        let pin_file = dir.join("pin");
        std::fs::write(&pin_file, &user_pin).expect("PIN file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&pin_file, std::fs::Permissions::from_mode(0o600))
                .expect("PIN file mode");
        }
        Some(TestToken {
            config: Pkcs11Config {
                module,
                token_label: label,
                pin_file,
            },
            dir,
            _turn: turn,
        })
    }

    /// The token, opened as the Server opens it.
    pub fn open(&self) -> Arc<Token> {
        Token::open(&self.config).expect("open the token")
    }

    /// A new `[pkcs11]` section naming the token.
    pub fn section(&self) -> String {
        format!(
            "[pkcs11]\nmodule = {:?}\ntoken_label = {:?}\npin_file = {:?}\n",
            self.config.module, self.config.token_label, self.config.pin_file
        )
    }

    /// A P-256 key pair made on the token under `label`, its private half sensitive and not
    /// extractable.
    pub fn generate(&self, label: &str) {
        let context = pkcs11::context(&self.config.module).expect("module");
        let slot = pkcs11::slot_of(&context, &self.config.token_label).expect("slot");
        let session = context.open_rw_session(slot).expect("session");
        let pin = std::fs::read_to_string(&self.config.pin_file).expect("PIN");
        pkcs11::log_in(&session, &AuthPin::from(pin.as_str())).expect("login");
        session
            .generate_key_pair(
                &Mechanism::EccKeyPairGen,
                &[
                    Attribute::Token(true),
                    Attribute::Label(label.as_bytes().to_vec()),
                    Attribute::EcParams(pkcs11::P256_PARAMS.to_vec()),
                    Attribute::Verify(true),
                ],
                &[
                    Attribute::Token(true),
                    Attribute::Private(true),
                    Attribute::Sensitive(true),
                    Attribute::Extractable(false),
                    Attribute::Sign(true),
                    Attribute::Label(label.as_bytes().to_vec()),
                ],
            )
            .expect("key pair");
    }

    /// A CA named `common_name` whose key is made on the token under `label`, its certificate
    /// signed by that key and written to `dir/<label>.pem`; answers its PEM and its path.
    pub fn ca(
        &self,
        token: &Arc<Token>,
        label: &str,
        common_name: &str,
        dir: &std::path::Path,
    ) -> (String, PathBuf) {
        self.generate(label);
        let key: TokenKey = token.key("test key", label).expect("the key");
        let mut params = rcgen::CertificateParams::default();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, common_name);
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params.key_usages = vec![
            rcgen::KeyUsagePurpose::KeyCertSign,
            rcgen::KeyUsagePurpose::CrlSign,
        ];
        params.not_before = time::OffsetDateTime::now_utc() - time::Duration::days(1);
        params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(365);
        let pem = params
            .self_signed(&key)
            .expect("signed through the token")
            .pem();
        let path = dir.join(format!("{label}.pem"));
        std::fs::write(&path, &pem).expect("CA file");
        (pem, path)
    }

    /// Whether the private key `label` is on the token, sensitive and never extractable.
    pub fn holds_unextractable(&self, label: &str) -> bool {
        use cryptoki::object::AttributeType;
        let context = pkcs11::context(&self.config.module).expect("module");
        let slot = pkcs11::slot_of(&context, &self.config.token_label).expect("slot");
        let session = context.open_ro_session(slot).expect("session");
        let pin = std::fs::read_to_string(&self.config.pin_file).expect("PIN");
        pkcs11::log_in(&session, &AuthPin::from(pin.as_str())).expect("login");
        let found = session
            .find_objects(&[
                Attribute::Class(ObjectClass::PRIVATE_KEY),
                Attribute::Label(label.as_bytes().to_vec()),
            ])
            .expect("find");
        let [key] = found.as_slice() else {
            return false;
        };
        let attributes = session
            .get_attributes(
                *key,
                &[
                    AttributeType::Extractable,
                    AttributeType::Sensitive,
                    AttributeType::NeverExtractable,
                ],
            )
            .expect("attributes");
        attributes.iter().all(|attribute| match attribute {
            Attribute::Extractable(extractable) => !extractable,
            Attribute::Sensitive(sensitive) | Attribute::NeverExtractable(sensitive) => *sensitive,
            _ => true,
        })
    }
}

/// Every entry written, or none taken while `refuse` is set.
#[derive(Default)]
pub struct Recorded {
    pub entries: Arc<Mutex<Vec<Entry>>>,
    pub refuse: bool,
}

impl Audit for Recorded {
    fn record(&self, entry: Entry) -> Result<(), Unavailable> {
        if self.refuse {
            return Err(Unavailable);
        }
        self.entries.lock().expect("entries").push(entry);
        Ok(())
    }

    fn reserve(&self) -> Result<Permit, Unavailable> {
        if self.refuse {
            return Err(Unavailable);
        }
        let entries = self.entries.clone();
        Ok(Permit::new(move |entry| {
            entries.lock().expect("entries").push(entry);
        }))
    }
}

impl Recorded {
    /// The entries of the event `event`, each as its fields' texts.
    pub fn events(&self, event: &str) -> Vec<std::collections::BTreeMap<String, String>> {
        self.entries
            .lock()
            .expect("entries")
            .iter()
            .filter(|entry| entry.event == event)
            .map(|entry| {
                entry
                    .fields
                    .iter()
                    .map(|(key, field)| {
                        let text = match field {
                            fleet_server::audit::Field::Text(text) => text.clone(),
                            other => format!("{other:?}"),
                        };
                        (key.clone(), text)
                    })
                    .collect()
            })
            .collect()
    }
}
