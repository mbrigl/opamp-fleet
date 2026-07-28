//! Named Configurations with Selectors (ADR-0012): the persistent store, the Selector matching,
//! and the composition of each Agent's Remote configuration out of everything that matches it.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::RwLock;

use opamp::proto::{any_value, AgentDescription, KeyValue};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

/// A named Configuration: an opaque body targeted at the subset of the fleet its Selector
/// matches. The body is the Managed Process's own format — never interpreted here (the
/// specification forbids abstracting over an agent's configuration language).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Configuration {
    /// The name: a config-map key on the wire and a file name on both ends, so it follows the
    /// ADR-0010 name grammar.
    pub name: String,
    /// The Selector (specification vocabulary): equality pairs, all of which must match an
    /// attribute the Agent reported. **Empty matches every Agent.**
    #[serde(default)]
    pub selector: BTreeMap<String, String>,
    /// The configuration text handed to the Managed Process.
    pub body: String,
}

/// The writable part of a [`Configuration`] — the `PUT` request body; the name comes from the URL.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationSpec {
    #[serde(default)]
    pub selector: BTreeMap<String, String>,
    pub body: String,
}

/// One Agent's composed Remote configuration: every matching Configuration as a named entry, in
/// name order, plus the hash that gates every push (goal 3). `None` entries never exist — an
/// Agent matching nothing gets no offer at all.
#[derive(Clone)]
pub struct DesiredConfig {
    /// `(name, body)` pairs, sorted by name — deterministic like the entry order the Managed
    /// Process sees (the Collector receives them as one `--config` per entry, ADR-0011).
    pub entries: Vec<(String, String)>,
    /// SHA-256 over the length-prefixed `(name, body)` pairs in name order.
    pub hash: Vec<u8>,
}

impl DesiredConfig {
    fn new(mut entries: Vec<(String, String)>) -> Self {
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let mut hasher = Sha256::new();
        for (name, body) in &entries {
            // Length-prefixed framing keeps the hash unambiguous across entry boundaries.
            hasher.update((name.len() as u64).to_le_bytes());
            hasher.update(name.as_bytes());
            hasher.update((body.len() as u64).to_le_bytes());
            hasher.update(body.as_bytes());
        }
        DesiredConfig {
            entries,
            hash: hasher.finalize().to_vec(),
        }
    }
}

/// Does this Selector match this Agent? Equality over every reported attribute — identifying and
/// non-identifying alike, string values only. An Agent that has not described itself yet matches
/// only the empty Selector.
pub fn matches(
    selector: &BTreeMap<String, String>,
    description: Option<&AgentDescription>,
) -> bool {
    if selector.is_empty() {
        return true;
    }
    let Some(description) = description else {
        return false;
    };
    selector.iter().all(|(key, value)| {
        attr_value(&description.identifying_attributes, key)
            .or_else(|| attr_value(&description.non_identifying_attributes, key))
            .is_some_and(|reported| reported == *value)
    })
}

fn attr_value<'a>(attributes: &'a [KeyValue], key: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|kv| kv.key == key)
        .and_then(|kv| kv.value.as_ref())
        .and_then(|v| v.value.as_ref())
        .and_then(|v| match v {
            any_value::Value::StringValue(s) => Some(s.as_str()),
            _ => None,
        })
}

/// The persistent Configuration store: one JSON file per Configuration under `config_dir`,
/// written atomically, restored at startup. The in-memory map is the single source the control
/// loop reads; the files exist so a Server restart does not lose what the fleet should run.
pub struct ConfigStore {
    dir: PathBuf,
    configs: RwLock<BTreeMap<String, Configuration>>,
}

impl ConfigStore {
    /// Opens the store, creating the directory and loading every persisted Configuration. A file
    /// that does not parse is a startup error — never silently ignored (ADR-0008's principle).
    pub fn open(dir: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let mut configs = BTreeMap::new();
        let entries =
            std::fs::read_dir(&dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        for entry in entries {
            let path = entry
                .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
                .path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let config: Configuration = serde_json::from_str(&text)
                .map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
            validate_name(&config.name)
                .map_err(|e| format!("invalid configuration name in {}: {e}", path.display()))?;
            configs.insert(config.name.clone(), config);
        }
        Ok(ConfigStore {
            dir,
            configs: RwLock::new(configs),
        })
    }

    /// All Configurations, in name order.
    pub fn list(&self) -> Vec<Configuration> {
        self.configs
            .read()
            .expect("configs lock")
            .values()
            .cloned()
            .collect()
    }

    pub fn get(&self, name: &str) -> Option<Configuration> {
        self.configs
            .read()
            .expect("configs lock")
            .get(name)
            .cloned()
    }

    /// Creates or replaces a Configuration: validated, persisted atomically (temp file + rename),
    /// then visible to the control loop.
    pub fn put(&self, config: Configuration) -> Result<(), String> {
        validate_name(&config.name).map_err(|e| format!("invalid name {:?}: {e}", config.name))?;
        if config.body.trim().is_empty() {
            return Err("the configuration body is empty; refusing to distribute it".to_string());
        }
        let path = self.dir.join(format!("{}.json", config.name));
        let temp = self.dir.join(format!("{}.json.tmp", config.name));
        let json = serde_json::to_vec_pretty(&config).expect("a Configuration serializes");
        std::fs::write(&temp, json).map_err(|e| format!("cannot write {}: {e}", temp.display()))?;
        std::fs::rename(&temp, &path)
            .map_err(|e| format!("cannot persist {}: {e}", path.display()))?;
        self.configs
            .write()
            .expect("configs lock")
            .insert(config.name.clone(), config);
        Ok(())
    }

    /// Deletes a Configuration; `Ok(false)` when none of that name exists.
    pub fn delete(&self, name: &str) -> Result<bool, String> {
        let mut configs = self.configs.write().expect("configs lock");
        if configs.remove(name).is_none() {
            return Ok(false);
        }
        let path = self.dir.join(format!("{name}.json"));
        std::fs::remove_file(&path)
            .map_err(|e| format!("cannot delete {}: {e}", path.display()))?;
        Ok(true)
    }

    /// The names of the Configurations matching this Agent, in name order.
    pub fn matching_names(&self, description: Option<&AgentDescription>) -> Vec<String> {
        self.configs
            .read()
            .expect("configs lock")
            .values()
            .filter(|c| matches(&c.selector, description))
            .map(|c| c.name.clone())
            .collect()
    }

    /// This Agent's composed Remote configuration, or `None` when nothing matches — in which
    /// case no offer is made and the Agent keeps running what it already runs (goal 9).
    pub fn desired_for(&self, description: Option<&AgentDescription>) -> Option<DesiredConfig> {
        let entries: Vec<(String, String)> = self
            .configs
            .read()
            .expect("configs lock")
            .values()
            .filter(|c| matches(&c.selector, description))
            .map(|c| (c.name.clone(), c.body.clone()))
            .collect();
        if entries.is_empty() {
            return None;
        }
        Some(DesiredConfig::new(entries))
    }
}

/// The ADR-0010 name grammar, applied to Configuration names: they become file names here, wire
/// config-map keys, and entry files on every Client — including Windows ones, hence the reserved
/// device names. Kept in sync with the Client's instance-name parser by the shared test corpus.
pub fn validate_name(name: &str) -> Result<(), String> {
    const WINDOWS_RESERVED: [&str; 22] = [
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
        "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    if name.is_empty() || name.len() > 32 {
        return Err("must be 1–32 characters".to_string());
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err("only lowercase letters, digits, and '-' are allowed".to_string());
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Err("must not start or end with '-'".to_string());
    }
    if WINDOWS_RESERVED.contains(&name) {
        return Err(format!("{name:?} is a reserved device name on Windows"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use opamp::proto::AnyValue;

    fn description(pairs: &[(&str, &str)]) -> AgentDescription {
        AgentDescription {
            identifying_attributes: pairs
                .iter()
                .map(|(k, v)| KeyValue {
                    key: k.to_string(),
                    value: Some(AnyValue {
                        value: Some(any_value::Value::StringValue(v.to_string())),
                    }),
                })
                .collect(),
            non_identifying_attributes: vec![],
        }
    }

    fn config(name: &str, selector: &[(&str, &str)], body: &str) -> Configuration {
        Configuration {
            name: name.to_string(),
            selector: selector
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: body.to_string(),
        }
    }

    #[test]
    fn an_empty_selector_matches_everything_even_an_undescribed_agent() {
        assert!(matches(&BTreeMap::new(), None));
        assert!(matches(&BTreeMap::new(), Some(&description(&[]))));
    }

    #[test]
    fn every_selector_pair_must_equal_a_reported_attribute() {
        let desc = description(&[("service.name", "otelcol"), ("os.type", "linux")]);
        let one = config("x", &[("os.type", "linux")], "b").selector;
        let both = config(
            "x",
            &[("os.type", "linux"), ("service.name", "otelcol")],
            "b",
        )
        .selector;
        let wrong = config("x", &[("os.type", "windows")], "b").selector;
        let extra = config("x", &[("os.type", "linux"), ("env", "prod")], "b").selector;
        assert!(matches(&one, Some(&desc)));
        assert!(matches(&both, Some(&desc)));
        assert!(!matches(&wrong, Some(&desc)));
        assert!(
            !matches(&extra, Some(&desc)),
            "an unreported key never matches"
        );
        assert!(
            !matches(&one, None),
            "no description matches only the empty Selector"
        );
    }

    #[test]
    fn non_identifying_attributes_match_too() {
        let desc = AgentDescription {
            identifying_attributes: vec![],
            non_identifying_attributes: description(&[("env", "prod")]).identifying_attributes,
        };
        let selector = config("x", &[("env", "prod")], "b").selector;
        assert!(matches(&selector, Some(&desc)));
    }

    #[test]
    fn the_store_round_trips_and_survives_a_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
        store
            .put(config("base", &[], "receivers: {}\n"))
            .expect("put");
        store
            .put(config(
                "linux-only",
                &[("os.type", "linux")],
                "exporters: {}\n",
            ))
            .expect("put");

        let reopened = ConfigStore::open(dir.path().to_path_buf()).expect("reopen");
        assert_eq!(reopened.list().len(), 2);
        assert_eq!(reopened.get("base").expect("base").body, "receivers: {}\n");

        assert!(reopened.delete("base").expect("delete"));
        assert!(!reopened
            .delete("base")
            .expect("second delete finds nothing"));
        assert_eq!(
            ConfigStore::open(dir.path().to_path_buf())
                .expect("open")
                .list()
                .len(),
            1
        );
    }

    #[test]
    fn the_store_rejects_bad_names_and_empty_bodies() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
        assert!(store.put(config("Bad Name", &[], "x")).is_err());
        assert!(store.put(config("con", &[], "x")).is_err());
        assert!(store.put(config("ok", &[], "  \n")).is_err());
    }

    #[test]
    fn composition_is_name_sorted_and_hash_stable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
        store.put(config("zz-extra", &[], "z")).expect("put");
        store.put(config("aa-base", &[], "a")).expect("put");

        let desired = store.desired_for(None).expect("desired");
        let names: Vec<&str> = desired.entries.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["aa-base", "zz-extra"]);
        assert_eq!(desired.hash, store.desired_for(None).expect("again").hash);

        // The hash covers names and bodies: renaming or editing either changes it.
        store.put(config("aa-base", &[], "a2")).expect("edit");
        assert_ne!(store.desired_for(None).expect("edited").hash, desired.hash);
    }

    #[test]
    fn only_matching_configurations_compose_and_none_means_no_offer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
        store.put(config("base", &[], "b")).expect("put");
        store
            .put(config("linux", &[("os.type", "linux")], "l"))
            .expect("put");
        store
            .put(config("windows", &[("os.type", "windows")], "w"))
            .expect("put");

        let linux = description(&[("os.type", "linux")]);
        let desired = store.desired_for(Some(&linux)).expect("desired");
        assert_eq!(store.matching_names(Some(&linux)), ["base", "linux"]);
        assert_eq!(desired.entries.len(), 2);

        store.delete("base").expect("delete");
        let nothing = description(&[("os.type", "darwin")]);
        assert!(store.desired_for(Some(&nothing)).is_none());
        assert!(store.matching_names(Some(&nothing)).is_empty());
    }
}
