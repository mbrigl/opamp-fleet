//! What one instance holds of the Agents it carries on a shared store (ADR-0045 clause 8): the
//! take and the release of an Agent, the presence it writes, the re-keys whose
//! answer was lost, the restarts kept for retry, the take-up of what other instances announced, and
//! the per-Agent locks. Behind the same `AppState` entry points as before.

use super::*;

/// What a take-up acts on: the Agents a notice names, or every Agent, which a missed notification
/// calls for.
#[derive(Clone, Copy, Debug)]
pub enum TakeUp<'a> {
    /// The Agents a notice names.
    Named(&'a HashSet<InstanceUid>),
    /// Every Agent: a notification was missed.
    Everything,
}

impl<'a> TakeUp<'a> {
    /// The Agents named, or `None` for every Agent.
    pub(super) fn named(self) -> Option<&'a HashSet<InstanceUid>> {
        match self {
            TakeUp::Named(uids) => Some(uids),
            TakeUp::Everything => None,
        }
    }
}

/// How many locks the Agents' own locks are spread over: two Agents share one only by their
/// `instance_uid`, and then one waits for the other's report.
pub(super) const AGENT_LOCKS: usize = 1024;

/// What became of an Agent found gone from the store: dropped as forgotten, or kept, as its row
/// moved through a re-key of this instance's own whose answer was lost, or as that is not known.
#[derive(Debug)]
pub(super) enum Gone {
    Forgotten,
    Moved,
    Unknown(String),
}

/// What a take written before the fleet lock answered
/// ([`take_row`](AppState::take_row)).
pub(super) enum Took {
    /// None was made: no shared store, no lease, or no record.
    Nothing,
    /// Taken under `holder`, with the count of takes the store answered.
    Taken { holder: String, take: u64 },
    /// Made under `holder`, its answer lost.
    Lost { holder: String },
}

/// Which connection carries an Agent a WebSocket report arrives for, as the duplicate detection
/// sees it
/// (ADR-0045
/// clause 8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Carried {
    /// None, or the connection the report arrived on.
    Not,
    /// An older connection of the host the report's connection presented: the report's connection
    /// takes the Agent.
    ByOlder,
    /// A newer connection of that host: the report is taken up, and does not take the Agent.
    ByNewer,
    /// A newer connection of that host that carried no report of it for a heartbeat interval: the
    /// report's connection takes the Agent back.
    ByStale,
    /// A connection of another peer, or one where either presented no host: a duplicate.
    ByAnotherPeer,
}

impl Carried {
    /// Who carries `record` for a report on `conn`, which presented a certificate of `host`, at
    /// `now_ms`, a carrier that has carried no report of it for `interval` being stale.
    /// Connection ids grow, so the higher one is the newer connection. The host `record` names is
    /// the one it last arrived over, which the connection that carries it presented.
    pub(super) fn of(
        record: &AgentRecord,
        conn: ConnId,
        host: Option<&str>,
        now_ms: u64,
        interval: Duration,
    ) -> Self {
        match record.owner {
            Some(owner) if record.connected && owner != conn => {
                let interval_ms = u64::try_from(interval.as_millis()).unwrap_or(u64::MAX);
                if host.is_none() || record.host.as_deref() != host {
                    Carried::ByAnotherPeer
                } else if owner < conn {
                    Carried::ByOlder
                } else if now_ms.saturating_sub(record.carried_ms) >= interval_ms {
                    Carried::ByStale
                } else {
                    Carried::ByNewer
                }
            }
            _ => Carried::Not,
        }
    }
}

/// An Agent in [`AppState::taking`] while a report makes its take, until the report ends.
pub(super) struct Taking<'a> {
    taking: &'a Mutex<HashSet<InstanceUid>>,
    uid: InstanceUid,
}

impl<'a> Taking<'a> {
    pub(super) fn of(taking: &'a Mutex<HashSet<InstanceUid>>, uid: InstanceUid) -> Self {
        taking.lock().expect("taking lock").insert(uid);
        Taking { taking, uid }
    }
}

impl Drop for Taking<'_> {
    fn drop(&mut self) {
        self.taking.lock().expect("taking lock").remove(&self.uid);
    }
}

/// A report's end decided under the fleet lock.
pub(super) enum Settled {
    /// The reply, and what the record held, to persist before it goes out.
    Done(Box<(Processed, PersistedAgent)>),
    /// A queued restart goes out instead once its clear is written; the certificate issued goes
    /// with the reply where it does not.
    Restart(Option<TlsCertificate>),
}

/// A re-key of this instance whose answer was lost and whose read back failed: the identity it
/// moved the record to, if it committed, and whether a read since found it did.
#[derive(Clone, Copy, Debug)]
pub(super) struct PendingRekey {
    pub(super) new: InstanceUid,
    pub(super) moved: bool,
}

/// What became of a re-key of this instance whose answer was lost and whose read back failed, read
/// back again at the Agent's next report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UnsureRekey {
    /// None is pending for the identity, or the one that was did not move the record.
    None,
    /// It moved the record to this identity.
    Moved(InstanceUid),
    /// It is still not known.
    Unknown,
}

/// What the last presence batch wrote: the holder it named, each Agent's presence, and the count of
/// takes the store last answered for each Agent, which a disconnect is written only under and which
/// outlives the holder it was answered under. The Agents whose take's answer was lost are unsure:
/// their count is read before their disconnect is written, and the count they were answered before
/// is kept apart, for the shutdown flush alone. Every change of what is kept of an Agent moves its
/// generation, so a batch applies its answers only to what did not change meanwhile.
#[derive(Default)]
pub(super) struct PresenceWritten {
    pub(super) holder: Option<String>,
    pub(super) agents: HashMap<InstanceUid, Presence>,
    pub(super) takes: HashMap<InstanceUid, u64>,
    pub(super) unsure: HashSet<InstanceUid>,
    /// The count of takes last answered for an Agent whose take since lost its answer, or 0 where
    /// this instance created its record: still the count this instance was last answered, so the
    /// shutdown flush writes the record under it, where the row names no holder and no take was
    /// counted since (ADR-0030 clause 2). No batch names this instance by it. A count answered since
    /// in `takes` outranks it, so an answer leaves it as it is.
    pub(super) before_lost: HashMap<InstanceUid, u64>,
    pub(super) generations: HashMap<InstanceUid, u64>,
    pub(super) generation: u64,
}

impl PresenceWritten {
    /// Whether a take of `uid` was answered with a count this instance holds.
    pub(super) fn counted(&self, uid: &InstanceUid) -> bool {
        self.takes.contains_key(uid)
    }

    /// The count of takes each Agent is flushed under: the one last answered, or where that was
    /// lost, the one before it.
    pub(super) fn flushed_takes(&self) -> HashMap<InstanceUid, u64> {
        let mut takes = self.before_lost.clone();
        takes.extend(self.takes.iter().map(|(uid, take)| (*uid, *take)));
        takes
    }

    /// Starts over under `holder`, where it names another: every presence is written again, and
    /// the counts of takes are kept, since only the database raises them, under any holder.
    pub(super) fn under(&mut self, holder: &str) {
        if self.holder.as_deref() != Some(holder) {
            self.holder = Some(holder.to_string());
            self.agents.clear();
            self.unsure.clear();
        }
    }

    /// The generation of what is kept of `uid`.
    pub(super) fn generation(&self, uid: &InstanceUid) -> u64 {
        self.generations.get(uid).copied().unwrap_or(0)
    }

    /// Moves the generation of `uid`: what is kept of it changed.
    pub(super) fn changed(&mut self, uid: InstanceUid) {
        self.generation += 1;
        self.generations.insert(uid, self.generation);
    }

    /// Keeps `take` as the count of takes of `uid` a take answered, whatever was kept: the take
    /// holds the Agent's own lock, which every take of this instance holds too, so its answer is the
    /// latest, also where the record was forgotten and created again with a count that starts over;
    /// and the generation moves, so a batch in flight drops its answer.
    pub(super) fn taken(&mut self, uid: InstanceUid, presence: Presence, take: u64) {
        self.agents.insert(uid, presence);
        self.takes.insert(uid, take);
        self.unsure.remove(&uid);
        self.changed(uid);
    }

    /// Notes that the take of `uid` may have been written, though its answer was lost: no count is
    /// known for it any more, so a batch names this instance over a row no live instance holds
    /// whatever its count, and its disconnect waits for a count read back. The count answered
    /// before is kept for the shutdown flush.
    pub(super) fn lost(&mut self, uid: InstanceUid) {
        if let Some(take) = self.takes.remove(&uid) {
            self.before_lost.insert(uid, take);
        }
        self.unsure.insert(uid);
        self.changed(uid);
    }

    /// Forgets what was written of `uid`, which left what this instance holds.
    pub(super) fn remove(&mut self, uid: &InstanceUid) {
        self.agents.remove(uid);
        self.takes.remove(uid);
        self.unsure.remove(uid);
        self.before_lost.remove(uid);
        self.changed(*uid);
    }

    /// Moves what was written of `old` to `new`, as a re-key moves the stored row whole.
    pub(super) fn rekey(&mut self, old: &InstanceUid, new: InstanceUid) {
        if let Some(presence) = self.agents.remove(old) {
            self.agents.insert(new, presence);
        }
        if let Some(take) = self.takes.remove(old) {
            self.takes.insert(new, take);
        }
        if let Some(take) = self.before_lost.remove(old) {
            self.before_lost.insert(new, take);
        }
        if self.unsure.remove(old) {
            self.unsure.insert(new);
        }
        self.changed(*old);
        self.changed(new);
    }

    /// Takes up what a re-key of this instance's own whose answer was lost left in the row it
    /// moved to: the old row copied whole, which named this instance as connected, as nothing
    /// wrote the old row since the re-key took it. What a batch wrote of the old identity
    /// meanwhile found no row, so it is not what the new row holds. The entry makes the next
    /// batch write the new row: a disconnected Agent differs from it, so its disconnect is
    /// written there.
    pub(super) fn moved_connected(&mut self, new: InstanceUid) {
        self.agents.insert(
            new,
            Presence {
                connected: true,
                last_seen_ms: 0,
            },
        );
        self.changed(new);
    }

    /// Keeps only what concerns an Agent of `fleet`.
    pub(super) fn retain(&mut self, fleet: &HashMap<InstanceUid, AgentRecord>) {
        self.agents.retain(|uid, _| fleet.contains_key(uid));
        self.takes.retain(|uid, _| fleet.contains_key(uid));
        self.unsure.retain(|uid| fleet.contains_key(uid));
        self.before_lost.retain(|uid, _| fleet.contains_key(uid));
        self.generations.retain(|uid, _| fleet.contains_key(uid));
    }
}

impl AppState {
    /// Takes up what another instance wrote to the records of Agents this one holds
    /// (ADR-0045
    /// clauses 8 and 9): the record of each Agent of `uids` this instance holds, or of every one
    /// it holds where `uids` is `None`, is read again from the store. Its assignment, its labels and
    /// its queued restart are taken into what is held, its group is placed again for its labels,
    /// and one the store no longer holds, forgotten through another instance, is dropped. The
    /// WebSocket loops are woken, so a connected Agent is offered what it was released, or sent the
    /// restart requested, at once. The records are read before the fleet lock is taken, so no
    /// report waits for the read; one that differs from what is held is read again under the lock
    /// and taken up from that read, since an act of this instance made after the first read, which
    /// it never hears announced, wrote the store and what is held together under that lock. That
    /// second read is one read of every record that differs, so the lock is held for one round
    /// trip however many differ.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read; what is held is then kept.
    ///
    /// On a shared store an Agent of `uids` this instance does not hold may be the Client Agent of
    /// a Host whose Agents it holds, carried by another instance: those groups are placed again,
    /// so what they inherit follows its labels, or its forgetting.
    pub fn take_up_agents(&self, take_up: TakeUp<'_>) -> Result<(), String> {
        let uids = take_up.named();
        if self.agent_store.is_shared() {
            self.place_again_for(take_up);
        }
        let held: Vec<InstanceUid> = {
            let fleet = self.fleet.lock().expect("fleet lock");
            match uids {
                None => fleet.keys().copied().collect(),
                Some(uids) => uids
                    .iter()
                    .filter(|uid| fleet.contains_key(uid))
                    .copied()
                    .collect(),
            }
        };
        let stored: Vec<(InstanceUid, Option<PersistedAgent>)> = match uids {
            // The Agents held, in one read, and not the fleet.
            None => {
                let mut all = self.agent_store.get_many(&held)?;
                held.into_iter()
                    .map(|uid| (uid, all.remove(&uid)))
                    .collect()
            }
            Some(_) => held
                .into_iter()
                .map(|uid| Ok((uid, self.agent_store.get(&uid)?)))
                .collect::<Result<_, String>>()?,
        };
        let mut fleet = self.fleet.lock().expect("fleet lock");
        let differing: Vec<InstanceUid> = stored
            .into_iter()
            .filter(|(uid, persisted)| {
                fleet.get(uid).is_some_and(|record| {
                    persisted.as_ref().is_none_or(|persisted| {
                        record.assignment != persisted.assignment
                            || record.restart_pending != persisted.restart_pending
                            || record.labels != persisted.labels
                    })
                })
            })
            .map(|(uid, _)| uid)
            .collect();
        if differing.is_empty() {
            return Ok(());
        }
        let mut again = self.agent_store.get_many(&differing)?;
        let mut moved = 0usize;
        let mut unknown = None;
        for uid in differing {
            let Some(record) = fleet.get_mut(&uid) else {
                continue;
            };
            let Some(persisted) = again.remove(&uid) else {
                // A re-key's read back that fails is tried again with this read.
                if let Gone::Unknown(e) = self.drop_forgotten(&mut fleet, &uid) {
                    unknown = Some(e);
                }
                moved += 1;
                continue;
            };
            let mut changed = false;
            if record.assignment != persisted.assignment {
                record.assignment = persisted.assignment;
                changed = true;
            }
            if record.restart_pending != persisted.restart_pending {
                record.restart_pending = persisted.restart_pending;
                changed = true;
            }
            if changed {
                self.written.lock().expect("written lock").remove(&uid);
            }
            if record.labels != persisted.labels {
                self.take_labels(&mut fleet, &uid, persisted.labels);
                changed = true;
            }
            moved += usize::from(changed);
        }
        drop(fleet);
        if moved > 0 {
            self.push.send_modify(|rev| *rev += 1);
            debug!(
                agents = moved,
                "took up what another instance wrote to the agents' records"
            );
        }
        match unknown {
            Some(e) => Err(format!(
                "cannot read back a re-key whose answer was lost: {e}"
            )),
            None => Ok(()),
        }
    }

    /// Places again each group this instance holds members of whose Client Agent may be one of
    /// `uids` that it does not hold — the group's Client Agent now, or, of a Host, an Agent the
    /// host register binds to it — or every group where `uids` is `None`. Behind a Gateway a
    /// Client Agent another instance carries that the group was not placed under yet is found here
    /// only where `uids` is `None`; the notice of its arrival places the group again
    /// ([`place_hosts_again`](AppState::place_hosts_again)).
    pub(super) fn place_again_for(&self, take_up: TakeUp<'_>) {
        let uids = take_up.named();
        let Some(revocations) = &self.revocations else {
            return;
        };
        let mut fleet = self.fleet.lock().expect("fleet lock");
        // The notified Agents this instance does not hold; `None` for every one.
        let elsewhere: Option<Vec<InstanceUid>> = uids.map(|uids| {
            uids.iter()
                .filter(|uid| !fleet.contains_key(uid))
                .copied()
                .collect()
        });
        if elsewhere.as_ref().is_some_and(Vec::is_empty) {
            return;
        }
        let groups = self.groups.lock().expect("groups lock");
        let due: Vec<Group> = groups
            .iter()
            .filter(|(group, members)| {
                let Some(elsewhere) = &elsewhere else {
                    return true;
                };
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

    /// Re-keys the stored record of `old` to `new`. Where the store answers an error, the move may
    /// have committed all the same, so it is read back before the Agent is answered: moved, it
    /// stands. A read back that fails leaves the pair for the Agent's next report to read back
    /// (ADR-0045
    /// clause 10).
    pub(super) fn rekey_reading_back(
        &self,
        old: &InstanceUid,
        new: &InstanceUid,
        record: &PersistedAgent,
    ) -> Result<(), (String, bool)> {
        let Err(e) = self.agent_store.rekey(old, new, record) else {
            return Ok(());
        };
        match self.agent_store.moved(old, new) {
            Ok(true) => {
                info!(%old, %new, error = %e, "the re-key committed though its answer was lost");
                Ok(())
            }
            Ok(false) => Err((e, false)),
            Err(read) => {
                self.unsure_rekeys.lock().expect("rekeys lock").insert(
                    *old,
                    PendingRekey {
                        new: *new,
                        moved: false,
                    },
                );
                Err((
                    format!(
                        "{e}; it could not be read back either, so the Agent's next report reads \
                         it back: {read}"
                    ),
                    true,
                ))
            }
        }
    }

    /// Reads back the re-key of `old` whose answer was lost and whose read back failed, if one
    /// is pending: once the store answers, it is pending no more. One a notification's read found
    /// moved is answered moved without a read. The read runs before the fleet lock is taken, under
    /// the Agent's own lock, as the re-key does, and may wait for the share lock up to the
    /// database's `lock_timeout` within the call's bound.
    pub(super) fn read_back_unsure_rekey(&self, old: &InstanceUid) -> UnsureRekey {
        let pending = self
            .unsure_rekeys
            .lock()
            .expect("rekeys lock")
            .get(old)
            .copied();
        let Some(PendingRekey { new, moved }) = pending else {
            return UnsureRekey::None;
        };
        if moved {
            self.unsure_rekeys.lock().expect("rekeys lock").remove(old);
            return UnsureRekey::Moved(new);
        }
        match self.agent_store.moved(old, &new) {
            Ok(moved) => {
                self.unsure_rekeys.lock().expect("rekeys lock").remove(old);
                if moved {
                    info!(%old, %new, "a re-key whose answer was lost committed; assigning its identity");
                    UnsureRekey::Moved(new)
                } else {
                    UnsureRekey::None
                }
            }
            Err(e) => {
                warn!(agent = %old, error = %e, "cannot read back a re-key whose answer was lost");
                UnsureRekey::Unknown
            }
        }
    }

    /// Reads back, wherever `old` is found gone from the store, the re-key of it whose answer was
    /// lost and whose read back failed, if one is pending: the identity it moved to, and whether
    /// the store holds it moved. Made without the fleet lock where a report finds the row gone;
    /// [`apply_unsure_rekey`](Self::apply_unsure_rekey) takes it up.
    pub(super) fn read_unsure_rekey(
        &self,
        old: &InstanceUid,
    ) -> Option<(InstanceUid, Result<bool, String>)> {
        let pending = self
            .unsure_rekeys
            .lock()
            .expect("rekeys lock")
            .get(old)
            .copied();
        let PendingRekey { new, .. } = pending?;
        Some((new, self.agent_store.moved(old, &new)))
    }

    /// Takes up what [`read_unsure_rekey`](Self::read_unsure_rekey) read of `old`. Moved, what is
    /// held moves to the new identity, the Agent's session stays, and the new identity is assigned
    /// at the next report under `old`; a session that ended meanwhile has its disconnect written to
    /// the new row by the next presence batch, since what is held moves disconnected. Not known,
    /// `old` is kept.
    pub(super) fn apply_unsure_rekey(
        &self,
        fleet: &mut HashMap<InstanceUid, AgentRecord>,
        old: &InstanceUid,
        read: Option<(InstanceUid, Result<bool, String>)>,
        elsewhere: &mut Elsewhere,
    ) -> Gone {
        let Some((new, moved)) = read else {
            return Gone::Forgotten;
        };
        match moved {
            Ok(true) => {
                self.rekey_held(fleet, old, new, elsewhere);
                self.presence_written
                    .lock()
                    .expect("presence lock")
                    .moved_connected(new);
                self.unsure_rekeys
                    .lock()
                    .expect("rekeys lock")
                    .insert(*old, PendingRekey { new, moved: true });
                info!(%old, %new, "a re-key whose answer was lost committed; its identity is assigned at the next report");
                Gone::Moved
            }
            Ok(false) => {
                self.unsure_rekeys.lock().expect("rekeys lock").remove(old);
                Gone::Forgotten
            }
            Err(e) => {
                warn!(agent = %old, error = %e, "cannot read back a re-key whose answer was lost; keeping the agent");
                Gone::Unknown(e)
            }
        }
    }

    /// Moves what this instance holds of `old` to `new`, as a re-key moves the stored row: the
    /// labels go with it (ADR-0030 clause 21), the old identity leaves its group, and what was
    /// written of its presence moves with the row. A put-back of its restart kept for retry moves
    /// too, so the retry queues the restart on the row's new identity
    /// (ADR-0045
    /// clause 8).
    pub(super) fn rekey_held(
        &self,
        fleet: &mut HashMap<InstanceUid, AgentRecord>,
        old: &InstanceUid,
        new: InstanceUid,
        elsewhere: &mut Elsewhere,
    ) {
        if let Some(mut record) = fleet.remove(old) {
            let group = record.placement.group.take();
            fleet.insert(new, record);
            self.unplace(fleet, old, group, elsewhere);
            self.written.lock().expect("written lock").remove(old);
            self.presence_written
                .lock()
                .expect("presence lock")
                .rekey(old, new);
            let mut kept = self.unput_restarts.lock().expect("restarts lock");
            if let Some(backoff) = kept.remove(old) {
                kept.insert(new, backoff);
            }
        }
    }

    /// Drops every pending re-key of `uid`, from it or to it: what an operator's forget does.
    pub(super) fn drop_unsure_rekeys(&self, uid: &InstanceUid) {
        self.unsure_rekeys
            .lock()
            .expect("rekeys lock")
            .retain(|old, pending| old != uid && pending.new != *uid);
    }

    /// A message its connection did not take. A restart command among them is queued again, so
    /// the Agent's next exchange is sent it: on a WebSocket a restart is delivered at least once
    /// wherever the send is seen to fail. That holds on a lease that lapsed meanwhile too, since
    /// setting it again only restores what the Operator asked for. A put-back the store does not
    /// take is kept and tried again by [`retry_restarts`](Self::retry_restarts)
    /// (ADR-0045
    /// clause 8).
    pub fn unsent(&self, message: &ServerToAgent) {
        let restart = opamp::proto::CommandType::Restart as i32;
        if message.command.as_ref().map(|c| c.r#type) != Some(restart) {
            return;
        }
        let Some(uid) = InstanceUid::from_wire(&message.instance_uid) else {
            return;
        };
        self.put_back(&uid, false);
    }

    /// Tries again each put-back of a restart that is due, and answers how many are still kept.
    /// One that fails again waits twice as many calls, up to [`Backoff::MAX_WAIT`]; one that holds
    /// is dropped, and so is one whose Agent's row is gone, forgotten or re-keyed through another
    /// instance, with a warning naming the Agent. One whose row moved through a re-key of this
    /// instance's own not read back yet waits for the read back. The composition root calls this every second, so
    /// the intervals grow to a minute; a process that ends first loses the restarts still kept
    /// (ADR-0045
    /// clause 8).
    pub fn retry_restarts(&self) -> usize {
        for uid in self.due_restarts() {
            self.put_back(&uid, true);
        }
        self.unput_restarts.lock().expect("restarts lock").len()
    }

    /// Counts each kept put-back down one call, and answers those whose try is due.
    pub(super) fn due_restarts(&self) -> Vec<InstanceUid> {
        let mut kept = self.unput_restarts.lock().expect("restarts lock");
        kept.iter_mut()
            .filter_map(|(uid, backoff)| backoff.tick().then_some(*uid))
            .collect()
    }

    /// Sets the restart of `uid` queued again in the store and in the record this instance holds,
    /// and tells the sessions, under the Agent's own lock, which requests, deliveries and forgets
    /// take too. A `retry` is made only while its put-back is still kept, so one a request, a delivery
    /// or a forget dropped since it was found due is not tried; any put-back drops what is kept
    /// once the store answers.
    /// One the store does not take is kept, or waits longer, for
    /// [`retry_restarts`](Self::retry_restarts). An Agent forgotten meanwhile has nothing to
    /// restart; one whose record the store does not hold, forgotten or re-keyed through another
    /// instance, is logged and dropped, and one whose record a re-key of this instance's own whose
    /// answer was lost may have moved stays kept, for the read back to move it
    /// (ADR-0045
    /// clause 8).
    pub(super) fn put_back(&self, uid: &InstanceUid, retry: bool) {
        let _agent = self.agent_lock(uid);
        let mut fleet = self.fleet.lock().expect("fleet lock");
        if retry
            && !self
                .unput_restarts
                .lock()
                .expect("restarts lock")
                .contains_key(uid)
        {
            return;
        }
        let answered = self.agent_store.set_restart_pending(uid, true);
        // A re-key of this instance's own whose answer was lost may have moved the row: until its
        // read back moves what is held, and this put-back with it, the put-back stays kept.
        let unsure = matches!(answered, Ok(None))
            && self
                .unsure_rekeys
                .lock()
                .expect("rekeys lock")
                .get(uid)
                .is_some_and(|pending| !pending.moved);
        let mut kept = self.unput_restarts.lock().expect("restarts lock");
        match answered {
            Ok(None) if unsure => {
                let backoff = kept.entry(*uid).or_insert_with(Backoff::first);
                if retry {
                    backoff.failed();
                }
                info!(agent = %uid, "a re-key of the agent is not read back yet; its restart is tried again after it");
            }
            Ok(held) => {
                kept.remove(uid);
                drop(kept);
                if held.is_none() {
                    // Forgotten, or re-keyed through another instance meanwhile: not seen apart.
                    warn!(agent = %uid, "the restart is no longer tried, the Agent's row is gone");
                    return;
                }
                if let Some(record) = fleet.get_mut(uid) {
                    record.restart_pending = true;
                    self.written.lock().expect("written lock").remove(uid);
                }
                drop(fleet);
                self.push.send_modify(|rev| *rev += 1);
                info!(agent = %uid, "the restart command was not sent; it stays queued");
            }
            Err(e) => {
                let backoff = kept.entry(*uid).or_insert_with(Backoff::first);
                if retry {
                    backoff.failed();
                }
                warn!(agent = %uid, error = %e, "cannot queue an unsent restart again yet");
            }
        }
    }

    /// Takes up what another instance announced of the restarts of `uids`, every Agent where a
    /// notification was missed: a request there is this restart now, so a put-back still kept is
    /// dropped. After a missed notification only one whose record shows a restart queued is
    /// (ADR-0045
    /// clause 8).
    ///
    /// A request of an Agent held here, announced or found after a missed notification, is read
    /// under the Agent's own lock and held: a delivery clears a restart in the store under that
    /// lock before it takes the clear into memory, and a request committed between the two is
    /// otherwise taken out of memory with it.
    ///
    /// # Errors
    /// Returns an error when a record cannot be read; what is kept stays kept.
    pub fn take_up_restarts(&self, take_up: TakeUp<'_>) -> Result<(), String> {
        let uids = take_up.named();
        self.hold_restarts(uids)?;
        let _fleet = self.fleet.lock().expect("fleet lock");
        let mut kept = self.unput_restarts.lock().expect("restarts lock");
        match uids {
            Some(uids) => kept.retain(|uid, _| !uids.contains(uid)),
            None => {
                let named: Vec<InstanceUid> = kept.keys().copied().collect();
                for uid in named {
                    if self
                        .agent_store
                        .get(&uid)?
                        .is_some_and(|stored| stored.restart_pending)
                    {
                        kept.remove(&uid);
                    }
                }
            }
        }
        Ok(())
    }

    /// Holds the restart the store queues for each Agent of `uids` held here, every one where
    /// `None`, read under its own lock ([`take_up_restarts`](Self::take_up_restarts)).
    ///
    /// Where `uids` is `None`, the Agents held are read first without their locks, and only those
    /// whose record queues a restart are read again under theirs, so the take-up does not wait for
    /// every lock. A request is committed before the first read it races, so that read finds it
    /// queued, unless a delivery cleared it since, which sent it.
    fn hold_restarts(&self, uids: Option<&HashSet<InstanceUid>>) -> Result<(), String> {
        let held: Vec<InstanceUid> = {
            let fleet = self.fleet.lock().expect("fleet lock");
            match uids {
                None => fleet.keys().copied().collect(),
                Some(uids) => uids
                    .iter()
                    .filter(|uid| fleet.contains_key(uid))
                    .copied()
                    .collect(),
            }
        };
        let held: HashSet<InstanceUid> = match uids {
            Some(_) => held.into_iter().collect(),
            None if held.is_empty() => HashSet::new(),
            // One read of them all, without their locks.
            None => self
                .agent_store
                .get_many(&held)?
                .into_iter()
                .filter(|(_, stored)| stored.restart_pending)
                .map(|(uid, _)| uid)
                .collect(),
        };
        if held.is_empty() {
            return Ok(());
        }
        let _agents = self.agent_locks_of(Some(&held));
        // One read of them all, so their locks are held for one round trip.
        let queued: Vec<InstanceUid> = self
            .agent_store
            .get_many(&held.into_iter().collect::<Vec<_>>())?
            .into_iter()
            .filter(|(_, stored)| stored.restart_pending)
            .map(|(uid, _)| uid)
            .collect();
        let mut fleet = self.fleet.lock().expect("fleet lock");
        let mut moved = false;
        for uid in queued {
            if let Some(record) = fleet.get_mut(&uid).filter(|record| !record.restart_pending) {
                record.restart_pending = true;
                self.written.lock().expect("written lock").remove(&uid);
                moved = true;
            }
        }
        drop(fleet);
        if moved {
            self.push.send_modify(|rev| *rev += 1);
        }
        Ok(())
    }

    /// Drops the put-back of `uid`'s restart kept for retry, if one is: what a request, a delivery
    /// and a forget of the Agent do
    /// (ADR-0045
    /// clause 8).
    pub(super) fn drop_kept_restart(&self, uid: &InstanceUid) {
        self.unput_restarts
            .lock()
            .expect("restarts lock")
            .remove(uid);
    }

    /// Answers a report of an Agent whose record another instance forgot while this one wrote it:
    /// the Agent is dropped here too and its session ended, and the reply offers nothing
    /// (ADR-0030 clause 10).
    ///
    /// Made after the report's write found no row, so what it reads is read before the fleet lock
    /// is taken: the re-key read back, and the Client Agents of the group the Agent leaves.
    pub(super) fn forgotten_elsewhere(
        &self,
        uid: &InstanceUid,
        reported: &InstanceUid,
    ) -> Processed {
        let (held, group) = {
            let fleet = self.fleet.lock().expect("fleet lock");
            let record = fleet.get(uid);
            (
                record.is_some(),
                record.and_then(|record| record.placement.group.clone()),
            )
        };
        let unsure = if held {
            self.read_unsure_rekey(uid)
        } else {
            None
        };
        let mut elsewhere = self.elsewhere_of(group.as_slice());
        let gone = {
            let mut fleet = self.fleet.lock().expect("fleet lock");
            self.drop_forgotten_read(&mut fleet, uid, unsure, &mut elsewhere)
        };
        self.place_again(elsewhere);
        // Kept where its row moved through a re-key of this instance's own: the connection goes
        // on carrying it under the identity it reported.
        let kept = !matches!(gone, Gone::Forgotten);
        self.push.send_modify(|rev| *rev += 1);
        Processed {
            reply: ServerToAgent {
                instance_uid: reported.as_bytes().to_vec(),
                capabilities: self.capabilities(),
                ..Default::default()
            },
            uid: kept.then_some(*uid),
            disconnected: false,
        }
    }

    /// Drops an Agent another instance forgot from what this one holds, and ends its session. One
    /// whose row moved through a re-key of this instance's own whose answer was lost is read back
    /// first ([`read_unsure_rekey`](Self::read_unsure_rekey)), and kept where it moved or
    /// where that is not known. A re-key pending to it is dropped with it.
    pub(super) fn drop_forgotten(
        &self,
        fleet: &mut HashMap<InstanceUid, AgentRecord>,
        uid: &InstanceUid,
    ) -> Gone {
        if !fleet.contains_key(uid) {
            return Gone::Forgotten;
        }
        let read = self.read_unsure_rekey(uid);
        self.drop_forgotten_read(fleet, uid, read, &mut Elsewhere::Now)
    }

    /// [`drop_forgotten`](Self::drop_forgotten) from the re-key read back `unsure` read already.
    pub(super) fn drop_forgotten_read(
        &self,
        fleet: &mut HashMap<InstanceUid, AgentRecord>,
        uid: &InstanceUid,
        unsure: Option<(InstanceUid, Result<bool, String>)>,
        elsewhere: &mut Elsewhere,
    ) -> Gone {
        if !fleet.contains_key(uid) {
            return Gone::Forgotten;
        }
        match self.apply_unsure_rekey(fleet, uid, unsure, elsewhere) {
            Gone::Forgotten => {}
            kept => return kept,
        }
        let Some(record) = fleet.remove(uid) else {
            return Gone::Forgotten;
        };
        self.end_session(uid, &record);
        self.drop_kept_restart(uid);
        self.unsure_rekeys
            .lock()
            .expect("rekeys lock")
            .retain(|_, pending| pending.new != *uid);
        self.written.lock().expect("written lock").remove(uid);
        self.presence_written
            .lock()
            .expect("presence lock")
            .remove(uid);
        self.unplace(fleet, uid, record.placement.group.clone(), elsewhere);
        info!(agent = %uid, "dropped an agent forgotten through another instance");
        Gone::Forgotten
    }

    /// Writes the presence of the Agents this instance carries that moved since the last batch, in
    /// one batch naming `holder`, the instance id of the lease it serves under
    /// (ADR-0030 clause 4): a heartbeat reaches the store at
    /// most once per batch and Agent. An Agent it holds and never carried under `holder`, one
    /// restored at startup among them, is written only once it is connected here, so the first
    /// batch writes the Agents carried and not the fleet; under a new holder the connected ones
    /// are written again. An Agent whose take's answer was lost and that disconnected since has its
    /// holder and count of takes read first, and its disconnect is written under that count where
    /// the row names `holder` and the Agent was not taken here again since it was looked at
    /// (ADR-0045
    /// clause 8). Read under the fleet lock and written after it is released, so no report waits
    /// for the batch; its answers are kept only of the Agents nothing was kept of anew meanwhile. A
    /// batch that fails is written again with the next one. Answers how many it wrote.
    ///
    /// # Errors
    /// Returns an error when the batch cannot be written.
    pub fn write_presence(&self, holder: &str) -> Result<usize, String> {
        type Entry = (InstanceUid, Presence, Option<u64>);
        let (mut batch, mut generations, unsure): (Vec<Entry>, HashMap<InstanceUid, u64>, Vec<_>) = {
            let fleet = self.fleet.lock().expect("fleet lock");
            let mut written = self.presence_written.lock().expect("presence lock");
            written.under(holder);
            written.retain(&fleet);
            let mut batch = Vec::new();
            let mut unsure = Vec::new();
            for (uid, record) in fleet.iter() {
                let presence = record.presence();
                if !presence.connected && written.unsure.contains(uid) {
                    unsure.push((*uid, written.generation(uid)));
                    continue;
                }
                let moved = match written.agents.get(uid) {
                    Some(written) => *written != presence,
                    None => presence.connected,
                };
                if moved {
                    batch.push((*uid, presence, written.takes.get(uid).copied()));
                }
            }
            let generations = batch
                .iter()
                .map(|(uid, ..)| (*uid, written.generation(uid)))
                .collect();
            (batch, generations, unsure)
        };
        let mut read = HashSet::new();
        if !unsure.is_empty() {
            let uids: Vec<InstanceUid> = unsure.iter().map(|(uid, _)| *uid).collect();
            let stored = self.agent_store.takes(&uids)?;
            let fleet = self.fleet.lock().expect("fleet lock");
            let mut written = self.presence_written.lock().expect("presence lock");
            for (uid, generation) in unsure {
                let Some(presence) = fleet.get(&uid).map(AgentRecord::presence) else {
                    continue;
                };
                if written.generation(&uid) != generation || presence.connected {
                    continue;
                }
                match stored.get(&uid) {
                    Some((Some(stored), take)) if stored == holder => {
                        batch.push((uid, presence, Some(*take)));
                        generations.insert(uid, generation);
                        read.insert(uid);
                    }
                    // The row names another holder or none, or is gone: nothing of it is ours.
                    _ => {
                        written.unsure.remove(&uid);
                        written.agents.insert(uid, presence);
                        written.changed(uid);
                    }
                }
            }
        }
        if batch.is_empty() {
            return Ok(0);
        }
        batch.sort_by_key(|(uid, ..)| uid.0);
        let takes = self.agent_store.write_presence(holder, &batch)?;
        let mut written = self.presence_written.lock().expect("presence lock");
        // Under another holder since, the presence is written again anyway; the counts are kept,
        // since they outlive the holder they were answered under.
        let same_holder = written.holder.as_deref() == Some(holder);
        for (uid, presence, take) in &batch {
            if generations.get(uid) != Some(&written.generation(uid)) {
                continue;
            }
            if same_holder {
                written.agents.insert(*uid, *presence);
            }
            if let Some(answered) = takes.get(uid) {
                written.takes.insert(*uid, *answered);
                written.unsure.remove(uid);
            } else if read.contains(uid) {
                written.takes.insert(*uid, take.expect("a count read"));
                written.unsure.remove(uid);
            }
            written.changed(*uid);
        }
        Ok(batch.len())
    }

    /// On a shared store, takes `uid` at once as connected here, naming this instance as its
    /// holder, so that no other instance forgets it as not connected while the next batch is due
    /// (ADR-0030 clause 11), and so that an instance that
    /// carried it before stops carrying it
    /// (ADR-0045
    /// clause 8): one write per connect, never per heartbeat. A take that makes the Agent's holder
    /// change more than ten times within a minute is logged as a likely duplicate. Without a lease
    /// nothing is written. One that fails is logged, and the next batch writes it, where no other
    /// instance took the Agent meanwhile.
    ///
    /// Written before the fleet lock is taken, under the Agent's own lock, which
    /// [`release_taken`](Self::release_taken) takes too; its answer is taken up with the record
    /// ([`took`](Self::took)).
    pub(super) fn take_row(&self, uid: &InstanceUid, last_seen_ms: u64) -> Took {
        if !self.agent_store.is_shared() {
            return Took::Nothing;
        }
        let Some(holder) = self.lease.get().and_then(|lease| lease.borrow().clone()) else {
            return Took::Nothing;
        };
        match self.agent_store.take(uid, &holder, last_seen_ms) {
            Ok(Some(taken)) => {
                if let Some(from) = &taken.from {
                    debug!(agent = %uid, from = %from, "took the agent from another instance");
                }
                if taken.likely_duplicate {
                    warn!(
                        agent = %uid,
                        "the agent's holder changed more than ten times within a minute; \
                         likely a duplicate instance_uid on two connections"
                    );
                }
                Took::Taken {
                    holder,
                    take: taken.take,
                }
            }
            // Forgotten through another instance: this exchange's write finds no record, and
            // drops it.
            Ok(None) => Took::Nothing,
            Err(e) => {
                warn!(agent = %uid, error = %e, "cannot write that the agent is connected here");
                Took::Lost { holder }
            }
        }
    }

    /// Takes up what [`take_row`](Self::take_row) answered, with the presence the record holds
    /// once the report is taken into it. A take whose answer was lost may have been written: its
    /// disconnect is written only under the count read back then, and its record flushed at the
    /// shutdown only under the count answered before, 0 where `created` this record.
    pub(super) fn took(&self, uid: &InstanceUid, took: Took, presence: Presence, created: bool) {
        let mut written = self.presence_written.lock().expect("presence lock");
        match took {
            Took::Nothing => {}
            Took::Taken { holder, take } => {
                written.under(&holder);
                written.taken(*uid, presence, take);
            }
            Took::Lost { holder } => {
                written.under(&holder);
                written.lost(*uid);
                // The record this instance created counted no take then.
                if created {
                    written.before_lost.insert(*uid, 0);
                }
            }
        }
    }

    /// Stops carrying each Agent of `uids`, every one this instance carries where `None`, whose
    /// stored presence no longer names this instance as its holder: another instance took it
    /// (ADR-0045
    /// clause 8). It is held as on a closed connection, and its disconnect is not written, since
    /// the row is the other instance's. An Agent the row still names is kept, so of two instances
    /// that take one Agent at once only the one the row does not name stops carrying it. The
    /// holders are read under the own locks of the Agents this instance carries or is taking,
    /// which every take of this instance holds until its report holds the Agent in memory: a take
    /// made here before the read is seen, and one another instance's take overtook before this
    /// instance held the Agent is read once it is held. On a store that keeps no presence, or
    /// without a lease, nothing is read.
    ///
    /// # Errors
    /// Returns an error when the holders cannot be read; what is held is then kept.
    pub fn release_taken(&self, take_up: TakeUp<'_>) -> Result<(), String> {
        let uids = take_up.named();
        if !self.agent_store.is_shared() {
            return Ok(());
        }
        let Some(holder) = self.lease.get().and_then(|lease| lease.borrow().clone()) else {
            return Ok(());
        };
        let named = |uid: &InstanceUid| uids.is_none_or(|uids| uids.contains(uid));
        let carried = |fleet: &HashMap<InstanceUid, AgentRecord>| -> HashSet<InstanceUid> {
            fleet
                .iter()
                .filter(|(uid, record)| record.connected && named(uid))
                .map(|(uid, _)| *uid)
                .collect()
        };
        let mut concerned = carried(&self.fleet.lock().expect("fleet lock"));
        concerned.extend(
            self.taking
                .lock()
                .expect("taking lock")
                .iter()
                .filter(|uid| named(uid))
                .copied(),
        );
        if concerned.is_empty() {
            return Ok(());
        }
        let _agents = self.agent_locks_of(Some(&concerned));
        let held: Vec<InstanceUid> = {
            let fleet = self.fleet.lock().expect("fleet lock");
            let carried = carried(&fleet);
            concerned
                .into_iter()
                .filter(|uid| carried.contains(uid))
                .collect()
        };
        if held.is_empty() {
            return Ok(());
        }
        let taken: Vec<InstanceUid> = self
            .agent_store
            .holders(&held)?
            .into_iter()
            .filter(|(_, stored)| stored.as_deref() != Some(holder.as_str()))
            .map(|(uid, _)| uid)
            .collect();
        if taken.is_empty() {
            return Ok(());
        }
        let mut fleet = self.fleet.lock().expect("fleet lock");
        let mut written = self.presence_written.lock().expect("presence lock");
        for uid in taken {
            let Some(record) = fleet.get_mut(&uid).filter(|record| record.connected) else {
                continue;
            };
            record.connected = false;
            record.owner = None;
            if written.holder.as_deref() == Some(holder.as_str()) {
                written.agents.insert(uid, record.presence());
            }
            written.unsure.remove(&uid);
            written.changed(uid);
            debug!(agent = %uid, "another instance took the agent; this one no longer carries it");
        }
        Ok(())
    }

    /// The Agents forgotten while connection `conn` carried them, taken once: its loop ends their
    /// session (ADR-0030 clause 10).
    pub fn take_forgotten(&self, conn: ConnId) -> Vec<InstanceUid> {
        self.forgotten
            .lock()
            .expect("forgotten lock")
            .remove(&conn)
            .unwrap_or_default()
    }

    /// The index of `uid`'s own lock among [`AGENT_LOCKS`].
    pub(super) fn agent_lock_index(uid: &InstanceUid) -> usize {
        use std::hash::{Hash as _, Hasher as _};
        let mut hasher = std::hash::DefaultHasher::new();
        uid.0.hash(&mut hasher);
        (hasher.finish() % AGENT_LOCKS as u64) as usize
    }

    /// Takes the lock at `index` among the Agents' own locks, counting a wait.
    pub(super) fn agent_lock_at(&self, index: usize) -> std::sync::MutexGuard<'_, ()> {
        let lock = &self.agent_locks[index];
        match lock.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                self.agent_lock_waits.fetch_add(1, Ordering::SeqCst);
                lock.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
            }
        }
    }

    /// Takes `uid`'s own lock. Never taken while the fleet lock is held.
    pub(super) fn agent_lock(&self, uid: &InstanceUid) -> std::sync::MutexGuard<'_, ()> {
        self.agent_lock_at(Self::agent_lock_index(uid))
    }

    /// Takes the own locks of the Agents of `uids`, every one where `None`, in the order of
    /// their index.
    pub(super) fn agent_locks_of(
        &self,
        uids: Option<&HashSet<InstanceUid>>,
    ) -> Vec<std::sync::MutexGuard<'_, ()>> {
        let indexes: BTreeSet<usize> = match uids {
            Some(uids) => uids.iter().map(Self::agent_lock_index).collect(),
            None => (0..AGENT_LOCKS).collect(),
        };
        indexes
            .into_iter()
            .map(|index| self.agent_lock_at(index))
            .collect()
    }

    /// How often an Agent's own lock was found taken and waited for.
    pub fn agent_lock_waits(&self) -> u64 {
        self.agent_lock_waits.load(Ordering::SeqCst)
    }
}
