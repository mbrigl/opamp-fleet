//! What the Server's token adapter and the operator program's alike do on a PKCS#11 token
//! (ADR-0043, ADR-0042 clause 5), with `cryptoki`: the module loaded once per process, the slot of
//! a token found by its label, the user logged in, the curve P-256 named as a key's
//! `CKA_EC_PARAMS`, its point read, and the `r || s` a token signs with `CKM_ECDSA` written as the
//! DER X.509 carries. Each program names its own setting in what it reports; the Server's pool of
//! sessions and the operator program's key generation stay with each.
//!
//! Built only with the `pkcs11` feature, which the Server and the operator program turn on, so the
//! Client does not build `cryptoki`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use cryptoki::context::{CInitializeArgs, CInitializeFlags, Pkcs11};
use cryptoki::error::{Error, RvError};
use cryptoki::session::{Session, UserType};
use cryptoki::slot::Slot;
use cryptoki::types::AuthPin;

/// The DER of the named curve P-256 (`prime256v1`), as `CKA_EC_PARAMS` holds it.
pub const P256_PARAMS: &[u8] = &[0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];

/// One context per module per process.
fn contexts() -> &'static Mutex<HashMap<PathBuf, Pkcs11>> {
    static CONTEXTS: OnceLock<Mutex<HashMap<PathBuf, Pkcs11>>> = OnceLock::new();
    CONTEXTS.get_or_init(Mutex::default)
}

/// The context of `module`, loaded and initialised on its first use in this process. PKCS#11
/// initialises a module once per process, so every caller of one module shares one context.
///
/// # Errors
/// Returns the `cryptoki` error when the module does not load or does not initialise.
pub fn context(module: &Path) -> Result<Pkcs11, Error> {
    let mut contexts = contexts()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(context) = contexts.get(module) {
        return Ok(context.clone());
    }
    let context = Pkcs11::new(module)?;
    match context.initialize(CInitializeArgs::new(CInitializeFlags::OS_LOCKING_OK)) {
        Ok(()) | Err(Error::Pkcs11(RvError::CryptokiAlreadyInitialized, _)) => {}
        Err(e) => return Err(e),
    }
    contexts.insert(module.to_path_buf(), context.clone());
    Ok(context)
}

/// A `cryptoki` error in a line: the function and the return value's name, not the
/// specification's paragraph that `Display` gives.
#[must_use]
pub fn text(error: &Error) -> String {
    match error {
        Error::Pkcs11(rv, function) => format!("{function}: {rv:?}"),
        other => other.to_string(),
    }
}

/// The slot that holds the initialised token labelled `label`. `setting` names where the label
/// was given, as the error says it.
///
/// # Errors
/// Returns an error naming `setting` and the label when the slots or a token cannot be read, or
/// when no slot, or more than one, holds the token.
pub fn slot_of(context: &Pkcs11, label: &str, setting: &str) -> Result<Slot, String> {
    let failed = |what: &str, e: &Error| format!("{setting} {label:?}: {what}: {}", text(e));
    let mut found = Vec::new();
    for slot in context
        .get_slots_with_token()
        .map_err(|e| failed("cannot list the slots", &e))?
    {
        let info = context
            .get_token_info(slot)
            .map_err(|e| failed("cannot read a token", &e))?;
        if info.token_initialized() && info.label() == label {
            found.push(slot);
        }
    }
    match found.as_slice() {
        [slot] => Ok(*slot),
        [] => Err(format!(
            "{setting} {label:?}: no token of the module carries this label"
        )),
        _ => Err(format!(
            "{setting} {label:?}: {} tokens carry this label",
            found.len()
        )),
    }
}

/// Logs the user in on `session`; a user logged in already, by another session of this process,
/// is logged in.
///
/// # Errors
/// Returns the `cryptoki` error when the PIN is refused.
pub fn log_in(session: &Session, pin: &AuthPin) -> Result<(), Error> {
    match session.login(UserType::User, Some(pin)) {
        Ok(()) | Err(Error::Pkcs11(RvError::UserAlreadyLoggedIn, _)) => Ok(()),
        Err(e) => Err(e),
    }
}

/// The uncompressed point of a P-256 public key from its `CKA_EC_POINT`, which a module gives as
/// the DER `OCTET STRING` around it or, against the specification, bare.
///
/// # Errors
/// Returns an error when it is neither.
pub fn ec_point(raw: &[u8]) -> Result<Vec<u8>, String> {
    match raw {
        [0x04, 0x41, point @ ..] if point.len() == 65 && point[0] == 0x04 => Ok(point.to_vec()),
        [0x04, ..] if raw.len() == 65 => Ok(raw.to_vec()),
        _ => Err("its public key is no uncompressed P-256 point".to_string()),
    }
}

/// The DER `ECDSA-Sig-Value` X.509 carries (RFC 5480) for the `r || s` a token signs with
/// `CKM_ECDSA`.
///
/// # Errors
/// Returns an error when the signature is not 64 bytes, as a P-256 one is.
pub fn der_signature(raw: &[u8]) -> Result<Vec<u8>, String> {
    if raw.len() != 64 {
        return Err(format!(
            "the token's signature is {} bytes, not the 64 of P-256",
            raw.len()
        ));
    }
    // Each integer is at most 33 bytes and the two at most 70, so every length is one byte.
    let integer = |bytes: &[u8]| {
        let start = bytes
            .iter()
            .position(|b| *b != 0)
            .unwrap_or(bytes.len() - 1);
        let trimmed = &bytes[start..];
        let pad = trimmed[0] & 0x80 != 0;
        let mut out = vec![
            0x02,
            u8::try_from(trimmed.len() + usize::from(pad)).unwrap_or(0),
        ];
        if pad {
            out.push(0);
        }
        out.extend_from_slice(trimmed);
        out
    };
    let (r, s) = (integer(&raw[..32]), integer(&raw[32..]));
    let mut out = vec![0x30, u8::try_from(r.len() + s.len()).unwrap_or(0)];
    out.extend(r);
    out.extend(s);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A P-256 `r || s` becomes the DER X.509 carries, a high bit padded with a zero and leading
    /// zeros dropped.
    /// Verifies: ADR-0043
    #[test]
    fn a_raw_signature_becomes_der() {
        let mut raw = [0u8; 64];
        raw[0] = 0x80;
        raw[31] = 1;
        raw[63] = 2;
        let der = der_signature(&raw).expect("der");
        assert_eq!(&der[..4], &[0x30, 38, 0x02, 33]);
        assert_eq!(der[4], 0);
        assert_eq!(&der[der.len() - 3..], &[0x02, 1, 2]);
        assert!(der_signature(&[0; 63]).is_err());
        assert!(der_signature(&[0; 10]).is_err());
    }

    /// The point is read with or without the `OCTET STRING` around it.
    /// Verifies: ADR-0043
    #[test]
    fn the_point_is_read_wrapped_or_bare() {
        let mut bare = vec![0x04];
        bare.extend([7u8; 64]);
        let mut wrapped = vec![0x04, 0x41];
        wrapped.extend(&bare);
        assert_eq!(ec_point(&bare).expect("bare"), bare);
        assert_eq!(ec_point(&wrapped).expect("wrapped"), bare);
        assert!(ec_point(&[0x04, 1, 2]).is_err());
        let mut compressed = vec![0x04, 0x41, 0x02];
        compressed.extend([7u8; 64]);
        assert!(
            ec_point(&compressed).is_err(),
            "a wrapped point not uncompressed"
        );
    }
}
