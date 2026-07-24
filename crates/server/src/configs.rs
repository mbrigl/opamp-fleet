//! Named Configurations with Selectors (ADR-0016): the persistent store, the type fit

use std::path::PathBuf;
use std::sync::RwLock;

use opamp::attributes;
use opamp::proto::AgentDescription;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

/// One revision of a Configuration: everything an operator writes, and everything the fleet can
/// be offered. The body is the Managed Process's own format — never interpreted here (the
/// specification forbids abstracting over an agent's configuration language).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Revision {
    /// The Selector (specification vocabulary): equality pairs, all of which must match an
    /// attribute the Agent reported. **Empty matches every Agent** (of the type, if one is set).
    #[serde(default)]
    pub selector: BTreeMap<String, String>,
    /// The configuration text handed to the Managed Process.
    pub body: String,
    /// The Baseline's `AgentConfigObject.role` (ADR-0016), travelling unchanged to the Agent.
    /// Empty — the default, and absent from the JSON — means top-level configuration, handled as
    /// it always was. `supplementary` means content the Managed Process reads *by path* rather
    /// than being configured with: a fragment, a certificate, a rule file. Any other value is
    /// carried verbatim and treated like `supplementary`; the protocol leaves the vocabulary to
    /// the Agent type, so nothing here guesses at one it does not know.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub role: String,
    /// The Agent type this Configuration is for (ADR-0016), compared raw for equality against
    /// the `service.name` the Agent reports — before the Selector, and independent of it.
    /// Empty — the default, and absent from the JSON — means every type: the fleet-wide
    /// degenerate case of ADR-0016 and cross-type `supplementary` content stay expressible.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub service_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Configuration {
    /// The name: a config-map key on the wire and a file name on both ends, so it follows the
    /// ADR-0014 name grammar.
    pub name: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
}

/// The role value this project understands (ADR-0016). Every other non-empty value is passed on
/// unchanged and handled the same way — written, not configured with.
pub const ROLE_SUPPLEMENTARY: &str = "supplementary";

/// The writable part of a [`Configuration`] — the `PUT` request body; the name comes from the
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationSpec {
    #[serde(default)]
    pub selector: BTreeMap<String, String>,
    pub body: String,
    /// See [`Revision::role`]. Absent means top-level configuration.
    #[serde(default)]
    pub role: String,
    /// See [`Revision::service_name`]. Absent means every Agent type.
    #[serde(default)]
    pub service_name: String,
}

/// One composed entry of an Agent's Remote configuration: what becomes one `AgentConfigMap` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigEntry {
    pub name: String,
    pub body: String,
    /// The Baseline's `AgentConfigObject.role` (ADR-0016); empty is top-level configuration.
    pub role: String,
}

/// entry, in name order, plus the hash that gates every push (goal 3). `None` entries never
#[derive(Clone)]
pub struct DesiredConfig {
    /// The entries, sorted by name — deterministic like the entry order the Managed Process sees
    /// (the Collector receives them as one `--config` per entry, ADR-0015).
    pub entries: Vec<ConfigEntry>,
    /// SHA-256 over the length-prefixed `(name, body, role)` triples in name order.
    pub hash: Vec<u8>,
}

impl DesiredConfig {
    fn new(mut entries: Vec<ConfigEntry>) -> Self {
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        let mut hasher = Sha256::new();
        for entry in &entries {
            // Length-prefixed framing keeps the hash unambiguous across entry boundaries.
            hasher.update((entry.name.len() as u64).to_le_bytes());
            hasher.update(entry.name.as_bytes());
            hasher.update((entry.body.len() as u64).to_le_bytes());
            hasher.update(entry.body.as_bytes());
            // A role changes what the Agent must *do* with an entry, so it belongs in the hash
            // that gates every push (goal 3) — an ungated role change would never be delivered.
            // An empty role is hashed as nothing at all rather than as an empty field: it means
            // "no role", it goes on the wire unset, and every Configuration that predates
            // ADR-0016 has one. Hashing it would move every existing hash on upgrade and restart
            // every Managed Process in the fleet to deliver a configuration identical to the one
            // it already runs — the precise opposite of what goal 3 asks. The framing stays
            // unambiguous: a role is length-prefixed like the other fields, and an omitted one
            // cannot be mistaken for a following entry, whose own two length-prefixed fields are
            // always longer than the single field a role would have been.
            // The type (ADR-0016) and the Selector stay out for the same reason as each other:
            // they decide *whom* an entry reaches, never what the Agent must do with it.
            if !entry.role.is_empty() {
                hasher.update((entry.role.len() as u64).to_le_bytes());
                hasher.update(entry.role.as_bytes());
            }
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
        attributes::string_value(&description.identifying_attributes, key)
            .or_else(|| attributes::string_value(&description.non_identifying_attributes, key))
            .is_some_and(|reported| reported == *value)
    })
}

/// Does this revision reach this Agent? Fit before aim (ADR-0016): a set `service_name` must
/// equal the `service.name` the Agent reports — compared raw, no canonicalisation, because there
/// is no canonical set of Agent types — and only then does the Selector run. An Agent that
/// reports no `service.name` matches only untyped revisions, exactly as any Selector pair fails
/// against an attribute the Agent does not report.
pub fn fits(revision: &Revision, description: Option<&AgentDescription>) -> bool {
    if !revision.service_name.is_empty() {
        let Some(description) = description else {
            return false;
        };
        let reported = attributes::string_value(
            &description.identifying_attributes,
            attributes::SERVICE_NAME,
        )
        .or_else(|| {
            attributes::string_value(
                &description.non_identifying_attributes,
                attributes::SERVICE_NAME,
            )
        });
        if reported != Some(revision.service_name.as_str()) {
            return false;
        }
    }
    matches(&revision.selector, description)
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

        validate_name(name).map_err(|e| format!("invalid name {name:?}: {e}"))?;
        if revision.body.trim().is_empty() {
            return Err("the configuration body is empty; refusing to store it".to_string());
        }
        let mut configs = self.configs.write().expect("configs lock");
        let config = match configs.get(name) {
            Some(existing) => Configuration {
                ..existing.clone()
            },
            None => Configuration {
                name: name.to_string(),
            },
        };
        self.persist(&config)?;
        configs.insert(config.name.clone(), config.clone());
        Ok(config)
    }

        let mut configs = self.configs.write().expect("configs lock");
        let Some(existing) = configs.get(name) else {
        };
        let mut configs = self.configs.write().expect("configs lock");
        let Some(existing) = configs.get(name) else {
        };
        self.persist(&config)?;
    }

    fn persist(&self, config: &Configuration) -> Result<(), String> {
        let path = self.dir.join(format!("{}.json", config.name));
        let temp = self.dir.join(format!("{}.json.tmp", config.name));
        let json = serde_json::to_vec_pretty(&config).expect("a Configuration serializes");
        std::fs::write(&temp, json).map_err(|e| format!("cannot write {}: {e}", temp.display()))?;
        std::fs::rename(&temp, &path).map_err(|e| format!("cannot persist {}: {e}", path.display()))
    }

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

    pub fn matching_names(&self, description: Option<&AgentDescription>) -> Vec<String> {
        self.configs
            .read()
            .expect("configs lock")
            .values()
            .map(|c| c.name.clone())
            .collect()
    }

            .read()
            .expect("configs lock")
            .values()
                Some(ConfigEntry {
                    body: revision.body.clone(),
                    role: revision.role.clone(),
                })
            })
            .collect();
        if entries.is_empty() {
            return None;
        }
        Some(DesiredConfig::new(entries))
    }
}

/// The ADR-0014 name grammar, applied to Configuration names: they become file names here, wire
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

    fn description(pairs: &[(&str, &str)]) -> AgentDescription {
        AgentDescription {
            identifying_attributes: pairs
                .iter()
                .map(|(k, v)| attributes::string_attr(k, v))
                .collect(),
            non_identifying_attributes: vec![],
        }
    }

    fn revision(selector: &[(&str, &str)], body: &str) -> Revision {
        Revision {
            selector: selector
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: body.to_string(),
            role: String::new(),
            service_name: String::new(),
        }
    }

    fn typed(mut revision: Revision, service_name: &str) -> Revision {
        revision.service_name = service_name.to_string();
        revision
    }

    fn with_role(mut revision: Revision, role: &str) -> Revision {
        revision.role = role.to_string();
        revision
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
        let one = revision(&[("os.type", "linux")], "b").selector;
        let both = revision(&[("os.type", "linux"), ("service.name", "otelcol")], "b").selector;
        let wrong = revision(&[("os.type", "windows")], "b").selector;
        let extra = revision(&[("os.type", "linux"), ("env", "prod")], "b").selector;
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
        let selector = revision(&[("env", "prod")], "b").selector;
        assert!(matches(&selector, Some(&desc)));
    }

    /// ADR-0016: the type fit runs before the Selector and independent of it.
    #[test]
    fn a_typed_revision_reaches_only_agents_of_its_type() {
        let otelcol = description(&[("service.name", "otelcol"), ("os.type", "linux")]);

        let for_otelcol = typed(revision(&[], "b"), "otelcol");
        assert!(fits(&for_otelcol, Some(&otelcol)));
        assert!(!fits(&for_otelcol, Some(&client)));
        assert!(
            !fits(&for_otelcol, None),
            "an undescribed agent matches only untyped revisions"
        );

        // Untyped means every type — ADR-0016's degenerate case survives.
        assert!(fits(&revision(&[], "b"), Some(&otelcol)));
        assert!(fits(&revision(&[], "b"), Some(&client)));
        assert!(fits(&revision(&[], "b"), None));

        // Type and Selector compose: both must hold.
        let narrowed = typed(revision(&[("os.type", "linux")], "b"), "otelcol");
        assert!(fits(&narrowed, Some(&otelcol)));
        assert!(!fits(
            &narrowed,
            Some(&description(&[("service.name", "otelcol")]))
        ));
    }

    /// ADR-0016 point 4: equality against a missing attribute fails, so an Agent that reports no
    /// `service.name` matches only untyped revisions.
    #[test]
    fn an_agent_without_a_type_matches_only_untyped_revisions() {
        let untyped_agent = description(&[("os.type", "linux")]);
        assert!(!fits(
            &typed(revision(&[], "b"), "otelcol"),
            Some(&untyped_agent)
        ));
        assert!(fits(&revision(&[], "b"), Some(&untyped_agent)));
    }

    #[test]
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
        store
            .expect("put");


        assert!(
        );
    }

    #[test]
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
        assert_eq!(released.entries[0].body, "v1\n");

        assert_eq!(
            released.hash,
        );

        assert_eq!(
            "v2\n"
        );
    }

        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
        let config = store.get("base").expect("base");
    #[test]
    fn the_store_round_trips_and_survives_a_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
        store
                "linux-only",
                revision(&[("os.type", "linux")], "exporters: {}\n"),
            )
            .expect("put");

        let reopened = ConfigStore::open(dir.path().to_path_buf()).expect("reopen");
        assert_eq!(reopened.list().len(), 2);
        let base = reopened.get("base").expect("base");
        assert!(
            reopened
                .get("linux-only")
                .expect("linux-only")
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
    }

    #[test]
    fn composition_is_name_sorted_and_hash_stable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");

        let names: Vec<&str> = desired.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["aa-base", "zz-extra"]);

    }

    #[test]
    fn a_role_travels_into_the_composed_entry_and_into_the_hash() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");

            &store,
            "ruleset",
            with_role(revision(&[], "rules: []\n"), ROLE_SUPPLEMENTARY),
        );
        assert_eq!(
            desired.entries,
            vec![
                ConfigEntry {
                    name: "base".to_string(),
                    body: "receivers: {}\n".to_string(),
                    role: String::new(),
                },
                ConfigEntry {
                    name: "ruleset".to_string(),
                    body: "rules: []\n".to_string(),
                    role: ROLE_SUPPLEMENTARY.to_string(),
                },
            ]
        );

        // Changing only the role changes the hash, so the edit actually reaches the fleet.
    }

    /// A Configuration written before ADR-0016 has no role, and its hash must not move when the
    /// Server is upgraded — a moved hash restarts every Managed Process in the fleet to deliver a
    #[test]
    fn an_empty_role_leaves_the_hash_where_it_was() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");

        // The hash this Server computed before `role` existed, pinned by construction: name and
        // body, length-prefixed, and nothing else.
        let mut expected = Sha256::new();
        expected.update((4u64).to_le_bytes());
        expected.update(b"base");
        expected.update((14u64).to_le_bytes());
        expected.update(b"receivers: {}\n");

        assert_eq!(
            expected.finalize().to_vec()
        );
    }

    #[test]
    fn a_role_survives_a_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
            &store,
            "certs",
            with_role(revision(&[], "PEM\n"), ROLE_SUPPLEMENTARY),
        );
        let reopened = ConfigStore::open(dir.path().to_path_buf()).expect("reopen");
        assert_eq!(
            ROLE_SUPPLEMENTARY
        );
    }

    /// absent on the way in and absent on the way out, so every stored file stays minimal.
    #[test]
    fn unset_role_and_type_are_absent_from_the_stored_json() {
        let json = serde_json::to_string(&revision(&[], "b")).expect("serialize");
        assert!(!json.contains("role"), "{json}");
        assert!(!json.contains("service_name"), "{json}");

        let restored: Revision = serde_json::from_str(r#"{"body":"b"}"#).expect("deserialize");
        assert_eq!(restored.role, "");
        assert_eq!(restored.service_name, "");

        let json = serde_json::to_string(&typed(
            with_role(revision(&[], "b"), "supplementary"),
            "otelcol",
        ))
        .expect("serialize");
        assert!(json.contains(r#""role":"supplementary""#), "{json}");
        assert!(json.contains(r#""service_name":"otelcol""#), "{json}");
    }

    #[test]
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");

        let linux = description(&[("os.type", "linux"), ("service.name", "otelcol")]);
        assert_eq!(
            store.matching_names(Some(&linux)),
            ["base", "linux", "otelcol-only"]
        );

        store.delete("base").expect("delete");
        store.delete("otelcol-only").expect("delete");
        let nothing = description(&[("os.type", "darwin")]);
        assert!(store.matching_names(Some(&nothing)).is_empty());
    }
}
