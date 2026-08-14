//! Named Configurations with Selectors (ADR-0012): the persistent store, the type fit
//! (ADR-0054) and Selector matching, and the composition of each Agent's Remote configuration
//! out of everything that matches it. Since ADR-0055 a Configuration carries two revisions —
//! a draft the operator edits and a published one the fleet is offered — and only publication
//! moves the draft into force.

use std::collections::BTreeMap;
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
    /// The Baseline's `AgentConfigFile.role` (ADR-0016), travelling unchanged to the Agent.
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

/// A named Configuration as the store holds it (ADR-0055): the draft revision every `PUT`
/// writes, and the published revision — the snapshot composition reads — if it has ever been
/// released. Saving only saves; `published` moves only through its own act.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Configuration {
    /// The name: a config-map key on the wire and a file name on both ends, so it follows the
    /// ADR-0010 name grammar.
    pub name: String,
    /// What editing operates on. Never composed, never offered, whatever it says.
    pub draft: Revision,
    /// What the fleet is offered — frozen at publication as one snapshot of the whole spec.
    /// `None` means never published (or retracted): this Configuration reaches nobody.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published: Option<Revision>,
}

impl Configuration {
    /// The draft differs from what is in force — the "pending changes" marker (ADR-0055).
    pub fn pending_changes(&self) -> bool {
        self.published
            .as_ref()
            .is_some_and(|published| *published != self.draft)
    }
}

/// The role value this project understands (ADR-0016). Every other non-empty value is passed on
/// unchanged and handled the same way — written, not configured with.
pub const ROLE_SUPPLEMENTARY: &str = "supplementary";

/// The writable part of a [`Configuration`] — the `PUT` request body; the name comes from the
/// URL. Writes the draft revision (ADR-0055): saving only saves.
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
    /// The Baseline's `AgentConfigFile.role` (ADR-0016); empty is top-level configuration.
    pub role: String,
}

/// One Agent's composed Remote configuration: every matching published Configuration as a named
/// entry, in name order, plus the hash that gates every push (goal 3). `None` entries never
/// exist — an Agent matching nothing gets no offer at all.
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
/// which was both the stored record and the API resource. It loads as published — draft equal to
/// published — because what it held was in force; reading it as a draft would empty every
/// composed map and actively reconfigure the whole fleet on upgrade (ADR-0055 point 5).
#[derive(Deserialize)]
struct LegacyConfiguration {
    name: String,
    #[serde(flatten)]
    revision: Revision,
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
            let value: serde_json::Value = serde_json::from_str(&text)
                .map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
            let config: Configuration = if value.get("draft").is_some() {
                serde_json::from_value(value)
                    .map_err(|e| format!("cannot parse {}: {e}", path.display()))?
            } else {
                let legacy: LegacyConfiguration = serde_json::from_value(value)
                    .map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
                Configuration {
                    name: legacy.name,
                    draft: legacy.revision.clone(),
                    published: Some(legacy.revision),
                }
            };
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

    /// Creates a Configuration or replaces its **draft** revision (ADR-0055): validated,
    /// persisted atomically (temp file + rename) — and offered to nobody until published. A
    /// published revision, where one exists, keeps being offered untouched.
    pub fn put_draft(&self, name: &str, revision: Revision) -> Result<Configuration, String> {
        validate_name(name).map_err(|e| format!("invalid name {name:?}: {e}"))?;
        if revision.body.trim().is_empty() {
            return Err("the configuration body is empty; refusing to store it".to_string());
        }
        let mut configs = self.configs.write().expect("configs lock");
        let config = match configs.get(name) {
            Some(existing) => Configuration {
                draft: revision,
                ..existing.clone()
            },
            None => Configuration {
                name: name.to_string(),
                draft: revision,
                published: None,
            },
        };
        self.persist(&config)?;
        configs.insert(config.name.clone(), config.clone());
        Ok(config)
    }

    /// Releases the draft as one snapshot, or retracts the published revision (ADR-0055).
    /// `Ok(None)` when no Configuration of that name exists. Retraction is **not inert**: the
    /// entry leaves every composed map, which the fleet applies (the caller's documentation and
    /// UI say so).
    pub fn set_published(
        &self,
        name: &str,
        published: bool,
    ) -> Result<Option<Configuration>, String> {
        let mut configs = self.configs.write().expect("configs lock");
        let Some(existing) = configs.get(name) else {
            return Ok(None);
        };
        let config = Configuration {
            published: published.then(|| existing.draft.clone()),
            ..existing.clone()
        };
        self.persist(&config)?;
        configs.insert(config.name.clone(), config.clone());
        Ok(Some(config))
    }

    fn persist(&self, config: &Configuration) -> Result<(), String> {
        let path = self.dir.join(format!("{}.json", config.name));
        let temp = self.dir.join(format!("{}.json.tmp", config.name));
        let json = serde_json::to_vec_pretty(&config).expect("a Configuration serializes");
        std::fs::write(&temp, json).map_err(|e| format!("cannot write {}: {e}", temp.display()))?;
        std::fs::rename(&temp, &path).map_err(|e| format!("cannot persist {}: {e}", path.display()))
    }

    /// Deletes a Configuration — both revisions; `Ok(false)` when none of that name exists.
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

    /// The names of the Configurations whose **published** revision reaches this Agent, in name
    /// order. Drafts are invisible here, whatever they say (ADR-0055).
    pub fn matching_names(&self, description: Option<&AgentDescription>) -> Vec<String> {
        self.configs
            .read()
            .expect("configs lock")
            .values()
            .filter(|c| {
                c.published
                    .as_ref()
                    .is_some_and(|revision| fits(revision, description))
            })
            .map(|c| c.name.clone())
            .collect()
    }

    /// This Agent's composed Remote configuration, or `None` when nothing matches — in which
    /// case no offer is made and the Agent keeps running what it already runs (goal 9).
    /// Composed from published revisions only (ADR-0055).
    pub fn desired_for(&self, description: Option<&AgentDescription>) -> Option<DesiredConfig> {
        let entries: Vec<ConfigEntry> = self
            .configs
            .read()
            .expect("configs lock")
            .values()
            .filter_map(|c| {
                let revision = c.published.as_ref().filter(|r| fits(r, description))?;
                Some(ConfigEntry {
                    name: c.name.clone(),
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

    /// Save and release in one step — the tests' shorthand for "this is in force".
    fn put_published(store: &ConfigStore, name: &str, revision: Revision) {
        store.put_draft(name, revision).expect("put");
        store
            .set_published(name, true)
            .expect("publish")
            .expect("the configuration exists");
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

    /// ADR-0055: saving only saves. A draft is composed for nobody until its own act releases
    /// it, and retraction takes it back out.
    #[test]
    fn a_draft_is_offered_to_nobody_until_published() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
        store
            .put_draft("base", revision(&[], "receivers: {}\n"))
            .expect("put");

        assert!(store.desired_for(None).is_none(), "a draft reaches nobody");
        assert!(store.matching_names(None).is_empty());

        store
            .set_published("base", true)
            .expect("publish")
            .expect("exists");
        assert_eq!(store.matching_names(None), ["base"]);
        assert_eq!(store.desired_for(None).expect("offered").entries.len(), 1);

        store
            .set_published("base", false)
            .expect("retract")
            .expect("exists");
        assert!(store.desired_for(None).is_none(), "retracted is withdrawn");

        assert!(
            store
                .set_published("missing", true)
                .expect("no io error")
                .is_none(),
            "publishing an unknown name finds nothing"
        );
    }

    /// ADR-0055 point 3: publication is a snapshot of the whole spec. Editing the draft
    /// afterwards changes nothing in force until the next release.
    #[test]
    fn publication_snapshots_the_draft_and_later_edits_stay_pending() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
        put_published(&store, "base", revision(&[], "v1\n"));
        let released = store.desired_for(None).expect("offered");
        assert_eq!(released.entries[0].body, "v1\n");
        assert!(!store.get("base").expect("base").pending_changes());

        store
            .put_draft("base", revision(&[], "v2\n"))
            .expect("edit");
        let config = store.get("base").expect("base");
        assert!(
            config.pending_changes(),
            "the edit is pending, not in force"
        );
        assert_eq!(
            store.desired_for(None).expect("offered").hash,
            released.hash,
            "the fleet still runs the published revision"
        );

        store
            .set_published("base", true)
            .expect("republish")
            .expect("exists");
        assert_eq!(
            store.desired_for(None).expect("offered").entries[0].body,
            "v2\n"
        );
        assert!(!store.get("base").expect("base").pending_changes());
    }

    /// ADR-0055 point 5: a file written before this ADR — the flat shape — loads as published,
    /// draft equal to published, so an upgrade neither stops an offer nor moves a hash.
    #[test]
    fn a_legacy_flat_file_loads_as_published() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("keeper.json"),
            r#"{"name":"keeper","selector":{"os.type":"linux"},"body":"receivers: {}\n","role":"supplementary"}"#,
        )
        .expect("write the legacy file");

        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
        let config = store.get("keeper").expect("keeper");
        assert_eq!(config.published, Some(config.draft.clone()));
        assert!(!config.pending_changes());
        assert_eq!(config.draft.role, "supplementary");
        assert_eq!(
            store.matching_names(Some(&description(&[("os.type", "linux")]))),
            ["keeper"]
        );
    }

    #[test]
    fn the_store_round_trips_and_survives_a_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
        put_published(&store, "base", revision(&[], "receivers: {}\n"));
        store
            .put_draft(
                "linux-only",
                revision(&[("os.type", "linux")], "exporters: {}\n"),
            )
            .expect("put");

        let reopened = ConfigStore::open(dir.path().to_path_buf()).expect("reopen");
        assert_eq!(reopened.list().len(), 2);
        let base = reopened.get("base").expect("base");
        assert_eq!(base.draft.body, "receivers: {}\n");
        assert!(base.published.is_some(), "publication survives the reopen");
        assert!(
            reopened
                .get("linux-only")
                .expect("linux-only")
                .published
                .is_none(),
            "a never-published draft stays a draft across the reopen"
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
        assert!(store.put_draft("Bad Name", revision(&[], "x")).is_err());
        assert!(store.put_draft("con", revision(&[], "x")).is_err());
        assert!(store.put_draft("ok", revision(&[], "  \n")).is_err());
    }

    #[test]
    fn composition_is_name_sorted_and_hash_stable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
        put_published(&store, "zz-extra", revision(&[], "z"));
        put_published(&store, "aa-base", revision(&[], "a"));

        let desired = store.desired_for(None).expect("desired");
        let names: Vec<&str> = desired.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["aa-base", "zz-extra"]);
        assert_eq!(desired.hash, store.desired_for(None).expect("again").hash);

        // The hash covers names and bodies: renaming or editing either changes it — once the
        // edit is released.
        put_published(&store, "aa-base", revision(&[], "a2"));
        assert_ne!(store.desired_for(None).expect("edited").hash, desired.hash);
    }

    #[test]
    fn a_role_travels_into_the_composed_entry_and_into_the_hash() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
        put_published(&store, "base", revision(&[], "receivers: {}\n"));
        let without = store.desired_for(None).expect("desired").hash;

        put_published(
            &store,
            "ruleset",
            with_role(revision(&[], "rules: []\n"), ROLE_SUPPLEMENTARY),
        );
        let desired = store.desired_for(None).expect("desired");
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
        put_published(&store, "ruleset", revision(&[], "rules: []\n"));
        assert_ne!(store.desired_for(None).expect("desired").hash, desired.hash);
        assert_ne!(store.desired_for(None).expect("desired").hash, without);
    }

    /// A Configuration written before ADR-0016 has no role, and its hash must not move when the
    /// Server is upgraded — a moved hash restarts every Managed Process in the fleet to deliver a
    /// configuration identical to the one it already runs. The same pin guards ADR-0054 and
    /// ADR-0055: neither the type nor the revision split may enter the hash.
    #[test]
    fn an_empty_role_leaves_the_hash_where_it_was() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
        put_published(&store, "base", revision(&[], "receivers: {}\n"));

        // The hash this Server computed before `role` existed, pinned by construction: name and
        // body, length-prefixed, and nothing else.
        let mut expected = Sha256::new();
        expected.update((4u64).to_le_bytes());
        expected.update(b"base");
        expected.update((14u64).to_le_bytes());
        expected.update(b"receivers: {}\n");

        assert_eq!(
            store.desired_for(None).expect("desired").hash,
            expected.finalize().to_vec()
        );
    }

    #[test]
    fn a_role_survives_a_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
        put_published(
            &store,
            "certs",
            with_role(revision(&[], "PEM\n"), ROLE_SUPPLEMENTARY),
        );
        let reopened = ConfigStore::open(dir.path().to_path_buf()).expect("reopen");
        assert_eq!(
            reopened.get("certs").expect("certs").draft.role,
            ROLE_SUPPLEMENTARY
        );
    }

    /// The JSON contract of ADR-0016 and ADR-0054, now on the stored revision: unset fields are
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
    fn only_matching_configurations_compose_and_none_means_no_offer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ConfigStore::open(dir.path().to_path_buf()).expect("open");
        put_published(&store, "base", revision(&[], "b"));
        put_published(&store, "linux", revision(&[("os.type", "linux")], "l"));
        put_published(&store, "windows", revision(&[("os.type", "windows")], "w"));
        put_published(&store, "otelcol-only", typed(revision(&[], "o"), "otelcol"));

        let linux = description(&[("os.type", "linux"), ("service.name", "otelcol")]);
        let desired = store.desired_for(Some(&linux)).expect("desired");
        assert_eq!(
            store.matching_names(Some(&linux)),
            ["base", "linux", "otelcol-only"]
        );
        assert_eq!(desired.entries.len(), 3);

        store.delete("base").expect("delete");
        store.delete("otelcol-only").expect("delete");
        let nothing = description(&[("os.type", "darwin")]);
        assert!(store.desired_for(Some(&nothing)).is_none());
        assert!(store.matching_names(Some(&nothing)).is_empty());
    }
}
