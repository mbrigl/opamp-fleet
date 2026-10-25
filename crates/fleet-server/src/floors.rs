//! The floors the operator signed (ADR-0060): one per Agent type, the version below which no
//! Client takes a package of that type. The Server stores them, serves them to every member on
//! the Agent plane, and checks their signatures only to say what it found — the Client's check
//! decides (clause 2).
//!
//! A floor is raised, or removed and set anew: a write below the one stored is refused. The Server
//! cannot sign a floor, so a Server that is compromised can withhold one but never lower what a
//! Client has kept (clause 7).

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::packages::{PackageId, SignatureState, SignatureVerifier, ED25519_SIGNATURE_LEN};

/// One stored floor: the version and the operator's signature over its statement
/// (`fleet_core::package::floor_statement`, ADR-0060 clause 1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Floor {
    pub version: String,
    pub signature: Vec<u8>,
}

/// The floors' store (ADR-0037, ADR-0050): the filesystem adapter keeps them in
/// `<data_dir>/package-floors.json` ([`FsFloorStore`](crate::fs::FsFloorStore)), the PostgreSQL
/// adapter in `package_floors`. Each write is persisted before the act that made it is answered.
pub trait FloorStore: Send + Sync {
    /// Every stored floor, by Agent type.
    ///
    /// # Errors
    /// Returns an error naming what cannot be read.
    fn load(&self) -> Result<BTreeMap<String, Floor>, String>;

    /// Stores `floor` for `agent_type`, replacing the one stored.
    ///
    /// # Errors
    /// Returns an error when the floor cannot be persisted; the store is then as it was.
    fn put(&self, agent_type: &str, floor: &Floor) -> Result<(), String>;

    /// Removes the floor of `agent_type`; content when there is none.
    ///
    /// # Errors
    /// Returns an error when the removal cannot be persisted; the store is then as it was.
    fn remove(&self, agent_type: &str) -> Result<(), String>;
}

/// Why a floor was not stored.
#[derive(Debug, PartialEq, Eq)]
pub enum FloorError {
    /// The type is not an Agent type, the version not SemVer, or the signature not 64 bytes.
    Invalid(String),
    /// The version orders below the floor stored for the type, which it names.
    Lower(String),
    /// The store could not persist it.
    Store(String),
}

/// The floors as the Server holds them: read from the store at startup, written through to it.
pub struct PackageFloors {
    store: Box<dyn FloorStore>,
    floors: Mutex<BTreeMap<String, Floor>>,
    /// The operator's public key, `[packages] verification_key`, when the Server holds it.
    verifier: Option<Box<dyn SignatureVerifier>>,
}

impl PackageFloors {
    /// The floors `store` holds. A stored floor whose type or version a write would refuse fails
    /// the open, naming it: the store holds only what was checked when it was written.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read or holds such a floor.
    pub fn open(store: Box<dyn FloorStore>) -> Result<Self, String> {
        let floors = store.load()?;
        for (agent_type, floor) in &floors {
            PackageId::new(agent_type, &floor.version)
                .map_err(|e| format!("the stored floor of {agent_type:?}: {e}"))?;
        }
        Ok(PackageFloors {
            store,
            floors: Mutex::new(floors),
            verifier: None,
        })
    }

    /// Checks every floor against the operator's public key (ADR-0060 clause 2) — feedback only.
    #[must_use]
    pub fn with_verifier(mut self, verifier: Option<Box<dyn SignatureVerifier>>) -> Self {
        self.verifier = verifier;
        self
    }

    /// What the Server found when it checked the floor of `agent_type`: `Unchecked` without a key.
    fn state(&self, agent_type: &str, floor: &Floor) -> SignatureState {
        let Some(verifier) = &self.verifier else {
            return SignatureState::Unchecked;
        };
        let statement = fleet_core::package::floor_statement(agent_type, &floor.version);
        if verifier.verifies(&statement, &floor.signature) {
            SignatureState::Valid
        } else {
            SignatureState::Invalid
        }
    }

    /// Every floor, by Agent type, with what its signature check found.
    pub fn list(&self) -> Vec<(String, Floor, SignatureState)> {
        self.floors
            .lock()
            .expect("floors lock")
            .iter()
            .map(|(agent_type, floor)| {
                let state = self.state(agent_type, floor);
                (agent_type.clone(), floor.clone(), state)
            })
            .collect()
    }

    /// Every floor, by Agent type — what the Agent plane serves.
    pub fn all(&self) -> BTreeMap<String, Floor> {
        self.floors.lock().expect("floors lock").clone()
    }

    /// Stores the floor of `agent_type` at `version` with `signature`, and says what its check
    /// found. An `Invalid` one is stored all the same: the Server's key may lag behind the
    /// operator's, and the Client's check decides (ADR-0060 clause 2).
    ///
    /// # Errors
    /// [`FloorError::Invalid`] for a type, version or signature a floor cannot have,
    /// [`FloorError::Lower`] for a version below the one stored, [`FloorError::Store`] when the
    /// floor could not be persisted; nothing is changed then.
    pub fn put(
        &self,
        agent_type: &str,
        version: &str,
        signature: Vec<u8>,
    ) -> Result<SignatureState, FloorError> {
        PackageId::new(agent_type, version).map_err(FloorError::Invalid)?;
        if signature.len() != ED25519_SIGNATURE_LEN {
            return Err(FloorError::Invalid(format!(
                "a floor signature is {ED25519_SIGNATURE_LEN} bytes, as `opamp-fleetctl package \
                 floor` prints it in hex; this one is {}",
                signature.len()
            )));
        }
        let mut floors = self.floors.lock().expect("floors lock");
        if let Some(stored) = floors.get(agent_type) {
            if fleet_core::version::precedence(version, &stored.version)
                == Some(std::cmp::Ordering::Less)
            {
                return Err(FloorError::Lower(stored.version.clone()));
            }
        }
        let floor = Floor {
            version: version.to_string(),
            signature,
        };
        self.store
            .put(agent_type, &floor)
            .map_err(FloorError::Store)?;
        let state = self.state(agent_type, &floor);
        floors.insert(agent_type.to_string(), floor);
        Ok(state)
    }

    /// Removes the floor of `agent_type`: `Ok(false)` when there is none. It only stops serving
    /// the floor; a Client that kept it keeps it (ADR-0060 clause 7).
    ///
    /// # Errors
    /// Returns an error when the removal could not be persisted; the floor is then kept.
    pub fn remove(&self, agent_type: &str) -> Result<bool, String> {
        let mut floors = self.floors.lock().expect("floors lock");
        if !floors.contains_key(agent_type) {
            return Ok(false);
        }
        self.store.remove(agent_type)?;
        floors.remove(agent_type);
        Ok(true)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A store in memory, for the tests of the logic above the port.
    #[derive(Default)]
    pub(crate) struct Memory(pub(crate) Mutex<BTreeMap<String, Floor>>);

    impl FloorStore for Memory {
        fn load(&self) -> Result<BTreeMap<String, Floor>, String> {
            Ok(self.0.lock().expect("lock").clone())
        }
        fn put(&self, agent_type: &str, floor: &Floor) -> Result<(), String> {
            self.0
                .lock()
                .expect("lock")
                .insert(agent_type.to_string(), floor.clone());
            Ok(())
        }
        fn remove(&self, agent_type: &str) -> Result<(), String> {
            self.0.lock().expect("lock").remove(agent_type);
            Ok(())
        }
    }

    /// A floor is raised or set anew, never lowered; a version equal to the stored one replaces
    /// it; a type, version or signature a floor cannot have is refused, and nothing changes.
    /// Verifies: ADR-0060
    #[test]
    fn a_floor_is_raised_and_never_lowered() {
        let floors = PackageFloors::open(Box::new(Memory::default())).expect("open");
        assert_eq!(
            floors.put("otelcol", "2.0.0", vec![1; 64]),
            Ok(SignatureState::Unchecked)
        );
        assert_eq!(
            floors.put("otelcol", "1.9.4", vec![2; 64]),
            Err(FloorError::Lower("2.0.0".to_string()))
        );
        assert_eq!(
            floors.put("otelcol", "2.0.0-rc.1", vec![2; 64]),
            Err(FloorError::Lower("2.0.0".to_string())),
            "a pre-release orders below its release"
        );
        assert!(floors.put("otelcol", "2.0.0", vec![3; 64]).is_ok());
        assert!(floors.put("otelcol", "2.1.0", vec![4; 64]).is_ok());
        for (agent_type, version, signature) in [
            ("otel col", "3.0.0", vec![0; 64]),
            ("otelcol", "3.0", vec![0; 64]),
            ("otelcol", "3.0.0", vec![0; 63]),
        ] {
            assert!(
                matches!(
                    floors.put(agent_type, version, signature),
                    Err(FloorError::Invalid(_))
                ),
                "{agent_type} {version}"
            );
        }
        assert_eq!(
            floors.all().get("otelcol").map(|f| f.version.as_str()),
            Some("2.1.0")
        );
        assert_eq!(floors.remove("otelcol"), Ok(true));
        assert_eq!(floors.remove("otelcol"), Ok(false));
        assert!(
            floors.put("otelcol", "1.0.0", vec![5; 64]).is_ok(),
            "removed, a floor is set anew"
        );
    }

    /// With a key, each floor is reported valid or invalid and stored either way.
    /// Verifies: ADR-0060
    #[test]
    fn a_floor_signature_is_checked_and_never_refused() {
        use ring::signature::{Ed25519KeyPair, KeyPair as _};
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).expect("key");
        let keypair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("keypair");
        let floors = PackageFloors::open(Box::new(Memory::default()))
            .expect("open")
            .with_verifier(Some(Box::new(crate::config::Ed25519Verifier::new(
                keypair.public_key().as_ref().to_vec(),
            ))));
        let good = keypair
            .sign(&fleet_core::package::floor_statement("otelcol", "2.0.0"))
            .as_ref()
            .to_vec();
        let package = keypair
            .sign(&fleet_core::package::statement(
                "otelcol", "2.0.0", &[0; 32],
            ))
            .as_ref()
            .to_vec();
        assert_eq!(
            floors.put("otelcol", "2.0.0", good),
            Ok(SignatureState::Valid)
        );
        assert_eq!(
            floors.put("otelcol", "2.0.0", package),
            Ok(SignatureState::Invalid),
            "a package signature is no floor, and it is stored all the same"
        );
        assert_eq!(floors.list()[0].2, SignatureState::Invalid);
    }
}
