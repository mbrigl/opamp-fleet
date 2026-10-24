//! Which host a Gateway takes to speak for an `instance_uid` (ADR-0060 clause 1, after ADR-0036
//! clause 2): the host of the first report for it, kept across the Gateway's restarts in its store
//! ([`store`](crate::gateway::store)). What the Gateway forwards, relays and passes on for an Agent
//! is decided against this binding; the state is the core's, and the package cache holds it and
//! asks it.

use std::collections::{HashMap, HashSet};

use opamp::uid::InstanceUid;

/// How many bindings there may be: per host, and in all.
#[derive(Clone, Copy)]
pub struct Limits {
    /// The most `instance_uid`s one host may bind: `max_carried_agents`.
    pub per_host: usize,
    /// The most bindings the Gateway holds at once.
    pub total: usize,
}

/// What a report did to its `instance_uid`'s binding.
#[derive(Debug, PartialEq, Eq)]
pub enum Bound {
    /// Bound already, to whichever host reported it first; now the most recently reported.
    Kept,
    /// Bound now, to the reporting host.
    New,
    /// Not bound, for this reason.
    Refused(&'static str),
}

/// Where a host's certificate ends, as the Gateway keeps it (ADR-0060 clause 2): the latest
/// `notAfter` among the valid certificates the host reported under, and that certificate as the
/// revocation list names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct End {
    /// The `notAfter`, in seconds since the Unix epoch.
    pub not_after: i64,
    /// The SHA-256 of the issuer's DER-encoded name, in lowercase hex.
    pub issuer: String,
    /// The serial in lowercase hex without leading zeros.
    pub serial: String,
}

/// One change to the bindings, as the store keeps it (ADR-0060 clause 2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// An `instance_uid` is bound to a host.
    Bound(InstanceUid, String),
    /// A binding is given up or ended.
    Unbound(InstanceUid),
    /// A binding was reported: it is now the most recently reported.
    Reported(InstanceUid),
    /// A host's certificate ends here.
    Ends(String, End),
}

/// An `instance_uid`'s host, and when it last reported.
struct Binding {
    host: String,
    reported: u64,
    /// Made, and not yet in the store.
    pending: bool,
}

/// Each `instance_uid` to the host of its first report.
#[derive(Default)]
pub struct Bindings {
    bound: HashMap<InstanceUid, Binding>,
    /// How many `instance_uid`s each host has bound.
    per_host: HashMap<String, usize>,
    /// Where the certificate of each host with a binding ends.
    ends: HashMap<String, End>,
    /// Orders the reports, so the least recently reported binding is known.
    tick: u64,
    /// The bindings reported since their order was last written.
    reported: HashSet<InstanceUid>,
}

/// What a report did: to its binding, to the bindings that made room for it, and what the store
/// must hold before the report is relied on.
#[derive(Debug, PartialEq, Eq)]
pub struct Outcome {
    pub bound: Bound,
    /// The `instance_uid` whose binding made room, if one did, so what else the Gateway holds for
    /// it can go too.
    pub evicted: Vec<InstanceUid>,
    /// What the store must hold before the report is forwarded (ADR-0060 clause 2): the new
    /// binding, the removals that made room for it, and the host's certificate end where the
    /// report moved it. Empty when the report changed nothing the store keeps.
    pub changes: Vec<Change>,
}

impl Bindings {
    /// Binds `uid` to `host` on its first report, and on every report marks it the most recently
    /// reported. A host binds at most `limits.per_host`, and the Gateway at most `limits.total`:
    /// past either the host's own least recently reported binding whose `instance_uid` is `idle`
    /// makes room, so a host whose Agents come and go is not shut out by its own past, and never
    /// another host's (ADR-0060 clause 1). Without one nothing is bound.
    ///
    /// A report under a certificate whose `notAfter` has passed by `now` binds nothing and moves
    /// no end (ADR-0060 clause 3); one under a certificate that ends later than the one the host's
    /// end was kept from moves it to `end` (clause 2). A new binding is pending until
    /// [`Bindings::commit`]: a pending binding makes no room, and a report for it binds nothing, so
    /// nothing is relied on before the store holds it.
    pub fn bind(
        &mut self,
        uid: InstanceUid,
        host: &str,
        end: &End,
        now: i64,
        limits: Limits,
        idle: impl Fn(&InstanceUid) -> bool,
    ) -> Outcome {
        let refused = |why, evicted| Outcome {
            bound: Bound::Refused(why),
            evicted,
            changes: Vec::new(),
        };
        if end.not_after < now {
            return refused(
                "the certificate it was reported under has expired",
                Vec::new(),
            );
        }
        self.tick += 1;
        let tick = self.tick;
        if let Some(binding) = self.bound.get_mut(&uid) {
            if binding.pending {
                return refused("its binding is still being written", Vec::new());
            }
            binding.reported = tick;
            let own = binding.host == host;
            self.reported.insert(uid);
            return Outcome {
                bound: Bound::Kept,
                evicted: Vec::new(),
                changes: if own {
                    self.move_end(host, end).into_iter().collect()
                } else {
                    Vec::new()
                },
            };
        }
        let idle = |uid: &InstanceUid, binding: &Binding| !binding.pending && idle(uid);
        let mut evicted = Vec::new();
        let full = if self.per_host.get(host).copied().unwrap_or(0) >= limits.per_host {
            Some("the host holds as many bindings as max_carried_agents allows, none of them idle")
        } else if self.bound.len() >= limits.total {
            Some(
                "the Gateway holds as many bindings as it may, and none of this host's own is idle",
            )
        } else {
            None
        };
        if let Some(why) = full {
            let Some(victim) = self
                .bound
                .iter()
                .filter(|(uid, binding)| binding.host == host && idle(uid, binding))
                .min_by_key(|(_, binding)| binding.reported)
                .map(|(uid, _)| *uid)
            else {
                return refused(why, evicted);
            };
            self.unbind(&victim);
            evicted.push(victim);
        }
        self.bound.insert(
            uid,
            Binding {
                host: host.to_string(),
                reported: tick,
                pending: true,
            },
        );
        *self.per_host.entry(host.to_string()).or_default() += 1;
        let mut changes: Vec<Change> = evicted.iter().map(|uid| Change::Unbound(*uid)).collect();
        changes.extend(self.move_end(host, end));
        changes.push(Change::Bound(uid, host.to_string()));
        Outcome {
            bound: Bound::New,
            evicted,
            changes,
        }
    }

    /// Moves `host`'s certificate end to `end` when it ends later, or when the host has none yet.
    fn move_end(&mut self, host: &str, end: &End) -> Option<Change> {
        if self
            .ends
            .get(host)
            .is_some_and(|kept| kept.not_after >= end.not_after)
        {
            return None;
        }
        self.ends.insert(host.to_string(), end.clone());
        Some(Change::Ends(host.to_string(), end.clone()))
    }

    /// The store holds `uid`'s new binding: it is relied on from now.
    pub fn commit(&mut self, uid: &InstanceUid) {
        if let Some(binding) = self.bound.get_mut(uid) {
            binding.pending = false;
        }
    }

    /// The store could not take `uid`'s new binding: it is not made. Returns its removal, should
    /// the store hold it after all.
    pub fn abort(&mut self, uid: &InstanceUid) -> Vec<Change> {
        if !self.bound.get(uid).is_some_and(|binding| binding.pending) {
            return Vec::new();
        }
        self.unbind(uid);
        vec![Change::Unbound(*uid)]
    }

    /// Whether `uid` is bound to `host` and relied on: its binding is not pending.
    #[must_use]
    pub fn holds(&self, uid: &InstanceUid, host: &str) -> bool {
        self.bound
            .get(uid)
            .is_some_and(|binding| binding.host == host && !binding.pending)
    }

    /// The bindings reported since the last call, the least recently reported first: what keeps
    /// their order across a restart (ADR-0060 clause 2).
    pub fn reports(&mut self) -> Vec<Change> {
        let mut reported: Vec<(u64, InstanceUid)> = std::mem::take(&mut self.reported)
            .into_iter()
            .filter_map(|uid| self.bound.get(&uid).map(|binding| (binding.reported, uid)))
            .collect();
        reported.sort_unstable_by_key(|(reported, _)| *reported);
        reported
            .into_iter()
            .map(|(_, uid)| Change::Reported(uid))
            .collect()
    }

    /// Ends every binding of each host whose kept certificate end has passed by `now`, or whose
    /// certificate `revoked` says the revocation list names (ADR-0060 clause 3). Returns each such
    /// host, and the `instance_uid`s it held.
    pub fn end(
        &mut self,
        now: i64,
        revoked: impl Fn(&End) -> bool,
    ) -> Vec<(String, Vec<InstanceUid>)> {
        let ended: HashSet<&String> = self
            .ends
            .iter()
            .filter(|(_, end)| end.not_after < now || revoked(end))
            .map(|(host, _)| host)
            .collect();
        if ended.is_empty() {
            return Vec::new();
        }
        let mut by_host: HashMap<String, Vec<InstanceUid>> = HashMap::new();
        for (uid, binding) in &self.bound {
            if ended.contains(&binding.host) {
                by_host.entry(binding.host.clone()).or_default().push(*uid);
            }
        }
        for uid in by_host.values().flatten() {
            self.unbind(uid);
        }
        by_host.into_iter().collect()
    }

    /// Removes `uid`'s binding; a host left with none keeps no certificate end.
    fn unbind(&mut self, uid: &InstanceUid) {
        let Some(binding) = self.bound.remove(uid) else {
            return;
        };
        if let Some(count) = self.per_host.get_mut(&binding.host) {
            *count -= 1;
            if *count == 0 {
                self.per_host.remove(&binding.host);
                self.ends.remove(&binding.host);
            }
        }
    }

    /// The bindings `changes` leave, applied in order: the store read back (ADR-0060 clause 2). A
    /// later report orders a binding after the earlier ones, and a removal of what is not bound
    /// changes nothing.
    ///
    /// # Errors
    /// Returns an error when a host is left with a binding and no certificate end, which only a
    /// damaged store holds: such a binding could never end.
    pub fn replay(changes: impl IntoIterator<Item = Change>) -> Result<Bindings, String> {
        let mut bindings = Bindings::default();
        let mut ends = HashMap::new();
        for change in changes {
            bindings.tick += 1;
            let tick = bindings.tick;
            match change {
                Change::Bound(uid, host) => {
                    bindings.unbind(&uid);
                    *bindings.per_host.entry(host.clone()).or_default() += 1;
                    bindings.bound.insert(
                        uid,
                        Binding {
                            host,
                            reported: tick,
                            pending: false,
                        },
                    );
                }
                Change::Unbound(uid) => bindings.unbind(&uid),
                Change::Reported(uid) => {
                    if let Some(binding) = bindings.bound.get_mut(&uid) {
                        binding.reported = tick;
                    }
                }
                Change::Ends(host, end) => {
                    ends.insert(host, end);
                }
            }
        }
        for host in bindings.per_host.keys() {
            let end = ends
                .remove(host)
                .ok_or_else(|| format!("the host {host} has a binding and no certificate end"))?;
            bindings.ends.insert(host.clone(), end);
        }
        Ok(bindings)
    }

    /// What the store holds when it is rewritten whole: each host's certificate end, then every
    /// binding, the least recently reported first, so that [`Bindings::replay`] restores the order.
    #[must_use]
    pub fn snapshot(&self) -> Vec<Change> {
        let mut bound: Vec<(&InstanceUid, &Binding)> = self.bound.iter().collect();
        bound.sort_by_key(|(_, binding)| binding.reported);
        self.ends
            .iter()
            .map(|(host, end)| Change::Ends(host.clone(), end.clone()))
            .chain(
                bound
                    .into_iter()
                    .map(|(uid, binding)| Change::Bound(*uid, binding.host.clone())),
            )
            .collect()
    }

    /// How many lines a store rewritten whole holds: one per binding, one per host's end.
    #[must_use]
    pub fn live(&self) -> usize {
        self.bound.len() + self.ends.len()
    }

    /// The host `uid` is bound to.
    pub fn host_of(&self, uid: &InstanceUid) -> Option<&str> {
        self.bound.get(uid).map(|binding| binding.host.as_str())
    }

    /// How many `instance_uid`s `host` has bound.
    #[cfg(test)]
    pub fn count_of(&self, host: &str) -> usize {
        self.per_host.get(host).copied().unwrap_or(0)
    }

    /// How many bindings there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bound.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bound.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits {
        per_host: 2,
        total: 3,
    };

    fn end(not_after: i64) -> End {
        End {
            not_after,
            issuer: "ab".repeat(32),
            serial: "1".to_string(),
        }
    }

    /// A report of `uid` from `host`, its binding written at once.
    fn bind(
        bindings: &mut Bindings,
        uid: InstanceUid,
        host: &str,
        idle: impl Fn(&InstanceUid) -> bool,
    ) -> (Bound, Vec<InstanceUid>) {
        let outcome = bindings.bind(uid, host, &end(i64::MAX), 0, LIMITS, idle);
        bindings.commit(&uid);
        (outcome.bound, outcome.evicted)
    }

    /// The first report binds, and a later one from another host leaves the binding where it was.
    /// Verifies: ADR-0026
    #[test]
    fn the_first_reporting_host_keeps_the_binding() {
        let mut bindings = Bindings::default();
        let uid = InstanceUid::default();
        assert_eq!(bind(&mut bindings, uid, "a", |_| true).0, Bound::New);
        assert_eq!(bind(&mut bindings, uid, "b", |_| true).0, Bound::Kept);
        assert_eq!(bindings.host_of(&uid), Some("a"));
        assert_eq!(bindings.count_of("b"), 0);
    }

    /// Past its share a host gives up its own least recently reported idle binding, never another
    /// host's, and with none of its own idle nothing is bound.
    /// Verifies: ADR-0039
    #[test]
    fn past_its_share_a_host_gives_up_its_own_idle_binding() {
        let mut bindings = Bindings::default();
        let [first, second, other] = [(); 3].map(|()| InstanceUid::default());
        bind(&mut bindings, first, "a", |_| true);
        bind(&mut bindings, other, "b", |_| true);
        bind(&mut bindings, second, "a", |_| true);
        let third = InstanceUid::default();
        assert_eq!(
            bind(&mut bindings, third, "a", |uid| *uid != second),
            (Bound::New, vec![first]),
            "its own idle one made room, and is named"
        );
        assert!(
            bindings.host_of(&first).is_none(),
            "its own idle one made room"
        );
        assert_eq!(bindings.host_of(&other), Some("b"), "another host's stayed");
        assert_eq!(bindings.count_of("a"), 2);
        assert!(matches!(
            bind(&mut bindings, InstanceUid::default(), "a", |_| false),
            (Bound::Refused(_), evicted) if evicted.is_empty()
        ));
    }

    /// At the total, a host's report for a new `instance_uid` takes the place of the host's own
    /// least recently reported idle binding — a report refreshing how recent a binding is — and
    /// never another host's: a host without one of its own binds nothing.
    /// Verifies: ADR-0060
    #[test]
    fn at_the_total_only_the_reporting_hosts_own_idle_binding_makes_room() {
        let mut bindings = Bindings::default();
        let [first, second, other] = [(); 3].map(|()| InstanceUid::default());
        bind(&mut bindings, first, "a", |_| true);
        bind(&mut bindings, other, "b", |_| true);
        bind(&mut bindings, second, "a", |_| true);
        assert_eq!(bind(&mut bindings, first, "a", |_| true).0, Bound::Kept);
        let limits = Limits {
            per_host: 3,
            total: 3,
        };
        let new = InstanceUid::default();
        let outcome = bindings.bind(new, "a", &end(i64::MAX), 0, limits, |_| true);
        assert_eq!(
            (outcome.bound, outcome.evicted),
            (Bound::New, vec![second]),
            "its own least recently reported"
        );
        bindings.commit(&new);
        assert_eq!(
            bindings.host_of(&first),
            Some("a"),
            "reported again, so kept"
        );
        assert_eq!(bindings.host_of(&other), Some("b"), "another host's stayed");
        assert_eq!(bindings.len(), 3);

        let outcome = bindings.bind(
            InstanceUid::default(),
            "c",
            &end(i64::MAX),
            0,
            limits,
            |_| true,
        );
        assert!(
            matches!(outcome.bound, Bound::Refused(_)) && outcome.evicted.is_empty(),
            "a host with no binding of its own binds nothing"
        );
        let outcome = bindings.bind(
            InstanceUid::default(),
            "b",
            &end(i64::MAX),
            0,
            limits,
            |_| false,
        );
        assert!(
            matches!(outcome.bound, Bound::Refused(_)) && outcome.evicted.is_empty(),
            "nor one whose own are not idle"
        );
        assert_eq!(
            [first, other, new].map(|uid| bindings.host_of(&uid).is_some()),
            [true; 3]
        );
    }

    /// Replaying a store keeps each host's last certificate end and only for a host left with a
    /// binding; a binding whose host has no end is a damaged store.
    /// Verifies: ADR-0060
    #[test]
    fn a_replayed_binding_keeps_its_hosts_last_end() {
        let (kept, gone) = (InstanceUid::default(), InstanceUid::default());
        let bindings = Bindings::replay([
            Change::Ends("a".into(), end(1)),
            Change::Bound(kept, "a".into()),
            Change::Ends("a".into(), end(2)),
            Change::Ends("b".into(), end(3)),
            Change::Bound(gone, "b".into()),
            Change::Unbound(gone),
            Change::Unbound(InstanceUid::default()),
        ])
        .expect("replay");
        assert_eq!(bindings.host_of(&kept), Some("a"));
        assert_eq!(
            bindings.snapshot(),
            vec![
                Change::Ends("a".into(), end(2)),
                Change::Bound(kept, "a".into())
            ]
        );
        assert_eq!(bindings.live(), 2);
        assert!(Bindings::replay([Change::Bound(kept, "a".into())]).is_err());
    }

    /// A new binding is pending until the store holds it: what the store must hold comes with it,
    /// the host's certificate end among it; a report for it meanwhile binds nothing, and it makes
    /// no room. A later certificate moves the end, an earlier one does not, and a binding the store
    /// could not take is not made.
    /// Verifies: ADR-0060
    #[test]
    fn a_new_binding_waits_for_the_store() {
        let mut bindings = Bindings::default();
        let uid = InstanceUid::default();
        let outcome = bindings.bind(uid, "a", &end(10), 0, LIMITS, |_| true);
        assert_eq!(
            outcome,
            Outcome {
                bound: Bound::New,
                evicted: Vec::new(),
                changes: vec![
                    Change::Ends("a".into(), end(10)),
                    Change::Bound(uid, "a".into())
                ],
            }
        );
        assert!(!bindings.holds(&uid, "a"), "pending");
        assert!(matches!(
            bindings.bind(uid, "a", &end(10), 0, LIMITS, |_| true).bound,
            Bound::Refused(_)
        ));
        bindings.commit(&uid);
        assert!(bindings.holds(&uid, "a"));
        assert_eq!(
            bindings
                .bind(uid, "a", &end(20), 0, LIMITS, |_| true)
                .changes,
            vec![Change::Ends("a".into(), end(20))],
            "a renewed certificate moves the end"
        );
        assert!(bindings
            .bind(uid, "a", &end(15), 0, LIMITS, |_| true)
            .changes
            .is_empty());

        let other = InstanceUid::default();
        bindings.bind(other, "b", &end(10), 0, LIMITS, |_| true);
        assert_eq!(bindings.abort(&other), vec![Change::Unbound(other)]);
        assert_eq!(bindings.host_of(&other), None);
        assert_eq!(bindings.live(), 2, "b keeps no end without a binding");
        assert!(bindings.abort(&uid).is_empty(), "a written binding stays");
    }

    /// A report under a certificate whose `notAfter` has passed binds nothing and moves no end. A
    /// host's bindings end once its kept end has passed, or once the revocation list names the
    /// certificate it was kept from; another host's are left, and the `instance_uid` is free to
    /// bind anew.
    /// Verifies: ADR-0060
    #[test]
    fn a_hosts_bindings_end_with_its_certificate() {
        let mut bindings = Bindings::default();
        let [first, second, other] = [(); 3].map(|()| InstanceUid::default());
        for (uid, host) in [(first, "a"), (second, "a"), (other, "b")] {
            bindings.bind(uid, host, &end(100), 50, LIMITS, |_| true);
            bindings.commit(&uid);
        }
        // Later than the end kept for "a", and expired by the time of the report.
        let outcome = bindings.bind(first, "a", &end(120), 150, LIMITS, |_| true);
        assert!(matches!(outcome.bound, Bound::Refused(_)), "expired at 150");
        assert!(outcome.changes.is_empty(), "and moves no end");
        assert!(bindings
            .bind(InstanceUid::default(), "c", &end(100), 150, LIMITS, |_| {
                true
            })
            .changes
            .is_empty());

        assert!(bindings.end(100, |_| false).is_empty(), "not yet passed");
        let mut ended = bindings.end(101, |end| end.serial == "none");
        for (_, uids) in &mut ended {
            uids.sort_by_key(InstanceUid::to_string);
        }
        ended.sort_by(|x, y| x.0.cmp(&y.0));
        let mut a = vec![first, second];
        a.sort_by_key(InstanceUid::to_string);
        assert_eq!(
            ended,
            vec![("a".to_string(), a), ("b".to_string(), vec![other])]
        );
        assert!(bindings.is_empty() && bindings.live() == 0);

        bindings.bind(first, "c", &end(500), 200, LIMITS, |_| true);
        bindings.commit(&first);
        assert_eq!(bindings.host_of(&first), Some("c"), "bound anew");
        let revoked = bindings.end(200, |end| end.serial == "1");
        assert_eq!(revoked, vec![("c".to_string(), vec![first])]);
    }
}
