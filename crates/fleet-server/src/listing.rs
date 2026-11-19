//! The lists that grow with the fleet, read a page at a time
//! (API-lists-paged-by-cursor):
//! the filters, the order, the opaque cursor, and what an Agent's record gives the queries to
//! search and sort by.
//!
//! The filter, the order and the cursor are the core's (clause 8). On the filesystem store the
//! core answers them from memory with what this module derives; on PostgreSQL the store answers
//! them from columns the core writes from the same derivation, so the two stores answer the same.

use std::collections::BTreeMap;

use opamp::attributes;
use opamp::proto::{
    any_value, AgentDescription, ComponentHealth, KeyValue, PackageStatuses, RemoteConfigStatus,
    RemoteConfigStatuses,
};
use opamp::uid::InstanceUid;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::fleet::Transport;

/// The most entries one page holds; a larger `limit` is answered as this (clause 3).
pub const MAX_LIMIT: usize = 1000;

/// The most entries a request without `limit` and `cursor` is answered with whole (clause 3).
pub const WHOLE_BOUND: usize = 10_000;

/// What the Agents are sorted by (clause 5), each meaning what the UI's column of that name shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AgentSort {
    /// The display name: the instance name, else the type, else the Instance UID.
    #[default]
    Name,
    /// Connected after disconnected.
    Connected,
    /// Unhealthy first, then not reported, then healthy.
    Health,
    /// The remote configuration status.
    Config,
    /// The version shown.
    Version,
    /// The operating system shown.
    Os,
    /// The sequence number.
    Seq,
    /// When it was last heard from.
    Seen,
}

impl AgentSort {
    /// Every key, as the REST API names it.
    pub const ALL: [AgentSort; 8] = [
        AgentSort::Name,
        AgentSort::Connected,
        AgentSort::Health,
        AgentSort::Config,
        AgentSort::Version,
        AgentSort::Os,
        AgentSort::Seq,
        AgentSort::Seen,
    ];

    /// The key the REST API names `name`.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|sort| sort.as_str() == name)
    }

    /// How the REST API names it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            AgentSort::Name => "name",
            AgentSort::Connected => "connected",
            AgentSort::Health => "health",
            AgentSort::Config => "config",
            AgentSort::Version => "version",
            AgentSort::Os => "os",
            AgentSort::Seq => "seq",
            AgentSort::Seen => "seen",
        }
    }
}

/// Which Agents `holder` asks for (clause 6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HolderFilter {
    /// Those the instance of this id holds under a live lease.
    Instance(String),
    /// Those no instance with a live lease holds.
    None,
}

/// The filters of the list of Agents (clause 6), and the one the Server uses for its own reads,
/// the Host an Agent arrived over (clause 9). A filter left `None` or empty narrows nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentFilter {
    /// A substring of the fields the UI's filter searches, already lowercased.
    pub q: Option<String>,
    /// Pairs the Agent's effective description matches, as a Deployment's Selector matches it.
    pub selector: BTreeMap<String, String>,
    pub connected: Option<bool>,
    pub stale: Option<bool>,
    pub service_name: Option<String>,
    pub holder: Option<HolderFilter>,
    /// The Host the Agent last arrived over; the REST API does not offer it.
    pub arrived_over: Option<String>,
}

/// A sort value: an integer, or a text compared byte by byte, as PostgreSQL's `"C"` collation
/// compares it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Key {
    Int(i64),
    Text(String),
}

/// Where a page ends: its last entry's sort value and its unique tiebreak (clause 5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub key: Key,
    pub tiebreak: String,
}

impl Position {
    /// Whether this entry comes after `after` in the order: by the key, descending where asked,
    /// then by the tiebreak, ascending.
    #[must_use]
    pub fn is_after(&self, after: &Position, descending: bool) -> bool {
        let by_key = if descending {
            self.key < after.key
        } else {
            self.key > after.key
        };
        by_key || (self.key == after.key && self.tiebreak > after.tiebreak)
    }

    /// The order of two entries.
    #[must_use]
    pub fn order(&self, other: &Position, descending: bool) -> std::cmp::Ordering {
        let by_key = if descending {
            other.key.cmp(&self.key)
        } else {
            self.key.cmp(&other.key)
        };
        by_key.then_with(|| self.tiebreak.cmp(&other.tiebreak))
    }
}

/// One page cut from entries that match the filters.
#[derive(Debug)]
pub struct Paged<T> {
    pub entries: Vec<T>,
    /// How many entries match the filters, on every page.
    pub total: usize,
    /// Where the next page starts, when another follows.
    pub next: Option<Position>,
}

/// Cuts a page from `entries`, every one of which matches the filters: sorted, after `after`, at
/// most `limit`.
pub fn page<T>(
    mut entries: Vec<(Position, T)>,
    descending: bool,
    after: Option<&Position>,
    limit: usize,
) -> Paged<T> {
    let total = entries.len();
    entries.sort_by(|(a, _), (b, _)| a.order(b, descending));
    let mut entries: Vec<(Position, T)> = entries
        .into_iter()
        .filter(|(position, _)| after.is_none_or(|after| position.is_after(after, descending)))
        .take(limit.saturating_add(1))
        .collect();
    let next = (entries.len() > limit).then(|| {
        entries.truncate(limit);
        entries.last().map(|(position, _)| position.clone())
    });
    Paged {
        entries: entries.into_iter().map(|(_, entry)| entry).collect(),
        total,
        next: next.flatten(),
    }
}

/// The digest a cursor names its sort and filters by: every parameter but `limit` and `cursor`,
/// each length-prefixed.
#[must_use]
pub fn digest(parameters: &[(&str, &str)]) -> String {
    let mut digest = Sha256::new();
    for (name, value) in parameters {
        for part in [name, value] {
            digest.update((part.len() as u64).to_le_bytes());
            digest.update(part.as_bytes());
        }
    }
    hex::encode(&digest.finalize()[..16])
}

/// What an Agent's record is derived from: the fields its derived values come from (clause 8).
pub struct Source<'a> {
    pub uid: &'a InstanceUid,
    pub description: Option<&'a AgentDescription>,
    pub health: Option<&'a ComponentHealth>,
    pub remote_config_status: Option<&'a RemoteConfigStatus>,
    pub package_statuses: Option<&'a PackageStatuses>,
    pub capabilities: u64,
    pub transport: Transport,
}

/// What the queries read of one Agent's record (clause 8): the display name and the other sort
/// keys, the reported values a Selector matches, the `service_name`, the machine that groups an
/// Agent behind a Gateway, whether it is its Client's own Agent, and one search text. On
/// PostgreSQL each is a column written with the record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Derived {
    pub name: String,
    /// 0 unhealthy, 1 not reported, 2 healthy.
    pub health: i64,
    pub config: String,
    pub version: String,
    pub os: String,
    pub service_name: String,
    /// Every reported key outside the Server's `fleet.` space, with the value a Selector matches,
    /// `None` where the first value reported is no string or an empty one: the key is taken, and
    /// no label can stand in for it (SRV-fleet-record-agents-hosts §18).
    pub reported: BTreeMap<String, Option<String>>,
    /// `host.id=…` or `host.name=…`, empty when neither is reported.
    pub machine: String,
    pub client: bool,
    /// The record's fields `q` searches, lowercased, one per line.
    pub search: String,
}

/// Text as the derived values keep it: a NUL, which PostgreSQL's `text` refuses, as U+FFFD.
#[must_use]
pub fn sanitize(text: &str) -> String {
    text.replace('\0', "\u{fffd}")
}

/// The display values of one Agent: what its listed record and its view both show of what it
/// reported, derived here alone, so the list and the view cannot drift apart. Unsanitized; the
/// derived values sanitize what they keep.
pub struct DisplayValues {
    /// The identifying attributes as the API shows them.
    pub identifying: BTreeMap<String, String>,
    /// The non-identifying attributes as the API shows them.
    pub non_identifying: BTreeMap<String, String>,
    pub service_name: String,
    pub instance_name: String,
    /// The `service.version` the Agent reported.
    pub build: String,
    /// The version a reader is shown (OPS-cargo-decides-product-version).
    pub version: String,
    /// `os.description`, or `os.type` where no description is reported.
    pub os: String,
    /// The Platform the offer reads (PKG-signed-packages-and-delivery §14), empty where none is reported.
    pub platform: String,
    /// The name of the remote configuration status, `UNSET` where none is reported.
    pub config: &'static str,
}

/// An Agent's service name and instance name, each read only from the attribute set
/// CLT-what-an-agent-reports §1 and §2 give it: `service.name` from the identifying attributes,
/// `service.instance.name` from the non-identifying ones. Empty where it is not reported there.
#[must_use]
pub fn names(description: Option<&AgentDescription>) -> (String, String) {
    let Some(description) = description else {
        return (String::new(), String::new());
    };
    (
        attributes::agent_type(description)
            .unwrap_or_default()
            .to_string(),
        attributes::string_value(
            &description.non_identifying_attributes,
            attributes::SERVICE_INSTANCE_NAME,
        )
        .unwrap_or_default()
        .to_string(),
    )
}

/// The display values of an Agent that reported `description` and `status`.
#[must_use]
pub fn display_values(
    description: Option<&AgentDescription>,
    status: Option<&RemoteConfigStatus>,
) -> DisplayValues {
    let (identifying, non_identifying) = match description {
        Some(d) => (
            attr_map(&d.identifying_attributes),
            attr_map(&d.non_identifying_attributes),
        ),
        None => (BTreeMap::new(), BTreeMap::new()),
    };
    let lookup = |map: &BTreeMap<String, String>, key: &str| -> String {
        map.get(key).cloned().unwrap_or_default()
    };
    let build = description
        .and_then(attributes::agent_version)
        .unwrap_or_default()
        .to_string();
    let os = match lookup(&non_identifying, attributes::OS_DESCRIPTION) {
        description if !description.is_empty() => description,
        _ => lookup(&non_identifying, attributes::OS_TYPE),
    };
    let (service_name, instance_name) = names(description);
    DisplayValues {
        service_name,
        instance_name,
        version: display_version(&build),
        build,
        os,
        platform: crate::packages::Platform::reported(description)
            .map(|platform| platform.tag())
            .unwrap_or_default(),
        config: match status.map(|s| s.status) {
            Some(s) if s == RemoteConfigStatuses::Applied as i32 => "APPLIED",
            Some(s) if s == RemoteConfigStatuses::Applying as i32 => "APPLYING",
            Some(s) if s == RemoteConfigStatuses::Failed as i32 => "FAILED",
            _ => "UNSET",
        },
        identifying,
        non_identifying,
    }
}

/// The version a reader of the fleet table wants: the release, without the commit the build came
/// from (OPS-cargo-decides-product-version).
///
/// A value that is not a version is returned as it stands. `service.version` is whatever an Agent
/// puts there, and a Foreign Agent numbers itself however its own project does — trimming a string
/// this Server does not understand would be inventing a version rather than showing one.
pub(crate) fn display_version(reported: &str) -> String {
    fleet_core::version::identity(reported)
        .unwrap_or(reported)
        .to_string()
}

/// A declared capability bitmask as the names from the Baseline's `AgentCapabilities`. Undefined
/// bits are surfaced verbatim rather than dropped — a peer declaring them is worth seeing.
pub(crate) fn capability_names(mask: u64) -> Vec<String> {
    use opamp::proto::AgentCapabilities as C;
    const KNOWN: [(C, &str); 16] = [
        (C::ReportsStatus, "ReportsStatus"),
        (C::AcceptsRemoteConfig, "AcceptsRemoteConfig"),
        (C::ReportsEffectiveConfig, "ReportsEffectiveConfig"),
        (C::AcceptsPackages, "AcceptsPackages"),
        (C::ReportsPackageStatuses, "ReportsPackageStatuses"),
        (C::ReportsOwnTraces, "ReportsOwnTraces"),
        (C::ReportsOwnMetrics, "ReportsOwnMetrics"),
        (C::ReportsOwnLogs, "ReportsOwnLogs"),
        (
            C::AcceptsOpAmpConnectionSettings,
            "AcceptsOpAMPConnectionSettings",
        ),
        (
            C::AcceptsOtherConnectionSettings,
            "AcceptsOtherConnectionSettings",
        ),
        (C::AcceptsRestartCommand, "AcceptsRestartCommand"),
        (C::ReportsHealth, "ReportsHealth"),
        (C::ReportsRemoteConfig, "ReportsRemoteConfig"),
        (C::ReportsHeartbeat, "ReportsHeartbeat"),
        (C::ReportsAvailableComponents, "ReportsAvailableComponents"),
        (
            C::ReportsConnectionSettingsStatus,
            "ReportsConnectionSettingsStatus",
        ),
    ];
    let mut names = Vec::new();
    let mut undefined = mask;
    for (bit, name) in KNOWN {
        if mask & bit as u64 != 0 {
            names.push(name.to_string());
            undefined &= !(bit as u64);
        }
    }
    if undefined != 0 {
        names.push(format!("unknown bits 0x{undefined:x}"));
    }
    names
}

/// Reported attributes as the API shows them: string values as-is, string arrays (the shape the
/// conventions give `host.ip` and `host.mac`, CLT-what-an-agent-reports) joined with a comma, other value kinds in
/// their debug form — the view is for reading, the wire keeps the typed original.
pub(crate) fn attr_map(attributes: &[KeyValue]) -> BTreeMap<String, String> {
    fn text(value: &any_value::Value) -> String {
        match value {
            any_value::Value::StringValue(s) => s.clone(),
            any_value::Value::ArrayValue(list) => list
                .values
                .iter()
                .filter_map(|v| v.value.as_ref())
                .map(text)
                .collect::<Vec<_>>()
                .join(", "),
            other => format!("{other:?}"),
        }
    }
    attributes
        .iter()
        .filter_map(|kv| {
            let value = kv.value.as_ref()?.value.as_ref()?;
            Some((kv.key.clone(), text(value)))
        })
        .collect()
}

/// The name of a package's installation state, as the list searches it and the view shows it:
/// `Unknown` for a state the Baseline does not name.
pub(crate) fn package_status_name(status: i32) -> &'static str {
    use opamp::proto::PackageStatusEnum as S;
    match status {
        s if s == S::Installed as i32 => "Installed",
        s if s == S::Installing as i32 => "Installing",
        s if s == S::InstallPending as i32 => "InstallPending",
        s if s == S::InstallFailed as i32 => "InstallFailed",
        s if s == S::Downloading as i32 => "Downloading",
        _ => "Unknown",
    }
}

/// The derived values of one record.
#[must_use]
pub fn derive(source: &Source<'_>) -> Derived {
    let DisplayValues {
        identifying,
        non_identifying,
        service_name,
        instance_name,
        build,
        version,
        os,
        platform,
        config,
    } = display_values(source.description, source.remote_config_status);
    let uid = source.uid.to_string();
    let name = if !instance_name.is_empty() {
        instance_name.clone()
    } else if !service_name.is_empty() {
        service_name.clone()
    } else {
        uid.clone()
    };
    let config = config.to_string();
    let healthy = source.health.is_some_and(|h| h.healthy);
    let health_status = source.health.map(|h| h.status.clone()).unwrap_or_default();
    let health_error = source
        .health
        .map(|h| h.last_error.clone())
        .unwrap_or_default();
    let health = if healthy {
        2
    } else if !health_status.is_empty() || !health_error.is_empty() {
        0
    } else {
        1
    };
    let mut reported = BTreeMap::new();
    if let Some(description) = source.description {
        for kv in description
            .identifying_attributes
            .iter()
            .chain(&description.non_identifying_attributes)
        {
            if kv.key.starts_with(crate::labels::RESERVED_PREFIX)
                || reported.contains_key(&sanitize(&kv.key))
            {
                continue;
            }
            let value = attributes::reported(description, &kv.key).map(sanitize);
            reported.insert(sanitize(&kv.key), value);
        }
    }
    let mut fields: Vec<String> = vec![
        service_name.clone(),
        instance_name,
        uid,
        os.clone(),
        platform,
        version.clone(),
        build,
        config.clone(),
        health_status,
        health_error,
        if healthy { "healthy" } else { "" }.to_string(),
        source.transport.as_str().to_string(),
    ];
    fields.extend(non_identifying.iter().map(|(k, v)| format!("{k}={v}")));
    fields.extend(identifying.iter().map(|(k, v)| format!("{k}={v}")));
    fields.extend(capability_names(source.capabilities));
    if let Some(statuses) = source.package_statuses {
        let mut packages: Vec<&opamp::proto::PackageStatus> = statuses.packages.values().collect();
        packages.sort_by(|a, b| a.name.cmp(&b.name));
        for package in packages {
            fields.extend([
                package.name.clone(),
                package.agent_has_version.clone(),
                package_status_name(package.status).to_string(),
            ]);
        }
    }
    Derived {
        name: sanitize(&name),
        health,
        config,
        version: sanitize(&version),
        os: sanitize(&os),
        service_name: sanitize(&service_name),
        reported,
        machine: crate::labels::machine(source.description)
            .map(|machine| sanitize(&machine))
            .unwrap_or_default(),
        client: crate::labels::is_client_agent(source.description),
        search: sanitize(&fields.join("\n").to_lowercase()),
    }
}

/// The whole text `q` is matched against (clause 6): the record's search text, then what is
/// joined at read time — the presence, the staleness, the Agent's own labels, its Client Agent's
/// and its Host's values — one per line, lowercased. PostgreSQL builds the same text.
#[must_use]
pub fn haystack(
    search: &str,
    connected: bool,
    stale: bool,
    own: &BTreeMap<String, String>,
    inherited: &BTreeMap<String, String>,
    host: &BTreeMap<String, String>,
) -> String {
    let pairs = |map: &BTreeMap<String, String>| {
        map.iter()
            .map(|(k, v)| format!("{k}={v}").to_lowercase())
            .collect::<Vec<_>>()
            .join("\n")
    };
    [
        search.to_string(),
        if connected {
            "connected"
        } else {
            "disconnected"
        }
        .to_string(),
        if stale { "stale" } else { "" }.to_string(),
        pairs(own),
        pairs(inherited),
        pairs(host),
    ]
    .join("\n")
}

/// The sort value of one Agent under `sort`.
#[must_use]
pub fn agent_key(
    sort: AgentSort,
    derived: &Derived,
    connected: bool,
    sequence_num: u64,
    last_seen_ms: u64,
) -> Key {
    match sort {
        AgentSort::Name => Key::Text(derived.name.clone()),
        AgentSort::Connected => Key::Int(i64::from(connected)),
        AgentSort::Health => Key::Int(derived.health),
        AgentSort::Config => Key::Text(derived.config.clone()),
        AgentSort::Version => Key::Text(derived.version.clone()),
        AgentSort::Os => Key::Text(derived.os.clone()),
        // As PostgreSQL keeps an unsigned value, its bits in a `bigint`.
        AgentSort::Seq => Key::Int(sequence_num as i64),
        AgentSort::Seen => Key::Int(last_seen_ms as i64),
    }
}

/// Whether an Agent that last reported at `last_seen_ms` with `capabilities` is stale for a
/// query that counts as stale whatever was heard before `seen_before_ms`.
#[must_use]
pub fn is_stale(capabilities: u64, last_seen_ms: u64, seen_before_ms: u64) -> bool {
    capabilities & opamp::proto::AgentCapabilities::ReportsHeartbeat as u64 != 0
        && last_seen_ms < seen_before_ms
}

/// What one Agent is matched by: its derived values and what is joined at read time.
pub struct Candidate<'a> {
    pub derived: &'a Derived,
    pub connected: bool,
    pub stale: bool,
    /// The instance that holds it under a live lease.
    pub holder: Option<&'a str>,
    /// The Host it last arrived over.
    pub host: Option<&'a str>,
    /// What a Selector is matched against (SRV-fleet-record-agents-hosts §18).
    pub effective: Option<&'a AgentDescription>,
    pub own: &'a BTreeMap<String, String>,
    pub inherited: &'a BTreeMap<String, String>,
    pub host_values: &'a BTreeMap<String, String>,
}

impl AgentFilter {
    /// Whether `candidate` passes every filter.
    #[must_use]
    pub fn admits(&self, candidate: &Candidate<'_>) -> bool {
        self.connected.is_none_or(|c| c == candidate.connected)
            && self.stale.is_none_or(|s| s == candidate.stale)
            && self
                .service_name
                .as_ref()
                .is_none_or(|name| *name == candidate.derived.service_name)
            && self.holder.as_ref().is_none_or(|holder| match holder {
                HolderFilter::Instance(id) => candidate.holder == Some(id.as_str()),
                HolderFilter::None => candidate.holder.is_none(),
            })
            && self
                .arrived_over
                .as_ref()
                .is_none_or(|host| candidate.host == Some(host.as_str()))
            && (self.selector.is_empty()
                || crate::deployments::matches(&self.selector, candidate.effective))
            && self.q.as_ref().is_none_or(|q| {
                haystack(
                    &candidate.derived.search,
                    candidate.connected,
                    candidate.stale,
                    candidate.own,
                    candidate.inherited,
                    candidate.host_values,
                )
                .contains(q.as_str())
            })
    }
}

/// The Selector of a `selector` parameter: `key=value` pairs separated by commas, `None` when a
/// pair is not one, as a Deployment's Selector is written.
#[must_use]
pub fn parse_selector(text: &str) -> Option<BTreeMap<String, String>> {
    let mut selector = BTreeMap::new();
    for pair in text.split(',') {
        let (key, value) = pair.split_once('=')?;
        let (key, value) = (key.trim(), value.trim());
        if key.is_empty() || value.is_empty() {
            return None;
        }
        selector.insert(key.to_string(), value.to_string());
    }
    Some(selector)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The view of an array-valued attribute — the shape `host.ip` and `host.mac` arrive in
    /// (CLT-what-an-agent-reports): joined for reading, not dumped in debug form.
    #[test]
    fn the_view_joins_a_string_array_attribute() {
        let attrs = vec![
            opamp::attributes::string_attr("host.name", "edge-01"),
            opamp::attributes::string_array_attr(
                "host.ip",
                &["10.0.0.7".into(), "192.168.1.140".into()],
            ),
        ];
        let map = attr_map(&attrs);
        assert_eq!(map["host.name"], "edge-01");
        assert_eq!(map["host.ip"], "10.0.0.7, 192.168.1.140");
    }

    /// OPS-cargo-decides-product-version: the fleet table shows the release, and the build stays reachable beside it. A
    /// Foreign Agent that numbers itself in its own way is shown as it reported.
    #[test]
    fn the_displayed_version_drops_the_commit_and_keeps_the_pre_release() {
        assert_eq!(display_version("0.1.1+799e36a"), "0.1.1");
        assert_eq!(display_version("0.1.1-dev+799e36a"), "0.1.1-dev");
        assert_eq!(display_version("0.1.1"), "0.1.1");
        // Not a version this Server understands — shown rather than trimmed into something else.
        assert_eq!(display_version("v2.9-nightly"), "v2.9-nightly");
        assert_eq!(display_version(""), "");
    }

    #[test]
    fn capability_names_decode_known_bits_and_surface_undefined_ones() {
        use opamp::proto::AgentCapabilities as C;
        assert!(capability_names(0).is_empty());
        assert_eq!(
            capability_names(C::ReportsStatus as u64 | C::ReportsHealth as u64),
            ["ReportsStatus", "ReportsHealth"]
        );
        let with_undefined = capability_names(C::ReportsStatus as u64 | 1 << 60);
        assert_eq!(
            with_undefined,
            ["ReportsStatus", "unknown bits 0x1000000000000000"]
        );
    }

    fn at(key: i64, tiebreak: &str) -> Position {
        Position {
            key: Key::Int(key),
            tiebreak: tiebreak.to_string(),
        }
    }

    /// Pages follow each other without a gap or a repeat, ties broken by the tiebreak, in either
    /// direction, and the last page names no next one.
    /// Verifies: API-lists-paged-by-cursor
    #[test]
    fn pages_follow_each_other_in_a_stable_order() {
        let entries = || {
            vec![
                (at(2, "b"), "2b"),
                (at(1, "z"), "1z"),
                (at(2, "a"), "2a"),
                (at(3, "a"), "3a"),
                (at(1, "a"), "1a"),
            ]
        };
        for (descending, expected) in [
            (false, ["1a", "1z", "2a", "2b", "3a"]),
            (true, ["3a", "2a", "2b", "1a", "1z"]),
        ] {
            let mut seen = Vec::new();
            let mut after = None;
            loop {
                let page = page(entries(), descending, after.as_ref(), 2);
                assert_eq!(page.total, 5);
                seen.extend(page.entries);
                match page.next {
                    Some(next) => after = Some(next),
                    None => break,
                }
            }
            assert_eq!(seen, expected);
        }
        let four = || (0..4).map(|n| (at(n, "u"), n)).collect::<Vec<_>>();
        let first = page(four(), false, None, 2);
        let next = first.next.expect("two more follow");
        let last = page(four(), false, Some(&next), 2);
        assert_eq!(last.entries, [2, 3]);
        assert_eq!(last.next, None, "a full last page names no next one");
    }

    /// The search text carries the platform the offer reads: folded through the core's table, and
    /// read from the identifying attributes where the non-identifying ones do not carry it.
    /// Verifies: API-lists-paged-by-cursor, PKG-signed-packages-and-delivery
    #[test]
    fn the_search_finds_an_agent_under_its_canonical_platform() {
        let search = |identifying: &[(&str, &str)], non_identifying: &[(&str, &str)]| {
            let attrs = |pairs: &[(&str, &str)]| {
                pairs
                    .iter()
                    .map(|(k, v)| attributes::string_attr(k, v))
                    .collect()
            };
            let description = AgentDescription {
                identifying_attributes: attrs(identifying),
                non_identifying_attributes: attrs(non_identifying),
            };
            derive(&Source {
                uid: &InstanceUid::default(),
                description: Some(&description),
                health: None,
                remote_config_status: None,
                package_statuses: None,
                capabilities: 0,
                transport: Transport::WebSocket,
            })
            .search
        };
        let reported = search(
            &[("service.name", "otelcol")],
            &[("os.type", "linux"), ("host.arch", "x86_64")],
        );
        assert!(
            reported.lines().any(|line| line == "linux-amd64"),
            "{reported}"
        );
        let identifying_only = search(
            &[
                ("service.name", "otelcol"),
                ("os.type", "macos"),
                ("host.arch", "aarch64"),
            ],
            &[],
        );
        assert!(
            identifying_only.lines().any(|line| line == "darwin-arm64"),
            "{identifying_only}"
        );
        let unreported = search(&[("service.name", "otelcol")], &[("os.type", "linux")]);
        assert!(!unreported.contains("linux-"), "{unreported}");
    }

    /// A Selector parameter is written as a Deployment's Selector is.
    #[test]
    fn a_selector_parameter_is_pairs_separated_by_commas() {
        assert_eq!(
            parse_selector("tier=edge, os.type=linux"),
            Some(BTreeMap::from([
                ("os.type".to_string(), "linux".to_string()),
                ("tier".to_string(), "edge".to_string()),
            ]))
        );
        assert_eq!(parse_selector("tier"), None);
        assert_eq!(parse_selector("tier="), None);
        assert_eq!(parse_selector(""), None);
    }

    /// The type of one Agent as each path reads it: the list and its `service_name` filter, the
    /// fit of a Configuration, and the offer of a Package (CLT-what-an-agent-reports §1).
    fn types_read(identifying: &[(&str, &str)], non_identifying: &[(&str, &str)]) -> [String; 3] {
        let attrs = |pairs: &[(&str, &str)]| -> Vec<opamp::proto::KeyValue> {
            pairs
                .iter()
                .map(|(k, v)| attributes::string_attr(k, v))
                .collect()
        };
        let description = AgentDescription {
            identifying_attributes: attrs(identifying),
            non_identifying_attributes: attrs(non_identifying),
        };
        let listed = derive(&Source {
            uid: &InstanceUid::default(),
            description: Some(&description),
            health: None,
            remote_config_status: None,
            package_statuses: None,
            capabilities: 0,
            transport: Transport::WebSocket,
        })
        .service_name;
        let fitted = ["otelcol", "first", "second"]
            .into_iter()
            .find(|name| {
                let revision = crate::configs::Revision {
                    body: String::new(),
                    role: String::new(),
                    service_name: (*name).to_string(),
                };
                crate::configs::fits(&revision, Some(&description))
            })
            .unwrap_or_default()
            .to_string();
        let offered = crate::packages::reported_agent_type(Some(&description))
            .unwrap_or_default()
            .to_string();
        [listed, fitted, offered]
    }

    #[test]
    fn every_path_reads_the_agent_type_from_the_identifying_attributes_alone() {
        let elsewhere = types_read(&[], &[("service.name", "otelcol")]);
        assert_eq!(elsewhere, ["", "", ""].map(String::from));
        let identifying = types_read(&[("service.name", "otelcol")], &[]);
        assert_eq!(
            identifying,
            ["otelcol", "otelcol", "otelcol"].map(String::from)
        );
    }

    #[test]
    fn a_type_reported_twice_or_empty_is_read_alike_by_the_list_and_the_offer() {
        let twice = types_read(
            &[("service.name", "first"), ("service.name", "second")],
            &[],
        );
        assert_eq!(twice, ["first", "first", "first"].map(String::from));
        let empty = types_read(&[("service.name", "")], &[]);
        assert_eq!(empty, ["", "", ""].map(String::from));
    }
}
