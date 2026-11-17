//! What one instance holds of the Agents it carries on a shared store (ADR-0025 clause 8): the
//! take and the release of an Agent, the presence it writes, the re-keys whose
//! answer was lost, the restarts kept for retry, the take-up of what other instances announced, and
//! the per-Agent locks. Behind the same `AppState` entry points as before.

use super::*;

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
/// (ADR-0025
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
    /// counted since (ADR-0029 clause 2). No batch names this instance by it. A count answered since
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

    /// Whether anything written of `uid` is kept. The generation is not: it outlives the Agent
    /// until the next batch, so a batch in flight drops its answer for it.
    #[cfg(test)]
    fn keeps(&self, uid: &InstanceUid) -> bool {
        let PresenceWritten {
            holder: _,
            agents,
            takes,
            unsure,
            before_lost,
            generations: _,
            generation: _,
        } = self;
        agents.contains_key(uid)
            || takes.contains_key(uid)
            || unsure.contains(uid)
            || before_lost.contains_key(uid)
    }
}

/// What an instance keeps of each Agent beside its record and its group: one entry of four parts,
/// each keyed by the Agent's `instance_uid`. A forget drops it ([`Kept::forget`]) and a re-key
/// moves it ([`Kept::rekey`]) as one, each naming every part, so a part added later does not
/// compile until both say what becomes of it. Each part keeps its own lock, every one taken
/// strictly after `fleet`.
pub(super) struct Kept {
    /// Each persisted record's durable digest as last written — the dirty check that keeps a
    /// heartbeat from reaching any storage backend (ADR-0029).
    pub(super) written: Mutex<HashMap<InstanceUid, [u8; 32]>>,
    /// The presence of each held Agent as the last batch wrote it, and the holder it named, so a
    /// batch writes only what moved since.
    pub(super) presence_written: Mutex<PresenceWritten>,
    /// The re-keys whose answer was lost and whose read back failed, by the old identity: the new
    /// one each moved to, if it committed, which the Agent's next report reads back and assigns.
    pub(super) unsure_rekeys: Mutex<HashMap<InstanceUid, PendingRekey>>,
    /// The restarts whose command was not sent and whose put-back the store did not take, each
    /// with when it is tried again ([`retry_restarts`](AppState::retry_restarts)).
    pub(super) unput_restarts: Mutex<HashMap<InstanceUid, Backoff>>,
}

impl Kept {
    /// Starts with `written`, the digests of the records restored, and nothing else.
    pub(super) fn with_written(written: HashMap<InstanceUid, [u8; 32]>) -> Self {
        Kept {
            written: Mutex::new(written),
            presence_written: Mutex::new(PresenceWritten::default()),
            unsure_rekeys: Mutex::new(HashMap::new()),
            unput_restarts: Mutex::new(HashMap::new()),
        }
    }

    /// Drops everything kept of `uid`, which left what this instance holds. That includes each
    /// pending re-key from `uid` and each pending re-key to `uid`.
    pub(super) fn forget(&self, uid: &InstanceUid) {
        let Kept {
            written,
            presence_written,
            unsure_rekeys,
            unput_restarts,
        } = self;
        written.lock().expect("written lock").remove(uid);
        presence_written.lock().expect("presence lock").remove(uid);
        unsure_rekeys
            .lock()
            .expect("rekeys lock")
            .retain(|old, pending| old != uid && pending.new != *uid);
        unput_restarts.lock().expect("restarts lock").remove(uid);
    }

    /// Moves what is kept of `old` to `new`, as a re-key moves the stored row whole: what was
    /// written of its presence and a restart kept for retry move, so the retry queues the restart
    /// on the new identity (ADR-0025 clause 8). The digest does not: the record is written whole
    /// under `new` at the report's end. A pending re-key of `old` is the caller's, which knows
    /// whether this re-key is the one it names: the report that made it drops it, and the read
    /// back that found it moved marks it so.
    pub(super) fn rekey(&self, old: &InstanceUid, new: InstanceUid) {
        let Kept {
            written,
            presence_written,
            unsure_rekeys: _,
            unput_restarts,
        } = self;
        written.lock().expect("written lock").remove(old);
        presence_written
            .lock()
            .expect("presence lock")
            .rekey(old, new);
        let mut kept = unput_restarts.lock().expect("restarts lock");
        if let Some(backoff) = kept.remove(old) {
            kept.insert(new, backoff);
        }
    }

    /// Whether anything is kept of `uid`.
    #[cfg(test)]
    pub(super) fn keeps(&self, uid: &InstanceUid) -> bool {
        !self.keeping(uid).is_empty()
    }

    /// The names of the parts that keep something of `uid`, so a test can show its setup reached
    /// each part before it checks that none is left.
    #[cfg(test)]
    pub(super) fn keeping(&self, uid: &InstanceUid) -> Vec<&'static str> {
        let Kept {
            written,
            presence_written,
            unsure_rekeys,
            unput_restarts,
        } = self;
        [
            (
                "written",
                written.lock().expect("written lock").contains_key(uid),
            ),
            (
                "presence_written",
                presence_written.lock().expect("presence lock").keeps(uid),
            ),
            (
                "unsure_rekeys",
                unsure_rekeys
                    .lock()
                    .expect("rekeys lock")
                    .iter()
                    .any(|(old, pending)| old == uid || pending.new == *uid),
            ),
            (
                "unput_restarts",
                unput_restarts
                    .lock()
                    .expect("restarts lock")
                    .contains_key(uid),
            ),
        ]
        .into_iter()
        .filter_map(|(part, keeps)| keeps.then_some(part))
        .collect()
    }
}

impl AppState {
    /// Takes up what another instance wrote to the records of Agents this one holds
    /// (ADR-0025
    /// clauses 8 and 9): the record of each Agent of `uids` this instance holds is read again from
    /// the store. Its assignment, its labels and
    /// its queued restart are taken into what is held, its group is placed again for its labels,
    /// and one the store no longer holds, forgotten through another instance, is dropped. The
    /// WebSocket loops are woken, so a connected Agent is offered what it was released, or sent the
    /// restart requested, at once. The records are read before the fleet lock is taken, in one
    /// read of all of them, so no report waits for the read and a take-up of many Agents costs one
    /// round trip, not one per Agent; one that differs from what is held is read again under the
    /// lock and taken up from that read, since an act of this instance made after the first read,
    /// which it never hears announced, wrote the store and what is held together under that lock.
    /// That second read is one read of every record that differs, so the lock is held for one
    /// round trip however many differ.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read; what is held is then kept.
    ///
    /// On a shared store an Agent of `uids` this instance does not hold may be the Client Agent of
    /// a Host whose Agents it holds, carried by another instance: those groups are placed again,
    /// so what they inherit follows its labels, or its forgetting.
    pub fn take_up_agents(&self, uids: &HashSet<InstanceUid>) -> Result<(), String> {
        if self.agent_store.is_shared() {
            self.place_again_for(uids);
        }
        let held: Vec<InstanceUid> = {
            let fleet = self.fleet.lock().expect("fleet lock");
            uids.iter()
                .filter(|uid| fleet.contains_key(uid))
                .copied()
                .collect()
        };
        if held.is_empty() {
            return Ok(());
        }
        let stored = self.agent_store.get_many(&held)?;
        let mut fleet = self.fleet.lock().expect("fleet lock");
        let differing: Vec<InstanceUid> = held
            .into_iter()
            .filter(|uid| {
                fleet.get(uid).is_some_and(|record| {
                    stored.get(uid).is_none_or(|persisted| {
                        record.assignment != persisted.assignment
                            || record.restart_pending != persisted.restart_pending
                            || record.labels != persisted.labels
                    })
                })
            })
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
                self.kept.written.lock().expect("written lock").remove(&uid);
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

    /// Catches up after a missed notification, which counts as every notification
    /// (ADR-0025
    /// clause 10): what other instances changed of the Agents this instance holds is taken up from
    /// one read of their records and one of their holders, whatever kinds of state it keeps of
    /// them. From these two reads:
    ///
    /// - each record's assignment, queued restart and labels are taken into what is held, and an
    ///   Agent whose record the store no longer holds, forgotten through another instance, is
    ///   dropped, or kept where a re-key of this instance's own whose answer was lost moved it
    ///   ([`drop_forgotten`](Self::drop_forgotten));
    /// - a put-back of a restart kept for retry is dropped where its record shows a restart queued;
    /// - an Agent carried here whose row names another holder is carried no more
    ///   ([`release_taken`](Self::release_taken));
    /// - on a shared store every group is placed again (clause 8): the Client Agents of a Host's
    ///   group that the host register binds to it are among the records read, and those behind a
    ///   Gateway are read through the Agents that arrived over its Host, once per Gateway, before
    ///   the fleet lock is taken. A group that came to be meanwhile is read and placed once the
    ///   locks are let go.
    ///
    /// The holders are read under the own locks of the Agents held or being taken, which every
    /// take of this instance holds. The records are read under those locks and the fleet lock: a
    /// delivery clears a restart in the store under the Agent's own lock before it takes the clear
    /// into memory, and an act of this instance, which it never hears announced, writes the store
    /// and what is held together under the fleet lock. The fleet lock thus waits for one round
    /// trip, however many Agents this instance holds. The WebSocket loops are woken where anything
    /// moved, so a connected Agent is offered what it was released, or sent the restart requested.
    ///
    /// # Errors
    /// Returns an error when the store cannot be read, and then keeps what is held. Returns one
    /// too when a re-key whose answer was lost cannot be read back, once everything else is taken
    /// up; that Agent is then kept under its old identity.
    pub fn catch_up(&self) -> Result<(), String> {
        let shared = self.agent_store.is_shared();
        let holder = if shared {
            self.lease.get().and_then(|lease| lease.borrow().clone())
        } else {
            None
        };
        let mut concerned: HashSet<InstanceUid> = self
            .fleet
            .lock()
            .expect("fleet lock")
            .keys()
            .copied()
            .collect();
        if holder.is_some() {
            concerned.extend(self.taking.lock().expect("taking lock").iter().copied());
        }
        let agents = self.agent_locks_of(&concerned);
        let holders = match &holder {
            Some(_) => {
                let carried: Vec<InstanceUid> = {
                    let fleet = self.fleet.lock().expect("fleet lock");
                    concerned
                        .iter()
                        .filter(|uid| fleet.get(uid).is_some_and(|record| record.connected))
                        .copied()
                        .collect()
                };
                if carried.is_empty() {
                    HashMap::new()
                } else {
                    self.agent_store.holders(&carried)?
                }
            }
            None => HashMap::new(),
        };
        // The Client Agents behind each Gateway, read before the fleet lock, as a report's
        // placement reads them, and sifted of what is held as each group is placed.
        let behind = if shared {
            self.read_behind_gateways()
        } else {
            HashMap::new()
        };

        let mut fleet = self.fleet.lock().expect("fleet lock");
        // Only the Agents whose own locks are held: one that arrived since was read at its report.
        let held: Vec<InstanceUid> = concerned
            .into_iter()
            .filter(|uid| fleet.contains_key(uid))
            .collect();
        let bound = if shared {
            self.bound_to_hosts()
        } else {
            HashMap::new()
        };
        let mut wanted: HashSet<InstanceUid> = held.iter().copied().collect();
        wanted.extend(
            self.kept
                .unput_restarts
                .lock()
                .expect("restarts lock")
                .keys()
                .copied(),
        );
        wanted.extend(bound.values().flatten().copied());
        let mut stored = if wanted.is_empty() {
            HashMap::new()
        } else {
            self.agent_store
                .get_many(&wanted.into_iter().collect::<Vec<_>>())?
        };

        // What every group's Client Agents held elsewhere are, sifted of what is held as each group
        // is placed. A Gateway's group not read before the lock is placed once it is let go.
        let mut elsewhere = placement::elsewhere_from(behind, &bound, &stored);

        let mut moved = 0usize;
        self.kept
            .unput_restarts
            .lock()
            .expect("restarts lock")
            .retain(|uid, _| !stored.get(uid).is_some_and(|record| record.restart_pending));
        let mut unknown = None;
        for uid in held {
            let Some(persisted) = stored.remove(&uid) else {
                let unsure = self.read_unsure_rekey(&uid);
                if let Gone::Unknown(e) =
                    self.drop_forgotten_read(&mut fleet, &uid, unsure, &mut elsewhere)
                {
                    unknown = Some(e);
                }
                moved += 1;
                continue;
            };
            let Some(record) = fleet.get_mut(&uid) else {
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
            if record.labels != persisted.labels {
                record.labels = persisted.labels;
                changed = true;
            }
            if changed {
                self.kept.written.lock().expect("written lock").remove(&uid);
                moved += 1;
            }
        }
        if let Some(holder) = &holder {
            let taken = holders
                .into_iter()
                .filter(|(_, stored)| stored.as_deref() != Some(holder.as_str()))
                .map(|(uid, _)| uid);
            self.stop_carrying(&mut fleet, holder, taken);
        }
        if shared {
            self.place_every_group(&mut fleet, &mut elsewhere);
        }
        drop(fleet);
        drop(agents);
        // A group the reads did not cover, one that came to be meanwhile, is read and placed again.
        self.place_again(elsewhere);
        if moved > 0 {
            self.push.send_modify(|rev| *rev += 1);
            debug!(
                agents = moved,
                "caught up with what other instances wrote to the agents' records"
            );
        }
        match unknown {
            Some(e) => Err(format!(
                "cannot read back a re-key whose answer was lost: {e}"
            )),
            None => Ok(()),
        }
    }

    /// Whether this instance holds anything of `uid`: its record, a place in a group, or anything
    /// [`Kept`] of it. What waits for a connection's loop to end its session is not counted, nor
    /// the presence's generation, each of which outlives the Agent on purpose.
    #[cfg(test)]
    pub(super) fn holds_anything_of(&self, uid: &InstanceUid) -> bool {
        let fleet = self.fleet.lock().expect("fleet lock");
        fleet.contains_key(uid) || self.groups.hold(uid) || self.kept.keeps(uid)
    }

    /// Re-keys the stored record of `old` to `new`. Where the store answers an error, the move may
    /// have committed all the same, so it is read back before the Agent is answered: moved, it
    /// stands. A read back that fails leaves the pair for the Agent's next report to read back
    /// (ADR-0025
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
                self.kept.unsure_rekeys.lock().expect("rekeys lock").insert(
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
            .kept
            .unsure_rekeys
            .lock()
            .expect("rekeys lock")
            .get(old)
            .copied();
        let Some(PendingRekey { new, moved }) = pending else {
            return UnsureRekey::None;
        };
        if moved {
            self.kept
                .unsure_rekeys
                .lock()
                .expect("rekeys lock")
                .remove(old);
            return UnsureRekey::Moved(new);
        }
        match self.agent_store.moved(old, &new) {
            Ok(moved) => {
                self.kept
                    .unsure_rekeys
                    .lock()
                    .expect("rekeys lock")
                    .remove(old);
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
            .kept
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
                self.kept
                    .presence_written
                    .lock()
                    .expect("presence lock")
                    .moved_connected(new);
                self.kept
                    .unsure_rekeys
                    .lock()
                    .expect("rekeys lock")
                    .insert(*old, PendingRekey { new, moved: true });
                info!(%old, %new, "a re-key whose answer was lost committed; its identity is assigned at the next report");
                Gone::Moved
            }
            Ok(false) => {
                self.kept
                    .unsure_rekeys
                    .lock()
                    .expect("rekeys lock")
                    .remove(old);
                Gone::Forgotten
            }
            Err(e) => {
                warn!(agent = %old, error = %e, "cannot read back a re-key whose answer was lost; keeping the agent");
                Gone::Unknown(e)
            }
        }
    }

    /// Moves what this instance holds of `old` to `new`, as a re-key moves the stored row: the
    /// labels go with it (ADR-0029 clause 21), the old identity leaves its group, and what was
    /// written of its presence moves with the row. A put-back of its restart kept for retry moves
    /// too, so the retry queues the restart on the row's new identity
    /// (ADR-0025
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
            self.kept.rekey(old, new);
        }
    }

    /// A message its connection did not take. A restart command among them is queued again, so
    /// the Agent's next exchange is sent it: on a WebSocket a restart is delivered at least once
    /// wherever the send is seen to fail. That holds on a lease that lapsed meanwhile too, since
    /// setting it again only restores what the Operator asked for. A put-back the store does not
    /// take is kept and tried again by [`retry_restarts`](Self::retry_restarts)
    /// (ADR-0025
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
    /// (ADR-0025
    /// clause 8).
    pub fn retry_restarts(&self) -> usize {
        for uid in self.due_restarts() {
            self.put_back(&uid, true);
        }
        self.kept
            .unput_restarts
            .lock()
            .expect("restarts lock")
            .len()
    }

    /// Counts each kept put-back down one call, and answers those whose try is due.
    pub(super) fn due_restarts(&self) -> Vec<InstanceUid> {
        let mut kept = self.kept.unput_restarts.lock().expect("restarts lock");
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
    /// (ADR-0025
    /// clause 8).
    pub(super) fn put_back(&self, uid: &InstanceUid, retry: bool) {
        let _agent = self.agent_lock(uid);
        let mut fleet = self.fleet.lock().expect("fleet lock");
        if retry
            && !self
                .kept
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
                .kept
                .unsure_rekeys
                .lock()
                .expect("rekeys lock")
                .get(uid)
                .is_some_and(|pending| !pending.moved);
        let mut kept = self.kept.unput_restarts.lock().expect("restarts lock");
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
                    self.kept.written.lock().expect("written lock").remove(uid);
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

    /// Takes up what another instance announced of the restarts of `uids`: a request there is this
    /// restart now, so a put-back still kept is dropped
    /// (ADR-0025
    /// clause 8).
    ///
    /// A request of an Agent held here is read under the Agent's own lock and held: a delivery
    /// clears a restart in the store under that lock before it takes the clear into memory, and a
    /// request committed between the two is otherwise taken out of memory with it. The records are
    /// read in one read, so the locks are held for one round trip.
    ///
    /// # Errors
    /// Returns an error when a record cannot be read; what is kept stays kept.
    pub fn take_up_restarts(&self, uids: &HashSet<InstanceUid>) -> Result<(), String> {
        let held: HashSet<InstanceUid> = {
            let fleet = self.fleet.lock().expect("fleet lock");
            uids.iter()
                .filter(|uid| fleet.contains_key(uid))
                .copied()
                .collect()
        };
        if !held.is_empty() {
            let _agents = self.agent_locks_of(&held);
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
                    self.kept.written.lock().expect("written lock").remove(&uid);
                    moved = true;
                }
            }
            drop(fleet);
            if moved {
                self.push.send_modify(|rev| *rev += 1);
            }
        }
        self.drop_kept_restarts(uids);
        Ok(())
    }

    /// Drops the put-backs kept for retry of the Agents of `uids`, whose restart another instance
    /// requested: that request is the restart now, whether or not its record still queues it
    /// (ADR-0025
    /// clause 8).
    pub(crate) fn drop_kept_restarts(&self, uids: &HashSet<InstanceUid>) {
        let _fleet = self.fleet.lock().expect("fleet lock");
        self.kept
            .unput_restarts
            .lock()
            .expect("restarts lock")
            .retain(|uid, _| !uids.contains(uid));
    }

    /// Drops the put-back of `uid`'s restart kept for retry, if one is: what a request, a delivery
    /// and a forget of the Agent do
    /// (ADR-0025
    /// clause 8).
    pub(super) fn drop_kept_restart(&self, uid: &InstanceUid) {
        self.kept
            .unput_restarts
            .lock()
            .expect("restarts lock")
            .remove(uid);
    }

    /// Answers a report of an Agent whose record another instance forgot while this one wrote it:
    /// the Agent is dropped here too and its session ended, and the reply offers nothing
    /// (ADR-0029 clause 10).
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
    /// where that is not known. A dropped Agent takes everything kept of it along
    /// ([`Kept::forget`]), each pending re-key from it or to it included.
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
        self.kept.forget(uid);
        self.unplace(fleet, uid, record.placement.group.clone(), elsewhere);
        info!(agent = %uid, "dropped an agent forgotten through another instance");
        Gone::Forgotten
    }

    /// Writes the presence of the Agents this instance carries that moved since the last batch, in
    /// one batch naming `holder`, the instance id of the lease it serves under
    /// (ADR-0029 clause 4): a heartbeat reaches the store at
    /// most once per batch and Agent. An Agent it holds and never carried under `holder`, one
    /// restored at startup among them, is written only once it is connected here, so the first
    /// batch writes the Agents carried and not the fleet; under a new holder the connected ones
    /// are written again. An Agent whose take's answer was lost and that disconnected since has its
    /// holder and count of takes read first, and its disconnect is written under that count where
    /// the row names `holder` and the Agent was not taken here again since it was looked at
    /// (ADR-0025
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
            let mut written = self.kept.presence_written.lock().expect("presence lock");
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
            let mut written = self.kept.presence_written.lock().expect("presence lock");
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
        let mut written = self.kept.presence_written.lock().expect("presence lock");
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
    /// (ADR-0029 clause 11), and so that an instance that
    /// carried it before stops carrying it
    /// (ADR-0025
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
        let mut written = self.kept.presence_written.lock().expect("presence lock");
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

    /// Stops carrying each Agent of `uids` this instance carries whose
    /// stored presence no longer names this instance as its holder: another instance took it
    /// (ADR-0025
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
    pub fn release_taken(&self, uids: &HashSet<InstanceUid>) -> Result<(), String> {
        if !self.agent_store.is_shared() {
            return Ok(());
        }
        let Some(holder) = self.lease.get().and_then(|lease| lease.borrow().clone()) else {
            return Ok(());
        };
        let named = |uid: &InstanceUid| uids.contains(uid);
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
        let _agents = self.agent_locks_of(&concerned);
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
        self.stop_carrying(&mut fleet, &holder, taken);
        Ok(())
    }

    /// Stops carrying each Agent of `taken` still connected here, whose row names another holder
    /// than `holder`: it is held as on a closed connection, and its disconnect is not written.
    fn stop_carrying(
        &self,
        fleet: &mut HashMap<InstanceUid, AgentRecord>,
        holder: &str,
        taken: impl IntoIterator<Item = InstanceUid>,
    ) {
        let mut written = self.kept.presence_written.lock().expect("presence lock");
        for uid in taken {
            let Some(record) = fleet.get_mut(&uid).filter(|record| record.connected) else {
                continue;
            };
            record.connected = false;
            record.owner = None;
            if written.holder.as_deref() == Some(holder) {
                written.agents.insert(uid, record.presence());
            }
            written.unsure.remove(&uid);
            written.changed(uid);
            debug!(agent = %uid, "another instance took the agent; this one no longer carries it");
        }
    }

    /// The Agents forgotten while connection `conn` carried them, taken once: its loop ends their
    /// session (ADR-0029 clause 10).
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

    /// Takes the own locks of the Agents of `uids`, in the order of their index.
    pub(super) fn agent_locks_of(
        &self,
        uids: &HashSet<InstanceUid>,
    ) -> Vec<std::sync::MutexGuard<'_, ()>> {
        let indexes: BTreeSet<usize> = uids.iter().map(Self::agent_lock_index).collect();
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

/// When a kept put-back of a restart is tried again, counted in calls of
/// [`AppState::retry_restarts`].
#[derive(Debug, Clone, Copy)]
pub(super) struct Backoff {
    /// Calls until the next try.
    left: u32,
    /// Calls the next failure waits.
    wait: u32,
}

impl Backoff {
    /// The longest wait between two tries: a minute, at one call a second.
    const MAX_WAIT: u32 = 60;

    /// After the first failure: tried again at the next call.
    fn first() -> Self {
        Backoff { left: 1, wait: 2 }
    }

    /// Counts one call down, and answers whether a try is due.
    fn tick(&mut self) -> bool {
        self.left = self.left.saturating_sub(1);
        self.left == 0
    }

    /// The try failed: the next waits twice as long, up to the bound.
    fn failed(&mut self) {
        self.left = self.wait;
        self.wait = (self.wait * 2).min(Self::MAX_WAIT);
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::fleet::fixtures::*;

    /// A restart whose command was not sent is queued again, on a lease that lapsed meanwhile
    /// too, and the Agent's next connection is sent it, once; a message that carries no restart
    /// leaves nothing queued. Run on each store.
    fn a_restart_whose_send_failed_is_queued_again(state: &AppState, stored: &dyn AgentStore) {
        let uid = InstanceUid([0x30; 16]);
        let restartable = |sequence_num| {
            let mut msg = report(&uid, sequence_num);
            msg.capabilities |= opamp::proto::AgentCapabilities::AcceptsRestartCommand as u64;
            msg
        };
        state.process(restartable(1), Transport::WebSocket, Some(1));
        assert!(state.request_restart(&uid).is_ok(), "queued");
        let command = state.restart_command_for(&uid, 1).expect("sent");
        assert!(
            !stored
                .get(&uid)
                .expect("get")
                .expect("stored")
                .restart_pending
        );

        state.unsent(&ServerToAgent {
            instance_uid: uid.as_bytes().to_vec(),
            ..Default::default()
        });
        assert!(
            !stored
                .get(&uid)
                .expect("get")
                .expect("stored")
                .restart_pending,
            "a message without the command queues nothing"
        );

        let mut pushes = state.subscribe();
        pushes.mark_unchanged();
        state.answer_while(|| false);
        state.unsent(&command);
        state.answer_while(|| true);
        assert!(
            pushes.has_changed().expect("pushes"),
            "a session that carries the Agent now is told"
        );
        state.mark_disconnected(&[uid], 1);
        assert!(
            stored
                .get(&uid)
                .expect("get")
                .expect("stored")
                .restart_pending,
            "queued again"
        );

        let reply = state
            .process(restartable(2), Transport::WebSocket, Some(2))
            .reply;
        assert_eq!(
            reply.command.map(|c| c.r#type),
            Some(opamp::proto::CommandType::Restart as i32),
            "the next connection is sent it"
        );
        let reply = state
            .process(restartable(3), Transport::WebSocket, Some(2))
            .reply;
        assert!(reply.command.is_none(), "once");
        assert!(
            !stored
                .get(&uid)
                .expect("get")
                .expect("stored")
                .restart_pending
        );
    }

    /// Verifies: ADR-0025
    #[test]
    fn a_restart_whose_send_failed_is_queued_again_on_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = AppState::new(dir.path().to_path_buf()).expect("state");
        let stored = crate::fs::FsAgentStore::open(dir.path()).expect("open");
        a_restart_whose_send_failed_is_queued_again(&state, &stored);
    }

    /// Verifies: ADR-0025
    #[test]
    fn a_restart_whose_send_failed_is_queued_again_on_postgres() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let state = on_postgres(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::open(database.clone()).expect("open")),
        );
        let stored = crate::postgres::agents::PgAgentStore::new(database.clone());
        a_restart_whose_send_failed_is_queued_again(&state, &stored);
    }

    /// An instance whose put-backs of restarts fail until `failing` is cleared, on files, with the
    /// store it writes and a second handle on that store, as another instance writes it.
    fn with_failing_restarts() -> (
        AppState,
        Arc<std::sync::atomic::AtomicBool>,
        crate::fs::FsAgentStore,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let hooked = Hooked::new(Box::new(
            crate::fs::FsAgentStore::open(dir.path()).expect("open"),
        ));
        let failing = hooked.failing_restarts.clone();
        let state = AppState::with_stores(
            Box::new(hooked),
            Box::new(crate::fs::FsConfigBackend::open(dir.path()).expect("configs")),
            Box::new(crate::clock::SystemClock),
        )
        .expect("state");
        let stored = crate::fs::FsAgentStore::open(dir.path()).expect("open");
        (state, failing, stored, dir)
    }

    /// A restart taken whose send failed and whose put-back the store refused: kept for retry.
    fn kept_put_back(state: &AppState, failing: &std::sync::atomic::AtomicBool, uid: &InstanceUid) {
        state.process(restartable(uid, 1), Transport::WebSocket, Some(1));
        assert!(state.request_restart(uid).is_ok(), "queued");
        let command = state.restart_command_for(uid, 1).expect("sent");
        failing.store(true, Ordering::SeqCst);
        state.unsent(&command);
        failing.store(false, Ordering::SeqCst);
    }

    /// A kept put-back is one restart with a request made meanwhile, and with a delivery of a
    /// restart another instance queued meanwhile: once that one is delivered, retrying queues no
    /// second.
    /// Verifies: ADR-0025
    #[test]
    fn a_kept_put_back_is_dropped_by_a_request_or_a_delivery_meanwhile() {
        let (state, failing, stored, _dir) = with_failing_restarts();
        let uid = InstanceUid([0x33; 16]);
        kept_put_back(&state, &failing, &uid);
        assert!(state.request_restart(&uid).is_ok(), "requested again");
        assert!(state.restart_command_for(&uid, 1).is_some(), "delivered");
        assert_eq!(state.retry_restarts(), 0, "nothing is kept");
        assert!(
            state.restart_command_for(&uid, 1).is_none(),
            "no second restart"
        );

        kept_put_back(&state, &failing, &uid);
        assert_eq!(
            stored.set_restart_pending(&uid, true),
            Ok(Some(false)),
            "requested through another instance"
        );
        state.catch_up().expect("taken up");
        assert!(state.restart_command_for(&uid, 1).is_some(), "delivered");
        assert_eq!(state.retry_restarts(), 0, "nothing is kept");
        assert!(
            state.restart_command_for(&uid, 1).is_none(),
            "no second restart"
        );

        // Requested here and delivered through another instance.
        kept_put_back(&state, &failing, &uid);
        assert!(state.request_restart(&uid).is_ok(), "requested again");
        assert_eq!(
            stored.set_restart_pending(&uid, false),
            Ok(Some(true)),
            "delivered through another instance"
        );
        assert_eq!(state.retry_restarts(), 0, "nothing is kept");
        assert!(
            !stored
                .get(&uid)
                .expect("get")
                .expect("stored")
                .restart_pending,
            "no second restart"
        );
    }

    /// A kept put-back goes with its Agent's forget, through this instance or one heard of from
    /// another: an Agent that comes back under the same identity is not restarted for it.
    /// Verifies: ADR-0025
    #[test]
    fn a_kept_put_back_goes_with_its_agents_forget() {
        let (state, failing, stored, _dir) = with_failing_restarts();
        let (here, elsewhere) = (InstanceUid([0x34; 16]), InstanceUid([0x35; 16]));
        kept_put_back(&state, &failing, &here);
        kept_put_back(&state, &failing, &elsewhere);
        state.mark_disconnected(&[here, elsewhere], 1);
        assert!(state.forget_agent(&here).is_ok(), "forgotten here");
        stored.remove(&elsewhere).expect("forgotten elsewhere");
        state.catch_up().expect("taken up");
        for (uid, conn) in [(here, 2), (elsewhere, 3)] {
            state.process(restartable(&uid, 1), Transport::WebSocket, Some(conn));
        }
        assert_eq!(state.retry_restarts(), 0, "nothing is kept");
        for uid in [here, elsewhere] {
            assert!(
                !stored
                    .get(&uid)
                    .expect("get")
                    .expect("stored")
                    .restart_pending,
                "the Agent that came back is not restarted"
            );
        }
    }

    /// A request announced by name in one read with a missed notification drops its kept
    /// put-back though the record no longer queues it, delivered through the other instance
    /// meanwhile: the catch-up keeps such a put-back, and the named request still drops it, so
    /// the Agent is not restarted twice.
    /// Verifies: ADR-0025
    #[test]
    fn a_request_announced_with_a_missed_notification_drops_a_put_back_its_delivery_left() {
        use crate::changed::Changed;
        use crate::postgres::notify::{Heard, Notice};
        let (state, failing, stored, _dir) = with_failing_restarts();
        let uid = InstanceUid([0x3a; 16]);
        kept_put_back(&state, &failing, &uid);
        assert_eq!(stored.set_restart_pending(&uid, true), Ok(Some(false)));
        assert_eq!(
            stored.set_restart_pending(&uid, false),
            Ok(Some(true)),
            "requested and delivered through another instance"
        );

        // A take that names no Agent has every holder read again: a catch-up.
        crate::instance::take_up_held_for(
            &state,
            &[
                Heard::Notified(Notice::new(Changed::Taken, "")),
                Heard::Notified(Notice::new(Changed::Restart, &uid.to_string())),
            ],
        )
        .expect("taken up");
        assert_eq!(state.retry_restarts(), 0, "nothing is kept");
        assert_eq!(
            queued(&stored, &uid),
            Some(false),
            "no second restart is queued"
        );
    }

    /// A try found due is not made once a request and its delivery dropped the put-back before
    /// the try took the fleet lock, so the Agent is not restarted a second time.
    /// Verifies: ADR-0025
    #[test]
    fn a_try_found_due_is_not_made_once_a_request_and_its_delivery_dropped_it() {
        let (state, failing, stored, _dir) = with_failing_restarts();
        let uid = InstanceUid([0x36; 16]);
        kept_put_back(&state, &failing, &uid);
        let due = state.due_restarts();
        assert_eq!(due, [uid], "due at the next call");
        // Between finding it due and trying it: requested and delivered.
        assert!(state.request_restart(&uid).is_ok(), "requested again");
        assert!(state.restart_command_for(&uid, 1).is_some(), "delivered");
        for uid in &due {
            state.put_back(uid, true);
        }
        assert!(
            !stored
                .get(&uid)
                .expect("get")
                .expect("stored")
                .restart_pending,
            "no second restart"
        );
        assert_eq!(state.retry_restarts(), 0, "nothing is kept");
    }

    /// A request through another instance, taken up from its announcement, drops a put-back kept
    /// here, so its delivery there is the one restart; after a missed notification one whose
    /// record shows a restart queued is dropped, and one whose record shows none is kept.
    /// Verifies: ADR-0025
    #[test]
    fn a_request_announced_by_another_instance_drops_a_kept_put_back() {
        let (state, failing, stored, _dir) = with_failing_restarts();
        let (announced, queued, other) = (
            InstanceUid([0x37; 16]),
            InstanceUid([0x38; 16]),
            InstanceUid([0x39; 16]),
        );
        for uid in [&announced, &queued, &other] {
            kept_put_back(&state, &failing, uid);
        }
        assert_eq!(
            stored.set_restart_pending(&announced, true),
            Ok(Some(false))
        );
        state
            .take_up_restarts(&HashSet::from([announced]))
            .expect("taken up");
        assert_eq!(
            stored.set_restart_pending(&announced, false),
            Ok(Some(true)),
            "delivered there"
        );

        assert_eq!(stored.set_restart_pending(&queued, true), Ok(Some(false)));
        state.catch_up().expect("taken up");
        assert_eq!(
            stored.set_restart_pending(&queued, false),
            Ok(Some(true)),
            "delivered there"
        );

        let mut kept = usize::MAX;
        for _ in 0..4 {
            kept = state.retry_restarts();
        }
        assert_eq!(kept, 0, "every put-back held or was dropped");
        for uid in [announced, queued] {
            assert!(
                !stored
                    .get(&uid)
                    .expect("get")
                    .expect("stored")
                    .restart_pending,
                "no second restart"
            );
        }
        assert!(
            stored
                .get(&other)
                .expect("get")
                .expect("stored")
                .restart_pending,
            "the put-back nothing announced is queued again"
        );
    }

    /// On PostgreSQL the announcement of a request through another instance is what drops a
    /// put-back kept here: once it is taken up, the request's delivery there leaves nothing for a
    /// retry here to restart again.
    /// Verifies: ADR-0025
    #[test]
    fn a_request_announced_by_another_instance_drops_a_kept_put_back_on_postgres() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.shared();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let failing = hooked.failing_restarts.clone();
        let a = Arc::new(on_postgres(&database, Box::new(hooked)));
        a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let following = {
            let _entered = runtime.enter();
            crate::instance::follow(&database, a.clone(), None)
        };
        until("the first read once listening", || following.reads() > 0);
        let uid = InstanceUid([0x3a; 16]);
        kept_put_back(&a, &failing, &uid);
        let kept = || a.kept.unput_restarts.lock().expect("restarts lock").len();
        assert_eq!(kept(), 1, "kept for retry");

        let other = crate::postgres::agents::PgAgentStore::new(test.shared());
        assert_eq!(
            other.set_restart_pending(&uid, true),
            Ok(Some(false)),
            "requested through another instance"
        );
        until("the request taken up", || kept() == 0);
        assert_eq!(
            other.set_restart_pending(&uid, false),
            Ok(Some(true)),
            "delivered there"
        );
        assert_eq!(a.retry_restarts(), 0, "nothing is kept");
        assert_eq!(
            other.get(&uid).map(|r| r.map(|r| r.restart_pending)),
            Ok(Some(false)),
            "no second restart"
        );
    }

    /// A put-back the store does not take is kept and tried again, at intervals that double, until
    /// it holds; the Agent's next connection is then sent the restart. One whose Agent is
    /// forgotten meanwhile is dropped.
    /// Verifies: ADR-0025
    #[test]
    fn a_put_back_the_store_refuses_is_tried_again_until_it_holds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let hooked = Hooked::new(Box::new(
            crate::fs::FsAgentStore::open(dir.path()).expect("open"),
        ));
        let (failing, set) = (hooked.failing_restarts.clone(), hooked.restarts_set.clone());
        let state = AppState::with_stores(
            Box::new(hooked),
            Box::new(crate::fs::FsConfigBackend::open(dir.path()).expect("configs")),
            Box::new(crate::clock::SystemClock),
        )
        .expect("state");
        let stored = crate::fs::FsAgentStore::open(dir.path()).expect("open");
        let (uid, gone) = (InstanceUid([0x31; 16]), InstanceUid([0x32; 16]));
        let restartable = |uid: &InstanceUid, sequence_num| {
            let mut msg = report(uid, sequence_num);
            msg.capabilities |= opamp::proto::AgentCapabilities::AcceptsRestartCommand as u64;
            msg
        };
        for uid in [&uid, &gone] {
            state.process(restartable(uid, 1), Transport::WebSocket, Some(1));
            assert!(state.request_restart(uid).is_ok(), "queued");
        }
        let command = state.restart_command_for(&uid, 1).expect("sent");
        let other = state.restart_command_for(&gone, 1).expect("sent");
        let tries = || set.load(Ordering::SeqCst);
        let base = tries();

        failing.store(true, Ordering::SeqCst);
        state.unsent(&command);
        state.unsent(&other);
        assert_eq!(tries(), base + 2, "each put back once");
        assert!(
            !stored
                .get(&uid)
                .expect("get")
                .expect("stored")
                .restart_pending
        );
        // Tried at the next call, then after two calls, then after four.
        let mut seen = Vec::new();
        for _ in 0..7 {
            state.retry_restarts();
            seen.push(tries() - base);
        }
        assert_eq!(seen, [4, 4, 6, 6, 6, 6, 8], "the intervals double");

        state.mark_disconnected(&[uid, gone], 1);
        assert!(state.forget_agent(&gone).is_ok(), "forgotten");
        failing.store(false, Ordering::SeqCst);
        let mut kept = usize::MAX;
        for _ in 0..8 {
            kept = state.retry_restarts();
            if kept == 0 {
                break;
            }
        }
        assert_eq!(kept, 0, "it held, and the forgotten Agent's was dropped");
        assert!(
            stored
                .get(&uid)
                .expect("get")
                .expect("stored")
                .restart_pending,
            "queued again"
        );
        let reply = state
            .process(restartable(&uid, 2), Transport::WebSocket, Some(2))
            .reply;
        assert_eq!(
            reply.command.map(|c| c.r#type),
            Some(opamp::proto::CommandType::Restart as i32),
            "the next connection is sent it"
        );
    }

    /// What an instance keeps of an Agent moves whole with a re-key on the same instance: a
    /// restart kept for retry and what was written of its presence go to the new identity, and
    /// nothing of them stays with the old. The digest goes with neither, so the record is written
    /// whole under the new identity, and a pending re-key of the old one is left to the caller,
    /// which knows whether this re-key is the one it names.
    /// Verifies: ADR-0025
    #[test]
    fn what_is_kept_of_an_agent_moves_whole_with_a_rekey() {
        let (old, new, pending) = (
            InstanceUid([0x51; 16]),
            InstanceUid([0x52; 16]),
            InstanceUid([0x53; 16]),
        );
        let kept = Kept::with_written(HashMap::from([(old, [7; 32])]));
        let presence = Presence {
            connected: true,
            last_seen_ms: 5,
        };
        kept.presence_written
            .lock()
            .expect("presence lock")
            .taken(old, presence, 3);
        kept.unput_restarts
            .lock()
            .expect("restarts lock")
            .insert(old, Backoff::first());
        kept.unsure_rekeys.lock().expect("rekeys lock").insert(
            old,
            PendingRekey {
                new: pending,
                moved: false,
            },
        );

        kept.rekey(&old, new);

        let restarts = kept.unput_restarts.lock().expect("restarts lock");
        assert!(restarts.contains_key(&new), "the kept restart moved");
        assert!(!restarts.contains_key(&old));
        drop(restarts);
        let written = kept.presence_written.lock().expect("presence lock");
        assert_eq!(
            written.agents.get(&new),
            Some(&presence),
            "the presence moved"
        );
        assert_eq!(
            written.takes.get(&new),
            Some(&3),
            "the count of takes moved"
        );
        assert!(!written.agents.contains_key(&old) && !written.takes.contains_key(&old));
        drop(written);
        let digests = kept.written.lock().expect("written lock");
        assert!(digests.is_empty(), "written whole under the new identity");
        drop(digests);
        assert_eq!(
            kept.unsure_rekeys
                .lock()
                .expect("rekeys lock")
                .get(&old)
                .map(|pending| pending.new),
            Some(pending),
            "the caller's to resolve"
        );
    }

    /// A re-key an Agent asks for on the instance that holds it leaves nothing of it under the old
    /// identity, on a shared store what was written of its presence included. The restart kept for
    /// retry, tried again, is queued on the new identity.
    fn a_rekey_on_the_same_instance_leaves_nothing_under_the_old_identity(
        state: &AppState,
        failing: &std::sync::atomic::AtomicBool,
        stored: &dyn AgentStore,
    ) {
        let old = InstanceUid([0x55; 16]);
        kept_put_back(state, failing, &old);
        let mut rekey = restartable(&old, 2);
        rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
        let new = state
            .process(rekey, Transport::WebSocket, Some(1))
            .uid
            .expect("carried");
        assert_ne!(new, old, "re-keyed");
        assert!(
            !state.holds_anything_of(&old),
            "nothing is left under the old identity"
        );
        assert_eq!(
            state.retry_restarts(),
            0,
            "the kept restart was tried and held"
        );
        assert_eq!(queued(stored, &new), Some(true), "on the new identity");
    }

    /// Verifies: ADR-0025
    #[test]
    fn a_rekey_on_the_same_instance_leaves_nothing_under_the_old_identity_on_files() {
        let (state, failing, stored, _dir) = with_failing_restarts();
        a_rekey_on_the_same_instance_leaves_nothing_under_the_old_identity(
            &state, &failing, &stored,
        );
    }

    /// Verifies: ADR-0025
    #[test]
    fn a_rekey_on_the_same_instance_leaves_nothing_under_the_old_identity_on_postgres() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let failing = hooked.failing_restarts.clone();
        let state = on_postgres(&database, Box::new(hooked));
        state.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let stored = crate::postgres::agents::PgAgentStore::new(database.clone());
        a_rekey_on_the_same_instance_leaves_nothing_under_the_old_identity(
            &state, &failing, &stored,
        );
    }

    /// A forgotten Agent leaves nothing of it in what the instance holds, whatever was kept of it.
    /// Before the forget it has a restart kept for retry, a re-key that failed and whose read back
    /// failed too, so not known to have moved, the digest of its record, which a report written
    /// since left, and on a shared store its presence written. Each part kept of it is shown to
    /// keep something, so a part the setup misses fails here. `hooks` are the put-backs that fail,
    /// the re-keys that fail and the read backs that fail of the [`Hooked`] store under `state`.
    fn a_forgotten_agent_leaves_nothing_behind(
        state: &AppState,
        hooks: [&std::sync::atomic::AtomicBool; 3],
        shared: bool,
    ) {
        let [failing, refused, unreadable] = hooks;
        let uid = InstanceUid([0x54; 16]);
        kept_put_back(state, failing, &uid);
        refused.store(true, Ordering::SeqCst);
        unreadable.store(true, Ordering::SeqCst);
        let mut rekey = restartable(&uid, 2);
        rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
        let answered = state.process(rekey, Transport::WebSocket, Some(1));
        refused.store(false, Ordering::SeqCst);
        assert_eq!(answered.uid, Some(uid), "not known: it keeps its identity");
        state.process(restartable(&uid, 3), Transport::WebSocket, Some(1));
        unreadable.store(false, Ordering::SeqCst);
        state.mark_disconnected(&[uid], 1);
        let mut parts = vec!["written", "unsure_rekeys", "unput_restarts"];
        if shared {
            parts.insert(1, "presence_written");
        }
        assert_eq!(state.kept.keeping(&uid), parts, "each part keeps something");
        assert!(state.forget_agent(&uid).is_ok(), "forgotten");
        assert!(
            !state.holds_anything_of(&uid),
            "nothing of the Agent is left"
        );
    }

    /// Verifies: ADR-0025
    #[test]
    fn a_forgotten_agent_leaves_nothing_behind_on_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let hooked = Hooked::new(Box::new(
            crate::fs::FsAgentStore::open(dir.path()).expect("open"),
        ));
        let hooks = (
            hooked.failing_restarts.clone(),
            hooked.failing_rekeys.clone(),
            hooked.failing_moved.clone(),
        );
        let state = AppState::with_stores(
            Box::new(hooked),
            Box::new(crate::fs::FsConfigBackend::open(dir.path()).expect("configs")),
            Box::new(crate::clock::SystemClock),
        )
        .expect("state");
        a_forgotten_agent_leaves_nothing_behind(&state, [&hooks.0, &hooks.1, &hooks.2], false);
    }

    /// Verifies: ADR-0025
    #[test]
    fn a_forgotten_agent_leaves_nothing_behind_on_postgres() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let hooks = (
            hooked.failing_restarts.clone(),
            hooked.failing_rekeys.clone(),
            hooked.failing_moved.clone(),
        );
        let state = on_postgres(&database, Box::new(hooked));
        state.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        a_forgotten_agent_leaves_nothing_behind(&state, [&hooks.0, &hooks.1, &hooks.2], true);
    }

    /// A kept put-back follows the re-key its Agent asks for: the retry queues the restart on the
    /// new identity, and the Agent's next report under it is sent the restart. Run on each store.
    fn a_kept_put_back_follows_a_rekey_the_agent_asks_for(
        state: &AppState,
        failing: &std::sync::atomic::AtomicBool,
        stored: &dyn AgentStore,
    ) {
        let uid = InstanceUid([0x3b; 16]);
        kept_put_back(state, failing, &uid);
        let mut rekey = restartable(&uid, 2);
        rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
        let new = state
            .process(rekey, Transport::WebSocket, Some(1))
            .uid
            .expect("carried");
        assert_ne!(new, uid, "re-keyed");
        assert_eq!(state.retry_restarts(), 0, "the retry held");
        assert_eq!(queued(stored, &uid), None, "the old row is gone");
        assert_eq!(
            queued(stored, &new),
            Some(true),
            "queued on the new identity"
        );
        let reply = state
            .process(restartable(&new, 3), Transport::WebSocket, Some(1))
            .reply;
        assert_eq!(
            reply.command.map(|c| c.r#type),
            Some(opamp::proto::CommandType::Restart as i32),
            "the next report is sent it"
        );
    }

    /// Verifies: ADR-0025
    #[test]
    fn a_kept_put_back_follows_a_rekey_the_agent_asks_for_on_files() {
        let (state, failing, stored, _dir) = with_failing_restarts();
        a_kept_put_back_follows_a_rekey_the_agent_asks_for(&state, &failing, &stored);
    }

    /// Verifies: ADR-0025
    #[test]
    fn a_kept_put_back_follows_a_rekey_the_agent_asks_for_on_postgres() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let failing = hooked.failing_restarts.clone();
        let state = on_postgres(&database, Box::new(hooked));
        state.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let stored = crate::postgres::agents::PgAgentStore::new(database.clone());
        a_kept_put_back_follows_a_rekey_the_agent_asks_for(&state, &failing, &stored);
    }

    /// A kept put-back follows a re-key whose answer was lost and whose read back failed, once a
    /// read again of what is held reads it back as moved: the retry queues the restart on the new
    /// identity.
    /// Verifies: ADR-0025
    #[test]
    fn a_kept_put_back_follows_a_rekey_read_back_as_moved() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        a_kept_put_back_follows_a_rekey_read_back(&test, false);
    }

    /// A try of a kept put-back that finds the row gone while a re-key of this instance's own is
    /// not read back yet keeps it, and the read back moves it: the restart is queued on the new
    /// identity, not dropped.
    /// Verifies: ADR-0025
    #[test]
    fn a_kept_put_back_tried_before_its_rekey_is_read_back_follows_it() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        a_kept_put_back_follows_a_rekey_read_back(&test, true);
    }

    /// A kept put-back and a re-key of its Agent whose answer was lost and whose read back failed;
    /// the store answers again, a try is made before the read back where `tried_first`, and the
    /// put-back is tried until it holds.
    fn a_kept_put_back_follows_a_rekey_read_back(
        test: &crate::postgres::testing::TestDatabase,
        tried_first: bool,
    ) {
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let (failing, losing, unreadable) = (
            hooked.failing_restarts.clone(),
            hooked.losing_rekeys.clone(),
            hooked.failing_moved.clone(),
        );
        let a = on_postgres(&database, Box::new(hooked));
        a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let stored = crate::postgres::agents::PgAgentStore::new(database.clone());
        let uid = InstanceUid([0x3c; 16]);
        kept_put_back(&a, &failing, &uid);
        losing.store(true, Ordering::SeqCst);
        unreadable.store(true, Ordering::SeqCst);
        let mut rekey = restartable(&uid, 2);
        rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
        let answered = a.process(rekey, Transport::WebSocket, Some(1));
        losing.store(false, Ordering::SeqCst);
        assert_eq!(answered.uid, Some(uid), "not known: it keeps its identity");
        unreadable.store(false, Ordering::SeqCst);
        if tried_first {
            assert_eq!(
                a.retry_restarts(),
                1,
                "kept while the re-key is not read back"
            );
        }
        a.catch_up().expect("read again");
        let held = held_by(&a);
        assert_eq!(held.len(), 1);
        let new = InstanceUid::parse(&held[0]).expect("a uid");
        assert_ne!(new, uid, "read back as moved");
        let kept = (0..8).map(|_| a.retry_restarts()).find(|kept| *kept == 0);
        assert_eq!(kept, Some(0), "the retry held");
        assert_eq!(
            queued(&stored, &new),
            Some(true),
            "queued on the new identity"
        );
    }

    /// A put-back whose Agent was re-keyed through another instance while this one retried finds
    /// its record gone: the restart is not tried again, and a warning names the Agent.
    /// Verifies: ADR-0025
    #[test]
    fn a_kept_put_back_whose_row_another_instance_rekeyed_is_logged() {
        crate::test_support::logged();
        let (state, failing, stored, _dir) = with_failing_restarts();
        let uid = InstanceUid([0x3e; 16]);
        kept_put_back(&state, &failing, &uid);
        state.mark_disconnected(&[uid], 1);
        let new = InstanceUid([0x3f; 16]);
        let record = stored.get(&uid).expect("get").expect("stored");
        stored
            .rekey(&uid, &new, &record)
            .expect("re-keyed through another instance");
        assert_eq!(state.retry_restarts(), 0, "not tried again");
        assert_eq!(queued(&stored, &new), Some(false), "not seen");
        assert_eq!(
            crate::test_support::lines_with(&[
                "the restart is no longer tried, the Agent's row is gone",
                &uid.to_string(),
            ])
            .len(),
            1,
            "a warning names the Agent"
        );
    }

    /// The re-key of a duplicate newcomer moves no row: a put-back kept for the Agent stays with
    /// it, the retry queues the restart on its identity, and the newcomer is sent none.
    /// Verifies: ADR-0025
    #[test]
    fn a_kept_put_back_stays_with_its_agent_when_a_duplicate_is_rekeyed() {
        let (state, failing, stored, _dir) = with_failing_restarts();
        let uid = InstanceUid([0x3d; 16]);
        kept_put_back(&state, &failing, &uid);
        let duplicate = state.process(restartable(&uid, 1), Transport::WebSocket, Some(2));
        let newcomer = duplicate.uid.expect("carried");
        assert_ne!(newcomer, uid, "the newcomer is re-keyed");
        assert!(duplicate.reply.agent_identification.is_some());
        assert!(duplicate.reply.command.is_none());
        assert_eq!(state.retry_restarts(), 0, "the retry held");
        assert_eq!(queued(&stored, &uid), Some(true), "queued on the Agent");
        assert_ne!(
            queued(&stored, &newcomer),
            Some(true),
            "not on the newcomer"
        );
        let reply = state
            .process(restartable(&newcomer, 2), Transport::WebSocket, Some(2))
            .reply;
        assert!(reply.command.is_none(), "the newcomer is sent no restart");
        let reply = state
            .process(restartable(&uid, 2), Transport::WebSocket, Some(1))
            .reply;
        assert_eq!(
            reply.command.map(|c| c.r#type),
            Some(opamp::proto::CommandType::Restart as i32),
            "the Agent is sent it"
        );
    }

    /// A store that counts what reaches it: the record writes, and each presence batch as it is
    /// written.
    struct Counted {
        inner: crate::fs::FsAgentStore,
        puts: Arc<AtomicU64>,
        batches: Arc<Mutex<Vec<Batch>>>,
    }

    /// One presence batch as it reached the store.
    type Batch = Vec<(InstanceUid, Presence, Option<u64>)>;

    impl AgentStore for Counted {
        fn load(&self) -> Result<HashMap<InstanceUid, PersistedAgent>, String> {
            self.inner.load()
        }
        fn put(&self, uid: &InstanceUid, record: &PersistedAgent) -> Result<(), String> {
            self.puts.fetch_add(1, Ordering::SeqCst);
            self.inner.put(uid, record)
        }
        fn set_labels(
            &self,
            uid: &InstanceUid,
            labels: &BTreeMap<String, String>,
        ) -> Result<bool, String> {
            self.inner.set_labels(uid, labels)
        }
        fn set_restart_pending(
            &self,
            uid: &InstanceUid,
            pending: bool,
        ) -> Result<Option<bool>, String> {
            self.inner.set_restart_pending(uid, pending)
        }
        fn remove(&self, uid: &InstanceUid) -> Result<(), String> {
            self.inner.remove(uid)
        }
        fn write_presence(
            &self,
            _holder: &str,
            presence: &[(InstanceUid, Presence, Option<u64>)],
        ) -> Result<HashMap<InstanceUid, u64>, String> {
            self.batches
                .lock()
                .expect("batches lock")
                .push(presence.to_vec());
            Ok(HashMap::new())
        }
        fn release(
            &self,
            revisions: &[crate::configs::Pinned],
            agents: &[(InstanceUid, PersistedAgent)],
        ) -> Result<Vec<InstanceUid>, String> {
            self.inner.release(revisions, agents)
        }
        fn pinning(&self, pin: Pin<'_>) -> usize {
            self.inner.pinning(pin)
        }
        fn referenced_revisions(&self, name: &str) -> BTreeSet<String> {
            self.inner.referenced_revisions(name)
        }
    }

    /// An instance carrying 1 000 Agents at a 30-second heartbeat writes their presence in
    /// batches: no heartbeat writes a record or a presence, each batch writes every presence that
    /// moved since the last one, in the order of the `instance_uid`, and nothing when none moved.
    /// A disconnect is written in the next batch, and under a new holder every presence again.
    /// Verifies: ADR-0029
    #[test]
    fn presence_is_written_in_batches_and_never_once_per_heartbeat() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (puts, batches) = (
            Arc::new(AtomicU64::new(0)),
            Arc::new(Mutex::new(Vec::new())),
        );
        let now = Arc::new(AtomicU64::new(1_000_000));
        let state = AppState::with_stores(
            Box::new(Counted {
                inner: crate::fs::FsAgentStore::open(dir.path()).expect("agents"),
                puts: puts.clone(),
                batches: batches.clone(),
            }),
            Box::new(crate::fs::FsConfigBackend::open(dir.path()).expect("configs")),
            Box::new(Manual(now.clone())),
        )
        .expect("state")
        .with_presence_interval(Duration::from_secs(30));
        let uids: Vec<InstanceUid> = (0..1000).map(|_| InstanceUid::default()).collect();
        for (conn, uid) in (1..).zip(&uids) {
            state.process(report(uid, 1), Transport::WebSocket, Some(conn));
        }
        let first_reports = puts.load(Ordering::SeqCst);
        assert!(
            batches.lock().expect("batches").is_empty(),
            "no report wrote a presence"
        );
        assert_eq!(state.write_presence("a"), Ok(1000));
        {
            let batches = batches.lock().expect("batches");
            let mut ordered = uids.clone();
            ordered.sort_by_key(|uid| uid.0);
            let batch: Vec<InstanceUid> = batches[0].iter().map(|(uid, ..)| *uid).collect();
            assert_eq!(batch, ordered, "in the order of the instance_uid");
            assert!(batches[0].iter().all(|(_, presence, _)| presence.connected));
        }

        for beat in 2..=4u64 {
            now.fetch_add(30_000, Ordering::SeqCst);
            for (conn, uid) in (1..).zip(&uids) {
                let heartbeat = AgentToServer {
                    instance_uid: uid.as_bytes().to_vec(),
                    sequence_num: beat,
                    ..Default::default()
                };
                state.process(heartbeat, Transport::WebSocket, Some(conn));
            }
            assert_eq!(
                puts.load(Ordering::SeqCst),
                first_reports,
                "no heartbeat writes a record"
            );
            assert_eq!(
                batches.lock().expect("batches").len() as u64,
                beat - 1,
                "no heartbeat writes a presence"
            );
            assert_eq!(
                state.write_presence("a"),
                Ok(1000),
                "one batch for the round"
            );
            let batches = batches.lock().expect("batches");
            let last = batches.last().expect("a batch");
            assert!(last
                .iter()
                .all(|(_, presence, _)| presence.last_seen_ms == now.load(Ordering::SeqCst)));
        }
        assert_eq!(state.write_presence("a"), Ok(0), "nothing moved");

        state.mark_disconnected(&uids[..1], 1);
        assert_eq!(state.write_presence("a"), Ok(1), "the disconnect");
        assert!(
            !batches.lock().expect("batches").last().expect("a batch")[0]
                .1
                .connected
        );
        assert_eq!(
            state.write_presence("b"),
            Ok(999),
            "a new holder writes every connected one"
        );
    }

    /// What is kept of an Agent's presence: a take's answer replaces the count, also a lower one,
    /// as a record forgotten and created again counts from the start, and moves the generation; a
    /// new holder writes every presence again and keeps the counts; a lost answer leaves no count
    /// and makes the Agent unsure until a take answers; a re-key moves all of it to the new
    /// identity.
    /// Verifies: ADR-0029
    #[test]
    fn what_is_kept_of_an_agents_presence_follows_its_takes() {
        let (uid, new) = (InstanceUid([1; 16]), InstanceUid([2; 16]));
        let presence = Presence {
            connected: true,
            last_seen_ms: 1,
        };
        let mut written = held::PresenceWritten::default();
        written.under("a");
        written.taken(uid, presence, 3);
        let first = written.generation(&uid);
        written.taken(uid, presence, 1);
        assert_eq!(written.takes.get(&uid), Some(&1), "a take's answer");
        assert_ne!(
            written.generation(&uid),
            first,
            "a take moves the generation"
        );
        written.under("b");
        assert!(
            written.agents.is_empty(),
            "a new holder writes every presence again"
        );
        assert_eq!(written.takes.get(&uid), Some(&1), "and keeps the counts");
        written.lost(uid);
        assert!(written.unsure.contains(&uid), "a lost answer");
        assert_eq!(written.takes.get(&uid), None, "leaves no count");
        written.agents.insert(uid, presence);
        written.rekey(&uid, new);
        assert_eq!(
            (
                written.agents.get(&new),
                written.takes.get(&new),
                written.unsure.contains(&new)
            ),
            (Some(&presence), None, true),
            "a re-key moves it"
        );
        written.taken(new, presence, 1);
        written.rekey(&new, uid);
        assert_eq!(written.takes.get(&uid), Some(&1), "and its count");
        assert!(!written.takes.contains_key(&new) && !written.unsure.contains(&uid));
        written.lost(uid);
        written.taken(uid, presence, 2);
        assert!(!written.unsure.contains(&uid), "a take answers");
    }

    /// The first presence batch of an instance writes the Agents it carries, not the fleet it
    /// restored: a restored Agent is written once it is connected here.
    /// Verifies: ADR-0029
    #[test]
    fn the_first_presence_batch_writes_only_the_agents_carried() {
        let dir = tempfile::tempdir().expect("tempdir");
        let restored: Vec<InstanceUid> = (0..100).map(|_| InstanceUid::default()).collect();
        {
            let store = crate::fs::FsAgentStore::open(dir.path()).expect("agents");
            for uid in &restored {
                store
                    .put(uid, &crate::agent_store::tests::record())
                    .expect("put");
            }
        }
        let batches = Arc::new(Mutex::new(Vec::new()));
        let state = AppState::with_stores(
            Box::new(Counted {
                inner: crate::fs::FsAgentStore::open(dir.path()).expect("agents"),
                puts: Arc::new(AtomicU64::new(0)),
                batches: batches.clone(),
            }),
            Box::new(crate::fs::FsConfigBackend::open(dir.path()).expect("configs")),
            Box::new(crate::clock::SystemClock),
        )
        .expect("state");
        assert_eq!(state.write_presence("a"), Ok(0), "none is carried");
        state.process(report(&restored[0], 8), Transport::WebSocket, Some(1));
        assert_eq!(state.write_presence("a"), Ok(1), "the one connected here");
        assert_eq!(batches.lock().expect("batches")[0][0].0, restored[0]);
    }

    /// An instance on PostgreSQL loads no Agent record when it starts, holds none until one
    /// reports to it, and reads again only the ones it holds when it reads everything again.
    /// Verifies: ADR-0025, ADR-0023
    #[test]
    fn an_instance_on_postgres_loads_no_fleet_and_reads_again_only_what_it_holds() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let elsewhere = InstanceUid([0x81; 16]);
        crate::postgres::agents::PgAgentStore::new(database.clone())
            .put(&elsewhere, &crate::agent_store::tests::record())
            .expect("another instance's record");
        let hooked = Hooked::new(Box::new(
            crate::postgres::agents::PgAgentStore::open(database.clone()).expect("open"),
        ));
        let loads = hooked.loads.clone();
        let state = on_postgres(&database, Box::new(hooked));
        assert!(state.snapshot().is_empty(), "it holds no Agent");
        let here = InstanceUid([0x82; 16]);
        state.process(report(&here, 1), Transport::WebSocket, Some(1));
        let held: Vec<String> = state
            .snapshot()
            .into_iter()
            .map(|v| v.instance_uid)
            .collect();
        assert_eq!(
            held,
            [here.to_string()],
            "it holds the Agent that reported to it"
        );
        state.catch_up().expect("read everything again");
        assert_eq!(
            loads.load(Ordering::SeqCst),
            0,
            "no record loaded but those it holds"
        );
        assert_eq!(
            state.fleet_view().expect("fleet").len(),
            2,
            "both are in the fleet"
        );
    }

    /// An Agent that comes back on a new connection of its host keeps its identity, the newest
    /// connection carries it and the move is logged; a report of the older connection is taken up
    /// and leaves it there, so the older one's close leaves it connected. On a connection of
    /// another host the same identity is a duplicate, re-keyed.
    /// Verifies: ADR-0025
    #[test]
    fn an_agent_on_a_new_connection_of_its_host_is_carried_by_it_and_not_rekeyed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let revocations = Arc::new(
            Revocations::open(
                Box::new(crate::fs::FsLedgerStore::open(dir.path()).expect("ledger")),
                Arc::new(crate::clock::SystemClock),
                Vec::new(),
            )
            .expect("revocations"),
        );
        let state = AppState::new(dir.path().join("configs"))
            .expect("state")
            .with_revocations(Some(revocations));
        let uid = InstanceUid::default();
        let (old, new, other) = (
            state.connection_id(),
            state.connection_id(),
            state.connection_id(),
        );
        let carrier = |state: &AppState| {
            state
                .fleet
                .lock()
                .expect("fleet lock")
                .get(&uid)
                .map(|record| (record.connected, record.owner))
        };
        crate::test_support::logged();
        {
            let host = of_host("edge-1");
            let first = state.process_presented(
                report(&uid, 1),
                Transport::WebSocket,
                Some(old),
                Some(&host),
            );
            assert!(first.reply.agent_identification.is_none());
            let back = state.process_presented(
                report(&uid, 2),
                Transport::WebSocket,
                Some(new),
                Some(&host),
            );
            assert!(
                back.reply.agent_identification.is_none(),
                "not a duplicate of itself"
            );
            assert_eq!(carrier(&state), Some((true, Some(new))));
            let late = state.process_presented(
                report(&uid, 3),
                Transport::WebSocket,
                Some(old),
                Some(&host),
            );
            assert!(late.reply.agent_identification.is_none());
            assert!(late.reply.error_response.is_none(), "taken up");
            assert_eq!(
                carrier(&state),
                Some((true, Some(new))),
                "the older one takes it no more"
            );
            state.mark_disconnected(&[uid], old);
            assert_eq!(carrier(&state), Some((true, Some(new))));

            let clone = state.process_presented(
                report(&uid, 1),
                Transport::WebSocket,
                Some(other),
                Some(&of_host("edge-2")),
            );
            let new_uid = clone
                .reply
                .agent_identification
                .expect("another host's is re-keyed");
            assert_ne!(new_uid.new_instance_uid, uid.as_bytes().to_vec());
        }
        let came_back = crate::test_support::lines_with(&[
            "came back on a new connection of its host",
            &uid.to_string(),
        ]);
        assert_eq!(
            came_back.len(),
            1,
            "the move to the new connection is logged once: {came_back:?}"
        );
    }

    /// Who carries `uid` in `state`'s memory: whether connected, and on which connection.
    fn carrier_of(state: &AppState, uid: &InstanceUid) -> Option<(bool, Option<ConnId>)> {
        state
            .fleet
            .lock()
            .expect("fleet lock")
            .get(uid)
            .map(|record| (record.connected, record.owner))
    }

    /// A goodbye the older connection of a host delivers while a newer one carries the Agent does
    /// not end the Agent's connection: it stays connected on the newer one.
    /// Verifies: ADR-0025
    #[test]
    fn a_goodbye_on_an_older_connection_of_the_host_leaves_the_agent_with_the_newer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let of = Arc::new(OnceLock::new());
        let state = with_probed_register(dir.path(), Arc::new(|_, _| {}), &of);
        let uid = InstanceUid::default();
        let (old, new) = (state.connection_id(), state.connection_id());
        let host = of_host("edge-1");
        state.process_presented(
            report(&uid, 1),
            Transport::WebSocket,
            Some(old),
            Some(&host),
        );
        state.process_presented(
            report(&uid, 3),
            Transport::WebSocket,
            Some(new),
            Some(&host),
        );
        let mut goodbye = report(&uid, 2);
        goodbye.agent_disconnect = Some(opamp::proto::AgentDisconnect {});
        let answered =
            state.process_presented(goodbye, Transport::WebSocket, Some(old), Some(&host));
        assert!(
            answered.disconnected,
            "the older connection lets the Agent go"
        );
        assert_eq!(carrier_of(&state, &uid), Some((true, Some(new))));
    }

    /// A queued restart is not sent on an older connection of the host while a newer one carries
    /// the Agent: it stays queued, and the newer connection's next report is sent it.
    /// Verifies: ADR-0025
    #[test]
    fn a_queued_restart_is_not_sent_on_an_older_connection_of_the_host() {
        let dir = tempfile::tempdir().expect("tempdir");
        let of = Arc::new(OnceLock::new());
        let state = with_probed_register(dir.path(), Arc::new(|_, _| {}), &of);
        let uid = InstanceUid::default();
        let (old, new) = (state.connection_id(), state.connection_id());
        let host = of_host("edge-1");
        let restartable = |sequence_num| {
            let mut msg = report(&uid, sequence_num);
            msg.capabilities |= opamp::proto::AgentCapabilities::AcceptsRestartCommand as u64;
            msg
        };
        state.process_presented(restartable(1), Transport::WebSocket, Some(old), Some(&host));
        state.process_presented(restartable(2), Transport::WebSocket, Some(new), Some(&host));
        assert!(state.request_restart(&uid).is_ok(), "queued");
        let late =
            state.process_presented(restartable(3), Transport::WebSocket, Some(old), Some(&host));
        assert!(
            late.reply.command.is_none(),
            "not sent on the older connection"
        );
        assert!(state.fleet.lock().expect("fleet lock")[&uid].restart_pending);
        let next =
            state.process_presented(restartable(4), Transport::WebSocket, Some(new), Some(&host));
        assert!(next.reply.command.is_some(), "sent on the newer one");
    }

    /// A report of an older connection of the host does not use up the demand for the component
    /// map: the newer connection is asked for it.
    /// Verifies: ADR-0025
    #[test]
    fn an_older_connection_of_the_host_is_not_asked_for_the_component_map() {
        let dir = tempfile::tempdir().expect("tempdir");
        let of = Arc::new(OnceLock::new());
        let state = with_probed_register(dir.path(), Arc::new(|_, _| {}), &of);
        let uid = InstanceUid::default();
        let (old, new) = (state.connection_id(), state.connection_id());
        let host = of_host("edge-1");
        let hashed = |sequence_num| {
            let mut msg = report(&uid, sequence_num);
            msg.capabilities |= opamp::proto::AgentCapabilities::ReportsAvailableComponents as u64;
            msg.available_components = Some(opamp::proto::AvailableComponents {
                hash: vec![sequence_num as u8],
                ..Default::default()
            });
            msg
        };
        let asked = |processed: &Processed| {
            processed.reply.flags & ServerToAgentFlags::ReportAvailableComponents as u64 != 0
        };
        state.process_presented(hashed(1), Transport::WebSocket, Some(old), Some(&host));
        state.process_presented(hashed(2), Transport::WebSocket, Some(new), Some(&host));
        let late = state.process_presented(hashed(3), Transport::WebSocket, Some(old), Some(&host));
        assert!(!asked(&late), "not asked on the older connection");
        let next = state.process_presented(hashed(3), Transport::WebSocket, Some(new), Some(&host));
        assert!(asked(&next), "asked on the newer one");
    }

    /// A state on files whose clock the test moves, with a host register in which `gw-1` is a
    /// Gateway's Host; beside it the clock and a certificate of that Gateway.
    fn with_a_gateway(
        dir: &std::path::Path,
        offer: Option<ConnectionOffer>,
    ) -> (AppState, Arc<AtomicU64>, Presented) {
        let now = Arc::new(AtomicU64::new(1_000_000));
        let revocations = Arc::new(
            Revocations::open(
                Box::new(crate::fs::FsLedgerStore::open(dir).expect("ledger")),
                Arc::new(crate::clock::SystemClock),
                Vec::new(),
            )
            .expect("revocations"),
        );
        let state = AppState::with_stores(
            Box::new(crate::fs::FsAgentStore::open(dir).expect("agents")),
            Box::new(crate::fs::FsConfigBackend::open(dir).expect("configs")),
            Box::new(Manual(now.clone())),
        )
        .expect("state")
        .with_revocations(Some(revocations.clone()))
        .with_connection_offer(offer);
        let gateway = of_host("gw-1");
        // Its first report makes its Host; the mark makes it a Gateway's.
        let first = state.connection_id();
        state.process_presented(
            report(&InstanceUid::default(), 1),
            Transport::WebSocket,
            Some(first),
            Some(&gateway),
        );
        assert!(revocations.set_gateway("gw-1", true).expect("mark"));
        (state, now, gateway)
    }

    /// An Agent behind a Gateway that its pool moved from the newer of two open connections to the
    /// older one is taken by its report there once the newer one has carried none of its reports
    /// for a heartbeat interval: it is asked for its component map there, and its goodbye there
    /// ends it.
    /// Verifies: ADR-0025
    #[test]
    fn a_gateway_s_agent_moved_to_an_older_connection_is_taken_there_after_an_interval() {
        crate::test_support::logged();
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, now, gateway) = with_a_gateway(dir.path(), None);
        let uid = InstanceUid::default();
        let (older, newer) = (state.connection_id(), state.connection_id());
        let hashed = |sequence_num: u64| {
            let mut msg = report(&uid, sequence_num);
            msg.capabilities |= opamp::proto::AgentCapabilities::ReportsAvailableComponents as u64;
            msg.available_components = Some(opamp::proto::AvailableComponents {
                hash: vec![7],
                ..Default::default()
            });
            msg
        };
        let asked = |processed: &Processed| {
            processed.reply.flags & ServerToAgentFlags::ReportAvailableComponents as u64 != 0
        };
        state.process_presented(hashed(1), Transport::WebSocket, Some(newer), Some(&gateway));
        assert_eq!(carrier_of(&state, &uid), Some((true, Some(newer))));
        // The Agent restarts, and the pool places it on the older connection; the newer one,
        // carrying other Agents, stays open and carries none of its reports from now on.
        now.fetch_add(DEFAULT_HEARTBEAT.as_millis() as u64, Ordering::SeqCst);
        let taken =
            state.process_presented(hashed(1), Transport::WebSocket, Some(older), Some(&gateway));
        assert_eq!(
            carrier_of(&state, &uid),
            Some((true, Some(older))),
            "the older connection carries it"
        );
        assert!(asked(&taken), "asked for its component map there");
        assert_eq!(
            crate::test_support::lines_with(&["carried none of its reports", &uid.to_string()])
                .len(),
            1,
            "the move to the stale carrier's older connection is logged"
        );
        let mut goodbye = report(&uid, 2);
        goodbye.agent_disconnect = Some(opamp::proto::AgentDisconnect {});
        state.process_presented(goodbye, Transport::WebSocket, Some(older), Some(&gateway));
        assert_eq!(
            carrier_of(&state, &uid),
            Some((false, None)),
            "its goodbye ends it"
        );
    }

    /// Within a heartbeat interval of the newer connection's last report of an Agent, a report on
    /// an older connection of the same Gateway does not take the Agent back.
    /// Verifies: ADR-0025
    #[test]
    fn within_an_interval_an_older_connection_of_a_gateway_does_not_take_the_agent_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, now, gateway) = with_a_gateway(dir.path(), None);
        let uid = InstanceUid::default();
        let (older, newer) = (state.connection_id(), state.connection_id());
        state.process_presented(
            report(&uid, 1),
            Transport::WebSocket,
            Some(newer),
            Some(&gateway),
        );
        now.fetch_add(DEFAULT_HEARTBEAT.as_millis() as u64 - 1, Ordering::SeqCst);
        state.process_presented(
            report(&uid, 2),
            Transport::WebSocket,
            Some(older),
            Some(&gateway),
        );
        assert_eq!(carrier_of(&state, &uid), Some((true, Some(newer))));
        // A report on the newer connection carries it on, so a heartbeat later the older one
        // still does not take it.
        state.process_presented(
            report(&uid, 3),
            Transport::WebSocket,
            Some(newer),
            Some(&gateway),
        );
        now.fetch_add(DEFAULT_HEARTBEAT.as_millis() as u64 - 1, Ordering::SeqCst);
        state.process_presented(
            report(&uid, 4),
            Transport::WebSocket,
            Some(older),
            Some(&gateway),
        );
        assert_eq!(carrier_of(&state, &uid), Some((true, Some(newer))));
    }

    /// The interval this Server offers decides when a Gateway's newer connection is stale: at an
    /// offered 60 seconds, a report on the older connection 30 seconds after the newer one's last
    /// does not take the Agent, and one 60 seconds after does.
    /// Verifies: ADR-0025
    #[test]
    fn the_offered_heartbeat_interval_decides_when_a_carrier_is_stale() {
        let dir = tempfile::tempdir().expect("tempdir");
        let offer = ConnectionOffer::from_config(
            &toml::from_str::<crate::config::ConnectionOfferConfig>(
                "heartbeat_interval_secs = 60\n",
            )
            .expect("offer config"),
        );
        let (state, now, gateway) = with_a_gateway(dir.path(), Some(offer));
        let uid = InstanceUid::default();
        let (older, newer) = (state.connection_id(), state.connection_id());
        state.process_presented(
            report(&uid, 1),
            Transport::WebSocket,
            Some(newer),
            Some(&gateway),
        );
        now.fetch_add(30_000, Ordering::SeqCst);
        state.process_presented(
            report(&uid, 2),
            Transport::WebSocket,
            Some(older),
            Some(&gateway),
        );
        assert_eq!(
            carrier_of(&state, &uid),
            Some((true, Some(newer))),
            "30 s is within the offered interval"
        );
        now.fetch_add(30_000, Ordering::SeqCst);
        state.process_presented(
            report(&uid, 3),
            Transport::WebSocket,
            Some(older),
            Some(&gateway),
        );
        assert_eq!(
            carrier_of(&state, &uid),
            Some((true, Some(older))),
            "60 s is one offered interval"
        );
    }

    /// A newer connection whose report waited on the store for a heartbeat interval is as fresh as
    /// its report once it took the Agent: the older connection's next report does not take the
    /// Agent back.
    /// Verifies: ADR-0025
    #[test]
    fn a_carrier_whose_report_waited_an_interval_is_not_stale_once_it_took_the_agent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let now = Arc::new(AtomicU64::new(1_000_000));
        let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let probe: Probe = {
            let (now, armed) = (now.clone(), armed.clone());
            Arc::new(move |call, _| {
                if call == "save_hosts" && armed.swap(false, Ordering::SeqCst) {
                    // The newer connection's round waits an interval on the store.
                    now.fetch_add(DEFAULT_HEARTBEAT.as_millis() as u64, Ordering::SeqCst);
                }
            })
        };
        let revocations = Arc::new(
            Revocations::open(
                Box::new(ProbedLedger {
                    inner: Box::new(crate::fs::FsLedgerStore::open(dir.path()).expect("ledger")),
                    probe,
                }),
                Arc::new(crate::clock::SystemClock),
                Vec::new(),
            )
            .expect("revocations"),
        );
        let state = AppState::with_stores(
            Box::new(crate::fs::FsAgentStore::open(dir.path()).expect("agents")),
            Box::new(crate::fs::FsConfigBackend::open(dir.path()).expect("configs")),
            Box::new(Manual(now.clone())),
        )
        .expect("state")
        .with_revocations(Some(revocations));
        let uid = InstanceUid::default();
        let (old, new) = (state.connection_id(), state.connection_id());
        let until = |not_after_ms| Presented {
            not_after_ms,
            ..of_host("edge-1")
        };
        state.process_presented(
            report(&uid, 1),
            Transport::WebSocket,
            Some(old),
            Some(&until(1)),
        );
        armed.store(true, Ordering::SeqCst);
        state.process_presented(
            report(&uid, 2),
            Transport::WebSocket,
            Some(new),
            Some(&until(2)),
        );
        assert!(
            !armed.load(Ordering::SeqCst),
            "the newer connection's round waited"
        );
        assert_eq!(carrier_of(&state, &uid), Some((true, Some(new))));
        state.process_presented(
            report(&uid, 3),
            Transport::WebSocket,
            Some(old),
            Some(&until(2)),
        );
        assert_eq!(
            carrier_of(&state, &uid),
            Some((true, Some(new))),
            "the older connection's backlog does not take it back"
        );
    }

    /// A report of an older connection of the host whose newer connection closes while the report
    /// is taken up is not left without a connection: the older one carries the Agent again.
    /// Verifies: ADR-0025
    #[test]
    fn an_older_connection_takes_the_agent_back_when_the_newer_closes_meanwhile() {
        let dir = tempfile::tempdir().expect("tempdir");
        let of: Arc<OnceLock<std::sync::Weak<AppState>>> = Arc::new(OnceLock::new());
        let uid = InstanceUid::default();
        let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let closing = Arc::new(Mutex::new(None::<ConnId>));
        let probe: Probe = {
            let (of, armed, closing) = (of.clone(), armed.clone(), closing.clone());
            Arc::new(move |call, _| {
                if call == "save_hosts" && armed.swap(false, Ordering::SeqCst) {
                    // The newer connection closes while the older one's report moves its Host's
                    // valid until.
                    let state = of.get().and_then(std::sync::Weak::upgrade).expect("state");
                    let conn = closing.lock().expect("closing").expect("a connection");
                    state.mark_disconnected(&[uid], conn);
                }
            })
        };
        let state = with_probed_register(dir.path(), probe, &of);
        let (old, new) = (state.connection_id(), state.connection_id());
        *closing.lock().expect("closing") = Some(new);
        let until = |not_after_ms| Presented {
            not_after_ms,
            ..of_host("edge-1")
        };
        state.process_presented(
            report(&uid, 1),
            Transport::WebSocket,
            Some(old),
            Some(&until(1)),
        );
        state.process_presented(
            report(&uid, 2),
            Transport::WebSocket,
            Some(new),
            Some(&until(1)),
        );
        assert_eq!(carrier_of(&state, &uid), Some((true, Some(new))));
        armed.store(true, Ordering::SeqCst);
        let late = state.process_presented(
            report(&uid, 3),
            Transport::WebSocket,
            Some(old),
            Some(&until(2)),
        );
        assert!(
            !armed.load(Ordering::SeqCst),
            "the newer connection closed meanwhile"
        );
        assert!(late.reply.error_response.is_none());
        assert_eq!(carrier_of(&state, &uid), Some((true, Some(old))));
    }

    /// What an instance wrote of an Agent's presence, the count of takes among it, leaves with the
    /// Agent wherever it leaves what the instance holds: when it is forgotten here, when it was
    /// forgotten through another instance, and under its old identity when it is re-keyed.
    /// Verifies: ADR-0029
    #[test]
    fn what_was_written_of_an_agents_presence_leaves_with_it() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let on = |lease: &str| {
            let state = on_postgres(
                &database,
                Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
            );
            state.serve_under(tokio::sync::watch::channel(Some(lease.to_string())).1);
            state
        };
        let (a, b) = (
            on("00000000-0000-7000-8000-00000000000a"),
            on("00000000-0000-7000-8000-00000000000b"),
        );
        let kept = |state: &AppState, uid: &InstanceUid| {
            let written = state.kept.presence_written.lock().expect("presence lock");
            (
                written.agents.contains_key(uid),
                written.takes.contains_key(uid),
            )
        };
        let (forgotten, elsewhere, rekeyed) = (
            InstanceUid([0x71; 16]),
            InstanceUid([0x72; 16]),
            InstanceUid([0x73; 16]),
        );
        for (conn, uid) in (1..).zip([forgotten, elsewhere, rekeyed]) {
            a.process(report(&uid, 1), Transport::WebSocket, Some(conn));
            assert_eq!(kept(&a, &uid), (true, true), "taken");
        }

        a.mark_disconnected(&[forgotten], 1);
        a.mark_disconnected(&[elsewhere], 2);
        assert!(a.forget_agent(&forgotten).is_ok(), "forget here");
        assert_eq!(kept(&a, &forgotten), (false, false), "forgotten here");

        assert!(b.forget_agent(&elsewhere).is_ok(), "forget through B");
        // A report that changes the record finds it gone.
        let mut changed = report(&elsewhere, 3);
        changed.health = Some(opamp::proto::ComponentHealth {
            healthy: true,
            ..Default::default()
        });
        a.process(changed, Transport::WebSocket, Some(2));
        assert_eq!(
            kept(&a, &elsewhere),
            (false, false),
            "forgotten through another instance"
        );

        let mut rekey = report(&rekeyed, 2);
        rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
        a.process(rekey, Transport::WebSocket, Some(3));
        assert_eq!(kept(&a, &rekeyed), (false, false), "re-keyed");
    }

    /// An instance that heard another took an Agent reads its holder under the Agent's own lock: a
    /// take of its own by the Agent's new connection here, made while the holder is read, waits
    /// for the read and keeps the Agent carried.
    /// Verifies: ADR-0025
    #[test]
    fn an_agent_taken_back_here_while_its_holder_is_read_stays_carried() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let after_holders = hooked.after_holders.clone();
        let a = Arc::new(on_postgres(&database, Box::new(hooked)));
        a.serve_under(
            tokio::sync::watch::channel(Some("00000000-0000-7000-8000-00000000000a".to_string())).1,
        );
        let b = on_postgres(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
        );
        b.serve_under(
            tokio::sync::watch::channel(Some("00000000-0000-7000-8000-00000000000b".to_string())).1,
        );
        let uid = InstanceUid([0x74; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        b.process(report(&uid, 2), Transport::WebSocket, Some(9));
        // While A reads the holder, the Agent's old connection to A closes and it connects to A
        // again, which takes it back.
        let again = a.clone();
        let (taken_back, taking_back) = std::sync::mpsc::channel();
        *after_holders.lock().expect("hook lock") = Some(Box::new(move || {
            std::thread::spawn(move || {
                again.mark_disconnected(&[uid], 7);
                again.process(report(&uid, 3), Transport::WebSocket, Some(8));
                let _ = taken_back.send(());
            });
        }));
        a.catch_up().expect("release");
        assert!(
            after_holders.lock().expect("hook lock").is_none(),
            "the take came while the holder was read"
        );
        taking_back
            .recv_timeout(Duration::from_secs(30))
            .expect("the Agent's new connection is answered");
        assert!(view(&a, &uid).connected, "still carried");
    }

    /// On PostgreSQL a first report here whose read of the stored record failed goes on from the
    /// record another instance created, as it is stored: its labels are held, and a report the
    /// binding then refuses leaves it stored and its Agent carried where it is.
    /// Verifies: ADR-0025
    #[test]
    fn a_record_another_instance_created_is_taken_as_stored_and_never_removed() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let failing = hooked.failing_gets.clone();
        let a = on_postgres(&database, Box::new(hooked));
        let b = on_postgres(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
        );
        let (bound, labelled) = (InstanceUid([0x61; 16]), InstanceUid([0x62; 16]));
        for uid in [bound, labelled] {
            b.process_presented(report(&uid, 1), Transport::Http, None, Some(&of_host("h1")));
        }
        label(&b, &labelled, &[("tier", "edge")]);

        failing.store(1, Ordering::SeqCst);
        let refused = a.process_presented(
            report(&bound, 2),
            Transport::Http,
            None,
            Some(&of_host("h2")),
        );
        assert!(
            refused.reply.error_response.is_some(),
            "the binding refuses it"
        );
        assert_eq!(
            test.rows(&format!(
                "SELECT instance_uid FROM agents WHERE instance_uid = '{bound}'"
            ))
            .len(),
            1,
            "the record B created stays"
        );

        failing.store(1, Ordering::SeqCst);
        a.process_presented(
            report(&labelled, 2),
            Transport::Http,
            None,
            Some(&of_host("h1")),
        );
        assert_eq!(view(&a, &labelled).labels["tier"], "edge", "held as stored");
    }

    /// A label change made on A after A's take-up read the records, before it took the fleet lock,
    /// is not undone by what that read found: what differs is read again under the lock. A never
    /// hears its own announcement, so nothing else would correct it.
    /// Verifies: ADR-0025
    #[test]
    fn a_take_up_never_undoes_a_newer_act_of_its_own_instance() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let after_get_many = hooked.after_get_many.clone();
        let a = Arc::new(on_postgres(&database, Box::new(hooked)));
        let b = on_postgres(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
        );
        let uid = InstanceUid([0x63; 16]);
        a.process(report(&uid, 1), Transport::Http, None);
        label(&b, &uid, &[("tier", "edge")]);

        // A's take-up reads B's labels in its first read; then an operator sets other labels
        // through A.
        let on_a = Arc::downgrade(&a);
        *after_get_many.lock().expect("hook lock") = Some(Box::new(move || {
            let a = on_a.upgrade().expect("A");
            // Another thread tries the fleet lock, so a first read under it fails here rather
            // than deadlocking on the label.
            let held = a.clone();
            let locked = std::thread::spawn(move || held.fleet.try_lock().is_err())
                .join()
                .expect("tried");
            assert!(!locked, "the first read comes before the fleet lock");
            label(&a, &uid, &[("tier", "core")]);
        }));
        a.take_up_agents(&HashSet::from([uid])).expect("take up");
        assert!(
            after_get_many.lock().expect("hook lock").is_none(),
            "the act came after the first read"
        );
        assert_eq!(view(&a, &uid).labels["tier"], "core", "A's own act stays");
    }

    /// A take-up reads the records of the Agents it holds in one read before the fleet lock, and
    /// what differs again under the lock in one read, however many records differ: neither read
    /// takes one round trip per record.
    /// Verifies: ADR-0025
    #[test]
    fn a_take_up_reads_again_what_differs_in_one_read() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let probe = hooked.probe.clone();
        let a = Arc::new(on_postgres(&database, Box::new(hooked)));
        let b = on_postgres(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
        );
        let uids = [0x64, 0x65, 0x66].map(|byte| InstanceUid([byte; 16]));
        for uid in &uids {
            a.process(report(uid, 1), Transport::Http, None);
            label(&b, uid, &[("tier", "edge")]);
        }
        let reads = Arc::new(Mutex::new(Vec::new()));
        {
            let (reads, on) = (reads.clone(), Arc::downgrade(&a));
            *probe.lock().expect("hook lock") = Some(Arc::new(move |call, _| {
                if call != "get" && call != "get_many" {
                    return;
                }
                // Whether the fleet lock is held at the read: another thread tries it.
                let a = on.upgrade().expect("A");
                let locked = std::thread::spawn(move || a.fleet.try_lock().is_err())
                    .join()
                    .expect("tried");
                reads.lock().expect("reads").push((call, locked));
            }));
        }

        a.take_up_agents(&HashSet::from(uids)).expect("take up");
        *probe.lock().expect("hook lock") = None;
        for uid in &uids {
            assert_eq!(view(&a, uid).labels["tier"], "edge", "{uid} taken up");
        }
        assert_eq!(
            reads.lock().expect("reads").clone(),
            [("get_many", false), ("get_many", true)],
            "one read of all three before the lock, and one under it"
        );
    }

    /// A catch-up after a missed notification takes up every kind of state this instance keeps of
    /// the Agents it holds from one read of their records and one of their holders, however many
    /// kinds there are: an assignment, labels, the labels of a Client Agent another instance
    /// carries, which the group inherits, of a Host and behind a Gateway, a requested restart, a
    /// put-back kept for retry, and a take. The Agents behind the Gateway are read once, before the
    /// fleet lock.
    /// Verifies: ADR-0025
    #[test]
    fn a_catch_up_reads_the_records_once_and_the_holders_once() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let (probe, failing) = (hooked.probe.clone(), hooked.failing_restarts.clone());
        let a = Arc::new(on_postgres(&database, Box::new(hooked)));
        a.serve_under(
            tokio::sync::watch::channel(Some("00000000-0000-7000-8000-00000000000a".to_string())).1,
        );
        let dir = tempfile::tempdir().expect("blobs");
        let b = delivering_on(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
            dir.path(),
        );
        b.serve_under(
            tokio::sync::watch::channel(Some("00000000-0000-7000-8000-00000000000b".to_string())).1,
        );
        let [client, agent, restarted, kept, taken, gateway, behind_client, behind] =
            [0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8].map(|byte| InstanceUid([byte; 16]));
        let subject = crate::revocation::SubjectValues::default;
        report_over(
            &b,
            "h1",
            subject(),
            client,
            &[("supervisor.kind.collector", "true")],
        );
        report_over(
            &b,
            "gw",
            subject(),
            gateway,
            &[("supervisor.kind.collector", "true"), ("host.id", "gw-1")],
        );
        assert!(b.set_gateway("gw", true).expect("marked"));
        report_over(
            &b,
            "gw",
            subject(),
            behind_client,
            &[("supervisor.kind.collector", "true"), ("host.id", "m-1")],
        );
        a.revocations()
            .expect("register")
            .reload_ledger()
            .expect("the binding and the mark B made");
        report_over(&a, "h1", subject(), agent, &[]);
        report_over(&a, "gw", subject(), behind, &[("host.id", "m-1")]);
        a.process(restartable(&restarted, 1), Transport::WebSocket, Some(1));
        kept_put_back(&a, &failing, &kept);
        a.process(report(&taken, 1), Transport::WebSocket, Some(2));

        // What B changes, A hears nothing of.
        label(&b, &client, &[("site", "wien")]);
        label(&b, &agent, &[("ring", "early")]);
        label(&b, &behind_client, &[("site", "graz")]);
        aim_at_the_early_ring(&b);
        assert!(b.rollout_to_agent(&agent, None).is_ok(), "released");
        assert!(b.request_restart(&restarted).is_ok(), "requested");
        assert!(b.request_restart(&kept).is_ok(), "requested again");
        b.process(report(&taken, 2), Transport::WebSocket, Some(9));

        let calls = Arc::new(Mutex::new(Vec::new()));
        {
            let (calls, on) = (calls.clone(), Arc::downgrade(&a));
            *probe.lock().expect("hook lock") = Some(Arc::new(move |call, _| {
                // Whether the fleet lock is held at the call: another thread tries it.
                let a = on.upgrade().expect("A");
                let locked = std::thread::spawn(move || a.fleet.try_lock().is_err())
                    .join()
                    .expect("tried");
                calls.lock().expect("calls").push((call, locked));
            }));
        }
        a.catch_up().expect("caught up");
        *probe.lock().expect("hook lock") = None;
        let calls = calls.lock().expect("calls").clone();
        let count = |name: &str| calls.iter().filter(|(call, _)| *call == name).count();
        assert_eq!(
            (
                count("get"),
                count("get_many"),
                count("holders"),
                count("query_page")
            ),
            (0, 1, 1, 1),
            "one read of the records, one of the holders and one of the Gateway: {calls:?}"
        );
        assert!(
            calls
                .iter()
                .all(|(call, locked)| *locked == (*call == "get_many")),
            "only the records are read under the fleet lock: {calls:?}"
        );

        let held = view(&a, &agent);
        assert_eq!(held.assigned_deployment, "early", "the assignment");
        assert_eq!(held.labels["ring"], "early", "the labels");
        assert_eq!(
            held.inherited_labels.get("site").map(String::as_str),
            Some("wien"),
            "the Client Agent's labels"
        );
        assert!(
            a.fleet.lock().expect("fleet")[&restarted].restart_pending,
            "the restart requested"
        );
        assert!(
            !a.kept
                .unput_restarts
                .lock()
                .expect("restarts lock")
                .contains_key(&kept),
            "the put-back, its restart queued again"
        );
        assert!(!view(&a, &taken).connected, "the take");
        assert_eq!(
            view(&a, &behind)
                .inherited_labels
                .get("site")
                .map(String::as_str),
            Some("graz"),
            "the labels of the Client Agent behind the Gateway"
        );
    }

    /// The hooks of A's store that the presence tests use.
    struct PresenceHooks {
        losing_takes: Arc<std::sync::atomic::AtomicBool>,
        failing_takes: Arc<std::sync::atomic::AtomicBool>,
        after_presence: Arc<Mutex<Option<Act>>>,
        before_takes: Arc<Mutex<Option<Act>>>,
        after_takes: Arc<Mutex<Option<Act>>>,
        takes_reads: Arc<AtomicU64>,
    }

    /// Instance A on `database`, its store hooked, and B, serving under `LEASE_A` and `LEASE_B`;
    /// neither lease is in the database, so neither is live.
    fn hooked_pair(
        database: &Arc<crate::postgres::Database>,
    ) -> (Arc<AppState>, Arc<AppState>, PresenceHooks) {
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let hooks = PresenceHooks {
            losing_takes: hooked.losing_takes.clone(),
            failing_takes: hooked.failing_takes.clone(),
            after_presence: hooked.after_presence.clone(),
            before_takes: hooked.before_takes.clone(),
            after_takes: hooked.after_takes.clone(),
            takes_reads: hooked.takes_reads.clone(),
        };
        let a = Arc::new(on_postgres(database, Box::new(hooked)));
        a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let b = Arc::new(on_postgres(
            database,
            Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
        ));
        b.serve_under(tokio::sync::watch::channel(Some(LEASE_B.to_string())).1);
        (a, b, hooks)
    }

    /// The stored holder (`none` where it names none), `connected` and the count of takes of `uid`.
    fn stored_presence(test: &crate::postgres::testing::TestDatabase, uid: &InstanceUid) -> String {
        test.rows(&format!(
            "SELECT coalesce(holder::text, 'none') || ' ' || connected || ' ' || takes \
             FROM agents WHERE instance_uid = '{uid}'"
        ))
        .into_iter()
        .flatten()
        .flatten()
        .collect()
    }

    /// Waits for the system clock to reach its next millisecond, so a report after it moves the
    /// presence an instance last wrote.
    fn next_millisecond() {
        let now = || {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_millis()
        };
        let start = now();
        while now() == start {
            std::hint::spin_loop();
        }
    }

    /// An Agent whose connecting take was written and whose answer was lost, and that disconnects
    /// before the next batch, has its row name no holder after that batch: the batch reads the
    /// row's holder and count first and writes the disconnect under the count it read, once.
    /// Verifies: ADR-0025
    #[test]
    fn the_goodbye_of_an_agent_whose_take_lost_its_answer_is_written() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, _b, hooks) = hooked_pair(&database);
        let uid = InstanceUid([0x75; 16]);
        hooks.losing_takes.store(true, Ordering::SeqCst);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        hooks.losing_takes.store(false, Ordering::SeqCst);
        assert_eq!(stored_presence(&test, &uid), format!("{LEASE_A} true 1"));
        a.mark_disconnected(&[uid], 7);
        a.write_presence(LEASE_A).expect("A's batch");
        assert_eq!(stored_presence(&test, &uid), "none false 1");
        assert_eq!(hooks.takes_reads.load(Ordering::SeqCst), 1, "read once");
        a.write_presence(LEASE_A).expect("A's next batch");
        assert_eq!(hooks.takes_reads.load(Ordering::SeqCst), 1, "and no more");
    }

    /// Of two Agents whose takes lost their answers, one B took since is not written by A's batch,
    /// and one A took again after the batch first looked at it and before it read the rows is left
    /// as that take wrote it.
    /// Verifies: ADR-0025
    #[test]
    fn a_lost_take_whose_row_was_taken_since_writes_no_goodbye() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b, hooks) = hooked_pair(&database);
        let (moved, back) = (InstanceUid([0x76; 16]), InstanceUid([0x77; 16]));
        hooks.losing_takes.store(true, Ordering::SeqCst);
        a.process(report(&moved, 1), Transport::WebSocket, Some(7));
        a.process(report(&back, 1), Transport::WebSocket, Some(8));
        hooks.losing_takes.store(false, Ordering::SeqCst);
        b.process(report(&moved, 2), Transport::WebSocket, Some(9));
        a.mark_disconnected(&[moved], 7);
        a.mark_disconnected(&[back], 8);
        let again = a.clone();
        *hooks.before_takes.lock().expect("hook lock") = Some(Box::new(move || {
            again.process(report(&back, 2), Transport::WebSocket, Some(10));
        }));
        a.write_presence(LEASE_A).expect("A's batch");
        assert!(
            hooks.before_takes.lock().expect("hook lock").is_none(),
            "the take came before the read"
        );
        assert_eq!(
            stored_presence(&test, &moved),
            format!("{LEASE_B} true 2"),
            "B's take stays"
        );
        assert_eq!(
            stored_presence(&test, &back),
            format!("{LEASE_A} true 2"),
            "A's new take stays"
        );
    }

    /// An Agent re-keyed on A that disconnects before A's next batch has the row under its new
    /// identity name no holder after that batch: what A wrote of its presence moves with it.
    /// Verifies: ADR-0025
    #[test]
    fn the_goodbye_of_an_agent_rekeyed_is_written_under_its_new_identity() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, _b, _hooks) = hooked_pair(&database);
        let uid = InstanceUid([0x78; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        let mut rekey = report(&uid, 2);
        rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
        let answered = a.process(rekey, Transport::WebSocket, Some(7));
        let new = answered.uid.expect("carried");
        assert_ne!(new, uid, "re-keyed");
        assert_eq!(stored_presence(&test, &new), format!("{LEASE_A} true 1"));
        a.mark_disconnected(&[new], 7);
        a.write_presence(LEASE_A).expect("A's batch");
        assert_eq!(stored_presence(&test, &new), "none false 1");
    }

    /// How long the one call whose answer a test loses waits for it: short of the five seconds of
    /// `lock_timeout` a read back that follows it waits for the row, and long enough for a
    /// database slowed by the tests beside it.
    const LOST_WITHIN: Duration = Duration::from_secs(3);

    /// Runs `act` with every call on `database` bounded by [`LOST_WITHIN`], and the usual bound
    /// again after it.
    fn losing_the_answer_of<T>(database: &crate::postgres::Database, act: impl FnOnce() -> T) -> T {
        database.bound_calls_by(LOST_WITHIN);
        let answered = act();
        database.bound_calls_by(crate::postgres::CALL_TIMEOUT);
        answered
    }

    /// Has the pool of `database` hold three idle connections, so the call after one that lost its
    /// answer is not queued behind that call's commit on its connection.
    fn spare_connections(database: &crate::postgres::Database) {
        database
            .run(|pool| async move {
                let held = (
                    crate::postgres::client(&pool).await?,
                    crate::postgres::client(&pool).await?,
                    crate::postgres::client(&pool).await?,
                );
                drop(held);
                Ok(())
            })
            .expect("three connections");
    }

    /// Instance A on a database of the usual bound, with floors and a revocation list of a
    /// `client` CA, following the notifications on a runtime of its own; it has read everything
    /// once. The one call whose answer a test loses is bounded short ([`LOST_WITHIN`]).
    struct LosingAnswers {
        a: Arc<AppState>,
        database: Arc<crate::postgres::Database>,
        following: crate::instance::Following,
        _runtime: tokio::runtime::Runtime,
    }

    impl LosingAnswers {
        fn on(test: &crate::postgres::testing::TestDatabase) -> Self {
            let database = test.shared();
            let records = crate::Records::Postgres(database.clone());
            let revocations = Revocations::open(
                records.ledger().expect("ledger"),
                Arc::new(crate::clock::SystemClock),
                vec![crate::revocation::Authority {
                    role: "client".to_string(),
                    subject: "ab".repeat(32),
                    name: "CN=test".to_string(),
                }],
            )
            .expect("revocations");
            let a = Arc::new(
                on_postgres(
                    &database,
                    Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
                )
                .with_revocations(Some(Arc::new(revocations)))
                .with_floors(Some(Arc::new(
                    crate::floors::PackageFloors::open(records.floors()).expect("floors"),
                ))),
            );
            a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
            let runtime = tokio::runtime::Runtime::new().expect("runtime");
            let following = {
                let _entered = runtime.enter();
                crate::instance::follow(&database, a.clone(), None)
            };
            until("the first read once listening", || following.reads() > 0);
            spare_connections(&database);
            LosingAnswers {
                a,
                database,
                following,
                _runtime: runtime,
            }
        }

        /// Runs `act`, whose commit `gate` holds, and expects it to fail on its bound; then waits
        /// for the read it calls for, finds `taken_up` not yet, opens the gate and waits for it.
        fn lose_the_answer_of<T: std::fmt::Debug>(
            &self,
            gate: crate::postgres::testing::Gate,
            act: impl FnOnce(&AppState) -> Result<T, String>,
            taken_up: impl Fn(&AppState) -> bool,
        ) {
            let reads = self.following.reads();
            let failed = losing_the_answer_of(&self.database, || act(&self.a))
                .expect_err("the call's answer is lost to its bound");
            assert!(failed.contains("no answer within"), "{failed}");
            until("the read the lost answer calls for", || {
                self.following.reads() > reads
            });
            assert!(!taken_up(&self.a), "the commit is held yet");
            gate.open();
            until("the commit taken up once it landed", || taken_up(&self.a));
        }
    }

    /// A label set whose commit lands after its call lost its answer is held in the memory of the
    /// instance that carries the Agent once it lands, though its own notification is otherwise
    /// passed over.
    /// Verifies: ADR-0025
    #[test]
    fn a_label_whose_answer_was_lost_is_taken_up_once_it_commits() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let losing = LosingAnswers::on(&test);
        let uid = InstanceUid([0x28; 16]);
        losing
            .a
            .process(report(&uid, 1), Transport::WebSocket, Some(7));
        let gate = test.hold_commits("agents", 0x28_0001);
        losing.lose_the_answer_of(
            gate,
            |a| match a.set_labels(&uid, BTreeMap::from([("site".into(), "linz".into())])) {
                Ok(()) => Ok(()),
                Err(LabelError::Storage(e)) => Err(e),
                Err(_) => Err("refused".to_string()),
            },
            |a| {
                a.fleet
                    .lock()
                    .expect("fleet lock")
                    .get(&uid)
                    .map(|r| r.labels.clone())
                    == Some(BTreeMap::from([("site".into(), "linz".into())]))
            },
        );
    }

    /// A floor set whose commit lands after its call lost its answer is served by the instance
    /// that set it once it lands.
    /// Verifies: ADR-0025
    #[test]
    fn a_floor_whose_answer_was_lost_is_taken_up_once_it_commits() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let losing = LosingAnswers::on(&test);
        let gate = test.hold_commits("package_floors", 0x28_0002);
        losing.lose_the_answer_of(
            gate,
            |a| {
                a.floors()
                    .expect("floors")
                    .put("otelcol", "1.2.0", vec![7; 64])
                    .map_err(|e| format!("{e:?}"))
            },
            |a| {
                a.floors()
                    .expect("floors")
                    .all()
                    .get("otelcol")
                    .map(|floor| floor.version.clone())
                    .as_deref()
                    == Some("1.2.0")
            },
        );
    }

    /// A revocation whose commit lands after its call lost its answer is in the list of the
    /// instance that made it once it lands, renewals aside.
    /// Verifies: ADR-0025
    #[test]
    fn a_revocation_whose_answer_was_lost_is_taken_up_once_it_commits() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let losing = LosingAnswers::on(&test);
        let gate = test.hold_commits("revocations", 0x28_0003);
        losing.lose_the_answer_of(
            gate,
            |a| {
                a.revocations()
                    .expect("revocations")
                    .revoke_certificate("client", "0a1b")
                    .map_err(|e| format!("{e:?}"))
            },
            |a| a.revocations().expect("revocations").list().len() == 1,
        );
    }

    /// A re-key whose commit lands after its call lost its answer is read back under a lock that
    /// waits for it: the Agent is assigned the identity its record moved to, one row names it,
    /// under that identity, and no record is created for the old one when it reports again.
    /// Verifies: ADR-0025
    #[test]
    fn a_rekey_whose_answer_was_lost_is_read_back_and_stands() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let losing = LosingAnswers::on(&test);
        let (a, uid) = (losing.a.clone(), InstanceUid([0x29; 16]));
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        let gate = test.hold_commits("agents", 0x28_0004);
        let (answered, answer) = std::sync::mpsc::channel();
        let rekeying = std::thread::spawn({
            let (a, database) = (a.clone(), losing.database.clone());
            move || {
                let mut rekey = report(&uid, 2);
                rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
                let _ = answered.send(losing_the_answer_of(&database, || {
                    a.process(rekey, Transport::WebSocket, Some(7))
                }));
            }
        });
        // Its commit is held; once the call lost its answer, its read back waits for the row.
        until("the read back waiting for the move, or the answer", || {
            rekeying.is_finished() || test.sessions("wait_event IN ('transactionid', 'tuple')") > 0
        });
        gate.open();
        let processed = answer
            .recv_timeout(Duration::from_secs(30))
            .expect("the re-key answers");
        rekeying.join().expect("the re-key's thread");
        let new = processed.uid.expect("carried");
        assert_ne!(new, uid, "re-keyed");
        assert_eq!(
            processed
                .reply
                .agent_identification
                .as_ref()
                .map(|id| id.new_instance_uid.clone()),
            Some(new.as_bytes().to_vec()),
            "the identity it moved to is assigned"
        );
        let stored = || -> Vec<String> {
            test.rows("SELECT instance_uid FROM agents")
                .into_iter()
                .flatten()
                .flatten()
                .collect()
        };
        assert_eq!(
            stored(),
            [new.to_string()],
            "one row, under the new identity"
        );
        a.process(report(&new, 3), Transport::WebSocket, Some(7));
        a.write_presence(LEASE_A).expect("A's batch");
        assert_eq!(
            stored(),
            [new.to_string()],
            "and still one after its next report"
        );
        let held: Vec<String> = a.snapshot().into_iter().map(|v| v.instance_uid).collect();
        assert_eq!(
            held,
            [new.to_string()],
            "A holds it under the new identity alone"
        );
    }

    /// A re-key whose answer was lost and whose read back failed is read back at the Agent's next
    /// report, whether it asks again or not, and assigns the identity the record moved to; no other
    /// is minted and no record is created for the old one.
    /// Verifies: ADR-0025
    #[test]
    fn a_rekey_whose_read_back_failed_is_read_back_at_the_next_report() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let (losing, failing) = (hooked.losing_rekeys.clone(), hooked.failing_moved.clone());
        let a = on_postgres(&database, Box::new(hooked));
        a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let uid = InstanceUid([0x2a; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        losing.store(true, Ordering::SeqCst);
        failing.store(true, Ordering::SeqCst);
        let mut rekey = report(&uid, 2);
        rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
        let first = a.process(rekey, Transport::WebSocket, Some(7));
        losing.store(false, Ordering::SeqCst);
        assert_eq!(first.uid, Some(uid), "not known: it keeps its identity");
        assert!(first.reply.agent_identification.is_none());
        let stored = || -> Vec<String> {
            test.rows("SELECT instance_uid FROM agents")
                .into_iter()
                .flatten()
                .flatten()
                .collect()
        };
        let moved_to = stored();
        assert_eq!(moved_to.len(), 1);
        assert_ne!(moved_to[0], uid.to_string(), "the move committed");
        let unknown = a.process(report(&uid, 3), Transport::WebSocket, Some(7));
        assert_eq!(unknown.uid, Some(uid), "not known still");
        // Held no more, as after a restart of what is held: a report under the old identity is
        // refused while the move is not known, and creates nothing.
        a.fleet.lock().expect("fleet lock").remove(&uid);
        let refused = a.process(report(&uid, 4), Transport::WebSocket, Some(7));
        assert_eq!(refused.uid, None, "not carried");
        assert_eq!(
            refused.reply.error_response.as_ref().map(|e| e.r#type),
            Some(ServerErrorResponseType::Unavailable as i32),
            "refused for now"
        );
        assert_eq!(stored(), moved_to, "no record for the old identity");
        failing.store(false, Ordering::SeqCst);
        let next = a.process(report(&uid, 5), Transport::WebSocket, Some(7));
        let new = next.uid.expect("carried");
        assert_eq!(new.to_string(), moved_to[0], "the identity it moved to");
        assert_eq!(
            next.reply
                .agent_identification
                .as_ref()
                .map(|id| id.new_instance_uid.clone()),
            Some(new.as_bytes().to_vec())
        );
        a.write_presence(LEASE_A).expect("A's batch");
        assert_eq!(
            stored(),
            moved_to,
            "one row, under the identity it moved to"
        );
        let held: Vec<String> = a.snapshot().into_iter().map(|v| v.instance_uid).collect();
        assert_eq!(held, moved_to, "A holds it under that identity alone");
    }

    /// A re-key whose answer was lost and whose read back failed, whose commit lands after that:
    /// its own notification that the old identity is forgotten reads the re-key back, and what A
    /// holds moves to the new identity, the Agent's session not ended. A disconnect on that session
    /// under the old identity is written to the new row, and a report under the old identity, on a
    /// new connection, is assigned the new one; one row names the Agent throughout.
    /// Verifies: ADR-0025
    #[test]
    fn a_rekey_whose_read_back_failed_is_taken_up_when_its_commit_lands() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.shared();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let failing = hooked.failing_moved.clone();
        let a = Arc::new(on_postgres(&database, Box::new(hooked)));
        a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let following = {
            let _entered = runtime.enter();
            crate::instance::follow(&database, a.clone(), None)
        };
        until("the first read once listening", || following.reads() > 0);
        spare_connections(&database);
        let uid = InstanceUid([0x2c; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        a.write_presence(LEASE_A).expect("A's batch");
        let gate = test.hold_commits("agents", 0x28_0007);
        failing.store(true, Ordering::SeqCst);
        let mut rekey = report(&uid, 2);
        rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
        let answered = losing_the_answer_of(&database, || {
            a.process(rekey, Transport::WebSocket, Some(7))
        });
        assert_eq!(answered.uid, Some(uid), "not known: it keeps its identity");
        assert!(answered.reply.agent_identification.is_none());
        failing.store(false, Ordering::SeqCst);
        gate.open();
        let stored = || -> Vec<String> {
            test.rows("SELECT instance_uid FROM agents")
                .into_iter()
                .flatten()
                .flatten()
                .collect()
        };
        until("the move landed", || {
            stored().first().is_some_and(|row| *row != uid.to_string())
        });
        let new = InstanceUid::parse(&stored()[0]).expect("a uid");
        let held = || -> Vec<String> { a.snapshot().into_iter().map(|v| v.instance_uid).collect() };
        until("A holds it under the new identity", || {
            held() == [new.to_string()]
        });
        assert!(a.take_forgotten(7).is_empty(), "the session is not ended");
        a.mark_disconnected(&[uid], 7);
        a.write_presence(LEASE_A).expect("A's batch");
        assert!(
            stored_presence(&test, &new).starts_with("none false"),
            "the disconnect is written to the new row: {}",
            stored_presence(&test, &new)
        );
        let next = a.process(report(&uid, 3), Transport::WebSocket, Some(8));
        assert_eq!(next.uid, Some(new), "assigned the identity it moved to");
        assert_eq!(
            next.reply
                .agent_identification
                .as_ref()
                .map(|id| id.new_instance_uid.clone()),
            Some(new.as_bytes().to_vec())
        );
        a.write_presence(LEASE_A).expect("A's batch");
        assert_eq!(stored(), [new.to_string()], "one row throughout");
        assert_eq!(held(), [new.to_string()]);
    }

    /// Instance A, its store hooked, following on a runtime of its own, and an Agent it carries on
    /// connection 7 whose re-key lost its answer and whose read back failed; the move has
    /// committed since, and its own notification's read back failed too, so A still holds the
    /// Agent under its old identity. Answers A, the switch that fails the read backs, the old and
    /// the new identity, and what keeps the instance running.
    fn rekey_unknown_after_it_landed(
        test: &crate::postgres::testing::TestDatabase,
    ) -> (
        Arc<AppState>,
        Arc<std::sync::atomic::AtomicBool>,
        InstanceUid,
        InstanceUid,
        tokio::runtime::Runtime,
    ) {
        let database = test.shared();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let (failing, reads) = (hooked.failing_moved.clone(), hooked.moved_reads.clone());
        let a = Arc::new(on_postgres(&database, Box::new(hooked)));
        a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let following = {
            let _entered = runtime.enter();
            crate::instance::follow(&database, a.clone(), None)
        };
        until("the first read once listening", || following.reads() > 0);
        spare_connections(&database);
        let uid = InstanceUid([0x2d; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        a.write_presence(LEASE_A).expect("A's batch");
        let gate = test.hold_commits("agents", 0x28_0008);
        failing.store(true, Ordering::SeqCst);
        let mut rekey = report(&uid, 2);
        rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
        let answered = losing_the_answer_of(&database, || {
            a.process(rekey, Transport::WebSocket, Some(7))
        });
        assert_eq!(answered.uid, Some(uid), "not known: it keeps its identity");
        let asked = reads.load(Ordering::SeqCst);
        gate.open();
        let rows = || -> Vec<String> {
            test.rows("SELECT instance_uid FROM agents")
                .into_iter()
                .flatten()
                .flatten()
                .collect()
        };
        until("the move landed", || {
            rows().first().is_some_and(|row| *row != uid.to_string())
        });
        until("its notification's read back tried", || {
            reads.load(Ordering::SeqCst) > asked
        });
        let new = InstanceUid::parse(&rows()[0]).expect("a uid");
        (a, failing, uid, new, runtime)
    }

    /// The held identities of `a`.
    fn held_by(a: &AppState) -> Vec<String> {
        a.snapshot().into_iter().map(|v| v.instance_uid).collect()
    }

    /// A re-key whose read back fails at its own notification too keeps the Agent and its session
    /// under the old identity, and is read back again until it holds: a session that ended
    /// meanwhile has its disconnect written to the row the move landed in, though a batch wrote
    /// it to the old row before, and the next report
    /// under the old identity is assigned the new one.
    /// Verifies: ADR-0025
    #[test]
    fn a_rekey_whose_read_back_fails_at_its_notification_keeps_the_agent_until_it_holds() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let (a, failing, uid, new, _runtime) = rekey_unknown_after_it_landed(&test);
        assert_eq!(
            held_by(&a),
            [uid.to_string()],
            "kept under the old identity"
        );
        assert!(a.take_forgotten(7).is_empty(), "the session is not ended");
        a.mark_disconnected(&[uid], 7);
        // A batch while the move is not known writes the disconnect to the old row, which is gone.
        a.write_presence(LEASE_A).expect("A's batch");
        failing.store(false, Ordering::SeqCst);
        until("read back again until it held", || {
            held_by(&a) == [new.to_string()]
        });
        a.write_presence(LEASE_A).expect("A's batch");
        assert!(
            stored_presence(&test, &new).starts_with("none false"),
            "the disconnect is written to the new row: {}",
            stored_presence(&test, &new)
        );
        let next = a.process(report(&uid, 3), Transport::WebSocket, Some(8));
        assert_eq!(next.uid, Some(new), "assigned the identity it moved to");
        let rows: Vec<String> = test
            .rows("SELECT instance_uid FROM agents")
            .into_iter()
            .flatten()
            .flatten()
            .collect();
        assert_eq!(rows, [new.to_string()], "one row throughout");
    }

    /// A report whose write finds the old row gone, while the re-key that moved it is not known,
    /// keeps the Agent and its session and the pending re-key, and creates nothing; once the
    /// store answers, the next report is assigned the new identity.
    /// Verifies: ADR-0025
    #[test]
    fn a_report_that_finds_the_old_row_gone_keeps_a_rekey_not_known() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let (a, failing, uid, new, _runtime) = rekey_unknown_after_it_landed(&test);
        let mut moved = report(&uid, 3);
        moved.agent_description = Some(AgentDescription::default());
        let answered = a.process(moved, Transport::WebSocket, Some(7));
        assert_eq!(answered.uid, Some(uid), "the connection still carries it");
        assert_eq!(
            held_by(&a),
            [uid.to_string()],
            "kept under the old identity"
        );
        assert!(a.take_forgotten(7).is_empty(), "the session is not ended");
        failing.store(false, Ordering::SeqCst);
        let next = a.process(report(&uid, 4), Transport::WebSocket, Some(7));
        assert_eq!(next.uid, Some(new), "assigned the identity it moved to");
        a.write_presence(LEASE_A).expect("A's batch");
        let rows: Vec<String> = test
            .rows("SELECT instance_uid FROM agents")
            .into_iter()
            .flatten()
            .flatten()
            .collect();
        assert_eq!(rows, [new.to_string()], "one row, under the new identity");
        assert_eq!(held_by(&a), [new.to_string()]);
    }

    /// The read back of a take still in flight waits for it: an Agent whose take's answer was lost
    /// and that disconnected has its disconnect written under the count that take commits, so the
    /// row names no holder once the batch ran.
    /// Verifies: ADR-0025
    #[test]
    fn the_read_back_of_a_take_waits_for_a_take_still_in_flight() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, _b, hooks) = hooked_pair(&database);
        let uid = InstanceUid([0x2b; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        a.mark_disconnected(&[uid], 7);
        a.write_presence(LEASE_A).expect("A's batch");
        assert_eq!(stored_presence(&test, &uid), "none false 1");
        // A takes it on a new connection, and the take's call gives up before the take is
        // written, which commits after A read its row back.
        hooks.failing_takes.store(true, Ordering::SeqCst);
        a.process(report(&uid, 2), Transport::WebSocket, Some(8));
        hooks.failing_takes.store(false, Ordering::SeqCst);
        a.mark_disconnected(&[uid], 8);
        let gate = test.gate(0x28_0005);
        std::thread::scope(|scope| {
            let in_flight = scope.spawn(|| {
                test.sql(&format!(
                    "BEGIN; UPDATE agents SET connected = true, holder = '{LEASE_A}', \
                     takes = takes + 1 WHERE instance_uid = '{uid}'; \
                     SELECT pg_advisory_lock_shared({}); COMMIT;",
                    0x28_0005
                ));
            });
            until("the take holding the row", || test.waits_for(0x28_0005));
            let batch = scope.spawn(|| a.write_presence(LEASE_A));
            until("A's batch waiting for the take, or done", || {
                batch.is_finished() || test.sessions("wait_event IN ('transactionid', 'tuple')") > 0
            });
            gate.open();
            in_flight.join().expect("the take's thread");
            batch
                .join()
                .expect("the batch's thread")
                .expect("A's batch");
        });
        assert_eq!(stored_presence(&test, &uid), "none false 2");
    }

    /// A batch held between its commit and its answers, while the Agent is forgotten through B,
    /// comes back through B and reconnects to A, which takes it again, leaves the newer take's count kept, though
    /// the record counts from the start again and the batch answered a greater one: the Agent's
    /// goodbye is then written.
    /// Verifies: ADR-0025
    #[test]
    fn a_batch_answered_after_a_newer_take_keeps_that_takes_count() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b, hooks) = hooked_pair(&database);
        let uid = InstanceUid([0x79; 16]);
        for conn in [1, 2] {
            a.process(report(&uid, conn), Transport::WebSocket, Some(conn));
            a.mark_disconnected(&[uid], conn);
        }
        a.process(report(&uid, 3), Transport::WebSocket, Some(3));
        next_millisecond();
        a.process(report(&uid, 4), Transport::WebSocket, Some(3));
        assert_eq!(stored_presence(&test, &uid), format!("{LEASE_A} true 3"));
        let (again, through) = (a.clone(), b.clone());
        *hooks.after_presence.lock().expect("hook lock") = Some(Box::new(move || {
            assert!(through.forget_agent(&uid).is_ok(), "forgotten through B");
            through.process(report(&uid, 1), Transport::WebSocket, Some(5));
            again.mark_disconnected(&[uid], 3);
            again.process(report(&uid, 2), Transport::WebSocket, Some(6));
        }));
        a.write_presence(LEASE_A).expect("A's batch");
        assert!(
            hooks.after_presence.lock().expect("hook lock").is_none(),
            "the take came before the answers"
        );
        assert_eq!(stored_presence(&test, &uid), format!("{LEASE_A} true 2"));
        a.mark_disconnected(&[uid], 6);
        a.write_presence(LEASE_A).expect("A's next batch");
        assert_eq!(stored_presence(&test, &uid), "none false 2", "the goodbye");
    }

    /// An Agent that A took and that B took since does not name A after A's batch: neither once B
    /// wrote its disconnect, before A heard of the take, nor while B, whose lease is not live,
    /// still names itself; A stops carrying both when it hears of the takes.
    /// Verifies: ADR-0025
    #[test]
    fn a_batch_names_its_instance_over_no_take_counted_since_its_own() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b, _hooks) = hooked_pair(&database);
        let (gone, dead) = (InstanceUid([0x7a; 16]), InstanceUid([0x7b; 16]));
        for (conn, uid) in [(7, gone), (8, dead)] {
            a.process(report(&uid, 1), Transport::WebSocket, Some(conn));
        }
        next_millisecond();
        // Heartbeats A has not written yet, then the Agents move to B.
        for (conn, uid) in [(7, gone), (8, dead)] {
            a.process(report(&uid, 2), Transport::WebSocket, Some(conn));
            b.process(report(&uid, 3), Transport::WebSocket, Some(9));
        }
        b.mark_disconnected(&[gone], 9);
        b.write_presence(LEASE_B).expect("B's batch");
        assert_eq!(stored_presence(&test, &gone), "none false 2");
        assert_eq!(stored_presence(&test, &dead), format!("{LEASE_B} true 2"));

        a.write_presence(LEASE_A).expect("A's batch");
        assert_eq!(
            stored_presence(&test, &gone),
            "none false 2",
            "B's goodbye stays"
        );
        assert_eq!(
            stored_presence(&test, &dead),
            format!("{LEASE_B} true 2"),
            "B's take stays"
        );
        a.catch_up().expect("A hears of the takes");
        for uid in [gone, dead] {
            assert!(!view(&a, &uid).connected, "A no longer carries {uid}");
        }
    }

    /// An Agent that disconnected under a lease A then lost has its record flushed at A's
    /// shutdown, whether or not A wrote a batch under its new lease: the count of takes outlives
    /// the lease it was answered under.
    /// Verifies: ADR-0025
    #[test]
    fn an_agent_disconnected_under_a_lost_lease_is_flushed_at_shutdown() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let a = on_postgres(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
        );
        let (lease, leases) = tokio::sync::watch::channel(Some(LEASE_A.to_string()));
        a.serve_under(leases);
        let uid = InstanceUid([0x7c; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        a.process(report(&uid, 2), Transport::WebSocket, Some(7));
        a.mark_disconnected(&[uid], 7);
        a.write_presence(LEASE_A).expect("A's batch");
        assert_eq!(stored_presence(&test, &uid), "none false 1");
        let renewed = "00000000-0000-7000-8000-0000000000aa";
        lease.send_replace(Some(renewed.to_string()));
        let sequence = || {
            test.rows(&format!(
                "SELECT sequence_num::text FROM agents WHERE instance_uid = '{uid}'"
            ))
        };
        let forget = || {
            test.sql(&format!(
                "UPDATE agents SET sequence_num = 0 WHERE instance_uid = '{uid}'"
            ));
        };

        forget();
        a.flush_agents();
        assert_eq!(
            sequence(),
            [[Some("2".to_string())]],
            "flushed before a batch"
        );
        forget();
        a.write_presence(renewed)
            .expect("A's batch under its new lease");
        a.flush_agents();
        assert_eq!(sequence(), [[Some("2".to_string())]], "and after one");
    }

    /// An Agent whose row named another holder when A's batch read it, and that connected to A
    /// again before the batch went on, with that take's answer lost too, and disconnected, stays
    /// unsure: A's next batch reads it again and writes its goodbye.
    /// Verifies: ADR-0025
    #[test]
    fn an_agent_taken_again_after_the_batch_read_its_row_is_read_again() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b, hooks) = hooked_pair(&database);
        let uid = InstanceUid([0x7d; 16]);
        hooks.losing_takes.store(true, Ordering::SeqCst);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        b.process(report(&uid, 2), Transport::WebSocket, Some(9));
        a.mark_disconnected(&[uid], 7);
        let again = a.clone();
        *hooks.after_takes.lock().expect("hook lock") = Some(Box::new(move || {
            again.process(report(&uid, 3), Transport::WebSocket, Some(8));
            again.mark_disconnected(&[uid], 8);
        }));
        a.write_presence(LEASE_A).expect("A's batch");
        hooks.losing_takes.store(false, Ordering::SeqCst);
        assert_eq!(stored_presence(&test, &uid), format!("{LEASE_A} true 3"));
        a.write_presence(LEASE_A).expect("A's next batch");
        assert_eq!(stored_presence(&test, &uid), "none false 3", "the goodbye");
    }

    /// Writes a live lease of `instance_id`, which the presence batches then read as live.
    fn live_lease(test: &crate::postgres::testing::TestDatabase, instance_id: &str) {
        test.sql(&format!(
            "INSERT INTO leases (instance_id, host, must_be_alone, taken_at, renewed_at, expires_at) \
             VALUES ('{instance_id}', 'b', false, now(), now(), now() + interval '1 hour')"
        ));
    }

    /// An Agent that B took and disconnected, which then connects to A again with a take that is
    /// not written, is named by A's next batch: a take that failed leaves A no count, so the batch
    /// takes back a row no instance holds whatever its count, and A, which carries the Agent, is
    /// named.
    /// Verifies: ADR-0025
    #[test]
    fn a_connect_whose_take_was_not_written_is_named_by_the_next_batch() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b, hooks) = hooked_pair(&database);
        let uid = InstanceUid([0x91; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        b.process(report(&uid, 2), Transport::WebSocket, Some(9));
        b.mark_disconnected(&[uid], 9);
        b.write_presence(LEASE_B).expect("B's batch");
        assert_eq!(stored_presence(&test, &uid), "none false 2");
        a.catch_up().expect("A hears of the take");
        hooks.failing_takes.store(true, Ordering::SeqCst);
        a.process(report(&uid, 3), Transport::WebSocket, Some(8));
        hooks.failing_takes.store(false, Ordering::SeqCst);
        assert!(view(&a, &uid).connected, "A carries it");
        assert_eq!(
            stored_presence(&test, &uid),
            "none false 2",
            "the take is not written"
        );
        a.write_presence(LEASE_A).expect("A's batch");
        a.write_presence(LEASE_A).expect("A's next batch");
        assert_eq!(stored_presence(&test, &uid), format!("{LEASE_A} true 3"));
    }

    /// An Agent whose take on A was not written, and that disconnected before A's next batch, has
    /// its last record written by A's shutdown flush: the batch read a row that names no holder
    /// and no take since, so what A heard of it last is the newest.
    /// Verifies: ADR-0025
    #[test]
    fn the_shutdown_flush_writes_an_agent_whose_take_was_not_written() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, _b, hooks) = hooked_pair(&database);
        let uid = InstanceUid([0x92; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        a.mark_disconnected(&[uid], 7);
        a.write_presence(LEASE_A).expect("A's batch");
        assert_eq!(stored_presence(&test, &uid), "none false 1");
        hooks.failing_takes.store(true, Ordering::SeqCst);
        a.process(report(&uid, 2), Transport::WebSocket, Some(8));
        hooks.failing_takes.store(false, Ordering::SeqCst);
        a.mark_disconnected(&[uid], 8);
        a.write_presence(LEASE_A).expect("A's batch");
        assert_eq!(
            stored_presence(&test, &uid),
            "none false 1",
            "the take is not written"
        );
        test.sql(&format!(
            "UPDATE agents SET sequence_num = 0 WHERE instance_uid = '{uid}'"
        ));
        a.flush_agents();
        assert_eq!(
            test.rows(&format!(
                "SELECT sequence_num::text FROM agents WHERE instance_uid = '{uid}'"
            )),
            [[Some("2".to_string())]],
            "the last record is written"
        );
    }

    /// An Agent whose take on A was not written, that B took and let go before A's next batch
    /// read the row, keeps what B flushed of it at A's shutdown flush: the row names no holder,
    /// but B's take counted since the one A was answered.
    /// Verifies: ADR-0025
    #[test]
    fn the_shutdown_flush_of_a_lost_take_keeps_a_take_counted_since() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b, hooks) = hooked_pair(&database);
        let uid = InstanceUid([0x93; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        a.mark_disconnected(&[uid], 7);
        a.write_presence(LEASE_A).expect("A's batch");
        hooks.failing_takes.store(true, Ordering::SeqCst);
        a.process(report(&uid, 2), Transport::WebSocket, Some(8));
        hooks.failing_takes.store(false, Ordering::SeqCst);
        a.mark_disconnected(&[uid], 8);
        b.process(report(&uid, 5), Transport::WebSocket, Some(9));
        b.mark_disconnected(&[uid], 9);
        b.write_presence(LEASE_B).expect("B's batch");
        b.flush_agents();
        assert_eq!(stored_presence(&test, &uid), "none false 2");
        a.write_presence(LEASE_A).expect("A's batch");
        a.flush_agents();
        assert_eq!(
            test.rows(&format!(
                "SELECT sequence_num::text FROM agents WHERE instance_uid = '{uid}'"
            )),
            [[Some("5".to_string())]],
            "B's record stays"
        );
    }

    /// An Agent first heard on A, whose record A created and whose take was not written, and that
    /// disconnected before A's next batch, has its last record written by A's shutdown flush: the
    /// record counted no take when A created it.
    /// Verifies: ADR-0025
    #[test]
    fn the_shutdown_flush_writes_an_agent_created_here_whose_take_was_not_written() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, _b, hooks) = hooked_pair(&database);
        let uid = InstanceUid([0x94; 16]);
        hooks.failing_takes.store(true, Ordering::SeqCst);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        hooks.failing_takes.store(false, Ordering::SeqCst);
        a.process(report(&uid, 2), Transport::WebSocket, Some(7));
        a.mark_disconnected(&[uid], 7);
        a.write_presence(LEASE_A).expect("A's batch");
        assert_eq!(
            stored_presence(&test, &uid),
            "none false 0",
            "the take is not written"
        );
        test.sql(&format!(
            "UPDATE agents SET sequence_num = 0 WHERE instance_uid = '{uid}'"
        ));
        a.flush_agents();
        assert_eq!(
            test.rows(&format!(
                "SELECT sequence_num::text FROM agents WHERE instance_uid = '{uid}'"
            )),
            [[Some("2".to_string())]],
            "the last record is written"
        );
    }

    /// An Agent that B carried and wrote the disconnect of, and that A then carried with a take that
    /// was not written, keeps what A wrote of it at B's shutdown flush: the row names no holder at
    /// the count B was answered, but holds the higher sequence number of A's newer record.
    /// Verifies: ADR-0029
    #[test]
    fn the_shutdown_flush_keeps_a_newer_carriers_record_whose_take_was_not_written() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b, hooks) = hooked_pair(&database);
        let uid = InstanceUid([0x95; 16]);
        b.process(report(&uid, 1), Transport::WebSocket, Some(9));
        b.mark_disconnected(&[uid], 9);
        b.write_presence(LEASE_B).expect("B's batch");
        assert_eq!(stored_presence(&test, &uid), "none false 1");
        hooks.failing_takes.store(true, Ordering::SeqCst);
        let mut newer = report(&uid, 2);
        newer.health = Some(opamp::proto::ComponentHealth {
            healthy: true,
            status: "running".to_string(),
            ..Default::default()
        });
        a.process(newer, Transport::WebSocket, Some(7));
        hooks.failing_takes.store(false, Ordering::SeqCst);
        assert_eq!(
            stored_presence(&test, &uid),
            "none false 1",
            "A's take is not written, and its report takes nothing"
        );
        let stored = || {
            test.rows(&format!(
                "SELECT sequence_num::text, (health IS NOT NULL)::text FROM agents \
                 WHERE instance_uid = '{uid}'"
            ))
        };
        assert_eq!(
            stored(),
            [[Some("2".to_string()), Some("true".to_string())]],
            "A wrote its report"
        );
        b.flush_agents();
        assert_eq!(
            stored(),
            [[Some("2".to_string()), Some("true".to_string())]],
            "A's newer record stays"
        );
    }

    /// An Agent another instance with a live lease holds is not taken by a report of A, whose take
    /// was not written: the report is written, and the holder and its count stay. The holder's
    /// shutdown flush, as it stops, writes no lower sequence number over A's report.
    /// Verifies: ADR-0025
    #[test]
    fn a_report_whose_take_was_not_written_takes_no_agent_a_live_instance_holds() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b, hooks) = hooked_pair(&database);
        live_lease(&test, LEASE_B);
        let uid = InstanceUid([0x96; 16]);
        b.process(report(&uid, 1), Transport::WebSocket, Some(9));
        assert_eq!(stored_presence(&test, &uid), format!("{LEASE_B} true 1"));
        hooks.failing_takes.store(true, Ordering::SeqCst);
        let mut newer = report(&uid, 2);
        newer.health = Some(opamp::proto::ComponentHealth {
            healthy: true,
            ..Default::default()
        });
        a.process(newer, Transport::WebSocket, Some(7));
        hooks.failing_takes.store(false, Ordering::SeqCst);
        assert_eq!(stored_presence(&test, &uid), format!("{LEASE_B} true 1"));
        let stored = || {
            test.rows(&format!(
                "SELECT sequence_num::text FROM agents WHERE instance_uid = '{uid}'"
            ))
        };
        assert_eq!(stored(), [[Some("2".to_string())]], "A's report is written");
        b.flush_agents();
        assert_eq!(
            stored(),
            [[Some("2".to_string())]],
            "B's flush writes no older sequence number over A's report"
        );
    }

    /// The sequence number and health stored of `uid`, as text.
    fn stored_report(
        test: &crate::postgres::testing::TestDatabase,
        uid: &InstanceUid,
    ) -> Vec<Vec<Option<String>>> {
        test.rows(&format!(
            "SELECT sequence_num::text, (health IS NOT NULL)::text FROM agents \
             WHERE instance_uid = '{uid}'"
        ))
    }

    /// The report of `uid` with `sequence_num` and a health.
    fn healthy_report(uid: &InstanceUid, sequence_num: u64) -> AgentToServer {
        let mut healthy = report(uid, sequence_num);
        healthy.health = Some(opamp::proto::ComponentHealth {
            healthy: true,
            ..Default::default()
        });
        healthy
    }

    /// An Agent that B carried up to a high sequence number and wrote the disconnect of, that
    /// started again and is carried by A with a take that was not written, has A's record again at
    /// A's next report, though B's shutdown flush wrote its older record over A's: A, answered no
    /// count for the Agent, writes every report of it.
    /// Verifies: ADR-0029, ADR-0025
    #[test]
    fn a_record_an_older_flush_wrote_over_a_restarted_agent_heals_at_its_next_report() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b, hooks) = hooked_pair(&database);
        let uid = InstanceUid([0x98; 16]);
        b.process(report(&uid, 50), Transport::WebSocket, Some(9));
        b.mark_disconnected(&[uid], 9);
        b.write_presence(LEASE_B).expect("B's batch");
        assert_eq!(stored_presence(&test, &uid), "none false 1");
        hooks.failing_takes.store(true, Ordering::SeqCst);
        a.process(healthy_report(&uid, 1), Transport::WebSocket, Some(7));
        hooks.failing_takes.store(false, Ordering::SeqCst);
        assert_eq!(
            stored_report(&test, &uid),
            [[Some("1".to_string()), Some("true".to_string())]],
            "A wrote its report"
        );
        b.flush_agents();
        assert_eq!(
            stored_report(&test, &uid),
            [[Some("50".to_string()), Some("false".to_string())]],
            "B's flush finds no holder, its count, and a lower sequence number"
        );
        a.process(report(&uid, 2), Transport::WebSocket, Some(7));
        assert_eq!(
            stored_report(&test, &uid),
            [[Some("2".to_string()), Some("true".to_string())]],
            "A's next report writes A's record again"
        );
        a.write_presence(LEASE_A).expect("A's batch");
        assert_eq!(
            stored_presence(&test, &uid),
            format!("{LEASE_A} true 2"),
            "the batch takes the Agent and answers a count"
        );
        a.process(report(&uid, 3), Transport::WebSocket, Some(7));
        let sequence = || {
            test.rows(&format!(
                "SELECT sequence_num::text FROM agents WHERE instance_uid = '{uid}'"
            ))
        };
        assert_eq!(
            sequence(),
            [[Some("3".to_string())]],
            "no digest was kept before"
        );
        test.sql(&format!(
            "UPDATE agents SET sequence_num = 0 WHERE instance_uid = '{uid}'"
        ));
        a.process(report(&uid, 4), Transport::WebSocket, Some(7));
        assert_eq!(
            sequence(),
            [[Some("0".to_string())]],
            "with a count, a report that changes nothing durable writes nothing"
        );
    }

    /// An Agent that B, stopping, still holds with a live lease, that started again and is carried
    /// by A with a take that was not written, has A's record again at A's next report, though B's
    /// shutdown flush wrote its older record over A's.
    /// Verifies: ADR-0029, ADR-0025
    #[test]
    fn a_record_a_stopping_holders_flush_wrote_over_a_restarted_agent_heals_at_its_next_report() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b, hooks) = hooked_pair(&database);
        live_lease(&test, LEASE_B);
        let uid = InstanceUid([0x99; 16]);
        b.process(report(&uid, 50), Transport::WebSocket, Some(9));
        assert_eq!(stored_presence(&test, &uid), format!("{LEASE_B} true 1"));
        hooks.failing_takes.store(true, Ordering::SeqCst);
        a.process(healthy_report(&uid, 1), Transport::WebSocket, Some(7));
        hooks.failing_takes.store(false, Ordering::SeqCst);
        b.flush_agents();
        assert_eq!(
            stored_report(&test, &uid),
            [[Some("50".to_string()), Some("false".to_string())]],
            "B's flush, holding the row, finds a lower sequence number"
        );
        a.process(report(&uid, 2), Transport::WebSocket, Some(7));
        assert_eq!(
            stored_report(&test, &uid),
            [[Some("2".to_string()), Some("true".to_string())]],
            "A's next report writes A's record again"
        );
    }

    /// An Agent A carried, that was forgotten through B and created again there, and that A then
    /// carries with a take that was not written, keeps what A wrote of it at B's shutdown flush:
    /// the count B was answered of the record created again is the one stored, but the row holds
    /// the higher sequence number of A's report.
    /// Verifies: ADR-0029, ADR-0025
    #[test]
    fn the_shutdown_flush_keeps_a_newer_carriers_record_after_a_record_created_again() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b, hooks) = hooked_pair(&database);
        let uid = InstanceUid([0x97; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        a.mark_disconnected(&[uid], 7);
        a.write_presence(LEASE_A).expect("A's batch");
        assert_eq!(stored_presence(&test, &uid), "none false 1");
        assert!(b.forget_agent(&uid).is_ok(), "forgotten through B");
        b.process(report(&uid, 1), Transport::WebSocket, Some(9));
        b.mark_disconnected(&[uid], 9);
        b.write_presence(LEASE_B).expect("B's batch");
        assert_eq!(
            stored_presence(&test, &uid),
            "none false 1",
            "created again"
        );
        hooks.failing_takes.store(true, Ordering::SeqCst);
        let mut newer = report(&uid, 2);
        newer.health = Some(opamp::proto::ComponentHealth {
            healthy: true,
            ..Default::default()
        });
        a.process(newer, Transport::WebSocket, Some(8));
        hooks.failing_takes.store(false, Ordering::SeqCst);
        b.flush_agents();
        assert_eq!(
            test.rows(&format!(
                "SELECT sequence_num::text, (health IS NOT NULL)::text FROM agents \
                 WHERE instance_uid = '{uid}'"
            )),
            [[Some("2".to_string()), Some("true".to_string())]],
            "A's newer record stays"
        );
    }

    /// An Agent whose take was written under A's old lease and whose answer was lost is named by
    /// A's batch under its new lease: A keeps no count from before the lost take to refuse it with.
    /// Verifies: ADR-0025
    #[test]
    fn a_lost_take_under_an_old_lease_is_named_by_the_batch_under_the_new_one() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let losing = hooked.losing_takes.clone();
        let a = on_postgres(&database, Box::new(hooked));
        let (lease, leases) = tokio::sync::watch::channel(Some(LEASE_A.to_string()));
        a.serve_under(leases);
        let uid = InstanceUid([0x93; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        a.mark_disconnected(&[uid], 7);
        a.write_presence(LEASE_A).expect("A's batch");
        assert_eq!(stored_presence(&test, &uid), "none false 1");
        losing.store(true, Ordering::SeqCst);
        a.process(report(&uid, 2), Transport::WebSocket, Some(8));
        losing.store(false, Ordering::SeqCst);
        assert_eq!(stored_presence(&test, &uid), format!("{LEASE_A} true 2"));
        let renewed = "00000000-0000-7000-8000-0000000000aa";
        lease.send_replace(Some(renewed.to_string()));
        a.write_presence(renewed)
            .expect("A's batch under its new lease");
        assert_eq!(stored_presence(&test, &uid), format!("{renewed} true 3"));
    }

    /// A batch never names its instance over a live take whose count equals its own: an Agent
    /// forgotten through B and created again there counts its takes from the start, and A, which
    /// has not heard of the forget, writes nothing of it.
    /// Verifies: ADR-0029, ADR-0025
    #[test]
    fn a_batch_never_names_its_instance_over_a_live_take_of_a_record_created_again() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b, _hooks) = hooked_pair(&database);
        live_lease(&test, LEASE_B);
        let uid = InstanceUid([0x92; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        next_millisecond();
        a.process(report(&uid, 2), Transport::WebSocket, Some(7));
        assert!(b.forget_agent(&uid).is_ok(), "forgotten through B");
        b.process(report(&uid, 1), Transport::WebSocket, Some(9));
        assert_eq!(stored_presence(&test, &uid), format!("{LEASE_B} true 1"));
        a.write_presence(LEASE_A)
            .expect("A's batch before it hears of the forget");
        assert_eq!(stored_presence(&test, &uid), format!("{LEASE_B} true 1"));
    }

    /// A batch that meets a change of lease between its commit and its answers keeps the counts
    /// it was answered: the Agent it named anew under the lease before is named again under the
    /// next one. Each batch takes back the row its instance's previous lease, now not live, holds
    /// under the count it was answered.
    /// Verifies: ADR-0025
    #[test]
    fn a_batch_that_meets_a_change_of_lease_keeps_the_counts_it_was_answered() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let after_presence = hooked.after_presence.clone();
        let a = Arc::new(on_postgres(&database, Box::new(hooked)));
        let (lease, leases) = tokio::sync::watch::channel(Some(LEASE_A.to_string()));
        a.serve_under(leases);
        let (uid, other) = (InstanceUid([0x94; 16]), InstanceUid([0x95; 16]));
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        assert_eq!(stored_presence(&test, &uid), format!("{LEASE_A} true 1"));
        let (second, third) = (
            "00000000-0000-7000-8000-0000000000a2",
            "00000000-0000-7000-8000-0000000000a3",
        );
        lease.send_replace(Some(second.to_string()));
        // Between the batch's commit and its answers the lease changes again, and a connect is
        // taken under the new one.
        let again = a.clone();
        *after_presence.lock().expect("hook lock") = Some(Box::new(move || {
            lease.send_replace(Some(third.to_string()));
            again.process(report(&other, 1), Transport::WebSocket, Some(8));
        }));
        a.write_presence(second)
            .expect("A's batch under its second lease");
        assert_eq!(stored_presence(&test, &uid), format!("{second} true 2"));
        a.write_presence(third)
            .expect("A's batch under its third lease");
        assert_eq!(stored_presence(&test, &uid), format!("{third} true 3"));
    }

    /// An instance whose take of an Agent another instance's take overtakes before it holds the
    /// Agent in memory stops carrying it once it hears of that take: hearing of it waits for the
    /// report under way here, so it reads the holder after this instance holds the Agent, and the
    /// newest connection keeps the Agent.
    /// Verifies: ADR-0025
    #[test]
    fn an_instance_whose_take_is_overtaken_before_it_holds_the_agent_stops_carrying_it() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let after_take = hooked.after_take.clone();
        let a = Arc::new(on_postgres(&database, Box::new(hooked)));
        a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let b = Arc::new(on_postgres(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
        ));
        b.serve_under(tokio::sync::watch::channel(Some(LEASE_B.to_string())).1);
        let uid = InstanceUid([0x94; 16]);
        b.process(report(&uid, 1), Transport::WebSocket, Some(3));

        // A's take commits; then B takes the Agent again, and A hears of it while its report has
        // not yet taken the Agent into what it holds.
        let (heard, hearing) = std::sync::mpsc::channel();
        let waits_before = a.agent_lock_waits();
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let (a, b, finished) = (Arc::downgrade(&a), b.clone(), finished.clone());
            *after_take.lock().expect("hook lock") = Some(Box::new(move || {
                // The Agent's connection to B closed, and it opens another to B.
                b.mark_disconnected(&[uid], 3);
                b.process(report(&uid, 2), Transport::WebSocket, Some(4));
                let a = a.upgrade().expect("A");
                let (on_a, done) = (a.clone(), finished.clone());
                std::thread::spawn(move || {
                    let read = on_a.release_taken(&HashSet::from([uid]));
                    done.store(true, Ordering::SeqCst);
                    let _ = heard.send(read);
                });
                // Hearing waits for this report, or is done before it goes on.
                until("A hears of B's take", || {
                    a.agent_lock_waits() > waits_before || finished.load(Ordering::SeqCst)
                });
            }));
        }
        a.process(report(&uid, 2), Transport::WebSocket, Some(7));
        hearing
            .recv_timeout(Duration::from_secs(30))
            .expect("A heard of the take")
            .expect("A read the holder");
        assert!(!view(&a, &uid).connected, "A no longer carries it");
        assert!(
            stored_presence(&test, &uid).starts_with(&format!("{LEASE_B} true")),
            "B holds it: {}",
            stored_presence(&test, &uid)
        );
    }

    /// What another instance commits to a new Agent's record between its creation here and the
    /// end of the report that created it is taken into what this instance holds when it hears of
    /// it: the record is held from its creation on, so the take-up finds it, and the report goes
    /// on from it rather than from a record of its own. A label is what such a commit can carry:
    /// a rollout then releases nothing and a restart is refused, since the record names neither a
    /// description nor a capability before the report that created it is written, and what is
    /// held agrees with the store on all three.
    /// Verifies: ADR-0025
    #[test]
    fn what_another_instance_commits_to_a_new_agent_while_its_report_runs_is_held() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let after_take = hooked.after_take.clone();
        let blobs = tempfile::tempdir().expect("blobs");
        let a = Arc::new(delivering_on(&database, Box::new(hooked), blobs.path()));
        a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let b = Arc::new(delivering_on(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
            blobs.path(),
        ));
        aim_at_the_early_ring(&b);
        let uid = InstanceUid([0x98; 16]);
        {
            let (a, b) = (Arc::downgrade(&a), b.clone());
            *after_take.lock().expect("hook lock") = Some(Box::new(move || {
                // The record is committed; B labels it, rolls its Deployment out to it and asks
                // it to restart, and A hears of it before its report holds the Agent.
                label(&b, &uid, &[("ring", "early")]);
                b.rollout_to_agent(&uid, None)
                    .expect("the rollout through B");
                assert!(
                    matches!(b.request_restart(&uid), Err(RestartError::NoCapability)),
                    "a restart is refused before the record names its capabilities"
                );
                let a = a.upgrade().expect("A");
                a.take_up_agents(&HashSet::from([uid]))
                    .expect("A takes it up");
            }));
        }
        let created = a.process(restartable(&uid, 1), Transport::WebSocket, Some(7));
        assert_eq!(created.uid, Some(uid));
        let stored = crate::postgres::agents::PgAgentStore::new(database.clone())
            .get(&uid)
            .expect("read")
            .expect("stored");
        let held = view(&a, &uid);
        assert_eq!(held.labels, stored.labels, "the label is held");
        assert_eq!(held.labels.get("ring").map(String::as_str), Some("early"));
        assert_eq!(
            held.assigned_deployment,
            stored
                .assignment
                .map(|assignment| assignment.deployment)
                .unwrap_or_default(),
            "the assignment held is the one stored"
        );
        assert!(!stored.restart_pending && created.reply.command.is_none());
    }

    /// A new Agent's record is held from its creation on but not carried until its report holds
    /// it: a presence batch run meanwhile writes nothing of it, and the Agent is not connected
    /// here until the report ends.
    /// Verifies: ADR-0025
    #[test]
    fn a_new_agents_record_is_held_from_its_creation_but_not_carried_before_its_report_ends() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let after_take = hooked.after_take.clone();
        let a = Arc::new(on_postgres(&database, Box::new(hooked)));
        a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let uid = InstanceUid([0x9d; 16]);
        let (looked, look) = std::sync::mpsc::channel();
        {
            let a = Arc::downgrade(&a);
            *after_take.lock().expect("hook lock") = Some(Box::new(move || {
                let a = a.upgrade().expect("A");
                let held = a
                    .fleet
                    .lock()
                    .expect("fleet lock")
                    .get(&uid)
                    .map(|record| (record.connected, record.owner));
                let _ = looked.send((held, a.write_presence(LEASE_A)));
            }));
        }
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        let (held, batch) = look.recv_timeout(Duration::from_secs(30)).expect("looked");
        assert_eq!(held, Some((false, None)), "held, not carried");
        assert_eq!(batch, Ok(0), "no presence written of it");
        assert!(view(&a, &uid).connected, "carried once the report ends");
    }

    /// A new Agent's report that the host register refuses leaves no record, neither stored nor
    /// held: the record its creation made goes from both.
    /// Verifies: ADR-0025
    #[test]
    fn a_new_agents_report_the_register_refuses_leaves_no_record_held_or_stored() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let probe = hooked.probe.clone();
        let a = on_postgres(&database, Box::new(hooked));
        let b = Arc::new(on_postgres(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
        ));
        let uid = InstanceUid([0x99; 16]);
        let subject = crate::revocation::SubjectValues::default;
        {
            // Before A creates the record, B binds the Agent to another host, and its record is
            // forgotten again, the binding staying: A's creation goes ahead, and the register
            // then refuses the report.
            let armed = Arc::new(std::sync::atomic::AtomicBool::new(true));
            let sql = test.sql_runner();
            *probe.lock().expect("hook lock") = Some(Arc::new(move |call, named| {
                if call != "create_within"
                    || named != Some(&uid)
                    || !armed.swap(false, Ordering::SeqCst)
                {
                    return;
                }
                report_over(&b, "h2", subject(), uid, &[]);
                sql(&format!("DELETE FROM agents WHERE instance_uid = '{uid}'"));
            }));
        }
        let refused = report_over(&a, "h1", subject(), uid, &[]);
        assert_eq!(refused.uid, None, "refused");
        assert!(
            !a.fleet.lock().expect("fleet lock").contains_key(&uid),
            "not held"
        );
        assert!(
            test.rows(&format!(
                "SELECT instance_uid FROM agents WHERE instance_uid = '{uid}'"
            ))
            .is_empty(),
            "not stored"
        );
    }

    /// A re-key an Agent asked for that a read again finds made before the report holds the Agent
    /// under its new identity keeps the Agent: the move is pending until then, so the read again
    /// reads it back and moves what is held, and the session goes on, its labels with it.
    /// Verifies: ADR-0025, ADR-0029
    #[test]
    fn a_rekey_a_read_again_finds_before_the_report_holds_it_keeps_the_agent() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let after_rekey = hooked.after_rekey.clone();
        let a = Arc::new(on_postgres(&database, Box::new(hooked)));
        a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let uid = InstanceUid([0x99; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        label(&a, &uid, &[("tier", "edge")]);
        {
            let on = Arc::downgrade(&a);
            *after_rekey.lock().expect("hook lock") = Some(Box::new(move || {
                // As on a notice naming it. A catch-up cannot come here: it waits for the
                // Agent's own lock, which the report holds.
                on.upgrade()
                    .expect("A")
                    .take_up_agents(&HashSet::from([uid]))
                    .expect("read again");
            }));
        }
        let mut rekey = report(&uid, 2);
        rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
        let answered = a.process(rekey, Transport::WebSocket, Some(7));
        let new = answered.uid.expect("carried");
        assert_ne!(new, uid, "re-keyed");
        assert!(a.take_forgotten(7).is_empty(), "its session goes on");
        assert_eq!(view(&a, &new).labels["tier"], "edge", "its labels with it");
    }

    /// The wait between two tries of a put-back doubles and stops at a minute of calls.
    #[test]
    fn the_wait_between_tries_of_a_put_back_grows_to_a_minute() {
        let mut backoff = Backoff::first();
        let mut waits = Vec::new();
        for _ in 0..8 {
            let mut calls = 1;
            while !backoff.tick() {
                calls += 1;
            }
            waits.push(calls);
            backoff.failed();
        }
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60]);
    }

    /// What another Server wrote to a held record is taken up from the store: a Client Agent's
    /// labels place its Client's other Agent again, so it inherits them, a queued restart is held,
    /// and a record the store no longer holds is dropped.
    /// Verifies: ADR-0025
    #[test]
    fn what_another_server_wrote_is_taken_up_and_places_the_group_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (client, supervisor) = (InstanceUid::default(), InstanceUid::default());
        let state = hosted_fleet(dir.path());
        report_over(
            &state,
            "h1",
            organization("Kunde GmbH"),
            client,
            &[("supervisor.kind.collector", "true")],
        );
        report_over(&state, "h1", organization("Kunde GmbH"), supervisor, &[]);
        let other = crate::fs::FsAgentStore::open(&dir.path().join("configs")).expect("open");
        let labels = BTreeMap::from([("site".to_string(), "wien".to_string())]);
        assert!(other.set_labels(&client, &labels).expect("labels"));
        assert_eq!(
            other.set_restart_pending(&supervisor, true),
            Ok(Some(false))
        );
        assert!(view(&state, &supervisor).inherited_labels.is_empty());

        state
            .take_up_agents(&HashSet::from([client, supervisor]))
            .expect("take up");
        assert_eq!(view(&state, &client).labels, labels);
        assert_eq!(view(&state, &supervisor).inherited_labels, labels);
        assert!(state.fleet.lock().expect("fleet")[&supervisor].restart_pending);

        other.remove(&client).expect("forget");
        state.catch_up().expect("take up");
        assert!(!state.fleet.lock().expect("fleet").contains_key(&client));
        assert!(
            view(&state, &supervisor).inherited_labels.is_empty(),
            "a forgotten Client Agent's labels are inherited no more"
        );
    }
}
