//! The keys `opamp-fleetctl pki token-init` and `pki bootstrap-ca --pkcs11-module` make on a
//! PKCS#11 token, on the Server host
//! (ADR-draft-the-operator-program-opamp-fleetctl clauses 5, 8 and 12): P-256, generated on the
//! token with `CKA_EXTRACTABLE` false and `CKA_SENSITIVE` true, and used only through `C_Sign`. The
//! CA certificates are built here and signed by the token with their own key.

use std::path::{Path, PathBuf};

use cryptoki::context::{CInitializeArgs, CInitializeFlags, Pkcs11};
use cryptoki::error::{Error as CkError, RvError};
use cryptoki::mechanism::Mechanism;
use cryptoki::object::{Attribute, AttributeType, ObjectHandle};
use cryptoki::session::{Session, UserType};
use cryptoki::types::AuthPin;

/// The DER of the named curve P-256, as `CKA_EC_PARAMS` holds it.
const P256_PARAMS: &[u8] = &[0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];

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

/// A `cryptoki` error in a line.
fn text(error: &CkError) -> String {
    match error {
        CkError::Pkcs11(rv, function) => format!("{function}: {rv:?}"),
        other => other.to_string(),
    }
}

/// A session on the token, logged in as the user.
pub struct Token {
    // Dropped before the context it was opened on.
    session: Session,
    _context: Pkcs11,
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
        let context = Pkcs11::new(&options.pkcs11_module).map_err(|e| module(&e))?;
        match context.initialize(CInitializeArgs::new(CInitializeFlags::OS_LOCKING_OK)) {
            Ok(()) | Err(CkError::Pkcs11(RvError::CryptokiAlreadyInitialized, _)) => {}
            Err(e) => return Err(module(&e)),
        }
        let slots = context.get_slots_with_token().map_err(|e| module(&e))?;
        let slot = slots
            .into_iter()
            .find(|slot| {
                context.get_token_info(*slot).is_ok_and(|info| {
                    info.token_initialized() && info.label() == options.token_label
                })
            })
            .ok_or_else(|| {
                format!(
                    "--token-label {:?}: no token of the module carries this label",
                    options.token_label
                )
            })?;
        let pin = zeroize::Zeroizing::new(
            std::fs::read_to_string(&options.pin_file)
                .map_err(|e| format!("--pin-file {}: {e}", options.pin_file.display()))?,
        );
        let pin = AuthPin::from(pin.trim());
        let session = context.open_rw_session(slot).map_err(|e| module(&e))?;
        match session.login(UserType::User, Some(&pin)) {
            Ok(()) | Err(CkError::Pkcs11(RvError::UserAlreadyLoggedIn, _)) => {}
            Err(e) => {
                return Err(format!(
                    "--pin-file {}: the token refuses the PIN: {}",
                    options.pin_file.display(),
                    text(&e)
                ))
            }
        }
        Ok(Token {
            session,
            _context: context,
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
        let point = match point.as_slice() {
            [0x04, 0x41, rest @ ..] if rest.len() == 65 => rest.to_vec(),
            [0x04, ..] if point.len() == 65 => point,
            _ => return Err(format!("the key {label:?} is no uncompressed P-256 point")),
        };
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
        der_signature(&raw).ok_or(rcgen::Error::RemoteKeyError)
    }
}

/// The DER `ECDSA-Sig-Value` for a P-256 `r || s`.
fn der_signature(raw: &[u8]) -> Option<Vec<u8>> {
    if raw.len() != 64 {
        return None;
    }
    let integer = |bytes: &[u8]| {
        let start = bytes
            .iter()
            .position(|b| *b != 0)
            .unwrap_or(bytes.len() - 1);
        let trimmed = &bytes[start..];
        let pad = trimmed[0] & 0x80 != 0;
        let mut out = vec![0x02, u8::try_from(trimmed.len() + usize::from(pad)).ok()?];
        if pad {
            out.push(0);
        }
        out.extend_from_slice(trimmed);
        Some(out)
    };
    let (r, s) = (integer(&raw[..32])?, integer(&raw[32..])?);
    let mut out = vec![0x30, u8::try_from(r.len() + s.len()).ok()?];
    out.extend(r);
    out.extend(s);
    Some(out)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A P-256 `r || s` becomes the DER X.509 carries.
    /// Verifies: ADR-draft-the-operator-program-opamp-fleetctl
    #[test]
    fn a_raw_signature_becomes_der() {
        let mut raw = [0u8; 64];
        raw[0] = 0x80;
        raw[63] = 2;
        let der = der_signature(&raw).expect("der");
        assert_eq!(&der[..4], &[0x30, 38, 0x02, 33]);
        assert_eq!(&der[der.len() - 3..], &[0x02, 1, 2]);
        assert!(der_signature(&[0; 10]).is_none());
    }
}
