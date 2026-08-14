//! Named Configurations with Selectors (ADR-0012): the persistent store, the type fit
//! (ADR-0054) and Selector matching, and the composition of each Agent's Remote configuration.
//! Since ADR-0061 saving is the only content state — **a saved Configuration reaches nobody by
//! itself**. What an Agent is offered is composed from the per-Agent assignments the operator's
//! explicit rollout acts wrote; the store's part is to keep the saved revision, and to retain
//! every pinned revision an assignment still references.

use std::collections::{BTreeMap, BTreeSet};
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
    /// The Agent type this Configuration is for (ADR-0054), compared raw for equality against
    /// the `service.name` the Agent reports — before the Selector, and independent of it.
    /// Empty — the default, and absent from the JSON — means every type: the fleet-wide
    /// degenerate case of ADR-0012 and cross-type `supplementary` content stay expressible.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub service_name: String,
}

/// The hash an assignment pins a revision by (ADR-0061): over what the Agent is delivered — body
/// and role, length-prefixed — never the Selector or the type, which decide *whom* a revision
/// reaches rather than what it is.
pub fn revision_hash(revision: &Revision) -> String {
    let mut hasher = Sha256::new();
    hasher.update((revision.body.len() as u64).to_le_bytes());
    hasher.update(revision.body.as_bytes());
    hasher.update((revision.role.len() as u64).to_le_bytes());
    hasher.update(revision.role.as_bytes());
    hex::encode(hasher.finalize())
}

/// A named Configuration as the store holds it (ADR-0061): the saved revision every `PUT`
/// writes — the only revision an operator edits — and the retained revisions that per-Agent
/// assignments pin by content hash. Saving only saves; a revision enters `retained` through a
/// rollout act and leaves it when no assignment references it any more.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Configuration {
    /// The name: a config-map key on the wire and a file name on both ends, so it follows the
    /// ADR-0010 name grammar.
    pub name: String,
    /// What editing operates on, and what a rollout act releases as one snapshot.
    pub saved: Revision,
    /// The revisions in force somewhere in the fleet, keyed by [`revision_hash`]. An assignment
    /// pins one of these; the saved revision is copied in here at the moment it is rolled out, so
    /// a later edit changes nothing on any Agent.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub retained: BTreeMap<String, Revision>,
}

/// The role value this project understands (ADR-0016). Every other non-empty value is passed on
/// unchanged and handled the same way — written, not configured with.
pub const ROLE_SUPPLEMENTARY: &str = "supplementary";

/// The writable part of a [`Configuration`] — the `PUT` request body; the name comes from the
/// URL. Writes the saved revision (ADR-0061): saving only saves.
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

/// One Agent's composed Remote configuration: every assigned Configuration revision as a named
/// entry, in name order, plus the hash that gates every push (goal 3). `None` entries never
/// exist — an Agent assigned nothing gets no offer at all.
#[derive(Clone)]
pub struct DesiredConfig {
    /// The entries, sorted by name — deterministic like the entry order the Managed Process sees
    /// (the Collector receives them as one `--config` per entry, ADR-0011).
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
            // The type (ADR-0054) and the Selector stay out for the same reason as each other:
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

/// Does this revision reach this Agent? Fit before aim (ADR-0054): a set `service_name` must
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

/// A Configuration file written before ADR-0055: the flat `{name, selector, body, role}` shape,
/// which was both the stored record and the API resource. What it held was in force, so it loads
/// as saved **and** formerly published — the migration seed for the per-Agent assignments
/// (ADR-0061 point 9).
#[derive(Deserialize)]
struct LegacyFlatConfiguration {
    name: String,
    #[serde(flatten)]
    revision: Revision,
}

/// A Configuration file written under ADR-0055: two revisions, draft and published. The draft
/// becomes the saved revision; a published revision is retained and remembered as formerly
/// published, so existing Agent records can load as "rolled out to what was in force"
/// (ADR-0061 point 9).
#[derive(Deserialize)]
struct LegacyTwoRevisionConfiguration {
    name: String,
    draft: Revision,
    #[serde(default)]
    published: Option<Revision>,
}

/// The persistent Configuration store: one JSON file per Configuration under `config_dir`,
/// written atomically, restored at startup. The in-memory map is the single source the control
/// loop reads; the files exist so a Server restart does not lose what the fleet should run.
pub struct ConfigStore {
    dir: PathBuf,
    configs: RwLock<BTreeMap<String, Configuration>>,
    /// What a pre-ADR-0061 store said was in force — Configuration name to the hash of its
    /// published revision (the revision itself is in `retained`). Read once by the fleet to seed
    /// the assignments of Agent records that predate the ADR; empty for a store born under it.
    formerly_published: BTreeMap<String, String>,
}

impl ConfigStore {
    /// Opens the store, creating the directory and loading every persisted Configuration. A file
    /// that does not parse is a startup error — never silently ignored (ADR-0008's principle).
    pub fn open(dir: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let mut configs = BTreeMap::new();
        let mut formerly_published = BTreeMap::new();
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
            let value: serde_json::Value = serde_json::from_str(&text)
                .map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
            let config: Configuration = if value.get("saved").is_some() {
                serde_json::from_value(value)
                    .map_err(|e| format!("cannot parse {}: {e}", path.display()))?
            } else if value.get("draft").is_some() {
                // ADR-0055 shape. The file is left as it is until the next write, so an Agent
                // record that has not migrated yet can still be seeded from it on a later start.
                let legacy: LegacyTwoRevisionConfiguration = serde_json::from_value(value)
                    .map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
                let mut retained = BTreeMap::new();
                if let Some(published) = legacy.published {
                    let hash = revision_hash(&published);
                    formerly_published.insert(legacy.name.clone(), hash.clone());
                    retained.insert(hash, published);
                }
                Configuration {
                    name: legacy.name,
                    saved: legacy.draft,
                    retained,
                }
            } else {
                let legacy: LegacyFlatConfiguration = serde_json::from_value(value)
                    .map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
                let hash = revision_hash(&legacy.revision);
                formerly_published.insert(legacy.name.clone(), hash.clone());
                Configuration {
                    name: legacy.name,
                    saved: legacy.revision.clone(),
                    retained: BTreeMap::from([(hash, legacy.revision)]),
                }
            };
            validate_name(&config.name)
                .map_err(|e| format!("invalid configuration name in {}: {e}", path.display()))?;
            configs.insert(config.name.clone(), config);
        }
        Ok(ConfigStore {
            dir,
            configs: RwLock::new(configs),
            formerly_published,
        })
    }

    /// What a pre-ADR-0061 store said was in force, for seeding the assignments of Agent records
    /// that predate the ADR: `(name, published revision, its hash)` per formerly published
    /// Configuration. Empty for a store born under ADR-0061.
    pub fn formerly_published(&self) -> Vec<(String, Revision, String)> {
        let configs = self.configs.read().expect("configs lock");
        self.formerly_published
            .iter()
            .filter_map(|(name, hash)| {
                let revision = configs.get(name)?.retained.get(hash)?.clone();
                Some((name.clone(), revision, hash.clone()))
            })
            .collect()
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

    /// Creates a Configuration or replaces its **saved** revision (ADR-0061): validated,
    /// persisted atomically (temp file + rename) — and distributed to nobody. Every retained
    /// revision keeps being offered untouched to the Agents assigned it.
    pub fn put_saved(&self, name: &str, revision: Revision) -> Result<Configuration, String> {
        validate_name(name).map_err(|e| format!("invalid name {name:?}: {e}"))?;
        if revision.body.trim().is_empty() {
            return Err("the configuration body is empty; refusing to store it".to_string());
        }
        let mut configs = self.configs.write().expect("configs lock");
        let config = match configs.get(name) {
            Some(existing) => Configuration {
                saved: revision,
                ..existing.clone()
            },
            None => Configuration {
                name: name.to_string(),
                saved: revision,
                retained: BTreeMap::new(),
            },
        };
        self.persist(&config)?;
        configs.insert(config.name.clone(), config.clone());
        Ok(config)
    }

    /// Pins the saved revision for an assignment (ADR-0061): copies it into `retained` under its
    /// content hash — idempotently — and returns that hash. This is the store's half of a rollout
    /// act; the fleet writes the returned hash into the Agent's assignment.
    pub fn retain_saved(&self, name: &str) -> Result<String, String> {
        let mut configs = self.configs.write().expect("configs lock");
        let Some(existing) = configs.get(name) else {
            return Err(format!("no configuration {name:?}"));
        };
        let hash = revision_hash(&existing.saved);
        if existing.retained.contains_key(&hash) {
            return Ok(hash);
        }
        let mut config = existing.clone();
        config.retained.insert(hash.clone(), config.saved.clone());
        self.persist(&config)?;
        configs.insert(config.name.clone(), config);
        Ok(hash)
    }

    /// Drops every retained revision of `name` that `referenced` does not name — the collection
    /// half of ADR-0061's "the store retains every revision an assignment still references". The
    /// caller computes `referenced` from the fleet's assignments; a revision left behind by a
    /// failed write is harmless and collected on the next act.
    pub fn retain_only(&self, name: &str, referenced: &BTreeSet<String>) -> Result<(), String> {
        let mut configs = self.configs.write().expect("configs lock");
        let Some(existing) = configs.get(name) else {
            return Ok(());
        };
        if existing
            .retained
            .keys()
            .all(|hash| referenced.contains(hash))
        {
            return Ok(());
        }
        let mut config = existing.clone();
        config.retained.retain(|hash, _| referenced.contains(hash));
        self.persist(&config)?;
        configs.insert(config.name.clone(), config);
        Ok(())
    }

    fn persist(&self, config: &Configuration) -> Result<(), String> {
        let path = self.dir.join(format!("{}.json", config.name));
        let temp = self.dir.join(format!("{}.json.tmp", config.name));
        let json = serde_json::to_vec_pretty(&config).expect("a Configuration serializes");
        std::fs::write(&temp, json).map_err(|e| format!("cannot write {}: {e}", temp.display()))?;
        std::fs::rename(&temp, &path).map_err(|e| format!("cannot persist {}: {e}", path.display()))
    }

    /// Deletes a Configuration — the saved revision and every retained one; `Ok(false)` when none
    /// of that name exists. The caller removes the assignments that referenced it.
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

    /// The names of the Configurations whose **saved** revision reaches this Agent, in name
    /// order — the candidates a rollout act would release to it (ADR-0061). Never an offer.
    pub fn matching_names(&self, description: Option<&AgentDescription>) -> Vec<String> {
        self.configs
            .read()
            .expect("configs lock")
            .values()
            .filter(|c| fits(&c.saved, description))
            .map(|c| c.name.clone())
            .collect()
    }

    /// The candidates for one Agent (ADR-0061): each Configuration whose saved revision fits it,
    /// as `(name, hash of the saved revision)` in name order. What the fleet view diffs against
    /// the Agent's assignments to show what is waiting, and what "roll out everything" assigns.
    pub fn candidates_for(
        &self,
        description: Option<&AgentDescription>,
    ) -> Vec<(String, String)> {
        self.configs
            .read()
            .expect("configs lock")
            .values()
            .filter(|c| fits(&c.saved, description))
            .map(|c| (c.name.clone(), revision_hash(&c.saved)))
            .collect()
    }

    /// One Agent's composed Remote configuration, from its assignments (ADR-0061): each assigned
    /// Configuration's pinned revision as one entry. `None` when the Agent is assigned nothing —
    /// no offer is made and it keeps running what it already runs (goal 9). An assignment whose
    /// Configuration or revision is gone composes nothing rather than failing: deletion removes
    /// assignments, so the case is a race, not a state.
    pub fn compose(&self, assignments: &BTreeMap<String, String>) -> Option<DesiredConfig> {
        let configs = self.configs.read().expect("configs lock");
        let entries: Vec<ConfigEntry> = assignments
            .iter()
            .filter_map(|(name, hash)| {
                let config = configs.get(name)?;
                let revision = config.retained.get(hash)?;
                Some(ConfigEntry {
                    name: name.clone(),
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

    /// Save and pin in one step — the tests' shorthand for "this is assigned somewhere", plus the
    /// assignment map an Agent holding exactly this would carry.
    fn put_assigned(
        store: &ConfigStore,
        name: &str,
        revision: Revision,
    ) -> BTreeMap<String, String> {
        store.put_saved(name, revision).expect("put");
        let hash = store.retain_saved(name).expect("retain");
        BTreeMap::from([(name.to_string(), hash)])
    }

    fn merge(maps: &[&BTreeMap<String, String>]) -> BTreeMap<String, String> {
        maps.iter()
            .flat_map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())))
            .collect()
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

    /// ADR-0054: the type fit runs before the Selector and independent of it.
    #[test]
    fn a_typed_revision_reaches_only_agents_of_its_type() {
        let otelcol = description(&[("service.name", "otelcol"), ("os.type", "linux")]);
        let client = description(&[("service.name", "opamp-fleet-client")]);

        let for_otelcol = typed(revision(&[], "b"), "otelcol");
        assert!(fits(&for_otelcol, Some(&otelcol)));
        assert!(!fits(&for_otelcol, Some(&client)));
        assert!(
            !fits(&for_otelcol, None),
            "an undescribed agent matches only untyped revisions"
        );

        // Untyped means every type — ADR-0012's degenerate case survives.
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

    /// ADR-0054 point 4: equality against a missing attribute fails, so an Agent that reports no
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

    /// ADR-0061: saving only saves. A saved Configuration is composed for nobody until an
    /// assignment pins it, and only the assignment decides what an Agent is offered.
    #[test]
    fn a_saved_configuration_reaches_nobody_without_an_assignment() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
        store
            .put_saved("base", revision(&[], "receivers: {}\n"))
            .expect("put");

        assert!(
            store.compose(&BTreeMap::new()).is_none(),
            "no assignment, no offer"
        );
        assert_eq!(
            store.matching_names(None),
            ["base"],
            "the candidate is visible"
        );

        let assignments = BTreeMap::from([(
            "base".to_string(),
            store.retain_saved("base").expect("retain"),
        )]);
        assert_eq!(
            store.compose(&assignments).expect("offered").entries.len(),
            1
        );
        assert!(
            store.retain_saved("missing").is_err(),
            "pinning an unknown name finds nothing"
        );
    }

    /// ADR-0061 point 2: a rollout pins a snapshot. Editing the saved revision afterwards changes
    /// nothing for an Agent assigned the pinned one, and the candidate hash moves so the fleet
    /// view can show a newer save waiting.
    #[test]
    fn an_assignment_pins_a_snapshot_and_later_edits_wait() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
        let assignments = put_assigned(&store, "base", revision(&[], "v1\n"));
        let released = store.compose(&assignments).expect("offered");
        assert_eq!(released.entries[0].body, "v1\n");

        store.put_saved("base", revision(&[], "v2\n")).expect("edit");
        assert_eq!(
            store.compose(&assignments).expect("offered").hash,
            released.hash,
            "the Agent keeps its pinned revision"
        );
        let candidates = store.candidates_for(None);
        assert_eq!(candidates.len(), 1);
        assert_ne!(
            candidates[0].1, assignments["base"],
            "the candidate hash moved: a newer save is waiting"
        );

        // The next rollout act pins the edit.
        let assignments = put_assigned(&store, "base", revision(&[], "v2\n"));
        assert_eq!(
            store.compose(&assignments).expect("offered").entries[0].body,
            "v2\n"
        );
    }

    /// ADR-0061: a retained revision lives exactly as long as an assignment references it.
    #[test]
    fn retain_only_collects_unreferenced_revisions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
        let first = put_assigned(&store, "base", revision(&[], "v1\n"));
        store.put_saved("base", revision(&[], "v2\n")).expect("edit");
        let second_hash = store.retain_saved("base").expect("retain");
        assert_eq!(store.get("base").expect("base").retained.len(), 2);

        store
            .retain_only("base", &BTreeSet::from([second_hash.clone()]))
            .expect("gc");
        let config = store.get("base").expect("base");
        assert_eq!(config.retained.len(), 1, "the orphaned revision is gone");
        assert!(config.retained.contains_key(&second_hash));
        assert!(
            store.compose(&first).is_none(),
            "the collected revision composes nothing"
        );
        store
            .retain_only("missing", &BTreeSet::new())
            .expect("collecting an unknown name is a no-op");
    }

    /// ADR-0061 point 9: a flat pre-ADR-0055 file loads as saved **and** formerly published, so
    /// the fleet can seed old Agent records as "rolled out to what was in force".
    #[test]
    fn a_legacy_flat_file_loads_as_formerly_published() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("keeper.json"),
            r#"{"name":"keeper","selector":{"os.type":"linux"},"body":"receivers: {}\n","role":"supplementary"}"#,
        )
        .expect("write the legacy file");

        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
        let config = store.get("keeper").expect("keeper");
        assert_eq!(config.saved.role, "supplementary");
        let formerly = store.formerly_published();
        assert_eq!(formerly.len(), 1);
        let (name, revision, hash) = &formerly[0];
        assert_eq!(name, "keeper");
        assert_eq!(revision.selector["os.type"], "linux");
        assert_eq!(config.retained[hash], *revision);
        assert_eq!(
            store.matching_names(Some(&description(&[("os.type", "linux")]))),
            ["keeper"]
        );
    }

    /// ADR-0061 point 9, the ADR-0055 shape: the draft becomes the saved revision, the published
    /// one is retained and reported as formerly published — and a never-published draft seeds no
    /// assignment at all.
    #[test]
    fn a_two_revision_file_loads_with_its_published_revision_retained() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("staged.json"),
            r#"{"name":"staged","draft":{"body":"v2\n"},"published":{"body":"v1\n"}}"#,
        )
        .expect("write");
        std::fs::write(
            dir.path().join("never.json"),
            r#"{"name":"never","draft":{"body":"n\n"}}"#,
        )
        .expect("write");

        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
        let staged = store.get("staged").expect("staged");
        assert_eq!(staged.saved.body, "v2\n", "the draft is the saved revision");
        assert_eq!(staged.retained.len(), 1);
        let formerly = store.formerly_published();
        assert_eq!(formerly.len(), 1, "the never-published draft seeds nothing");
        assert_eq!(formerly[0].0, "staged");
        assert_eq!(formerly[0].1.body, "v1\n", "what was in force is the seed");
        assert!(store.get("never").expect("never").retained.is_empty());
    }

    #[test]
    fn the_store_round_trips_and_survives_a_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
        let assignments = put_assigned(&store, "base", revision(&[], "receivers: {}\n"));
        store
            .put_saved(
                "linux-only",
                revision(&[("os.type", "linux")], "exporters: {}\n"),
            )
            .expect("put");

        let reopened = ConfigStore::open(dir.path().to_path_buf()).expect("reopen");
        assert_eq!(reopened.list().len(), 2);
        let base = reopened.get("base").expect("base");
        assert_eq!(base.saved.body, "receivers: {}\n");
        assert_eq!(
            reopened
                .compose(&assignments)
                .expect("the pinned revision survives the reopen")
                .entries[0]
                .body,
            "receivers: {}\n"
        );
        assert!(
            reopened
                .get("linux-only")
                .expect("linux-only")
                .retained
                .is_empty(),
            "a never-assigned Configuration retains nothing across the reopen"
        );
        assert!(
            reopened.formerly_published().is_empty(),
            "a store born under ADR-0061 seeds no migration"
        );

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
        assert!(store.put_saved("Bad Name", revision(&[], "x")).is_err());
        assert!(store.put_saved("con", revision(&[], "x")).is_err());
        assert!(store.put_saved("ok", revision(&[], "  \n")).is_err());
    }

    #[test]
    fn composition_is_name_sorted_and_hash_stable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
        let zz = put_assigned(&store, "zz-extra", revision(&[], "z"));
        let aa = put_assigned(&store, "aa-base", revision(&[], "a"));
        let assignments = merge(&[&zz, &aa]);

        let desired = store.compose(&assignments).expect("desired");
        let names: Vec<&str> = desired.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["aa-base", "zz-extra"]);
        assert_eq!(desired.hash, store.compose(&assignments).expect("again").hash);

        // The hash covers names and bodies: an edit changes it — once a rollout act pins it.
        let aa = put_assigned(&store, "aa-base", revision(&[], "a2"));
        let assignments = merge(&[&zz, &aa]);
        assert_ne!(store.compose(&assignments).expect("edited").hash, desired.hash);
    }

    #[test]
    fn a_role_travels_into_the_composed_entry_and_into_the_hash() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
        let base = put_assigned(&store, "base", revision(&[], "receivers: {}\n"));
        let without = store.compose(&base).expect("desired").hash;

        let ruleset = put_assigned(
            &store,
            "ruleset",
            with_role(revision(&[], "rules: []\n"), ROLE_SUPPLEMENTARY),
        );
        let assignments = merge(&[&base, &ruleset]);
        let desired = store.compose(&assignments).expect("desired");
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
        let ruleset = put_assigned(&store, "ruleset", revision(&[], "rules: []\n"));
        let assignments = merge(&[&base, &ruleset]);
        assert_ne!(store.compose(&assignments).expect("desired").hash, desired.hash);
        assert_ne!(store.compose(&assignments).expect("desired").hash, without);
    }

    /// A Configuration written before ADR-0016 has no role, and its hash must not move when the
    /// Server is upgraded — a moved hash restarts every Managed Process in the fleet to deliver a
    /// configuration identical to the one it already runs. The same pin guards ADR-0054, ADR-0055
    /// and ADR-0061: neither the type, nor a revision split, nor the assignment model may enter
    /// the hash.
    #[test]
    fn an_empty_role_leaves_the_hash_where_it_was() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
        let assignments = put_assigned(&store, "base", revision(&[], "receivers: {}\n"));

        // The hash this Server computed before `role` existed, pinned by construction: name and
        // body, length-prefixed, and nothing else.
        let mut expected = Sha256::new();
        expected.update((4u64).to_le_bytes());
        expected.update(b"base");
        expected.update((14u64).to_le_bytes());
        expected.update(b"receivers: {}\n");

        assert_eq!(
            store.compose(&assignments).expect("desired").hash,
            expected.finalize().to_vec()
        );
    }

    #[test]
    fn a_role_survives_a_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
        put_assigned(
            &store,
            "certs",
            with_role(revision(&[], "PEM\n"), ROLE_SUPPLEMENTARY),
        );
        let reopened = ConfigStore::open(dir.path().to_path_buf()).expect("reopen");
        assert_eq!(
            reopened.get("certs").expect("certs").saved.role,
            ROLE_SUPPLEMENTARY
        );
    }

    /// The JSON contract of ADR-0016 and ADR-0054, on the stored revision: unset fields are
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
    fn candidates_follow_the_fit_and_none_means_nothing_to_roll_out() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
        store.put_saved("base", revision(&[], "b")).expect("put");
        store
            .put_saved("linux", revision(&[("os.type", "linux")], "l"))
            .expect("put");
        store
            .put_saved("windows", revision(&[("os.type", "windows")], "w"))
            .expect("put");
        store
            .put_saved("otelcol-only", typed(revision(&[], "o"), "otelcol"))
            .expect("put");

        let linux = description(&[("os.type", "linux"), ("service.name", "otelcol")]);
        assert_eq!(
            store.matching_names(Some(&linux)),
            ["base", "linux", "otelcol-only"]
        );
        assert_eq!(store.candidates_for(Some(&linux)).len(), 3);

        store.delete("base").expect("delete");
        store.delete("otelcol-only").expect("delete");
        let nothing = description(&[("os.type", "darwin")]);
        assert!(store.candidates_for(Some(&nothing)).is_empty());
        assert!(store.matching_names(Some(&nothing)).is_empty());
    }
}
