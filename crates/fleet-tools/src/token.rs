//! The keys `opamp-fleetctl pki token-init` and `pki bootstrap-ca --pkcs11-module` make on a
//! PKCS#11 token, on the Server host
//! (ADR-0042 clauses 5, 8 and 12): P-256, generated on the
//! token with `CKA_EXTRACTABLE` false and `CKA_SENSITIVE` true, and used only through `C_Sign`. The
//! CA certificates are built here and signed by the token with their own key. The module, the
//! slot, the login, the curve and the signature's DER are `fleet_core::pkcs11`'s, the Server's own.

use std::path::{Path, PathBuf};

use cryptoki::error::Error as CkError;
use cryptoki::mechanism::Mechanism;
use cryptoki::object::{Attribute, AttributeType, ObjectHandle};
use cryptoki::session::Session;
use cryptoki::types::AuthPin;
use fleet_core::pkcs11::{der_signature, ec_point, log_in, slot_of, text, P256_PARAMS};

/// Where a token is reached: the module, the token's label, and the file of the user PIN.
#[derive(clap::Args, Clone, Debug)]
pub struct TokenOptions {
    /// The path of the PKCS#11 module the Server loads.
    #[arg(long)]
    pub pkcs11_module: PathBuf,
    /// The label of the token.
    #[arg(long)]
    pub token_label: String,
    /// The file holding the user PIN, as `[pkcs11] pin_file` names it.
    #[arg(long)]
    pub pin_file: PathBuf,
}

/// A session on the token, logged in as the user.
pub struct Token {
    session: Session,
    label: String,
}

impl Token {
    /// Loads the module, finds the token and logs in.
    ///
    /// # Errors
    /// Returns an error naming the flag when the module does not load, the token is not found, the
    /// PIN file cannot be read, or the PIN is refused.
    pub fn open(options: &TokenOptions) -> Result<Self, String> {
        let module = |e: &CkError| {
            format!(
                "--pkcs11-module {}: {}",
                options.pkcs11_module.display(),
                text(e)
            )
        };
        let context =
            fleet_core::pkcs11::context(&options.pkcs11_module).map_err(|e| module(&e))?;
        let slot = slot_of(&context, &options.token_label, "--token-label")?;
        let pin = zeroize::Zeroizing::new(
            std::fs::read_to_string(&options.pin_file)
                .map_err(|e| format!("--pin-file {}: {e}", options.pin_file.display()))?,
        );
        let pin = AuthPin::from(pin.trim());
        let session = context.open_rw_session(slot).map_err(|e| module(&e))?;
        log_in(&session, &pin).map_err(|e| {
            format!(
                "--pin-file {}: the token refuses the PIN: {}",
                options.pin_file.display(),
                text(&e)
            )
        })?;
        Ok(Token {
            session,
            label: options.token_label.clone(),
        })
    }

    /// The session, for a test that reads what the token holds.
    #[cfg(test)]
    pub fn session_for_tests(&self) -> &Session {
        &self.session
    }

    /// Whether any object on the token carries `label`.
    ///
    /// # Errors
    /// Returns an error when the token cannot be searched.
    pub fn holds(&self, label: &str) -> Result<bool, String> {
        self.session
            .find_objects(&[Attribute::Label(label.as_bytes().to_vec())])
            .map(|found| !found.is_empty())
            .map_err(|e| format!("the token {:?}: {}", self.label, text(&e)))
    }

    /// Refuses, naming it, each of `labels` the token holds already.
    ///
    /// # Errors
    /// Returns the refusal, nothing having been made.
    pub fn refuse_held(&self, labels: &[&str]) -> Result<(), String> {
        for label in labels {
            if self.holds(label)? {
                return Err(format!(
                    "the token {:?} holds a key labelled {label:?} already; nothing was made",
                    self.label
                ));
            }
        }
        Ok(())
    }

    /// Destroys every object labelled with one of `labels`: the keys a command made, when what it
    /// does after fails. Answers the labels whose objects it could not destroy.
    pub fn destroy(&self, labels: &[&str]) -> Vec<String> {
        labels
            .iter()
            .filter(|label| {
                self.session
                    .find_objects(&[Attribute::Label(label.as_bytes().to_vec())])
                    .and_then(|found| {
                        found
                            .into_iter()
                            .try_for_each(|object| self.session.destroy_object(object))
                    })
                    .is_err()
            })
            .map(|label| (*label).to_string())
            .collect()
    }

    /// A new P-256 key pair under `label`: its private half on the token, sensitive and never
    /// extractable, used only for signing.
    ///
    /// # Errors
    /// Returns an error when the token does not make it.
    pub fn generate(&self, label: &str) -> Result<TokenKey<'_>, String> {
        let failed = |e: CkError| {
            format!(
                "the token {:?} did not make the key {label:?}: {}",
                self.label,
                text(&e)
            )
        };
        let (public, private) = self
            .session
            .generate_key_pair(
                &Mechanism::EccKeyPairGen,
                &[
                    Attribute::Token(true),
                    Attribute::Private(false),
                    Attribute::Label(label.as_bytes().to_vec()),
                    Attribute::EcParams(P256_PARAMS.to_vec()),
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
            .map_err(failed)?;
        let point = self
            .session
            .get_attributes(public, &[AttributeType::EcPoint])
            .map_err(failed)?
            .into_iter()
            .find_map(|attribute| match attribute {
                Attribute::EcPoint(point) => Some(point),
                _ => None,
            })
            .ok_or_else(|| format!("the token gave no public key for {label:?}"))?;
        let point = ec_point(&point).map_err(|e| format!("the key {label:?}: {e}"))?;
        Ok(TokenKey {
            session: &self.session,
            private,
            point,
        })
    }
}

/// A key made on the token, which signs there.
pub struct TokenKey<'a> {
    session: &'a Session,
    private: ObjectHandle,
    /// The uncompressed P-256 point.
    point: Vec<u8>,
}

impl rcgen::PublicKeyData for TokenKey<'_> {
    fn der_bytes(&self) -> &[u8] {
        &self.point
    }

    fn algorithm(&self) -> &'static rcgen::SignatureAlgorithm {
        &rcgen::PKCS_ECDSA_P256_SHA256
    }
}

impl rcgen::SigningKey for TokenKey<'_> {
    fn sign(&self, msg: &[u8]) -> Result<Vec<u8>, rcgen::Error> {
        use sha2::{Digest, Sha256};
        let raw = self
            .session
            .sign(&Mechanism::Ecdsa, self.private, &Sha256::digest(msg))
            .map_err(|_| rcgen::Error::RemoteKeyError)?;
        der_signature(&raw).map_err(|_| rcgen::Error::RemoteKeyError)
    }
}

/// An absolute path for `path`, as a fragment names it.
///
/// # Errors
/// Returns an error when the working directory cannot be read.
pub fn absolute(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    std::env::current_dir()
        .map(|dir| dir.join(path))
        .map_err(|e| format!("cannot resolve {}: {e}", path.display()))
}
