//! The lists that grow with the fleet, read a page at a time
//! (ADR-draft-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server):
//! the filters, the order, the opaque cursor, and what an Agent's record gives the queries to
//! search and sort by.
//!
//! The filter, the order and the cursor are the core's (clause 8). On the filesystem store the
//! core answers them from memory with what this module derives; on PostgreSQL the store answers
//! them from columns the core writes from the same derivation, so the two stores answer the same.

use std::collections::BTreeMap;

use opamp::attributes;
use opamp::proto::{
    AgentDescription, ComponentHealth, PackageStatuses, RemoteConfigStatus, RemoteConfigStatuses,
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
    /// no label can stand in for it (ADR-0043 clause 18).
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

/// The derived values of one record.
#[must_use]
pub fn derive(source: &Source<'_>) -> Derived {
    let (identifying, non_identifying) = match source.description {
        Some(d) => (
            crate::fleet::attr_map(&d.identifying_attributes),
            crate::fleet::attr_map(&d.non_identifying_attributes),
        ),
        None => (BTreeMap::new(), BTreeMap::new()),
    };
    let lookup = |map: &BTreeMap<String, String>, key: &str| -> String {
        map.get(key).cloned().unwrap_or_default()
    };
    let service_name = lookup(&identifying, attributes::SERVICE_NAME);
    let instance_name = lookup(&non_identifying, attributes::SERVICE_INSTANCE_NAME);
    let uid = source.uid.to_string();
    let name = if !instance_name.is_empty() {
        instance_name.clone()
    } else if !service_name.is_empty() {
        service_name.clone()
    } else {
        uid.clone()
    };
    let build = lookup(&identifying, attributes::SERVICE_VERSION);
    let version = crate::fleet::display_version(&build);
    let os = match lookup(&non_identifying, attributes::OS_DESCRIPTION) {
        description if !description.is_empty() => description,
        _ => lookup(&non_identifying, attributes::OS_TYPE),
    };
    let platform = match (
        non_identifying.get("os.type"),
        non_identifying.get("host.arch"),
    ) {
        (Some(os), Some(arch)) if !os.is_empty() && !arch.is_empty() => format!("{os}-{arch}"),
        _ => String::new(),
    };
    let config = match source.remote_config_status.map(|s| s.status) {
        Some(s) if s == RemoteConfigStatuses::Applied as i32 => "APPLIED",
        Some(s) if s == RemoteConfigStatuses::Applying as i32 => "APPLYING",
        Some(s) if s == RemoteConfigStatuses::Failed as i32 => "FAILED",
        _ => "UNSET",
    }
    .to_string();
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
            let value = attributes::string_value(&description.identifying_attributes, &kv.key)
                .or_else(|| {
                    attributes::string_value(&description.non_identifying_attributes, &kv.key)
                })
                .map(sanitize);
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
    fields.extend(crate::fleet::capability_names(source.capabilities));
    if let Some(statuses) = source.package_statuses {
        let mut packages: Vec<crate::fleet::PackageStatusView> = statuses
            .packages
            .values()
            .map(crate::fleet::PackageStatusView::from_status)
            .collect();
        packages.sort_by(|a, b| a.name.cmp(&b.name));
        for package in packages {
            fields.extend([package.name, package.version, package.status]);
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
    /// What a Selector is matched against (ADR-0043 clause 18).
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

/// Where an Agent's group places it: its Client Agent, whether the group is ambiguous, and the
/// labels it inherits.
pub type Placed = (Option<InstanceUid>, bool, BTreeMap<String, String>);

/// Where the Agents that arrived over one Host place each other (ADR-0043 clause 19): on a direct
/// connection they are one group, behind a Gateway one group per machine they report. A group with
/// exactly one Client Agent places the others under it, and they inherit its labels; one with more
/// places nobody and marks every member ambiguous. Answers, per Agent of a group, its Client Agent,
/// whether it is ambiguous, and the labels it inherits.
#[must_use]
pub fn place_on_host(
    gateway: bool,
    agents: &[(
        InstanceUid,
        Option<&AgentDescription>,
        &BTreeMap<String, String>,
    )],
) -> std::collections::HashMap<InstanceUid, Placed> {
    let group_of = |description: Option<&AgentDescription>| {
        if gateway {
            crate::labels::machine(description)
        } else {
            Some(String::new())
        }
    };
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, (_, description, _)) in agents.iter().enumerate() {
        if let Some(group) = group_of(*description) {
            groups.entry(group).or_default().push(i);
        }
    }
    let mut placed = std::collections::HashMap::new();
    for members in groups.values() {
        let clients: Vec<usize> = members
            .iter()
            .copied()
            .filter(|i| crate::labels::is_client_agent(agents[*i].1))
            .collect();
        let client = match clients.as_slice() {
            [one] => Some(*one),
            _ => None,
        };
        for i in members {
            let (uid, _, _) = agents[*i];
            let inherited = match client {
                Some(c) if c != *i => agents[c].2.clone(),
                _ => BTreeMap::new(),
            };
            placed.insert(
                uid,
                (client.map(|c| agents[c].0), clients.len() > 1, inherited),
            );
        }
    }
    placed
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

    fn at(key: i64, tiebreak: &str) -> Position {
        Position {
            key: Key::Int(key),
            tiebreak: tiebreak.to_string(),
        }
    }

    /// Pages follow each other without a gap or a repeat, ties broken by the tiebreak, in either
    /// direction, and the last page names no next one.
    /// Verifies: ADR-draft-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server
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

    /// On a direct connection the Agents of a Host are one group, behind a Gateway one per
    /// machine; one Client Agent places the others, two place nobody.
    /// Verifies: ADR-0043
    #[test]
    fn the_agents_of_one_host_are_placed_by_their_group() {
        use opamp::attributes::string_attr;
        let described = |pairs: &[(&str, &str)]| AgentDescription {
            identifying_attributes: Vec::new(),
            non_identifying_attributes: pairs.iter().map(|(k, v)| string_attr(k, v)).collect(),
        };
        let (client, fellow, other, second) = (
            InstanceUid([1; 16]),
            InstanceUid([2; 16]),
            InstanceUid([3; 16]),
            InstanceUid([4; 16]),
        );
        let ring = BTreeMap::from([("ring".to_string(), "early".to_string())]);
        let none = BTreeMap::new();
        let c = described(&[("supervisor.kind.x", "true"), ("host.name", "m1")]);
        let f = described(&[("host.name", "m1")]);
        let o = described(&[("host.name", "m2")]);
        let agents = [
            (client, Some(&c), &ring),
            (fellow, Some(&f), &none),
            (other, Some(&o), &none),
        ];
        let behind = place_on_host(true, &agents);
        assert_eq!(behind[&fellow], (Some(client), false, ring.clone()));
        assert_eq!(behind[&client], (Some(client), false, BTreeMap::new()));
        assert_eq!(
            behind[&other],
            (None, false, BTreeMap::new()),
            "another machine"
        );
        let direct = place_on_host(false, &agents);
        assert_eq!(
            direct[&other],
            (Some(client), false, ring.clone()),
            "one group"
        );
        let s = described(&[("supervisor.kind.y", "true"), ("host.name", "m1")]);
        let two = place_on_host(true, &[agents[0], agents[1], (second, Some(&s), &none)]);
        assert_eq!(two[&fellow], (None, true, BTreeMap::new()), "ambiguous");
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
}
