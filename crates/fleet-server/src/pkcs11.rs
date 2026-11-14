//! The Server's signing keys on a PKCS#11 token
//! (ADR-0043).
//!
//! `[pkcs11]` names a module, a token and a PIN file. The Server loads the module through
//! `cryptoki`, logs in once, and keeps a pool of four sessions on the token. A key on it is named
//! by its label and implements `rcgen`'s [`SigningKey`](rcgen::SigningKey): the Server builds every
//! certificate as it does with a key file, and only the signature is the token's, one `C_Sign` with
//! `CKM_ECDSA` over the SHA-256 the Server computes. The key never leaves the token.
//!
//! PKCS#11 initialises a module once per process, so every token opened on one module shares one
//! context: instances that share a process, as the tests run them, share it too. The context, the
//! slot lookup, the login, the curve and the signature's DER are `fleet_core::pkcs11`'s, which the
//! operator program uses for the same token.

use std::sync::{Arc, Condvar, Mutex, OnceLock};

use cryptoki::context::Pkcs11;
use cryptoki::error::{Error as CkError, RvError};
use cryptoki::mechanism::Mechanism;
use cryptoki::object::{Attribute, AttributeType, ObjectClass, ObjectHandle};
use cryptoki::session::Session;
use cryptoki::slot::Slot;
use cryptoki::types::AuthPin;
use fleet_core::pkcs11::{der_signature, ec_point, log_in, slot_of, text, P256_PARAMS};

use crate::audit::{Audit, Entry, Permit};
use crate::config::Pkcs11Config;
pub use crate::fleet::UseCase;

/// The sessions a token keeps open at most; no setting changes it.
pub const SESSIONS: usize = 4;

/// The word a software token's module names as its manufacturer.
const SOFTWARE_MANUFACTURER: &str = "SoftHSM";

/// The audit entry a signature with a token key writes.
pub const SIGNED: &str = "pkcs11.signed";

/// The audit entry every startup on a software token writes.
pub const SOFTWARE_TOKEN: &str = "pkcs11.software_token";

/// The record the token's signatures are written to, set once the record is open: the keys are
/// loaded before it, so that a key that is missing fails startup before anything is decided.
pub type AuditCell = Arc<OnceLock<Arc<dyn Audit>>>;

/// The sessions of one token: those idle, and how many are open.
struct Pool {
    idle: Vec<Session>,
    open: usize,
}

/// One token, opened from `[pkcs11]`: logged in, with a pool of [`SESSIONS`] sessions.
pub struct Token {
    context: Pkcs11,
    slot: Slot,
    label: String,
    pin: AuthPin,
    /// A session held for as long as the token is, so the login, which PKCS#11 ends with the last
    /// session of the process, lasts.
    _login: Mutex<Session>,
    pool: Mutex<Pool>,
    freed: Condvar,
    software: bool,
    audit: AuditCell,
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Token")
            .field("label", &self.label)
            .field("software", &self.software)
            .finish_non_exhaustive()
    }
}

impl Token {
    /// Loads the module, finds the token and logs in with the PIN of `pin_file`.
    ///
    /// # Errors
    /// Fails startup naming the key: a PIN file that others may read or its group may write, a
    /// module that does not load, a token that is not found, and a PIN that is refused.
    pub fn open(config: &Pkcs11Config) -> Result<Arc<Self>, String> {
        let pin = crate::config::read_secret_file("[pkcs11] pin_file", &config.pin_file)?;
        let pin = AuthPin::from(pin.as_str());
        let context = fleet_core::pkcs11::context(&config.module)
            .map_err(|e| format!("[pkcs11] module {}: {}", config.module.display(), text(&e)))?;
        let software = context
            .get_library_info()
            .map_err(|e| format!("[pkcs11] module {}: {}", config.module.display(), text(&e)))?
            .manufacturer_id()
            .contains(SOFTWARE_MANUFACTURER);
        let slot = slot_of(&context, &config.token_label, "[pkcs11] token_label")?;
        let login = context.open_ro_session(slot).map_err(|e| {
            format!(
                "[pkcs11] token_label {:?}: cannot open a session: {}",
                config.token_label,
                text(&e)
            )
        })?;
        log_in(&login, &pin).map_err(|e| {
            format!(
                "[pkcs11] pin_file {}: the token {:?} refuses the PIN: {}",
                config.pin_file.display(),
                config.token_label,
                text(&e)
            )
        })?;
        Ok(Arc::new(Token {
            context,
            slot,
            label: config.token_label.clone(),
            pin,
            _login: Mutex::new(login),
            pool: Mutex::new(Pool {
                idle: Vec::new(),
                open: 0,
            }),
            freed: Condvar::new(),
            software,
            audit: AuditCell::default(),
        }))
    }

    /// Whether the module names `SoftHSM` as its manufacturer: a token whose keys have no hardware
    /// protection.
    #[must_use]
    pub fn is_software(&self) -> bool {
        self.software
    }

    /// How many sessions of the pool are open: never more than [`SESSIONS`].
    #[must_use]
    pub fn open_sessions(&self) -> usize {
        self.pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .open
    }

    /// The token's label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Hands the token the record its signatures are written to. Until it has one, nothing is
    /// signed with its keys.
    pub fn set_audit(&self, audit: Arc<dyn Audit>) {
        let _ = self.audit.set(audit);
    }

    /// Says at startup that the keys have no hardware protection, when the token is a software
    /// one: a warning in the log, and `pkcs11.software_token` in the record.
    pub fn warn_if_software(&self, audit: &dyn Audit) {
        if !self.software {
            return;
        }
        tracing::warn!(
            token = %self.label,
            "the PKCS#11 module is SoftHSM: the keys on the token have no hardware protection"
        );
        let _ = audit
            .record(Entry::new(SOFTWARE_TOKEN, "warned").with("token_label", self.label.clone()));
    }

    /// Holds a place in the record for the entry of one signature, before the token is asked.
    ///
    /// # Errors
    /// Returns an error when the record has no place: nothing is signed then.
    pub fn reserve(&self) -> Result<Permit, String> {
        self.audit
            .get()
            .ok_or(())
            .and_then(|audit| audit.reserve().map_err(|_| ()))
            .map_err(|()| {
                "the audit record cannot take the entry of the signature, so nothing is signed"
                    .to_string()
            })
    }

    /// Runs `act` on a session of the pool, waiting while all four are in use. A session whose
    /// call failed is closed rather than given back.
    fn with_session<T>(
        &self,
        act: impl FnOnce(&Session) -> Result<T, CkError>,
    ) -> Result<T, String> {
        let session = {
            let mut pool = self
                .pool
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            loop {
                if let Some(session) = pool.idle.pop() {
                    break session;
                }
                if pool.open < SESSIONS {
                    let session = self.context.open_ro_session(self.slot).map_err(|e| {
                        format!(
                            "[pkcs11] token {:?}: cannot open a session: {}",
                            self.label,
                            text(&e)
                        )
                    })?;
                    log_in(&session, &self.pin).map_err(|e| {
                        format!(
                            "[pkcs11] token {:?} refuses the PIN: {}",
                            self.label,
                            text(&e)
                        )
                    })?;
                    pool.open += 1;
                    break session;
                }
                pool = self
                    .freed
                    .wait(pool)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        let result = act(&session);
        let mut pool = self
            .pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if result.is_ok() {
            pool.idle.push(session);
        } else {
            pool.open -= 1;
            drop(session);
        }
        self.freed.notify_one();
        result.map_err(|e| format!("[pkcs11] token {:?}: {}", self.label, text(&e)))
    }

    /// The private key labelled `label`, and the public half beside it.
    ///
    /// # Errors
    /// Fails naming `key` (the setting that names the label) when the token holds no such private
    /// key, holds more than one, or its public half is not a P-256 point.
    pub fn key(self: &Arc<Self>, key: &str, label: &str) -> Result<TokenKey, String> {
        let missing = |what: &str| format!("{key} {label:?}: {what} on the token {:?}", self.label);
        let point = self.with_session(|session| {
            let private = session.find_objects(&[
                Attribute::Class(ObjectClass::PRIVATE_KEY),
                Attribute::Label(label.as_bytes().to_vec()),
            ])?;
            let public = session.find_objects(&[
                Attribute::Class(ObjectClass::PUBLIC_KEY),
                Attribute::Label(label.as_bytes().to_vec()),
            ])?;
            let point = match public.as_slice() {
                [one] => session
                    .get_attributes(*one, &[AttributeType::EcParams, AttributeType::EcPoint])?
                    .into_iter()
                    .fold((None, None), |(params, point), attribute| match attribute {
                        Attribute::EcParams(p) => (Some(p), point),
                        Attribute::EcPoint(p) => (params, Some(p)),
                        _ => (params, point),
                    }),
                _ => (None, None),
            };
            Ok((private.len(), public.len(), point))
        })?;
        match point {
            (0, _, _) => Err(missing("no private key carries this label")),
            (1, 1, (Some(params), Some(point))) => {
                if params != P256_PARAMS {
                    return Err(missing("the key is not on the curve P-256"));
                }
                let point = ec_point(&point).map_err(|e| missing(&e))?;
                Ok(TokenKey {
                    token: self.clone(),
                    label: label.to_string(),
                    point,
                })
            }
            (1, 0, _) => Err(missing(
                "no public key carries this label beside the private one",
            )),
            (1, 1, _) => Err(missing("the key is no EC key")),
            (private, public, _) => Err(missing(&format!(
                "{private} private and {public} public keys carry this label"
            ))),
        }
    }

    /// Signs `digest` with the private key labelled `label`: one `C_Sign` with `CKM_ECDSA`.
    fn sign_digest(&self, label: &str, digest: &[u8]) -> Result<Vec<u8>, String> {
        let raw = self.with_session(|session| {
            let handle: ObjectHandle = session
                .find_objects(&[
                    Attribute::Class(ObjectClass::PRIVATE_KEY),
                    Attribute::Label(label.as_bytes().to_vec()),
                ])?
                .first()
                .copied()
                .ok_or(CkError::Pkcs11(
                    RvError::KeyHandleInvalid,
                    cryptoki::context::Function::FindObjects,
                ))?;
            session.sign(&Mechanism::Ecdsa, handle, digest)
        })?;
        der_signature(&raw)
    }
}

/// A private key on the token, by its label, with its public half read once.
#[derive(Clone)]
pub struct TokenKey {
    token: Arc<Token>,
    label: String,
    /// The uncompressed P-256 point.
    point: Vec<u8>,
}

impl std::fmt::Debug for TokenKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenKey")
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

impl TokenKey {
    /// The key's label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The token it is on.
    #[must_use]
    pub fn token(&self) -> &Arc<Token> {
        &self.token
    }
}

impl rcgen::PublicKeyData for TokenKey {
    fn der_bytes(&self) -> &[u8] {
        &self.point
    }

    fn algorithm(&self) -> &'static rcgen::SignatureAlgorithm {
        &rcgen::PKCS_ECDSA_P256_SHA256
    }
}

impl rcgen::SigningKey for TokenKey {
    fn sign(&self, msg: &[u8]) -> Result<Vec<u8>, rcgen::Error> {
        use sha2::{Digest, Sha256};
        self.token
            .sign_digest(&self.label, &Sha256::digest(msg))
            .map_err(|e| {
                tracing::error!(key = %self.label, error = %e, "the token did not sign");
                rcgen::Error::RemoteKeyError
            })
    }
}

/// The audit key signs checkpoints through the signer port
/// (ADR-0041 clause 1): one
/// `C_Sign` over the digest the writer computes. The checkpoint is its own signature's entry, so
/// no `pkcs11.signed` is written beside it.
impl crate::checkpoint::CheckpointSigner for TokenKey {
    fn label(&self) -> &str {
        &self.label
    }

    fn public_key_der(&self) -> Vec<u8> {
        rcgen::PublicKeyData::subject_public_key_info(self)
    }

    fn sign_digest(&self, digest: &[u8]) -> Result<Vec<u8>, String> {
        self.token.sign_digest(&self.label, digest)
    }
}

/// The entry of one certificate signed with the token key labelled `label`, naming its issuer and
/// serial where they are known.
#[must_use]
pub fn signed_entry(
    label: &str,
    id: Option<&crate::revocation::CertId>,
    use_case: UseCase,
) -> Entry {
    Entry::new(SIGNED, "signed")
        .with("key_label", label.to_string())
        .with("issuer", id.map(|id| id.issuer.clone()))
        .with("serial", id.map(|id| id.serial.clone()))
        .with("use_case", use_case.as_str())
}
