//! Server-set labels on an Agent (ADR-0042).
//!
//! The attribute a staged rollout wants — `rollout = "canary"` — is one an operator invents, and
//! until now it could only be invented in `supervisor.toml` on the machine. Moving a host between rings
//! was therefore a file edit plus a restart *on that host*: the per-host wiring ADR-0017 set out to
//! remove, surviving in the one place it mattered most.
//!
//! A label joins the attribute set a Selector matches against — for Configurations (ADR-0012) and
//! for packages (ADR-0017) alike, since both resolve against the same `AgentDescription`. It never
//! travels to the Agent: it is an input to matching here, and the Agent experiences it only as the
//! Configuration and the packages it is offered.
//!
//! **A label can never restate a reported attribute.** `os.type` and `host.arch` decide which
//! artifact an Agent is offered (ADR-0031) and `service.name` decides which packages fit it at all
//! (ADR-0034), so a label that outranked them would let a typo hand a Windows binary to a Linux
//! host. Labels annotate; they do not correct.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::RwLock;

use opamp::proto::{any_value, AgentDescription, AnyValue, KeyValue};
use opamp::uid::InstanceUid;

/// The labels of one Agent, and the store they persist in.
///
/// One file per Agent under a directory the Configuration store's loader ignores, so a write
/// touches nothing else and clearing a set is a deletion.
pub struct LabelStore {
    dir: PathBuf,
    labels: RwLock<HashMap<InstanceUid, BTreeMap<String, String>>>,
}

impl LabelStore {
    /// Opens the store, creating the directory and loading every persisted set. A file that does
    /// not parse fails startup rather than being skipped: a rollout ring that silently vanished is
    /// worse than one that refuses to start (ADR-0008's principle).
    pub fn open(dir: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let mut labels = HashMap::new();
        let entries =
            std::fs::read_dir(&dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        for entry in entries {
            let path = entry
                .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
                .path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| format!("cannot read the Agent identity from {}", path.display()))?;
            let uid = InstanceUid::parse(stem)
                .ok_or_else(|| format!("{} is not named after an Instance UID", path.display()))?;
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let set: BTreeMap<String, String> = serde_json::from_str(&text)
                .map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
            labels.insert(uid, set);
        }
        Ok(LabelStore {
            dir,
            labels: RwLock::new(labels),
        })
    }

    /// This Agent's labels, empty when it has none.
    pub fn get(&self, uid: &InstanceUid) -> BTreeMap<String, String> {
        self.labels
            .read()
            .expect("labels lock")
            .get(uid)
            .cloned()
            .unwrap_or_default()
    }

    /// Replaces this Agent's labels with `set`; an empty set clears them and deletes the file.
    ///
    /// The whole set at once rather than add-and-remove operations: the resource is small, the
    /// write is idempotent, and an operator sees the resulting state in the request they sent.
    pub fn put(&self, uid: &InstanceUid, set: BTreeMap<String, String>) -> Result<(), String> {
        let path = self.dir.join(format!("{uid}.json"));
        if set.is_empty() {
            if let Err(e) = std::fs::remove_file(&path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    return Err(format!("cannot delete {}: {e}", path.display()));
                }
            }
            self.labels.write().expect("labels lock").remove(uid);
            return Ok(());
        }
        let temp = self.dir.join(format!("{uid}.json.tmp"));
        let json = serde_json::to_vec_pretty(&set).expect("labels serialize");
        std::fs::write(&temp, json).map_err(|e| format!("cannot write {}: {e}", temp.display()))?;
        std::fs::rename(&temp, &path)
            .map_err(|e| format!("cannot persist {}: {e}", path.display()))?;
        self.labels.write().expect("labels lock").insert(*uid, set);
        Ok(())
    }
}

/// Why a label set was refused (`PUT /api/v1/agents/{uid}/labels`).
pub enum LabelError {
    /// No Agent of that identity is known. Labels are attached to something the Server has seen.
    UnknownAgent,
    /// A label key the Agent already reports. Refused rather than applied: a label that could
    /// override `os.type`, `host.arch`, or `service.name` would let a slip in the UI offer an
    /// Agent an artifact built for another machine (ADR-0031, ADR-0034).
    RestatesReported(String),
    /// The store could not be written.
    Storage(String),
}

/// Whether an empty key or value was given — neither can match a Selector, and both are far more
/// likely to be a mistake than an intent.
pub fn check_pairs(set: &BTreeMap<String, String>) -> Result<(), String> {
    for (key, value) in set {
        if key.trim().is_empty() {
            return Err("a label key cannot be empty".to_string());
        }
        if value.trim().is_empty() {
            return Err(format!(
                "label {key:?} has an empty value — remove the key instead, or give it the value a \
                 Selector should match"
            ));
        }
    }
    Ok(())
}

/// The keys this Agent reports, which a label may not restate.
pub fn reported_keys(description: Option<&AgentDescription>) -> Vec<&str> {
    let Some(description) = description else {
        return Vec::new();
    };
    description
        .identifying_attributes
        .iter()
        .chain(&description.non_identifying_attributes)
        .map(|kv| kv.key.as_str())
        .collect()
}

/// The description a Selector is matched against: what the Agent reported, plus the labels that do
/// not collide with it.
///
/// Reported always wins. The API refuses a colliding key up front, so this is the second line
/// rather than the first — but it is the one that holds when an Agent *starts* reporting a key that
/// was labelled earlier, which no up-front check can prevent.
pub fn effective_description(
    description: Option<&AgentDescription>,
    labels: &BTreeMap<String, String>,
) -> Option<AgentDescription> {
    if labels.is_empty() {
        return description.cloned();
    }
    let mut effective = description.cloned().unwrap_or_default();
    let reported: Vec<String> = reported_keys(Some(&effective))
        .into_iter()
        .map(str::to_string)
        .collect();
    for (key, value) in labels {
        if reported.iter().any(|r| r == key) {
            continue;
        }
        effective.non_identifying_attributes.push(KeyValue {
            key: key.clone(),
            value: Some(AnyValue {
                value: Some(any_value::Value::StringValue(value.clone())),
            }),
        });
    }
    Some(effective)
}

/// The labels this Agent has that its own reports shadow — set, and matching nothing.
///
/// Surfaced on the fleet row rather than dropped in silence: doing what can be done and saying what
/// was not is the correction ADR-0014 already made for connection settings.
pub fn shadowed(
    description: Option<&AgentDescription>,
    labels: &BTreeMap<String, String>,
) -> Vec<String> {
    let reported = reported_keys(description);
    labels
        .keys()
        .filter(|key| reported.iter().any(|r| r == key))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn described(pairs: &[(&str, &str)]) -> AgentDescription {
        AgentDescription {
            identifying_attributes: vec![KeyValue {
                key: "service.name".to_string(),
                value: Some(AnyValue {
                    value: Some(any_value::Value::StringValue("otelcol".to_string())),
                }),
            }],
            non_identifying_attributes: pairs
                .iter()
                .map(|(k, v)| KeyValue {
                    key: (*k).to_string(),
                    value: Some(AnyValue {
                        value: Some(any_value::Value::StringValue((*v).to_string())),
                    }),
                })
                .collect(),
        }
    }

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    /// The point of the decision: a label is matchable exactly like a reported attribute, so a
    /// Selector written for `rollout = canary` finds a host an operator moved into that ring.
    #[test]
    fn a_label_is_matched_like_a_reported_attribute() {
        let described = described(&[("os.type", "linux")]);
        let effective = effective_description(Some(&described), &labels(&[("rollout", "canary")]))
            .expect("some");
        assert!(crate::configs::matches(
            &labels(&[("rollout", "canary")]),
            Some(&effective)
        ));
        // And the reported ones still match, so a label adds reach rather than replacing it.
        assert!(crate::configs::matches(
            &labels(&[("os.type", "linux")]),
            Some(&effective)
        ));
    }

    /// The crux (ADR-0042 point 3): reported wins. A label that could rewrite `os.type` would let a
    /// slip in the UI offer this Agent an artifact built for another machine.
    #[test]
    fn a_label_never_overrides_what_the_agent_reports() {
        let described = described(&[("os.type", "linux")]);
        let effective = effective_description(Some(&described), &labels(&[("os.type", "windows")]))
            .expect("some");
        assert!(
            crate::configs::matches(&labels(&[("os.type", "linux")]), Some(&effective)),
            "the reported platform is what matches"
        );
        assert!(
            !crate::configs::matches(&labels(&[("os.type", "windows")]), Some(&effective)),
            "and the label does not"
        );
        assert_eq!(
            shadowed(Some(&described), &labels(&[("os.type", "windows")])),
            ["os.type"],
            "the shadowed label is surfaced, not silently dropped"
        );
    }

    /// An Agent that has described itself with nothing can still be labelled — it is the operator's
    /// annotation, not a report.
    #[test]
    fn an_agent_that_reported_nothing_can_still_be_labelled() {
        let effective =
            effective_description(None, &labels(&[("rollout", "canary")])).expect("some");
        assert!(crate::configs::matches(
            &labels(&[("rollout", "canary")]),
            Some(&effective)
        ));
    }

    #[test]
    fn empty_keys_and_values_are_refused() {
        assert!(check_pairs(&labels(&[("rollout", "canary")])).is_ok());
        assert!(check_pairs(&labels(&[("", "canary")])).is_err());
        assert!(check_pairs(&labels(&[("rollout", "  ")])).is_err());
    }

    /// A ring assignment that evaporated with a restart would be worse than none, because it would
    /// evaporate quietly.
    #[test]
    fn labels_survive_a_reopen_and_an_empty_set_clears_them() {
        let dir = tempfile::tempdir().expect("tempdir");
        let uid = InstanceUid::default();
        {
            let store = LabelStore::open(dir.path().to_path_buf()).expect("open");
            store
                .put(&uid, labels(&[("rollout", "canary")]))
                .expect("put");
            assert_eq!(store.get(&uid), labels(&[("rollout", "canary")]));
        }

        let reopened = LabelStore::open(dir.path().to_path_buf()).expect("reopen");
        assert_eq!(reopened.get(&uid), labels(&[("rollout", "canary")]));

        reopened.put(&uid, BTreeMap::new()).expect("clear");
        assert!(reopened.get(&uid).is_empty());
        let again = LabelStore::open(dir.path().to_path_buf()).expect("reopen");
        assert!(again.get(&uid).is_empty(), "the clear persisted too");
    }
}
