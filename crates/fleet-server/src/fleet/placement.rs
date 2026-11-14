//! How the Agents of one Client are placed under its Client Agent (ADR-0030 clauses 18 and 19,
//! ADR-0045 clause 8): the rule for one group, which the fleet in memory and a page read from the
//! store share, the index of the groups this instance holds members of, how the Client Agents of a
//! group another instance carries are read, and the placing again after a notice or a catch-up.
//! Behind the same `AppState` entry points as before.

use std::sync::MutexGuard;

use super::*;

/// The Agents one Client presents (ADR-0030 clause 19), as far as the Server can tell them apart.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Group {
    /// On a direct connection: the Agents arriving over the same Host.
    Host(String),
    /// Behind a Gateway: the Agents arriving over the same Gateway Host that report the same
    /// machine — `host.id`, else `host.name` ([`crate::labels::machine`]).
    Behind { gateway: String, machine: String },
}

/// What the Host an Agent arrived over and its Client Agent add to its effective description
/// (ADR-0030 clauses 18 and 19). Derived from the records and the host register whenever either
/// changes; never persisted.
#[derive(Clone, Debug, Default)]
pub struct Placement {
    /// The `fleet.host.*` values of the Host it arrived over directly; empty behind a Gateway.
    pub host_values: BTreeMap<String, String>,
    /// The Client it belongs to, when the Server can tell.
    pub group: Option<Group>,
    /// Its Client Agent — itself, for the Client Agent — when its group holds exactly one.
    pub client_agent: Option<InstanceUid>,
    /// The labels of its Client Agent; empty for the Client Agent itself.
    pub inherited: BTreeMap<String, String>,
    /// Its group holds more than one Client Agent, so nothing is inherited.
    pub ambiguous: bool,
}

/// Which Agents each [`Group`] holds (ADR-0030 clause 19), so placing one Agent touches its group
/// alone. Locked strictly after `fleet`, and only while it is held.
#[derive(Default)]
pub(super) struct Groups(Mutex<HashMap<Group, HashSet<InstanceUid>>>);

impl Groups {
    fn lock(&self) -> MutexGuard<'_, HashMap<Group, HashSet<InstanceUid>>> {
        self.0.lock().expect("groups lock")
    }

    /// Whether any group holds `uid`.
    #[cfg(test)]
    pub(super) fn hold(&self, uid: &InstanceUid) -> bool {
        self.lock().values().any(|members| members.contains(uid))
    }
}

/// The Client Agents of groups another instance carries, with their labels, by group.
pub(super) type ClientsByGroup = HashMap<Group, Vec<(InstanceUid, BTreeMap<String, String>)>>;

/// How a placement reads the Client Agents of a group that this instance does not hold
/// (ADR-0030 clause 19,
/// ADR-0045
/// clause 8).
pub(super) enum Elsewhere {
    /// From the store as the group is placed, under the fleet lock.
    Now,
    /// Read before the fleet lock was taken, by group, not yet sifted of what is held; a group
    /// placed that was not read is noted in `missed` and placed again once the lock is let go.
    Read {
        read: ClientsByGroup,
        missed: Vec<Group>,
    },
}

/// Where an Agent's group places it: its Client Agent, whether the group is ambiguous, and the
/// labels it inherits.
pub type Placed = (Option<InstanceUid>, bool, BTreeMap<String, String>);

/// Where the Agents that arrived over one Host place each other (ADR-0030 clause 19): on a direct
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
        let clients: Vec<(InstanceUid, &BTreeMap<String, String>)> = members
            .iter()
            .filter(|i| crate::labels::is_client_agent(agents[**i].1))
            .map(|i| (agents[*i].0, agents[*i].2))
            .collect();
        placed.extend(place_group(members.iter().map(|i| agents[*i].0), &clients));
    }
    placed
}

/// The one rule of ADR-0030 clause 19, for one group: `members` placed by the group's Client Agents
/// `clients`, with their labels. Exactly one places every member under it, and the others inherit
/// its labels; more place nobody and mark every member ambiguous; none places nobody. Answers each
/// member's Client Agent, whether the group is ambiguous, and the labels it inherits. What the
/// fleet holds in memory and what a page reads from the store are both placed by it.
#[must_use]
pub fn place_group(
    members: impl IntoIterator<Item = InstanceUid>,
    clients: &[(InstanceUid, &BTreeMap<String, String>)],
) -> Vec<(InstanceUid, Placed)> {
    let (client, labels) = match clients {
        [(one, labels)] => (Some(*one), (*labels).clone()),
        _ => (None, BTreeMap::new()),
    };
    let ambiguous = clients.len() > 1;
    members
        .into_iter()
        .map(|uid| {
            // Empty where no one Client Agent places the group.
            let inherited = if client != Some(uid) {
                labels.clone()
            } else {
                BTreeMap::new()
            };
            (uid, (client, ambiguous, inherited))
        })
        .collect()
}

impl AppState {
    /// Where the Host an Agent arrived over places it (ADR-0030 clauses 18 and 19): the
    /// `fleet.host.*` values of a Host it arrived over directly, and the group its Client is
    /// recognised in. Nothing without a host register, and nothing for an Agent that names no
    /// Host.
    pub(super) fn locate(&self, record: &AgentRecord) -> (BTreeMap<String, String>, Option<Group>) {
        let (Some(host), Some(revocations)) = (&record.host, &self.revocations) else {
            return (BTreeMap::new(), None);
        };
        let (gateway, subject) = revocations.placement(host);
        if gateway {
            let group =
                crate::labels::machine(record.description.as_ref()).map(|machine| Group::Behind {
                    gateway: host.clone(),
                    machine,
                });
            return (BTreeMap::new(), group);
        }
        let values = subject
            .attributes()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        (values, Some(Group::Host(host.clone())))
    }

    /// Places every Agent again — at startup, once the host register is known, and when a Gateway
    /// mark changes.
    pub(super) fn place_all(&self) {
        let mut fleet = self.fleet.lock().expect("fleet lock");
        let mut groups = self.groups.lock();
        groups.clear();
        self.locate_all(&mut fleet, &mut groups);
        for group in groups.keys() {
            self.inherit_held(&mut fleet, &groups, group, &mut Elsewhere::Now);
        }
    }

    /// Locates every Agent of `fleet` and indexes it in its group, so its Client Agent can be
    /// found next.
    fn locate_all(
        &self,
        fleet: &mut HashMap<InstanceUid, AgentRecord>,
        groups: &mut HashMap<Group, HashSet<InstanceUid>>,
    ) {
        for (uid, record) in fleet.iter_mut() {
            let (host_values, group) = self.locate(record);
            record.placement = Placement {
                host_values,
                group: group.clone(),
                ..Placement::default()
            };
            if let Some(group) = group {
                groups.entry(group).or_default().insert(*uid);
            }
        }
    }

    /// Places the members of `group` this Server holds under their Client Agent, as [`inherit`]
    /// does. On a shared store the Client Agent of a Host may be carried by another instance, so
    /// the Agents the host register binds to the Host that this instance does not hold are read
    /// from the store, and a Client Agent among them counts as one of the group
    /// (ADR-0045
    /// clause 8). Behind a Gateway, where the register binds no Agent, the Agents that arrived
    /// over the Gateway's Host are read instead ([`arrived_over`](Self::arrived_over)).
    ///
    /// What is read of a group's Client Agents held elsewhere is read as `elsewhere` says: at once,
    /// under the fleet lock, or before it was taken, as a report places its Agent.
    fn inherit_held(
        &self,
        fleet: &mut HashMap<InstanceUid, AgentRecord>,
        groups: &HashMap<Group, HashSet<InstanceUid>>,
        group: &Group,
        elsewhere: &mut Elsewhere,
    ) {
        let outside = match elsewhere {
            Elsewhere::Now => self.client_agents_elsewhere(fleet, group),
            Elsewhere::Read { read, missed } => match read.get(group) {
                Some(found) => {
                    let mut outside: Vec<(InstanceUid, BTreeMap<String, String>)> = found
                        .iter()
                        .filter(|(uid, _)| !fleet.contains_key(uid))
                        .cloned()
                        .collect();
                    outside.sort_by_key(|(uid, _)| uid.0);
                    outside
                }
                None => {
                    if !missed.contains(group) {
                        missed.push(group.clone());
                    }
                    Vec::new()
                }
            },
        };
        inherit(fleet, groups, group, &outside);
    }

    /// Reads, before the fleet lock is taken, the Client Agents of each of `wanted` that this
    /// instance does not hold, as [`client_agents_elsewhere`](Self::client_agents_elsewhere) reads
    /// them under it: what a report's placement inherits from, so the lock waits for no read. What
    /// this instance comes to hold meanwhile is sifted out when the group is placed.
    /// The Agents that arrived over a Gateway's Host are read once for all its groups.
    fn read_elsewhere(&self, wanted: &[Group]) -> ClientsByGroup {
        let held = |uid: &InstanceUid| self.fleet.lock().expect("fleet lock").contains_key(uid);
        let mut read = HashMap::new();
        let mut behind: HashMap<&str, Result<HashMap<InstanceUid, AgentRecord>, String>> =
            HashMap::new();
        for group in wanted {
            if read.contains_key(group) {
                continue;
            }
            let found = match group {
                Group::Behind { gateway, machine } if self.agent_store.is_shared() => {
                    let arrived = behind
                        .entry(gateway.as_str())
                        .or_insert_with(|| self.arrived_over(gateway));
                    clients_behind(gateway, machine, arrived.as_ref(), &held)
                }
                _ => self.clients_elsewhere(group, &held),
            };
            read.insert(group.clone(), found);
        }
        read
    }

    /// Places again each group of `missed` that a report's placement found unread: read first,
    /// then placed under the fleet lock.
    fn place_groups_again(&self, missed: Vec<Group>) {
        if missed.is_empty() {
            return;
        }
        let mut elsewhere = Elsewhere::Read {
            read: self.read_elsewhere(&missed),
            missed: Vec::new(),
        };
        let mut fleet = self.fleet.lock().expect("fleet lock");
        let groups = self.groups.lock();
        for group in &missed {
            self.inherit_held(&mut fleet, &groups, group, &mut elsewhere);
        }
    }

    /// Places again every group of each of `hosts` that this instance holds members of: of a Host,
    /// the group under the Host, and of a Gateway's Host, every group behind it. What a notice that
    /// a binding of the Host changed, or that a Client Agent first arrived over it, calls for
    /// (ADR-0045
    /// clause 8). Nothing on a store this Server alone writes, whose own acts place what they
    /// change.
    pub fn place_hosts_again(&self, hosts: &BTreeSet<String>) {
        if hosts.is_empty() || !self.agent_store.is_shared() {
            return;
        }
        let due: Vec<Group> = self
            .groups
            .lock()
            .keys()
            .filter(|group| match group {
                Group::Host(host) => hosts.contains(host),
                Group::Behind { gateway, .. } => hosts.contains(gateway),
            })
            .cloned()
            .collect();
        self.place_groups_again(due);
    }

    /// The Client Agents of a group that this instance does not hold, with their labels, as
    /// stored; none on a store this Server alone writes. Of a Host, those the host register binds
    /// to it; behind a Gateway, which the register binds none to, those that arrived over the
    /// Gateway's Host and report the group's machine, read through the port's `query_page`
    /// (ADR-0045
    /// clause 8). One that cannot be read is logged, and the group is placed from what this
    /// instance holds.
    fn client_agents_elsewhere(
        &self,
        fleet: &HashMap<InstanceUid, AgentRecord>,
        group: &Group,
    ) -> Vec<(InstanceUid, BTreeMap<String, String>)> {
        self.clients_elsewhere(group, &|uid| fleet.contains_key(uid))
    }

    /// The Client Agents of `group` read from the store, of those `held` says this instance does
    /// not hold, as [`client_agents_elsewhere`](Self::client_agents_elsewhere) answers them.
    fn clients_elsewhere(
        &self,
        group: &Group,
        held: &dyn Fn(&InstanceUid) -> bool,
    ) -> Vec<(InstanceUid, BTreeMap<String, String>)> {
        if !self.agent_store.is_shared() {
            return Vec::new();
        }
        if let Group::Behind { gateway, machine } = group {
            return clients_behind(gateway, machine, self.arrived_over(gateway).as_ref(), held);
        }
        let Group::Host(host) = group else {
            return Vec::new();
        };
        let elsewhere: Vec<InstanceUid> = self
            .bound_to(host)
            .into_iter()
            .filter(|uid| !held(uid))
            .collect();
        if elsewhere.is_empty() {
            return Vec::new();
        }
        match self.agent_store.get_many(&elsewhere) {
            Ok(found) => clients_of_host(host, found.iter()),
            Err(e) => {
                warn!(host = %host, error = %e, "cannot read the agents of the host that another instance carries");
                Vec::new()
            }
        }
    }

    /// The Agents the host register binds to `host`, none where it binds every one or there is no
    /// register.
    fn bound_to(&self, host: &str) -> Vec<InstanceUid> {
        let Some(revocations) = &self.revocations else {
            return Vec::new();
        };
        match revocations.speaks_for(host) {
            SpeaksFor::Agents(bound) => bound.iter().filter_map(|uid| parse_hex_uid(uid)).collect(),
            SpeaksFor::Any => Vec::new(),
        }
    }

    /// Records read from the store, each placed among the others, as [`place_all`](Self::place_all)
    /// places what this Server holds: what the Operator plane shows of them where the fleet is not
    /// in memory.
    pub(super) fn placed(
        &self,
        records: HashMap<InstanceUid, AgentRecord>,
    ) -> HashMap<InstanceUid, AgentRecord> {
        let mut fleet = records;
        let mut groups = HashMap::new();
        self.locate_all(&mut fleet, &mut groups);
        for group in groups.keys() {
            inherit(&mut fleet, &groups, group, &[]);
        }
        fleet
    }

    /// Places one Agent again after what places it may have changed — its Host, its report, or
    /// its identity — and the groups it left and joined with it.
    pub(super) fn place(
        &self,
        fleet: &mut HashMap<InstanceUid, AgentRecord>,
        uid: &InstanceUid,
        elsewhere: &mut Elsewhere,
    ) {
        let Some(record) = fleet.get_mut(uid) else {
            return;
        };
        let (host_values, group) = self.locate(record);
        record.placement.host_values = host_values;
        let left = std::mem::replace(&mut record.placement.group, group.clone());
        if group.is_none() {
            record.placement.client_agent = None;
            record.placement.inherited.clear();
            record.placement.ambiguous = false;
        }
        let mut groups = self.groups.lock();
        if left != group {
            if let Some(left) = &left {
                unindex(&mut groups, left, uid);
            }
        }
        if let Some(joined) = &group {
            groups.entry(joined.clone()).or_default().insert(*uid);
            self.inherit_held(fleet, &groups, joined, elsewhere);
        }
        if let Some(left) = left.filter(|left| Some(left) != group.as_ref()) {
            self.inherit_held(fleet, &groups, &left, elsewhere);
        }
    }

    /// Takes an Agent that left the fleet, or its old identity, out of its group, and places what
    /// is left of the group again.
    pub(super) fn unplace(
        &self,
        fleet: &mut HashMap<InstanceUid, AgentRecord>,
        uid: &InstanceUid,
        group: Option<Group>,
        elsewhere: &mut Elsewhere,
    ) {
        let Some(group) = group else {
            return;
        };
        let mut groups = self.groups.lock();
        unindex(&mut groups, &group, uid);
        self.inherit_held(fleet, &groups, &group, elsewhere);
    }

    /// How a report's placement reads the Client Agents of `groups` held elsewhere: read now,
    /// before the fleet lock is taken, on a shared store; on a store this Server alone writes
    /// there is nothing to read.
    pub(super) fn elsewhere_of(&self, groups: &[Group]) -> Elsewhere {
        if !self.agent_store.is_shared() {
            return Elsewhere::Now;
        }
        Elsewhere::Read {
            read: self.read_elsewhere(groups),
            missed: Vec::new(),
        }
    }

    /// Places again what a placement from `elsewhere` found unread; called with no lock held.
    pub(super) fn place_again(&self, elsewhere: Elsewhere) {
        if let Elsewhere::Read { missed, .. } = elsewhere {
            self.place_groups_again(missed);
        }
    }

    /// The group an Agent arriving over `host` and described by `description` is placed in, as
    /// [`locate`](Self::locate) places it.
    pub(super) fn group_of(
        &self,
        host: Option<&str>,
        description: Option<&AgentDescription>,
    ) -> Option<Group> {
        let (Some(host), Some(revocations)) = (host, &self.revocations) else {
            return None;
        };
        if revocations.placement(host).0 {
            return crate::labels::machine(description).map(|machine| Group::Behind {
                gateway: host.to_string(),
                machine,
            });
        }
        Some(Group::Host(host.to_string()))
    }

    /// The records of the Agents that last arrived over `host`, as stored, through
    /// [`AgentStore::query_page`] filtered on that Host
    /// (ADR-0031
    /// clause 9): the Agents behind a Gateway, whichever instance carries them.
    pub(super) fn arrived_over(
        &self,
        host: &str,
    ) -> Result<HashMap<InstanceUid, AgentRecord>, String> {
        let filter = crate::listing::AgentFilter {
            arrived_over: Some(host.to_string()),
            ..crate::listing::AgentFilter::default()
        };
        let page = self
            .agent_store
            .query_page(&crate::agent_store::PageQuery {
                filter: &filter,
                sort: crate::listing::AgentSort::Seq,
                descending: false,
                after: None,
                limit: self.max_agents,
                seen_before_ms: 0,
            })?;
        Ok(self.records_of(
            page.agents
                .into_iter()
                .map(|found| (found.uid, found.stored))
                .collect(),
        ))
    }

    /// Places again each group this instance holds members of whose Client Agent may be one of
    /// `uids` that it does not hold — the group's Client Agent now, or, of a Host, an Agent the
    /// host register binds to it. Behind a Gateway a Client Agent another instance carries that
    /// the group was not placed under yet is not found here: the notice of its arrival places the
    /// group again ([`place_hosts_again`](AppState::place_hosts_again)), and so does a catch-up
    /// ([`catch_up`](AppState::catch_up)).
    pub(super) fn place_again_for(&self, uids: &HashSet<InstanceUid>) {
        let Some(revocations) = &self.revocations else {
            return;
        };
        let mut fleet = self.fleet.lock().expect("fleet lock");
        // The notified Agents this instance does not hold.
        let elsewhere: Vec<InstanceUid> = uids
            .iter()
            .filter(|uid| !fleet.contains_key(uid))
            .copied()
            .collect();
        if elsewhere.is_empty() {
            return;
        }
        let groups = self.groups.lock();
        let due: Vec<Group> = groups
            .iter()
            .filter(|(group, members)| {
                // Behind a Gateway the register binds no Agent; the group's Client Agent is the
                // one it was placed under.
                let bound = match group {
                    Group::Host(host) => match revocations.speaks_for(host) {
                        SpeaksFor::Agents(bound) => bound,
                        SpeaksFor::Any => BTreeSet::new(),
                    },
                    Group::Behind { .. } => BTreeSet::new(),
                };
                elsewhere.iter().any(|uid| {
                    bound.contains(&hex::encode(uid.as_bytes()))
                        || members.iter().any(|member| {
                            fleet
                                .get(member)
                                .and_then(|record| record.placement.client_agent)
                                == Some(*uid)
                        })
                })
            })
            .map(|(group, _)| group.clone())
            .collect();
        for group in &due {
            self.inherit_held(&mut fleet, &groups, group, &mut Elsewhere::Now);
        }
    }

    /// Places `group` again from what this instance holds and what the store holds of its Client
    /// Agents now, as an act of this instance that changed a member's labels calls for.
    pub(super) fn place_group_now(
        &self,
        fleet: &mut HashMap<InstanceUid, AgentRecord>,
        group: &Group,
    ) {
        let groups = self.groups.lock();
        self.inherit_held(fleet, &groups, group, &mut Elsewhere::Now);
    }

    /// What a catch-up reads before the fleet lock of the Client Agents behind each Gateway whose
    /// groups this instance holds members of: the Agents that arrived over each Gateway's Host, read
    /// once per Gateway, not yet sifted of what is held (ADR-0045 clause 10).
    pub(super) fn read_behind_gateways(&self) -> ClientsByGroup {
        let gateways: Vec<Group> = self
            .groups
            .lock()
            .keys()
            .filter(|group| matches!(group, Group::Behind { .. }))
            .cloned()
            .collect();
        let mut behind = HashMap::new();
        let mut arrived = HashMap::new();
        for group in gateways {
            let Group::Behind { gateway, machine } = &group else {
                continue;
            };
            let arrived = arrived
                .entry(gateway.clone())
                .or_insert_with(|| self.arrived_over(gateway));
            let found = clients_behind(gateway, machine, arrived.as_ref(), &|_| false);
            behind.insert(group, found);
        }
        behind
    }

    /// The Agents the host register binds to each Host whose group this instance holds members
    /// of: what a catch-up reads with the records of the Agents it holds, to find the Client Agents
    /// of those groups that another instance carries.
    pub(super) fn bound_to_hosts(&self) -> HashMap<String, Vec<InstanceUid>> {
        let hosts: Vec<String> = self
            .groups
            .lock()
            .keys()
            .filter_map(|group| match group {
                Group::Host(host) => Some(host.clone()),
                Group::Behind { .. } => None,
            })
            .collect();
        hosts
            .into_iter()
            .map(|host| {
                let bound = self.bound_to(&host);
                (host, bound)
            })
            .collect()
    }

    /// Places every group this instance holds members of again, the Client Agents held elsewhere
    /// read as `elsewhere` says.
    pub(super) fn place_every_group(
        &self,
        fleet: &mut HashMap<InstanceUid, AgentRecord>,
        elsewhere: &mut Elsewhere,
    ) {
        let groups = self.groups.lock();
        let placed: Vec<Group> = groups.keys().cloned().collect();
        for group in &placed {
            self.inherit_held(fleet, &groups, group, elsewhere);
        }
    }
}

/// Takes `uid` out of `group`, and the group out of the index once it holds nobody.
fn unindex(groups: &mut HashMap<Group, HashSet<InstanceUid>>, group: &Group, uid: &InstanceUid) {
    if let Some(members) = groups.get_mut(group) {
        members.remove(uid);
        if members.is_empty() {
            groups.remove(group);
        }
    }
}

/// The Client Agents behind `gateway` that report `machine` and that `held` says this instance does
/// not hold, with their labels, of the Agents `arrived` read as arrived over the Gateway's Host. A
/// read that failed is logged, and none is answered.
fn clients_behind(
    gateway: &str,
    machine: &str,
    arrived: Result<&HashMap<InstanceUid, AgentRecord>, &String>,
    held: &dyn Fn(&InstanceUid) -> bool,
) -> Vec<(InstanceUid, BTreeMap<String, String>)> {
    match arrived {
        Ok(found) => {
            let mut clients: Vec<(InstanceUid, BTreeMap<String, String>)> = found
                .iter()
                .filter(|(uid, record)| {
                    crate::labels::is_client_agent(record.description.as_ref())
                        && crate::labels::machine(record.description.as_ref()).as_deref()
                            == Some(machine)
                        && !held(uid)
                })
                .map(|(uid, record)| (*uid, record.labels.clone()))
                .collect();
            clients.sort_by_key(|(uid, _)| uid.0);
            clients
        }
        Err(e) => {
            warn!(gateway = %gateway, error = %e, "cannot read the agents behind the gateway that another instance carries");
            Vec::new()
        }
    }
}

/// The Client Agents among `found` that arrived over `host`, with their labels, in the order of
/// their `instance_uid`: of a Host's group, those of the Agents the host register binds to it.
fn clients_of_host<'a>(
    host: &str,
    found: impl IntoIterator<Item = (&'a InstanceUid, &'a PersistedAgent)>,
) -> Vec<(InstanceUid, BTreeMap<String, String>)> {
    let mut clients: Vec<(InstanceUid, BTreeMap<String, String>)> = found
        .into_iter()
        .filter(|(_, record)| {
            record.host.as_deref() == Some(host)
                && crate::labels::is_client_agent(record.description.as_ref())
        })
        .map(|(uid, record)| (*uid, record.labels.clone()))
        .collect();
    clients.sort_by_key(|(uid, _)| uid.0);
    clients
}

/// Places the members of one group under their Client Agent (ADR-0030 clause 19): with exactly
/// one, the others inherit its labels; with more than one, nobody inherits and every member is
/// marked ambiguous; with none, nobody inherits. `elsewhere` are the group's Client Agents that
/// `fleet` does not hold, with their labels.
fn inherit(
    fleet: &mut HashMap<InstanceUid, AgentRecord>,
    groups: &HashMap<Group, HashSet<InstanceUid>>,
    group: &Group,
    elsewhere: &[(InstanceUid, BTreeMap<String, String>)],
) {
    let Some(members) = groups.get(group) else {
        return;
    };
    let mut clients: Vec<(InstanceUid, &BTreeMap<String, String>)> = members
        .iter()
        .filter_map(|uid| {
            let record = fleet.get(uid)?;
            crate::labels::is_client_agent(record.description.as_ref())
                .then_some((*uid, &record.labels))
        })
        .collect();
    clients.extend(elsewhere.iter().map(|(uid, labels)| (*uid, labels)));
    let placed = place_group(members.iter().copied(), &clients);
    for (uid, (client_agent, ambiguous, inherited)) in placed {
        if let Some(record) = fleet.get_mut(&uid) {
            record.placement.client_agent = client_agent;
            record.placement.inherited = inherited;
            record.placement.ambiguous = ambiguous;
        }
    }
}

/// How a catch-up places the groups: the Client Agents behind each Gateway as `behind` read them,
/// and those of each Host's group among the records `stored` read of the Agents `bound` to it. A
/// group neither covers is read and placed again once the locks are let go.
pub(super) fn elsewhere_from(
    behind: ClientsByGroup,
    bound: &HashMap<String, Vec<InstanceUid>>,
    stored: &HashMap<InstanceUid, PersistedAgent>,
) -> Elsewhere {
    let mut read = behind;
    for (host, uids) in bound {
        let found = clients_of_host(
            host,
            uids.iter()
                .filter_map(|uid| stored.get(uid).map(|record| (uid, record))),
        );
        read.insert(Group::Host(host.clone()), found);
    }
    Elsewhere::Read {
        read,
        missed: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one rule for a group: one Client Agent places every member and the others inherit its
    /// labels; several place nobody and mark every member ambiguous; none places nobody.
    /// Verifies: ADR-0030
    #[test]
    fn a_group_is_placed_by_its_one_client_agent_and_by_no_other() {
        let (client, other, member) = (
            InstanceUid([1; 16]),
            InstanceUid([2; 16]),
            InstanceUid([3; 16]),
        );
        let labels = BTreeMap::from([("tier".to_string(), "edge".to_string())]);
        let none = BTreeMap::new();
        let placed: std::collections::HashMap<_, _> =
            place_group([client, member], &[(client, &labels)])
                .into_iter()
                .collect();
        assert_eq!(placed[&client], (Some(client), false, none.clone()));
        assert_eq!(placed[&member], (Some(client), false, labels.clone()));
        let placed: std::collections::HashMap<_, _> = place_group(
            [client, other, member],
            &[(client, &labels), (other, &labels)],
        )
        .into_iter()
        .collect();
        assert!(placed.values().all(|p| *p == (None, true, none.clone())));
        let placed = place_group([member], &[]);
        assert_eq!(placed, [(member, (None, false, none))]);
    }

    /// On a direct connection the Agents of a Host are one group, behind a Gateway one per
    /// machine; one Client Agent places the others, two place nobody.
    /// Verifies: ADR-0030
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
}
