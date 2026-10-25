//! The floors on the filesystem (ADR-0061 clause 2): the default adapter behind
//! [`FloorStore`](crate::floors::FloorStore).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::floors::{Floor, FloorStore};

/// `<data_dir>/package-floors.json`: a JSON object that maps an Agent type to its version and
/// signature, owner-only, replaced in one step and synced before the act that changed it is
/// answered.
pub struct FsFloorStore {
    path: PathBuf,
}

impl FsFloorStore {
    /// The store in the data directory `root`.
    #[must_use]
    pub fn open(root: &Path) -> Self {
        FsFloorStore {
            path: root.join(super::PACKAGE_FLOORS),
        }
    }

    fn write(&self, floors: &BTreeMap<String, Floor>) -> Result<(), String> {
        let document: BTreeMap<&String, FloorMeta> = floors
            .iter()
            .map(|(agent_type, floor)| {
                (
                    agent_type,
                    FloorMeta {
                        version: floor.version.clone(),
                        signature: hex::encode(&floor.signature),
                    },
                )
            })
            .collect();
        let json = serde_json::to_vec_pretty(&document).expect("the floors serialize");
        super::replace_owner_only(&self.path, &json)
    }
}

/// One floor as the file holds it, the signature in hex.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FloorMeta {
    version: String,
    signature: String,
}

impl FloorStore for FsFloorStore {
    fn load(&self) -> Result<BTreeMap<String, Floor>, String> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => return Err(format!("cannot read {}: {e}", self.path.display())),
        };
        let document: BTreeMap<String, FloorMeta> = serde_json::from_slice(&bytes)
            .map_err(|e| format!("cannot parse {}: {e}", self.path.display()))?;
        document
            .into_iter()
            .map(|(agent_type, meta)| {
                let signature = hex::decode(&meta.signature).map_err(|e| {
                    format!(
                        "cannot parse {}: the signature of {agent_type:?} is not hex: {e}",
                        self.path.display()
                    )
                })?;
                Ok((
                    agent_type,
                    Floor {
                        version: meta.version,
                        signature,
                    },
                ))
            })
            .collect()
    }

    fn put(&self, agent_type: &str, floor: &Floor) -> Result<(), String> {
        let mut floors = self.load()?;
        floors.insert(agent_type.to_string(), floor.clone());
        self.write(&floors)
    }

    fn remove(&self, agent_type: &str) -> Result<(), String> {
        let mut floors = self.load()?;
        if floors.remove(agent_type).is_some() {
            self.write(&floors)?;
        }
        Ok(())
    }
}
