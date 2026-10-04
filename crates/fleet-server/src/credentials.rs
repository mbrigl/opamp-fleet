//! The credential check both planes use (ADR-0026).
//!
//! One primitive, because the two planes ask the same question of a request — does this
//! `Authorization` header match something configured? — and the *answer* is what differs: the Agent
//! plane pairs it with a client certificate, the Operator plane guards a browser. What must not
//! differ is how the comparison is made, which is why it is written once.
//!
//! Nothing configured authenticates on its own (ADR-0026 clause 26): a Bearer token is kept as its
//! SHA-256, a Basic password as an Argon2id hash. A Bearer token is compared by hash in constant
//! time; a Basic password is verified with Argon2id, and a success is remembered for a while by the
//! hash of the whole header, so an Agent polling with Basic pays the password hash once and not per
//! request.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use axum::http::{header, HeaderMap};
use base64::Engine as _;
use sha2::{Digest, Sha256};

/// The prefix of a stored Bearer entry.
pub const BEARER_PREFIX: &str = "sha256:";

/// The shortest Bearer token `hash-credential` accepts: its strength is its entropy alone.
pub const MIN_BEARER_LEN: usize = 32;

/// The least an Argon2id hash may cost: OWASP's minimum (`m=19456`, `t=2`, `p=1`).
pub const MIN_MEMORY_KIB: u32 = 19_456;
pub const MIN_ITERATIONS: u32 = 2;
pub const MIN_PARALLELISM: u32 = 1;

/// How long a successful Basic verification is remembered.
pub const REMEMBER_FOR: Duration = Duration::from_secs(600);

/// How many successful Basic verifications are remembered at once.
pub const REMEMBERED_MAX: usize = 1024;

/// How many password hashes one plane computes at once; past it a request is answered `503`, so a
/// flood of wrong passwords cannot take every worker thread.
pub const CONCURRENT_HASHES: usize = 4;

/// The shortest hash output and salt a Basic entry may carry, in bytes.
pub const MIN_OUTPUT_LEN: usize = 32;
pub const MIN_SALT_LEN: usize = 16;

/// What a check of a request comes to.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Permitted,
    Refused,
    /// Too many password hashes are being computed; try again shortly.
    Busy,
}

/// The configured credentials of one plane, and the challenge a refusal carries.
pub struct Credentials {
    /// SHA-256 of every accepted Bearer token.
    bearer: Vec<[u8; 32]>,
    /// Every accepted Basic user and the Argon2id hash of its password.
    basic: BTreeMap<String, String>,
    /// SHA-256 of a Basic header that verified, and until when that holds.
    remembered: Mutex<HashMap<[u8; 32], Instant>>,
    /// Verified against for an unknown user, at the highest cost any user's hash has, so the
    /// answer's timing does not tell which users exist.
    dummy: String,
    hashing: Arc<tokio::sync::Semaphore>,
    /// The `WWW-Authenticate` value a `401` answers with (RFC 9110).
    challenge: String,
}

impl Credentials {
    /// Credentials from entries [`check_bearer`] and [`check_basic`] accepted.
    ///
    /// # Errors
    /// Returns an error naming the first entry that is not a hash this Server keeps.
    pub fn new(
        bearer: &[String],
        basic: &BTreeMap<String, String>,
        challenge: String,
    ) -> Result<Self, String> {
        let bearer = bearer
            .iter()
            .enumerate()
            .map(|(index, entry)| check_bearer(entry).map_err(|e| format!("entry {index}: {e}")))
            .collect::<Result<Vec<_>, _>>()?;
        let mut costliest = Params::new(MIN_MEMORY_KIB, MIN_ITERATIONS, MIN_PARALLELISM, None)
            .map_err(|e| format!("cannot set Argon2 parameters: {e}"))?;
        for (user, phc) in basic {
            let params = check_basic(phc).map_err(|e| format!("user {user:?}: {e}"))?;
            if (params.m_cost(), params.t_cost(), params.p_cost())
                > (costliest.m_cost(), costliest.t_cost(), costliest.p_cost())
            {
                costliest = params;
            }
        }
        let dummy = if basic.is_empty() {
            String::new()
        } else {
            let mut secret = [0u8; 24];
            use ring::rand::SecureRandom as _;
            ring::rand::SystemRandom::new()
                .fill(&mut secret)
                .map_err(|_| "no secure random source".to_string())?;
            Argon2::new(Algorithm::Argon2id, Version::V0x13, costliest)
                .hash_password(&secret, &salt()?)
                .map_err(|e| format!("cannot make the comparison hash: {e}"))?
                .to_string()
        };
        Ok(Credentials {
            bearer,
            basic: basic.clone(),
            remembered: Mutex::new(HashMap::new()),
            dummy,
            hashing: Arc::new(tokio::sync::Semaphore::new(CONCURRENT_HASHES)),
            challenge,
        })
    }

    pub fn challenge(&self) -> &str {
        &self.challenge
    }

    /// Checks a request's `Authorization` header. A password hash, when one is needed, runs on a
    /// blocking thread and at most [`CONCURRENT_HASHES`] at once; past that the answer is
    /// [`Verdict::Busy`].
    pub async fn check(self: &Arc<Self>, headers: &HeaderMap) -> Verdict {
        let presented = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let needs_hash = presented.starts_with("Basic ") && !self.is_remembered(&presented);
        if !needs_hash {
            return if self.verify(&presented) {
                Verdict::Permitted
            } else {
                Verdict::Refused
            };
        }
        let Ok(permit) = self.hashing.clone().try_acquire_owned() else {
            return Verdict::Busy;
        };
        let this = self.clone();
        let verified = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            this.verify(&presented)
        })
        .await
        .unwrap_or(false);
        if verified {
            Verdict::Permitted
        } else {
            Verdict::Refused
        }
    }

    fn is_remembered(&self, header: &str) -> bool {
        let key: [u8; 32] = Sha256::digest(header.as_bytes()).into();
        self.remembered
            .lock()
            .expect("credentials lock")
            .get(&key)
            .is_some_and(|until| *until > Instant::now())
    }

    /// Whether an `Authorization` value matches a configured credential.
    #[must_use]
    pub fn verify(&self, authorization: &str) -> bool {
        if let Some(token) = authorization.strip_prefix("Bearer ") {
            let presented: [u8; 32] = Sha256::digest(token.as_bytes()).into();
            // Constant-time per candidate, so a comparison never leaks how far it matched.
            return self.bearer.iter().fold(false, |found, accepted| {
                found | constant_time_eq::constant_time_eq(accepted, &presented)
            });
        }
        if let Some(encoded) = authorization.strip_prefix("Basic ") {
            return self.verify_basic(authorization, encoded);
        }
        false
    }

    fn verify_basic(&self, header: &str, encoded: &str) -> bool {
        let key: [u8; 32] = Sha256::digest(header.as_bytes()).into();
        let now = Instant::now();
        {
            let remembered = self.remembered.lock().expect("credentials lock");
            if remembered.get(&key).is_some_and(|until| *until > now) {
                return true;
            }
        }
        let Some((user, password)) = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .and_then(|text| {
                text.split_once(':')
                    .map(|(user, password)| (user.to_string(), password.to_string()))
            })
        else {
            return false;
        };
        let known = self.basic.get(&user);
        // An unknown user costs the same verification, so the answer's timing does not tell which
        // users exist.
        let phc = known.unwrap_or(&self.dummy);
        let verified = PasswordHash::new(phc).ok().is_some_and(|hash| {
            Argon2::default()
                .verify_password(password.as_bytes(), &hash)
                .is_ok()
        });
        if !(verified && known.is_some()) {
            return false;
        }
        let mut remembered = self.remembered.lock().expect("credentials lock");
        if remembered.len() >= REMEMBERED_MAX {
            remembered.retain(|_, until| *until > now);
            if remembered.len() >= REMEMBERED_MAX {
                if let Some(oldest) = remembered
                    .iter()
                    .min_by_key(|(_, until)| **until)
                    .map(|(key, _)| *key)
                {
                    remembered.remove(&oldest);
                }
            }
        }
        remembered.insert(key, now + REMEMBER_FOR);
        true
    }
}

/// A Bearer entry as `server.toml` keeps it: `sha256:` and 64 hex digits.
///
/// # Errors
/// Returns an error saying what the entry must be, never echoing it.
pub fn check_bearer(entry: &str) -> Result<[u8; 32], String> {
    let mut hash = [0u8; 32];
    entry
        .strip_prefix(BEARER_PREFIX)
        .filter(|hex| hex.len() == 64)
        .and_then(|hex| hex::decode_to_slice(hex, &mut hash).ok())
        .ok_or_else(|| {
            "is not `sha256:` and 64 hex digits — a token is never kept in clear; \
             make the entry with `server hash-credential --bearer`"
                .to_string()
        })?;
    Ok(hash)
}

/// A Basic entry as `server.toml` keeps it: an Argon2id PHC string at least as costly as OWASP's
/// minimum.
///
/// # Errors
/// Returns an error saying what the entry must be, never echoing it.
pub fn check_basic(phc: &str) -> Result<Params, String> {
    let refused = || {
        "is not an Argon2id hash — a password is never kept in clear; make the entry with \
         `server hash-credential --basic`"
            .to_string()
    };
    let hash = PasswordHash::new(phc).map_err(|_| refused())?;
    if hash.algorithm.as_str() != "argon2id" {
        return Err(refused());
    }
    let params = Params::try_from(&hash).map_err(|_| refused())?;
    if params.m_cost() < MIN_MEMORY_KIB
        || params.t_cost() < MIN_ITERATIONS
        || params.p_cost() < MIN_PARALLELISM
    {
        return Err(format!(
            "is an Argon2id hash cheaper than m={MIN_MEMORY_KIB}, t={MIN_ITERATIONS}, \
             p={MIN_PARALLELISM} — make it again with `server hash-credential --basic`"
        ));
    }
    let output = hash.hash.map_or(0, |output| output.len());
    let salt = hash.salt.map_or(0, |salt| salt.len());
    // The salt is base64 in the string: 22 characters carry 16 bytes.
    if output < MIN_OUTPUT_LEN || salt < MIN_SALT_LEN * 4 / 3 {
        return Err(format!(
            "is an Argon2id hash shorter than {MIN_OUTPUT_LEN} bytes or with a salt shorter than \
             {MIN_SALT_LEN} bytes — make it again with `server hash-credential --basic`"
        ));
    }
    Ok(params)
}

/// The entry `server.toml` keeps for a Bearer token.
///
/// # Errors
/// Refuses a token shorter than [`MIN_BEARER_LEN`].
pub fn hash_bearer(token: &str) -> Result<String, String> {
    if token.chars().count() < MIN_BEARER_LEN {
        return Err(format!(
            "a Bearer token needs at least {MIN_BEARER_LEN} characters — its strength is its \
             entropy alone"
        ));
    }
    Ok(bearer_entry(token))
}

/// The entry for a Bearer token, whatever its length — what a test with a short token uses.
#[must_use]
pub fn bearer_entry(token: &str) -> String {
    format!(
        "{BEARER_PREFIX}{}",
        hex::encode(Sha256::digest(token.as_bytes()))
    )
}

/// The entry `server.toml` keeps for a Basic password: Argon2id at OWASP's minimum cost.
///
/// # Errors
/// Returns an error when no salt can be drawn or the hash cannot be made.
pub fn hash_basic(password: &str) -> Result<String, String> {
    if password.is_empty() {
        return Err("a password must not be empty".to_string());
    }
    Ok(argon2id()
        .hash_password(password.as_bytes(), &salt()?)
        .map_err(|e| format!("cannot hash the password: {e}"))?
        .to_string())
}

fn argon2id() -> Argon2<'static> {
    Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(MIN_MEMORY_KIB, MIN_ITERATIONS, MIN_PARALLELISM, None)
            .expect("OWASP's minimum is valid Argon2 parameters"),
    )
}

fn salt() -> Result<SaltString, String> {
    use ring::rand::SecureRandom as _;
    let mut bytes = [0u8; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "no secure random source for a salt".to_string())?;
    SaltString::encode_b64(&bytes).map_err(|e| format!("cannot encode the salt: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "a-fleet-token-of-at-least-32-characters";

    fn header(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, value.parse().expect("header"));
        headers
    }

    fn basic(user: &str, password: &str) -> String {
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
        )
    }

    /// Verifies: ADR-0026
    #[test]
    fn a_bearer_token_is_admitted_by_its_hash() {
        let entry = hash_bearer(TOKEN).expect("hash");
        assert!(!entry.contains(TOKEN));
        let credentials =
            Credentials::new(&[entry], &BTreeMap::new(), "Bearer".into()).expect("credentials");
        assert!(credentials.verify(&format!("Bearer {TOKEN}")));
        assert!(!credentials.verify("Bearer another-token-of-32-characters-xx"));
        assert!(!credentials.verify(TOKEN), "the scheme is part of it");
        assert!(hash_bearer("short").is_err());
    }

    /// Verifies: ADR-0026
    #[test]
    fn a_basic_password_is_verified_against_its_argon2id_hash() {
        let phc = hash_basic("s3cret").expect("hash");
        assert!(phc.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "{phc}");
        let users = BTreeMap::from([("ops".to_string(), phc)]);
        let credentials = Credentials::new(&[], &users, "Basic".into()).expect("credentials");
        assert!(credentials.verify(&basic("ops", "s3cret")));
        assert!(!credentials.verify(&basic("ops", "wrong")));
    }

    /// Verifies: ADR-0026
    #[test]
    fn an_unknown_user_costs_the_same_verification() {
        let users = BTreeMap::from([("ops".to_string(), hash_basic("s3cret").expect("hash"))]);
        let credentials = Credentials::new(&[], &users, "Basic".into()).expect("credentials");
        let started = Instant::now();
        assert!(!credentials.verify(&basic("ops", "wrong")));
        let known = started.elapsed();
        let started = Instant::now();
        assert!(!credentials.verify(&basic("nobody", "wrong")));
        let unknown = started.elapsed();
        assert!(
            unknown * 4 > known,
            "an unknown user answered far faster: {unknown:?} against {known:?}"
        );
        assert!(
            credentials.remembered.lock().expect("lock").is_empty(),
            "a failure is never remembered"
        );
    }

    /// Verifies: ADR-0026
    #[test]
    fn a_basic_verification_is_remembered_and_bounded() {
        let users = BTreeMap::from([("ops".to_string(), hash_basic("s3cret").expect("hash"))]);
        let credentials = Credentials::new(&[], &users, "Basic".into()).expect("credentials");
        assert!(credentials.verify(&basic("ops", "s3cret")));
        let started = Instant::now();
        assert!(credentials.verify(&basic("ops", "s3cret")));
        assert!(
            started.elapsed() < Duration::from_millis(5),
            "a remembered success is answered without the hash"
        );
        {
            // A full table of other headers: the next success must make room, not grow it.
            let mut remembered = credentials.remembered.lock().expect("lock");
            remembered.clear();
            let far = Instant::now() + REMEMBER_FOR;
            for n in 0..REMEMBERED_MAX {
                let key: [u8; 32] = Sha256::digest(n.to_le_bytes()).into();
                remembered.insert(key, far);
            }
        }
        assert!(credentials.verify(&basic("ops", "s3cret")));
        assert!(credentials.remembered.lock().expect("lock").len() <= REMEMBERED_MAX);
    }

    #[test]
    fn a_cheap_or_foreign_hash_is_refused() {
        assert!(check_basic("s3cret").is_err());
        let cheap = Argon2::new(
            Algorithm::Argon2id,
            Version::V0x13,
            Params::new(1024, 1, 1, None).expect("params"),
        )
        .hash_password(b"x", &salt().expect("salt"))
        .expect("hash")
        .to_string();
        assert!(check_basic(&cheap).expect_err("cheap").contains("cheaper"));
        assert!(check_bearer("sha256:abcd").is_err());
        assert!(check_bearer(TOKEN).is_err());
    }

    /// Password hashes run off the async workers and at most a few at once; past that a request
    /// is answered busy rather than queued behind them.
    /// Verifies: ADR-0026
    #[tokio::test]
    async fn password_hashes_are_bounded_and_off_the_async_workers() {
        let users = BTreeMap::from([("ops".to_string(), hash_basic("s3cret").expect("hash"))]);
        let credentials =
            Arc::new(Credentials::new(&[], &users, "Basic".into()).expect("credentials"));
        assert_eq!(
            credentials.check(&header(&basic("ops", "s3cret"))).await,
            Verdict::Permitted
        );
        let held: Vec<_> = (0..CONCURRENT_HASHES)
            .map(|_| {
                credentials
                    .hashing
                    .clone()
                    .try_acquire_owned()
                    .expect("permit")
            })
            .collect();
        assert_eq!(
            credentials.check(&header(&basic("ops", "wrong"))).await,
            Verdict::Busy
        );
        assert_eq!(
            credentials.check(&header(&basic("ops", "s3cret"))).await,
            Verdict::Permitted,
            "a remembered success needs no hash"
        );
        drop(held);
        assert_eq!(
            credentials.check(&header(&basic("ops", "wrong"))).await,
            Verdict::Refused
        );
    }

    /// The comparison hash for an unknown user costs what the costliest user's hash costs.
    /// Verifies: ADR-0026
    #[test]
    fn the_comparison_hash_matches_the_costliest_user() {
        let costly = Argon2::new(
            Algorithm::Argon2id,
            Version::V0x13,
            Params::new(MIN_MEMORY_KIB * 2, 3, 1, None).expect("params"),
        )
        .hash_password(b"s3cret", &salt().expect("salt"))
        .expect("hash")
        .to_string();
        let users = BTreeMap::from([("ops".to_string(), costly)]);
        let credentials = Credentials::new(&[], &users, "Basic".into()).expect("credentials");
        let dummy = PasswordHash::new(&credentials.dummy).expect("phc");
        let params = Params::try_from(&dummy).expect("params");
        assert_eq!((params.m_cost(), params.t_cost()), (MIN_MEMORY_KIB * 2, 3));
    }

    #[test]
    fn a_short_output_is_refused() {
        let short = Argon2::new(
            Algorithm::Argon2id,
            Version::V0x13,
            Params::new(MIN_MEMORY_KIB, MIN_ITERATIONS, MIN_PARALLELISM, Some(16)).expect("params"),
        )
        .hash_password(b"x", &salt().expect("salt"))
        .expect("hash")
        .to_string();
        assert!(check_basic(&short).expect_err("short").contains("shorter"));
    }
}
