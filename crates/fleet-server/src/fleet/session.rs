//! The round of an Agent's report (ADR-0025, ADR-0029): a report processed under the Agent's
//! lock, its record created, taken or settled, the reply it is answered with, the connection
//! settings it is offered, the persist path that writes what changed, a goodbye and a forget, and
//! the answers every refused report gets. Its entry points are methods of `AppState`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};

use opamp::proto::{
    AgentConfigMap, AgentIdentification, AgentToServer, AgentToServerFlags,
    ConnectionSettingsOffers, OpAmpConnectionSettings, PackageStatuses, ServerErrorResponse,
    ServerErrorResponseType, ServerToAgent, ServerToAgentFlags, TlsCertificate,
};
use opamp::uid::InstanceUid;
use prost::Message as _;
use sha2::{Digest, Sha256};
use tokio::sync::watch;
use tracing::{info, warn};

use crate::agent_store::{Creation, PersistedAgent};
use crate::audit::Entry;
use crate::revocation::Presented;

use super::held::{Carried, PendingRekey, Settled, Taking, Took, UnsureRekey};
use super::placement::{Elsewhere, Group, Placement};
use super::rollout::{offer, restart_command};
use super::{
    is_silent, AgentRecord, AppState, ConnId, ForgetError, TelemetryOffer, Transport,
    SERVER_CAPABILITIES,
};

impl AppState {
    /// A fresh identity for one WebSocket connection.
    pub fn connection_id(&self) -> ConnId {
        self.next_conn.fetch_add(1, Ordering::Relaxed)
    }

    /// A receiver that fires whenever any Configuration changes; WebSocket loops use it to push
    /// offers without waiting for the Agent to speak.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.push.subscribe()
    }

    /// Forgets everything this Server knows about one Agent (ADR-0029): the record is dropped and
    /// the row leaves the fleet view. Nothing reaches the host — no process is stopped and no
    /// certificate revoked, since a certificate here proves fleet membership and its host, never
    /// which Agent is speaking (ADR-0027 §11). A Client still running therefore reappears on its next
    /// report, which this Server answers with `ReportFullState` as it does for any unknown Agent.
    /// The `instance_uid` is released from the Host it was bound to, so that report binds it to
    /// the host whose certificate carries it (ADR-0029 §10).
    ///
    /// Refused while the Agent is still reporting: the record holds the hashes that gate
    /// re-offering, so forgetting a live Agent has it offered its configuration again — and the
    /// Collector plugin restarts its Managed Process when a configuration arrives. An operator who
    /// wants a restart asks for one through [`request_restart`](Self::request_restart).
    ///
    /// `connected` alone would not do. Behind a Gateway the open connection is the *Gateway's*, so
    /// a Gatewayed Agent that died still reads as connected; and plain-HTTP polling has no socket
    /// to close, so an Agent that stops polling without saying goodbye stays connected forever.
    /// Silence is the second half of the test — and it is [`is_silent`], not
    /// [`is_stale`](super::is_stale): an Agent that declared no heartbeat is never called stale,
    /// and gating on staleness would leave
    /// its row on a decommissioned host permanently unremovable.
    pub fn forget_agent(&self, uid: &InstanceUid) -> Result<(), ForgetError> {
        let _agent = self.agent_lock(uid);
        let mut fleet = self.fleet.lock().expect("fleet lock");
        let (budget, now_ms) = (self.stale_after(), self.clock.now_ms());
        if let Some(record) = fleet.get(uid) {
            if record.connected && !is_silent(record, budget, now_ms) {
                return Err(ForgetError::StillReporting);
            }
        }
        // Forgetting that left a stored record behind would be the "remembering under another
        // name" ADR-0029 rejected: the record leaves the store with the row, its labels with it
        // (ADR-0029 §21). On a shared store the instance that carries the Agent may be
        // another, so what decides is the presence stored, read and removed in one step
        // (ADR-0029 §11).
        // On a shared store the record's removal answers what its stored assignment named, read
        // under the row's lock in the same step, so the revisions only it pinned are collected
        // whatever another instance released meanwhile.
        let mut stored_configurations = Vec::new();
        if self.agent_store.is_shared() {
            let silent_before = now_ms.saturating_sub(budget.as_millis() as u64);
            match self
                .agent_store
                .remove_if_silent(uid, silent_before)
                .map_err(ForgetError::Storage)?
            {
                Some(crate::agent_store::Silenced::Forgotten(configurations)) => {
                    stored_configurations = configurations;
                }
                Some(crate::agent_store::Silenced::Reporting) => {
                    return Err(ForgetError::StillReporting)
                }
                // Held but not stored: forgotten or re-keyed through another instance, and not yet
                // heard of here. What is held is forgotten.
                None if fleet.contains_key(uid) => {}
                None => return Err(ForgetError::UnknownAgent),
            }
        } else {
            if !fleet.contains_key(uid) {
                return Err(ForgetError::UnknownAgent);
            }
            self.unpersist(uid);
        }
        let removed = fleet.remove(uid);
        self.kept.forget(uid);
        // Its `instance_uid` leaves the Host it was bound to: whichever host reports it next binds
        // it (ADR-0029 §10).
        if let Some(revocations) = &self.revocations {
            if let Err(e) = revocations.release(uid.as_bytes()) {
                warn!(agent = %uid, error = %e, "cannot write the released host binding");
            }
        }
        if let Some(record) = removed {
            // A session that still carries it ends (ADR-0029 §10).
            self.end_session(uid, &record);
            // A Client Agent forgotten takes its labels from the Agents that inherited them.
            self.unplace(
                &mut fleet,
                uid,
                record.placement.group.clone(),
                &mut Elsewhere::Now,
            );
            // A pinned revision only this Agent referenced is unreferenced now (ADR-0032): what
            // it held, and what the store named, which a release through another instance may
            // have changed before its notification arrived here.
            let mut configurations: BTreeSet<String> =
                record.assigned_configurations().keys().cloned().collect();
            configurations.extend(stored_configurations);
            self.collect_revisions(configurations);
        } else {
            self.collect_revisions(stored_configurations);
        }
        drop(fleet);
        self.push.send_modify(|rev| *rev += 1);
        info!(agent = %uid, "agent forgotten");
        Ok(())
    }

    /// Persists one record when — and only when — its durable content changed since it was last
    /// written (ADR-0029). `last_seen_ms` and `sequence_num` are outside the comparison and ride
    /// along on whatever write happens, so the common heartbeat reaches no storage backend at
    /// all; [`flush_agents`](Self::flush_agents) is what makes them current on a graceful stop.
    /// A write that fails is logged, never fatal: a fleet that keeps running on a full disk beats
    /// one that refuses reports. Answers whether the record is still stored: on a shared store
    /// one forgotten through another instance is not, and is not written again.
    pub(super) fn persist_if_dirty(&self, uid: &InstanceUid, persisted: &PersistedAgent) -> bool {
        match self.persist(uid, persisted) {
            Ok(stored) => stored,
            Err(e) => {
                warn!(agent = %uid, error = %e, "cannot persist the agent record");
                true
            }
        }
    }

    /// Writes one record when its durable content changed since it was last written, and says
    /// whether the write failed — for an operator's act that must not be answered unwritten — or
    /// found no record to write, as of an Agent forgotten through another instance. The record was
    /// created or read before (ADR-0029 §14), so only what a report decides is written of
    /// it, and nothing creates it again.
    ///
    /// `persisted` is what the record held when the report let the fleet lock go, and it is
    /// written after: no other report waits for the write. Two writes of one record do not race,
    /// so an older state never lands after a newer one, since the Agent's own lock, which every
    /// report of it holds from its start to its answer, is held across the write; and a report is
    /// answered only once its record is written (ADR-0029 §4). What a store's write may take
    /// is bounded: a PostgreSQL call fails after `postgres::CALL_TIMEOUT` (ADR-0039 §24).
    ///
    /// On a shared store, an Agent this instance was answered no count of takes for, its take not
    /// written or its answer lost, has its record written at every report and no digest kept: a
    /// shutdown flush of another instance with a higher sequence number, as of an Agent that started
    /// again, may have written over it, and the next report writes it again. Once a count is
    /// answered, a report that changes nothing durable writes nothing again
    /// (ADR-0025 §8).
    fn persist(&self, uid: &InstanceUid, persisted: &PersistedAgent) -> Result<bool, String> {
        let digest = persisted.durable_digest();
        let counted = !self.agent_store.is_shared()
            || self
                .kept
                .presence_written
                .lock()
                .expect("presence lock")
                .counted(uid);
        if counted && self.kept.written.lock().expect("written lock").get(uid) == Some(&digest) {
            return Ok(true);
        }
        if !self.agent_store.write_report(uid, persisted)? {
            return Ok(false);
        }
        if !counted {
            self.kept.written.lock().expect("written lock").remove(uid);
            return Ok(true);
        }
        self.kept
            .written
            .lock()
            .expect("written lock")
            .insert(*uid, digest);
        Ok(true)
    }

    /// Drops one record from the store and the dirty-check ledger — the storage half of
    /// forgetting (ADR-0029) and of an identity reassignment.
    fn unpersist(&self, uid: &InstanceUid) {
        if let Err(e) = self.agent_store.remove(uid) {
            warn!(agent = %uid, error = %e, "cannot remove the agent record from the store");
        }
        self.kept.written.lock().expect("written lock").remove(uid);
    }

    /// Notes that `record`, just forgotten as `uid`, was carried by a live WebSocket, whose loop
    /// then ends its session.
    pub(super) fn end_session(&self, uid: &InstanceUid, record: &AgentRecord) {
        if let (true, Some(conn)) = (record.connected, record.owner) {
            self.forgotten
                .lock()
                .expect("forgotten lock")
                .entry(conn)
                .or_default()
                .push(*uid);
        }
    }

    /// Writes every record's current state, timestamps and sequence numbers included — the
    /// graceful-shutdown flush (ADR-0029) that lets the ordinary restart restore a fleet whose
    /// `last_seen` is current and whose next compressed report is accepted without a gap. On a
    /// shared store only the records of the Agents this instance heard from are written: of the
    /// others it holds what it loaded, and another instance carries them
    /// (ADR-0029 §2). Of those, only the ones no other
    /// instance holds the connection of: an Agent heard here that moved to another instance keeps
    /// what that instance wrote of it. Whether one moved is told by the count of takes this
    /// instance was last answered, never by comparing its clock with another's.
    pub fn flush_agents(&self) {
        let fleet = self.fleet.lock().expect("fleet lock");
        let shared = self.agent_store.is_shared();
        let holder = self.lease.get().and_then(|lease| lease.borrow().clone());
        // The counts outlive the lease they were answered under: a count still stored says no
        // instance took the Agent since, whatever lease this instance serves under now.
        // An Agent whose take lost its answer is flushed under the count answered before it, which
        // a take counted since, also its own lost one, no longer matches.
        let takes = self
            .kept
            .presence_written
            .lock()
            .expect("presence lock")
            .flushed_takes();
        let mut written = self.kept.written.lock().expect("written lock");
        for (uid, record) in fleet.iter().filter(|(_, record)| record.heard || !shared) {
            let persisted = record.to_persisted();
            match self.agent_store.write_report_unless_held_elsewhere(
                uid,
                &persisted,
                holder.as_deref(),
                takes.get(uid).copied(),
            ) {
                Ok(true) => {
                    written.insert(*uid, persisted.durable_digest());
                }
                // Forgotten through another instance, or held by another: not written.
                Ok(false) => {}
                Err(e) => warn!(agent = %uid, error = %e, "cannot flush the agent record"),
            }
        }
    }

    /// The control loop for one report, shared by both transports (ADR-0012): update what we know,
    /// then answer with what the Agent still lacks — the config offer gated by the hash comparison.
    /// `conn` identifies the WebSocket connection that carried the report; `None` for plain HTTP.
    ///
    /// The reported `instance_uid` is taken at face value: admission proved fleet membership, not
    /// which Agent is speaking, so within an admitted fleet a report's identity is self-asserted and
    /// not authorized against any other Agent (ADR-0026). The plain-HTTP path in particular offers
    /// nothing to tell two pollers apart; the WebSocket duplicate-`instance_uid` rekey below is
    /// collision handling, not authorization.
    pub fn process(
        &self,
        msg: AgentToServer,
        transport: Transport,
        conn: Option<ConnId>,
    ) -> Processed {
        self.process_presented(msg, transport, conn, None)
    }

    /// [`process`](Self::process) for a connection that presented a certificate: the host it was
    /// issued to binds the Agents it reports for, and a CSR without a renewal proof renews it
    /// unless its host is a Gateway's (ADR-0027 §11, ADR-0016).
    pub fn process_presented(
        &self,
        msg: AgentToServer,
        transport: Transport,
        conn: Option<ConnId>,
        presented: Option<&Presented>,
    ) -> Processed {
        let Some(reported) = InstanceUid::from_wire(&msg.instance_uid) else {
            warn!(
                len = msg.instance_uid.len(),
                "report with a malformed instance_uid"
            );
            return Processed {
                reply: bad_request("instance_uid must be 16 bytes (UUID v7 recommended)"),
                uid: None,
                disconnected: false,
            };
        };
        // The Agent's own lock, held across everything this report reads and writes in the
        // store: two reports of one Agent are taken up and written in order, a take is not
        // overtaken by the reading of the holder again, and a restart's clear by a request, a try
        // or a forget. The fleet lock is taken only to take into memory what was read and
        // written, so no other Agent's report waits for a call to the store.
        let _agent = self.agent_lock(&reported);
        let mut msg = msg;
        for _ in 0..REPORT_ROUNDS {
            match self.report_round(msg, transport, conn, presented, reported) {
                Ok(processed) => return processed,
                Err(again) => msg = *again,
            }
        }
        warn!(
            agent = %reported,
            "the agent left memory while each round of its report read the store"
        );
        Processed {
            reply: unavailable(
                &msg.instance_uid,
                "the Server cannot take this report up now; retry later",
            ),
            uid: None,
            disconnected: false,
        }
    }

    /// One round of [`process_presented`](Self::process_presented): what the report needs of the
    /// store is read and written first, then taken into memory under the fleet lock. A round
    /// whose Agent left memory meanwhile — forgotten, or moved through another instance — hands
    /// the report back, and the next round goes on from what the store holds then.
    fn report_round(
        &self,
        msg: AgentToServer,
        transport: Transport,
        conn: Option<ConnId>,
        presented: Option<&Presented>,
        reported: InstanceUid,
    ) -> Result<Processed, Box<AgentToServer>> {
        let mut sender = msg.instance_uid.clone();
        // The reply is addressed to the identity the Agent reported, even when it is re-keyed
        // below: the Baseline makes a reply's instance_uid match the message's, and the Agent
        // routes by it — a reply addressed to the new identity reaches no one, and the Agent goes
        // on reporting under the old one. The new identity travels in agent_identification alone.
        let mut uid = reported;
        // One moment for the round's two looks at a stale carrier, so the take-up judges it as the
        // first look did.
        let now_ms = self.clock.now_ms();
        let mut loaded = self.hold_from_store(&uid);
        let mut reply_flags = 0u64;
        let mut identification = None;

        // An instance_uid belongs to the host whose certificate first reported it (ADR-0027 §11):
        // a certificate of another host does not speak for it, nor re-keys it. Such a
        // reporter is re-keyed — it gets an identity of its own, never the one it claimed. The
        // binding is made only once the report is admitted below.
        let mut binding = None;
        if let (Some(host), Some(revocations)) =
            (presented.and_then(|p| p.host.as_deref()), &self.revocations)
        {
            if revocations.may_report(host, uid.as_bytes()).is_err() {
                let new_uid = InstanceUid::default();
                warn!(
                    claimed = %uid, new = %new_uid, host,
                    "an Agent of another host was claimed; rekeying the reporter"
                );
                self.audit_refusal(
                    Entry::new("identity.claimed", "rekeyed")
                        .with("claimed", uid.to_string())
                        .with("host", host.to_string()),
                );
                identification = Some(AgentIdentification {
                    new_instance_uid: new_uid.as_bytes().to_vec(),
                });
                uid = new_uid;
                // A CSR in the same message must not name the claimed identity either.
                sender = uid.as_bytes().to_vec();
                if let Err(e) = revocations.may_report(host, uid.as_bytes()) {
                    return Ok(Processed {
                        reply: bad_request(&e),
                        uid: None,
                        disconnected: false,
                    });
                }
            }
            binding = Some((host, uid));
        }

        // The Agent asked the Server to assign its identity (AgentToServerFlags_RequestInstanceUid):
        // mint a UUID v7 and re-key the record; the reply tells the Agent to adopt it. A re-key
        // whose answer was lost and could not be read back is read back first, whether the Agent
        // asks again or not: moved, the identity it moved to is assigned now.
        let mut rekeyed = None;
        let unsure = if identification.is_none() {
            self.read_back_unsure_rekey(&uid)
        } else {
            UnsureRekey::None
        };
        let asked = identification.is_none()
            && msg.flags & AgentToServerFlags::RequestInstanceUid as u64 != 0;
        let moves = matches!(unsure, UnsureRekey::Moved(_))
            || (asked && matches!(unsure, UnsureRekey::None));
        let (held, to_move) = {
            let fleet = self.fleet.lock().expect("fleet lock");
            let to_move = (moves && unsure == UnsureRekey::None)
                .then(|| fleet.get(&uid).map(AgentRecord::to_persisted))
                .flatten();
            (fleet.contains_key(&uid), to_move)
        };
        // While it is not known, a report under the old identity of which nothing is held creates
        // no record: the row may have moved.
        if unsure == UnsureRekey::Unknown && !held {
            return Ok(Processed {
                reply: unavailable(
                    &msg.instance_uid,
                    "the Server cannot tell yet whether this Agent's identity moved; retry later",
                ),
                uid: None,
                disconnected: false,
            });
        }
        if moves {
            // The stored record follows the identity in one step (ADR-0029 §14), before what
            // is held does; what this report changes is written by the dirty check at this
            // exchange's end. A move that fails leaves the Agent under the identity it has, and
            // the reply assigns none, so the Agent asks again with its next report. Until what is
            // held follows, the move is kept as pending, so a read again that finds the old row
            // gone meanwhile reads it back rather than drop the Agent.
            let (new_uid, moved) = match unsure {
                UnsureRekey::Moved(new_uid) => (new_uid, Ok(())),
                _ => {
                    let new_uid = InstanceUid::default();
                    let moved = match &to_move {
                        None => Ok(()),
                        Some(record) => {
                            self.kept.unsure_rekeys.lock().expect("rekeys lock").insert(
                                uid,
                                PendingRekey {
                                    new: new_uid,
                                    moved: false,
                                },
                            );
                            self.rekey_reading_back(&uid, &new_uid, record)
                                .map_err(|(e, kept)| {
                                    if !kept {
                                        self.kept
                                            .unsure_rekeys
                                            .lock()
                                            .expect("rekeys lock")
                                            .remove(&uid);
                                    }
                                    e
                                })
                        }
                    };
                    (new_uid, moved)
                }
            };
            match moved {
                Ok(()) => {
                    // The new identity is placed below.
                    info!(old = %uid, new = %new_uid, "assigned a server-generated instance_uid");
                    identification = Some(AgentIdentification {
                        new_instance_uid: new_uid.as_bytes().to_vec(),
                    });
                    rekeyed = Some((uid, new_uid));
                    uid = new_uid;
                }
                Err(e) => {
                    warn!(
                        agent = %uid, error = %e,
                        "cannot move the agent record; it keeps its identity"
                    );
                }
            }
        }

        // What memory holds of the Agent, read once, the record a re-key moves standing for the
        // one under its new identity.
        //
        // Duplicate instance_uid detection (a Baseline SHOULD): the same identity alive on a
        // WebSocket connection of *another peer* — bad UID generators, cloned VMs — is rekeyed
        // exactly as the Baseline prescribes: mint a fresh uid and answer with
        // AgentIdentification, which the Client adopts. The newcomer starts a record of its own;
        // the incumbent and its connection stay untouched. (Stateless plain-HTTP polling offers
        // nothing to distinguish two pollers by, so detection is WebSocket-only.) An Agent that
        // comes back on a new connection of the host whose certificate its old connection
        // presented is not a duplicate of itself: the newest connection takes it, and the older
        // one's reports no longer take it back
        // (ADR-0025 §8).
        let arrived_over = self
            .revocations
            .as_ref()
            .and(presented.and_then(|p| p.host.clone()));
        let described = msg.agent_description.is_some();
        let mut superseded = false;
        let mut seen = {
            let fleet = self.fleet.lock().expect("fleet lock");
            let mut record = fleet
                .get(&uid)
                .or_else(|| rekeyed.and_then(|(old, _)| fleet.get(&old)));
            if let Some(this_conn) = conn {
                match record.map_or(Carried::Not, |existing| {
                    Carried::of(
                        existing,
                        this_conn,
                        arrived_over.as_deref(),
                        now_ms,
                        self.heartbeat_interval(),
                    )
                }) {
                    Carried::Not => {}
                    Carried::ByOlder => {
                        info!(
                            agent = %uid,
                            "the agent came back on a new connection of its host, which carries it now"
                        );
                    }
                    Carried::ByStale => {
                        info!(
                            agent = %uid,
                            "the connection of its host that carried the agent carried none of its \
                             reports for a heartbeat interval; this one carries it now"
                        );
                    }
                    Carried::ByNewer => superseded = true,
                    Carried::ByAnotherPeer => {
                        let new_uid = InstanceUid::default();
                        warn!(duplicate = %uid, new = %new_uid, "duplicate instance_uid; rekeying the newcomer");
                        identification = Some(AgentIdentification {
                            new_instance_uid: new_uid.as_bytes().to_vec(),
                        });
                        uid = new_uid;
                        record = None;
                    }
                }
            }
            Seen::of(record, |record| {
                (!described)
                    .then(|| self.group_of(arrived_over.as_deref(), record.description.as_ref()))
                    .flatten()
            })
        };

        // Admitting a genuinely new Agent past the ceiling would let a peer cycling self-asserted
        // UIDs (ADR-0026) grow the in-memory map and its per-Agent disk mirror without bound. Known
        // Agents keep reporting; only a *new* UID at capacity is refused, `Unavailable` so a Client
        // that legitimately raced in retries rather than gives up. The real gate is admission
        // (ADR-0026); this bounds the damage while the endpoint is open. On a shared store the
        // ceiling bounds the records stored, counted by the store as it creates this one, so it
        // holds for the fleet and not for one instance. A record stored already passes none: it
        // is another instance's, which the read before the lock missed, and this report goes on
        // from it as from one read then. On a store this Server alone writes, the new Agent is
        // counted against what memory holds and what other reports counted and have not yet
        // taken into it.
        let mut created = false;
        let mut reserved = Reservation::none(&self.reserved_new);
        if !seen.known {
            let at_ceiling = if self.agent_store.is_shared() {
                let persisted = self
                    .new_record(msg.sequence_num, transport, conn)
                    .to_persisted();
                match self
                    .agent_store
                    .create_within(&uid, &persisted, self.max_agents)
                {
                    Ok(Creation::Created) => {
                        info!(agent = %uid, transport = transport.as_str(), "new agent");
                        created = true;
                        // Held from its commit on, not yet carried, as a record found is: what
                        // another instance commits to it from now on and announces is taken up
                        // into it, and the report goes on from it.
                        let digest = persisted.durable_digest();
                        let mut fleet = self.fleet.lock().expect("fleet lock");
                        if let std::collections::hash_map::Entry::Vacant(held) = fleet.entry(uid) {
                            self.kept
                                .written
                                .lock()
                                .expect("written lock")
                                .insert(uid, digest);
                            held.insert(AgentRecord::from_persisted(persisted));
                        }
                        false
                    }
                    Ok(Creation::Found(stored)) => {
                        let digest = stored.durable_digest();
                        let found = AgentRecord::from_persisted(*stored);
                        let mut fleet = self.fleet.lock().expect("fleet lock");
                        let joined = |record: &AgentRecord| {
                            (!described)
                                .then(|| {
                                    self.group_of(
                                        arrived_over.as_deref(),
                                        record.description.as_ref(),
                                    )
                                })
                                .flatten()
                        };
                        if let Some(record) = fleet.get(&uid) {
                            seen = Seen::of(Some(record), joined);
                        } else {
                            self.kept
                                .written
                                .lock()
                                .expect("written lock")
                                .insert(uid, digest);
                            seen = Seen::of(Some(&found), joined);
                            fleet.insert(uid, found);
                        }
                        loaded = true;
                        false
                    }
                    Ok(Creation::Full) => true,
                    Err(e) => {
                        warn!(agent = %uid, error = %e, "cannot store a new agent's record");
                        return Ok(Processed {
                            reply: unavailable(
                                &msg.instance_uid,
                                "the Server cannot store this Agent's record now; retry later",
                            ),
                            uid: None,
                            disconnected: false,
                        });
                    }
                }
            } else {
                let fleet = self.fleet.lock().expect("fleet lock");
                reserved = Reservation::of(&self.reserved_new, fleet.len(), self.max_agents);
                !reserved.held()
            };
            if at_ceiling {
                warn!(
                    agent = %uid,
                    max_agents = self.max_agents,
                    "refusing a new agent: the fleet is at its record ceiling"
                );
                return Ok(Processed {
                    reply: unavailable(
                        &msg.instance_uid,
                        "the Server is at its Agent-record ceiling; retry later",
                    ),
                    uid: None,
                    disconnected: false,
                });
            }
        }
        if let Some(revocations) = &self.revocations {
            if let Some((host, bound)) = binding {
                let subject = presented
                    .map(|p| p.subject_values.clone())
                    .unwrap_or_default();
                // The binding exists only for a presented certificate that names a host.
                let not_after_ms = presented.map_or(0, |p| p.not_after_ms);
                if let Err(e) =
                    revocations.check_report(host, &subject, not_after_ms, bound.as_bytes())
                {
                    // A record this report created goes with it, from the store and from what
                    // is held; one another instance created stays. A re-key made stands, and what
                    // is held follows it.
                    if created {
                        self.unpersist(&uid);
                        self.fleet.lock().expect("fleet lock").remove(&uid);
                    }
                    self.follow_move(rekeyed, seen.group.clone());
                    return Ok(Processed {
                        reply: bad_request(&e),
                        uid: None,
                        disconnected: false,
                    });
                }
            }
            // A re-key an Agent asks for keeps its host (ADR-0027 §11).
            if let Some((old, new)) = rekeyed {
                if let Err(e) = revocations.rebind(old.as_bytes(), new.as_bytes()) {
                    warn!(error = %e, "cannot move the host binding to the new instance_uid");
                }
            }
        }

        // The take of an Agent this connection begins to carry, written now: its answer is taken
        // up with the record below.
        // A report of an older connection of the host whose newer one carries the Agent takes it
        // no more.
        let connects = !superseded && (created || !seen.connected || seen.owner != conn);
        let _taking = connects.then(|| Taking::of(&self.taking, uid));
        let took = if connects {
            self.take_row(&uid, self.clock.now_ms())
        } else {
            Took::Nothing
        };

        // What places it in its Client and under its Host may have changed (ADR-0029): a new
        // record, a new Host, a new description, or a new identity — a re-key took the old one
        // out of its group, and a compressed message carries no description to place it by. The
        // Client Agents of the groups it leaves and joins that another instance carries are read
        // now.
        let moved = seen.host != arrived_over;
        let places = !seen.known || moved || described || rekeyed.is_some() || loaded;
        let joined = if described || !seen.known {
            self.group_of(arrived_over.as_deref(), msg.agent_description.as_ref())
        } else {
            seen.joined.clone()
        };
        let groups: Vec<Group> = if places || rekeyed.is_some() {
            [seen.group.clone(), joined].into_iter().flatten().collect()
        } else {
            Vec::new()
        };
        let mut elsewhere = self.elsewhere_of(&groups);

        let mut fleet = self.fleet.lock().expect("fleet lock");
        if let Some((old, new)) = rekeyed {
            if fleet.contains_key(&old) {
                self.rekey_held(&mut fleet, &old, new, &mut elsewhere);
            }
            let mut pending = self.kept.unsure_rekeys.lock().expect("rekeys lock");
            if pending.get(&old).is_some_and(|pending| pending.new == new) {
                pending.remove(&old);
            }
        }
        // Whether a newer connection of the host took the Agent while this round read and wrote
        // the store, or still carries it: this connection then leaves it to that one.
        let superseded = conn.is_some_and(|this_conn| {
            fleet.get(&uid).is_some_and(|record| {
                Carried::of(
                    record,
                    this_conn,
                    arrived_over.as_deref(),
                    now_ms,
                    self.heartbeat_interval(),
                ) == Carried::ByNewer
            })
        });
        let now_connects = created
            || fleet
                .get(&uid)
                .is_none_or(|record| !record.connected || record.owner != conn);
        let left = (seen.known || created) && rekeyed.is_none() && !fleet.contains_key(&uid);
        // A round that re-keyed the Agent goes on under the identity it assigned: a next round
        // would start from the message under the old one and assign a second. It finds the Agent
        // no longer carried, its connection closed meanwhile, and leaves it so: it wrote no take,
        // and nothing would mark the Agent disconnected again.
        let uncarried = now_connects && !connects && !superseded;
        if left || (uncarried && rekeyed.is_none()) {
            // Forgotten or moved through another instance, or no longer carried, while this
            // round read and wrote the store: the next round goes on from what it holds now.
            drop(fleet);
            self.place_again(elsewhere);
            return Err(Box::new(msg));
        }
        let known = seen.known;
        let record = fleet.entry(uid).or_insert_with(|| {
            info!(agent = %uid, transport = transport.as_str(), "new agent");
            self.new_record(msg.sequence_num, transport, conn)
        });
        reserved.release();

        // A compressed report (unchanged fields omitted) is only usable if our state is current.
        // A gap in sequence_num — or an Agent we have never seen describing itself with nothing —
        // means state was lost somewhere; the Baseline's recovery is to demand a full report.
        let compressed = msg.agent_description.is_none();
        let gap = known && msg.sequence_num != record.sequence_num.wrapping_add(1);
        // Counting from the start again is an Agent that restarted. A gap alone is not: an Agent
        // that never counts shows one on every message.
        let restarted = known && msg.sequence_num < record.sequence_num;
        if compressed && (!known || gap) {
            reply_flags |= ServerToAgentFlags::ReportFullState as u64;
        }

        // The Host it arrived over (ADR-0029 §16) — none without a host register.
        let moved = record.host != arrived_over;
        record.host = arrived_over;

        record.sequence_num = msg.sequence_num;
        record.transport = transport;
        // A demand for the component map may have been lost with the previous connection, or with
        // an Agent that restarted, which plain HTTP shows instead of a connection, so either may be
        // asked again.
        if !uncarried && !superseded {
            if !record.connected || record.owner != conn || restarted {
                record.components_demanded = None;
            }
            record.connected = true;
            record.owner = conn;
            // When the take-up commits, not when the round began: a round that waited on the store
            // leaves its carrier as fresh as its report.
            record.carried_ms = self.clock.now_ms();
        }
        record.heard = true;
        record.last_seen_ms = self.clock.now_ms();
        if connects {
            self.took(&uid, took, record.presence(), created);
        }
        if msg.capabilities != 0 {
            record.capabilities = msg.capabilities;
        }
        if let Some(description) = msg.agent_description {
            record.description = Some(description);
        }
        if let Some(health) = msg.health {
            record.health = Some(health);
        }
        if let Some(effective) = msg.effective_config {
            record.effective_config = Some(config_map_text(effective.config_map.as_ref()));
        }
        if let Some(status) = msg.remote_config_status {
            record.remote_config_status = Some(status);
        }
        if let Some(status) = msg.connection_settings_status {
            if status.status == opamp::proto::ConnectionSettingsStatuses::Failed as i32 {
                warn!(agent = %uid, error = %status.error_message, "connection settings rejected");
            }
            record.connection_settings_status = Some(status);
        }
        if let Some(statuses) = msg.package_statuses {
            self.take_up_package_statuses(&uid, record, statuses);
        }
        if let Some(incoming) = msg.available_components {
            // A routine hash-only update must not degrade an already-fetched full map of the
            // same hash; anything else (first sight, or a changed hash) replaces the stored value.
            let keep_stored_full = record.available_components.as_ref().is_some_and(|stored| {
                incoming.components.is_empty()
                    && !stored.components.is_empty()
                    && stored.hash == incoming.hash
            });
            if !keep_stored_full {
                record.available_components = Some(incoming);
            }
        }

        if !known || moved || described || rekeyed.is_some() || loaded {
            self.place(&mut fleet, &uid, &mut elsewhere);
        }
        let record = fleet.get_mut(&uid).expect("the record was just written");

        // The Agent asked to be issued a client certificate (ADR-0026); it is signed once the
        // fleet lock is let go.
        let request = msg
            .connection_settings_request
            .as_ref()
            .and_then(|request| request.opamp.as_ref())
            .and_then(|opamp| opamp.certificate_request.as_ref())
            .map(|request| request.csr.clone());
        let mut ending = Ending {
            uid,
            reported,
            disconnected: msg.agent_disconnect.is_some(),
            superseded,
            identification,
            reply_flags,
        };
        let pending = match request {
            None => Pending::Settled(self.settle(record, &mut ending, None)),
            Some(csr) => Pending::Issue(Box::new(record.to_persisted()), csr),
        };
        drop(fleet);
        self.place_again(elsewhere);
        Ok(self.finish(pending, &sender, presented, ending))
    }

    /// Takes a report's package statuses into `record`: an outcome that moved is audited, and a
    /// failed installation and an offer the Agent refused are logged.
    fn take_up_package_statuses(
        &self,
        uid: &InstanceUid,
        record: &mut AgentRecord,
        statuses: PackageStatuses,
    ) {
        use opamp::proto::PackageStatusEnum as P;
        for (name, status) in &statuses.packages {
            let before = record
                .package_statuses
                .as_ref()
                .and_then(|previous| previous.packages.get(name))
                .map(|previous| (previous.status, previous.agent_has_version.clone()));
            let outcome = if status.status == P::Installed as i32 {
                Some("installed")
            } else if status.status == P::InstallFailed as i32 {
                Some("failed")
            } else {
                None
            };
            if let (Some(outcome), true) = (
                outcome,
                before != Some((status.status, status.agent_has_version.clone())),
            ) {
                self.audit_refusal(
                    Entry::new("package.outcome", outcome)
                        .with("instance_uid", uid.to_string())
                        .with("package", name.clone())
                        .with("version", status.agent_has_version.clone())
                        .with("offered_version", status.server_offered_version.clone())
                        .with(
                            "error",
                            (!status.error_message.is_empty())
                                .then(|| status.error_message.clone()),
                        ),
                );
            }
        }
        for status in statuses.packages.values() {
            if status.status == P::InstallFailed as i32 {
                warn!(agent = %uid, package = %status.name, error = %status.error_message, "package installation failed");
            }
        }
        // An Agent that refuses the offer itself has no package to hang the reason on, so the
        // report carries it. Logged and surfaced, or a Client refusing every offer it is sent
        // would look like one that is simply not installing anything.
        if !statuses.error_message.is_empty() {
            warn!(agent = %uid, error = %statuses.error_message, "the agent refused the package offer");
        }
        record.package_statuses = Some(statuses);
    }

    /// Moves what this instance holds to the identity a re-key moved the stored record to, where
    /// a report that made the move is refused before it takes it into memory.
    fn follow_move(&self, rekeyed: Option<(InstanceUid, InstanceUid)>, left: Option<Group>) {
        let Some((old, new)) = rekeyed else {
            return;
        };
        let mut elsewhere = self.elsewhere_of(left.as_slice());
        {
            let mut fleet = self.fleet.lock().expect("fleet lock");
            if fleet.contains_key(&old) {
                self.rekey_held(&mut fleet, &old, new, &mut elsewhere);
            }
            let mut pending = self.kept.unsure_rekeys.lock().expect("rekeys lock");
            if pending.get(&old).is_some_and(|pending| pending.new == new) {
                pending.remove(&old);
            }
        }
        self.place_again(elsewhere);
    }

    /// Under the fleet lock, what a report's end makes of `record` once a certificate it asked
    /// for is `issued` or refused: its goodbye, the demand for its component map, and the reply,
    /// or a queued restart that goes out instead, whose clear is written once the lock is let go.
    fn settle(
        &self,
        record: &mut AgentRecord,
        ending: &mut Ending,
        issued: Option<TlsCertificate>,
    ) -> Settled {
        if ending.disconnected && !ending.superseded {
            info!(agent = %ending.uid, "agent disconnected");
            record.connected = false;
            record.owner = None;
        }

        // A hash without the map is an offer to fetch: demand the full component list from an
        // Agent that declared it can report one (the flag is meaningless toward any other), once
        // per hash on a connection.
        if !ending.disconnected
            && !ending.superseded
            && record.capabilities
                & opamp::proto::AgentCapabilities::ReportsAvailableComponents as u64
                != 0
        {
            if let Some(stored) = record
                .available_components
                .as_ref()
                .filter(|ac| ac.components.is_empty())
            {
                if record.components_demanded.as_ref() != Some(&stored.hash) {
                    record.components_demanded = Some(stored.hash.clone());
                    ending.reply_flags |= ServerToAgentFlags::ReportAvailableComponents as u64;
                }
            }
        }

        // A queued restart goes out as the Baseline's command-only message: nothing but
        // identity, capabilities, and the command. Anything else the reply would carry —
        // an identity reassignment, a demanded full report — defers the command to the next
        // exchange instead of being combined with it.
        if record.restart_pending
            && !ending.disconnected
            && !ending.superseded
            && ending.identification.is_none()
            && ending.reply_flags == 0
            && record.capabilities & opamp::proto::AgentCapabilities::AcceptsRestartCommand as u64
                != 0
        {
            return Settled::Restart(issued);
        }
        Settled::Done(Box::new((
            self.reply_of(record, ending, issued),
            record.to_persisted(),
        )))
    }

    /// The reply to a report, with the offers `record` is due.
    fn reply_of(
        &self,
        record: &AgentRecord,
        ending: &Ending,
        issued: Option<TlsCertificate>,
    ) -> Processed {
        // The config offer — composed from this Agent's assignments (ADR-0032), gated by the
        // hash comparison, and only toward an Agent that both said goodbye ≠ true and declared
        // AcceptsRemoteConfig (capability negotiation is binding). Matching proposes; only an
        // operator's rollout act made anything an assignment.
        let remote_config = if ending.disconnected {
            None
        } else {
            let desired = self.configs.compose(record.assigned_configurations());
            offer(record, desired.as_ref())
        };

        // The connection-settings offer (ADR-0013), gated the same way: by capability and by
        // the hash the Agent last reported — the Baseline's own "compare and include" MUST.
        let connection_settings = if ending.disconnected {
            None
        } else {
            self.settings_offer(record, issued)
        };

        // The package offer (ADR-0033), gated by capability and the reported
        // server_provided_all_packages_hash — the Baseline's "compare and include" for packages.
        let packages_available = if ending.disconnected {
            None
        } else {
            self.packages_offer(record)
        };

        Processed {
            reply: ServerToAgent {
                instance_uid: ending.reported.as_bytes().to_vec(),
                capabilities: self.capabilities(),
                flags: ending.reply_flags,
                remote_config,
                connection_settings,
                packages_available,
                agent_identification: ending.identification.clone(),
                ..Default::default()
            },
            uid: Some(ending.uid),
            disconnected: ending.disconnected,
        }
    }

    /// The end of a report once its record is taken into memory, with the fleet lock let go: a
    /// certificate it asked for is signed and recorded, a queued restart cleared, and everything
    /// the report changed is persisted if it moved the durable state (ADR-0029) — a heartbeat did
    /// not, and writes nothing — before the Agent is answered.
    fn finish(
        &self,
        pending: Pending,
        sender: &[u8],
        presented: Option<&Presented>,
        mut ending: Ending,
    ) -> Processed {
        let settled = match pending {
            Pending::Issue(persisted, csr) => {
                match self.issue(csr, sender, presented, &ending, &persisted) {
                    Ok(issued) => {
                        let mut fleet = self.fleet.lock().expect("fleet lock");
                        let Some(record) = fleet.get_mut(&ending.uid) else {
                            drop(fleet);
                            return self.forgotten_elsewhere(&ending.uid, &ending.reported);
                        };
                        self.settle(record, &mut ending, Some(issued))
                    }
                    Err(answered) => return *answered,
                }
            }
            Pending::Settled(settled) => settled,
        };
        let (processed, persisted) = match settled {
            Settled::Done(done) => *done,
            Settled::Restart(issued) => {
                let cleared = self.clear_restart(&ending.uid);
                let mut fleet = self.fleet.lock().expect("fleet lock");
                let Some(record) = fleet.get_mut(&ending.uid) else {
                    drop(fleet);
                    return self.forgotten_elsewhere(&ending.uid, &ending.reported);
                };
                if self.take_restart(&ending.uid, record, cleared) {
                    let processed = Processed {
                        reply: restart_command(&ending.uid, self.capabilities()),
                        uid: Some(ending.uid),
                        disconnected: false,
                    };
                    (processed, record.to_persisted())
                } else {
                    (
                        self.reply_of(record, &ending, issued),
                        record.to_persisted(),
                    )
                }
            }
        };
        if !self.persist_if_dirty(&ending.uid, &persisted) {
            return self.forgotten_elsewhere(&ending.uid, &ending.reported);
        }
        processed
    }

    /// The record of an Agent first heard of now.
    fn new_record(
        &self,
        sequence_num: u64,
        transport: Transport,
        conn: Option<ConnId>,
    ) -> AgentRecord {
        AgentRecord {
            labels: BTreeMap::new(),
            host: None,
            placement: Placement::default(),
            holder: None,
            sequence_num,
            capabilities: 0,
            description: None,
            health: None,
            effective_config: None,
            remote_config_status: None,
            transport,
            connected: true,
            last_seen_ms: self.clock.now_ms(),
            restart_pending: false,
            available_components: None,
            components_demanded: None,
            connection_settings_status: None,
            package_statuses: None,
            owner: conn,
            carried_ms: self.clock.now_ms(),
            heard: true,
            // A new Agent waits (ADR-0032 §10): it is assigned nothing until an operator's
            // rollout act says so, and the fleet view shows what it could get.
            assignment: None,
        }
    }

    /// On a shared store, loads the record of `uid` from it when this Server does not hold one, as
    /// at an Agent's first report here: another instance may have written it, and its labels, its
    /// assignment and its queued restart are what this report goes on from
    /// (ADR-0025 §8).
    /// Read before the fleet lock is taken, so no other report waits for it. Answers
    /// whether it loaded one; one that cannot be read is logged, and the report goes on as an
    /// Agent's first.
    fn hold_from_store(&self, uid: &InstanceUid) -> bool {
        if !self.agent_store.is_shared() || self.fleet.lock().expect("fleet lock").contains_key(uid)
        {
            return false;
        }
        let stored = match self.agent_store.get(uid) {
            Ok(Some(stored)) => stored,
            Ok(None) => return false,
            Err(e) => {
                warn!(agent = %uid, error = %e, "cannot read the agent's stored record");
                return false;
            }
        };
        let mut fleet = self.fleet.lock().expect("fleet lock");
        if fleet.contains_key(uid) {
            return false;
        }
        self.kept
            .written
            .lock()
            .expect("written lock")
            .insert(*uid, stored.durable_digest());
        fleet.insert(*uid, AgentRecord::from_persisted(stored));
        true
    }

    /// The connection-settings offer for one Agent, or `None` when it cannot accept one or its
    /// reported hash says it already runs (or refused) exactly this offer.
    ///
    /// `issued` is a certificate just signed for this Agent (ADR-0026). It overrides the hash gate
    /// — the Agent asked for it in this very exchange — and rides whatever else the standing offer
    /// carries, so one message can hand over a certificate and the endpoint or heartbeat that go
    /// with it, exactly as the Baseline describes.
    fn settings_offer(
        &self,
        record: &AgentRecord,
        issued: Option<TlsCertificate>,
    ) -> Option<ConnectionSettingsOffers> {
        // The own-telemetry destinations (ADR-0018), offered only for the signals this Agent says
        // it can report — the protocol's negotiation rule, and an offer for a capability the peer
        // lacks is one nobody will ever act on.
        let telemetry = TelemetryOffer {
            own_metrics: self.telemetry_offer.own_metrics.clone().filter(|_| {
                record.capabilities & opamp::proto::AgentCapabilities::ReportsOwnMetrics as u64 != 0
            }),
            own_traces: self.telemetry_offer.own_traces.clone().filter(|_| {
                record.capabilities & opamp::proto::AgentCapabilities::ReportsOwnTraces as u64 != 0
            }),
            own_logs: self.telemetry_offer.own_logs.clone().filter(|_| {
                record.capabilities & opamp::proto::AgentCapabilities::ReportsOwnLogs as u64 != 0
            }),
        };

        if let Some(certificate) = issued {
            let mut settings = self
                .connection_offer
                .as_ref()
                .map(|offer| offer.settings.clone())
                .unwrap_or_default();
            settings.certificate = Some(certificate);
            // Not gated, and hashed over the settings as sent: the standing offer's hash would
            // tell the Agent nothing changed, and it would never adopt the certificate.
            return Some(compose_settings_offer(Some(settings), telemetry));
        }

        // An Agent that accepts no OpAMP settings may still report telemetry, so the two are
        // gated separately: with only a telemetry destination to offer, that is the whole offer.
        let settings = self
            .connection_offer
            .as_ref()
            .filter(|_| {
                record.capabilities
                    & opamp::proto::AgentCapabilities::AcceptsOpAmpConnectionSettings as u64
                    != 0
            })
            .map(|offer| offer.settings.clone());
        if settings.is_none() && telemetry.is_empty() {
            return None;
        }
        gate(record, compose_settings_offer(settings, telemetry))
    }

    /// Marks the Agents a closing WebSocket connection carried as no longer connected — but only
    /// those the connection still *owns*: after a rekey (or a transport switch) another live
    /// connection may legitimately carry an identity this one once saw, and a closing socket
    /// must not take it down. State stays: the fleet remembers what each Agent last reported.
    pub fn mark_disconnected(&self, uids: &[InstanceUid], conn: ConnId) {
        let mut fleet = self.fleet.lock().expect("fleet lock");
        for uid in uids {
            // An Agent whose re-key a notification found moved goes by its new identity here
            // before the connection is told it.
            let uid = &if fleet.contains_key(uid) {
                *uid
            } else {
                self.kept
                    .unsure_rekeys
                    .lock()
                    .expect("rekeys lock")
                    .get(uid)
                    .filter(|pending| pending.moved)
                    .map_or(*uid, |pending| pending.new)
            };
            if let Some(record) = fleet.get_mut(uid) {
                if record.owner == Some(conn) {
                    record.connected = false;
                    record.owner = None;
                }
            }
        }
        self.forgotten.lock().expect("forgotten lock").remove(&conn);
    }
}

/// The result of processing one `AgentToServer`: the reply to send back on the same transport, and
/// what the transport layer needs to know for its own bookkeeping.
pub struct Processed {
    pub reply: ServerToAgent,
    /// The identity the Agent goes by *after* this message (it may have been reassigned).
    pub uid: Option<InstanceUid>,
    /// The Agent said goodbye; a WebSocket loop drops it from its connection-local set.
    pub disconnected: bool,
}

/// How often a report starts again where an Agent it went on from left memory while it read and
/// wrote the store, before it is answered `Unavailable`.
const REPORT_ROUNDS: usize = 3;

/// What memory held of an Agent as its report began: what the report decides its calls to the
/// store from, before it takes the fleet lock to take them up.
struct Seen {
    /// Memory held a record of it.
    known: bool,
    connected: bool,
    owner: Option<ConnId>,
    /// The Host it last arrived over.
    host: Option<String>,
    /// The group it is placed in.
    group: Option<Group>,
    /// The group the report places it in where the report carries no description.
    joined: Option<Group>,
}

impl Seen {
    /// What `record` holds, `joined` the group it joins without a description.
    fn of(
        record: Option<&AgentRecord>,
        joined: impl FnOnce(&AgentRecord) -> Option<Group>,
    ) -> Self {
        match record {
            None => Seen {
                known: false,
                connected: false,
                owner: None,
                host: None,
                group: None,
                joined: None,
            },
            Some(record) => Seen {
                known: true,
                connected: record.connected,
                owner: record.owner,
                host: record.host.clone(),
                group: record.placement.group.clone(),
                joined: joined(record),
            },
        }
    }
}

/// A new Agent counted against `max_agents` on a store the Server alone writes, until the report
/// takes it into memory or ends.
struct Reservation<'a> {
    count: &'a AtomicUsize,
    held: bool,
}

impl<'a> Reservation<'a> {
    /// None held.
    fn none(count: &'a AtomicUsize) -> Self {
        Reservation { count, held: false }
    }

    /// One counted beside the `held` records memory holds and those `count` counts, unless that
    /// reaches `max`. Made under the fleet lock, so two reports do not count past it together.
    fn of(count: &'a AtomicUsize, held: usize, max: usize) -> Self {
        if held.saturating_add(count.load(Ordering::SeqCst)) >= max {
            return Reservation::none(count);
        }
        count.fetch_add(1, Ordering::SeqCst);
        Reservation { count, held: true }
    }

    fn held(&self) -> bool {
        self.held
    }

    /// No longer counted: taken into memory, where it counts, or let go.
    fn release(&mut self) {
        if std::mem::take(&mut self.held) {
            self.count.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        self.release();
    }
}

/// What a report's end answers with, once its record is taken into memory.
pub(super) struct Ending {
    /// The identity the Agent goes by after the report.
    pub(super) uid: InstanceUid,
    /// The identity it reported, which the reply is addressed to.
    pub(super) reported: InstanceUid,
    /// It said goodbye.
    disconnected: bool,
    /// It came on an older connection of the host whose newer one carries the Agent, which its
    /// goodbye does not end and a queued restart is not sent on
    /// (ADR-0025 §8).
    superseded: bool,
    identification: Option<AgentIdentification>,
    reply_flags: u64,
}

/// Where a report's end stands once its record is taken into memory.
enum Pending {
    /// Its reply, or a restart to clear first.
    Settled(Settled),
    /// A certificate it asked for to sign first, with what its record held.
    Issue(Box<PersistedAgent>, Vec<u8>),
}

/// One `ConnectionSettingsOffers` message, with one hash over everything it carries: the Agent
/// acknowledges the message, not its parts.
pub(super) fn compose_settings_offer(
    opamp: Option<OpAmpConnectionSettings>,
    telemetry: TelemetryOffer,
) -> ConnectionSettingsOffers {
    let mut offer = ConnectionSettingsOffers {
        opamp,
        own_metrics: telemetry.own_metrics,
        own_traces: telemetry.own_traces,
        own_logs: telemetry.own_logs,
        ..Default::default()
    };
    offer.hash = Sha256::digest(offer.encode_to_vec()).to_vec();
    offer
}

/// The Baseline's gate: send the offer when the Agent's reported hash differs. An APPLYING echo of
/// the same hash keeps it coming — a verification whose outcome was lost (a dropped connection
/// mid-switch) must heal by retry, not hang.
fn gate(record: &AgentRecord, offer: ConnectionSettingsOffers) -> Option<ConnectionSettingsOffers> {
    if let Some(status) = &record.connection_settings_status {
        if status.last_connection_settings_hash == offer.hash
            && status.status != opamp::proto::ConnectionSettingsStatuses::Applying as i32
        {
            return None;
        }
    }
    Some(offer)
}

/// The `ServerToAgent` for a report the Server cannot make sense of.
pub fn bad_request(message: &str) -> ServerToAgent {
    ServerToAgent {
        capabilities: SERVER_CAPABILITIES,
        error_response: Some(ServerErrorResponse {
            r#type: ServerErrorResponseType::BadRequest as i32,
            error_message: message.to_string(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// What a CSR with a bootstrap proof over a member's certificate is answered (ADR-0016 §25).
pub const MEMBER_DOES_NOT_ENROL: &str =
    "a certificate signing request with a bootstrap proof is an enrolment, and a member does not \
     enrol";

/// What a CSR with neither proof over a Gateway's certificate is answered (ADR-0016 §18).
pub const NEEDS_A_PROOF: &str =
    "a certificate requested through a Gateway needs a renewal proof or a bootstrap proof";

/// How long an Agent told `Unavailable` waits before it asks again.
pub const RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(30);

/// The `ServerToAgent` for a report the Server is momentarily unable to accept — the Baseline's
/// `Unavailable`, which unlike `BadRequest` tells the Agent to **retry later** rather than give up,
/// and with `retry_info` says when. It carries the `instance_uid` of the message it answers, the
/// field a Client and a Gateway route a reply by (ADR-0012 §27).
pub fn unavailable(instance_uid: &[u8], message: &str) -> ServerToAgent {
    ServerToAgent {
        instance_uid: instance_uid.to_vec(),
        capabilities: SERVER_CAPABILITIES,
        error_response: Some(ServerErrorResponse {
            r#type: ServerErrorResponseType::Unavailable as i32,
            error_message: message.to_string(),
            details: Some(opamp::proto::server_error_response::Details::RetryInfo(
                opamp::proto::RetryInfo {
                    retry_after_nanoseconds: RETRY_AFTER.as_nanos() as u64,
                },
            )),
        }),
        ..Default::default()
    }
}

/// Renders a reported config map for the operator: single unnamed entry as-is, named entries with
/// a `# <name>` heading.
fn config_map_text(map: Option<&AgentConfigMap>) -> String {
    let Some(map) = map else {
        return String::new();
    };
    let mut entries: Vec<_> = map.config_map.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    entries
        .into_iter()
        .map(|(name, file)| {
            let body = String::from_utf8_lossy(&file.body);
            if name.is_empty() {
                body.into_owned()
            } else {
                format!("# {name}\n{body}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_store::AgentStore;
    use crate::configs::ConfigBackend;
    use crate::deployments::DeploymentError;
    use crate::fleet::fixtures::*;
    use crate::fleet::views::AgentView;
    use crate::fleet::{is_stale, Assignment};
    use crate::revocation::{CertId, Revocations, SpeaksFor};
    use std::collections::HashSet;
    use std::sync::atomic::AtomicU64;
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::Duration;

    /// ADR-0029. The tidy-up case: a host that was decommissioned, its Agent gone with it.
    #[test]
    fn a_disconnected_agent_is_forgotten() {
        let state = forgettable_state();
        let uid = insert(&state, record_with(0));
        assert!(state.forget_agent(&uid).is_ok());
        assert!(state.snapshot().is_empty(), "the row is gone");
    }

    /// The gate: forgetting a live Agent would drop the hashes that stop the Server re-offering,
    /// so its next exchange re-applies its configuration — and a managed process restarts with it.
    #[test]
    fn an_agent_that_is_still_reporting_is_refused() {
        let state = forgettable_state();
        let mut record = record_with(0);
        record.connected = true;
        record.last_seen_ms = now_ms();
        let uid = insert(&state, record);
        assert!(matches!(
            state.forget_agent(&uid),
            Err(ForgetError::StillReporting)
        ));
        assert_eq!(state.snapshot().len(), 1, "the row stays");
    }

    /// The gatewayed case: the connection is up because it is the *Gateway's*, and the Agent behind
    /// it stopped talking long ago. `connected` alone would refuse this forever.
    #[test]
    fn a_connected_agent_that_went_quiet_is_forgotten() {
        let state = forgettable_state();
        let mut record = record_with(opamp::proto::AgentCapabilities::ReportsHeartbeat as u64);
        record.connected = true;
        record.last_seen_ms = now_ms() - 120_000;
        let uid = insert(&state, record);
        assert!(state.forget_agent(&uid).is_ok());
    }

    /// The case that made the rule test silence rather than staleness (ADR-0029): an Agent that
    /// promised no heartbeat is never *stale*, and plain-HTTP polling never clears `connected` —
    /// so gating on the flag would have left this row on a dead host permanently unremovable.
    #[test]
    fn a_silent_agent_is_forgotten_although_it_can_never_be_stale() {
        let state = forgettable_state();
        let mut record = record_with(opamp::proto::AgentCapabilities::ReportsStatus as u64);
        record.connected = true;
        record.transport = Transport::Http;
        record.last_seen_ms = now_ms() - 86_400_000;
        let uid = insert(&state, record);
        assert!(
            !is_stale(
                &record_at(now_ms() - 86_400_000),
                Duration::from_secs(90),
                now_ms()
            ),
            "it declares no heartbeat, so it is never stale"
        );
        assert!(state.forget_agent(&uid).is_ok(), "but it is forgettable");
    }

    #[test]
    fn forgetting_an_agent_that_was_never_known_says_so() {
        let state = forgettable_state();
        assert!(matches!(
            state.forget_agent(&InstanceUid::default()),
            Err(ForgetError::UnknownAgent)
        ));
    }

    fn forgettable_state() -> AppState {
        let dir = tempfile::tempdir().expect("tempdir");
        // The directory outlives the state only for the length of a test; the Configuration store
        // is not what these exercise.
        AppState::new(dir.keep().join("configs"))
            .expect("state")
            .with_stale_after(Duration::from_secs(90))
    }

    fn record_at(last_seen_ms: u64) -> AgentRecord {
        let mut record = record_with(opamp::proto::AgentCapabilities::ReportsStatus as u64);
        record.last_seen_ms = last_seen_ms;
        record
    }

    /// ADR-0029: the fleet survives a restart — the record is restored with everything the Agent
    /// reported, shown honestly as disconnected until live evidence says otherwise.
    // Verifies: ADR-0029
    #[test]
    fn the_fleet_is_restored_disconnected_after_a_restart() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let uid = InstanceUid::default();
        {
            let state = AppState::new(dir.clone()).expect("state");
            state.process(report(&uid, 1), Transport::WebSocket, Some(1));
        }
        let state = AppState::new(dir).expect("reopened state");
        let snapshot = state.snapshot();
        assert_eq!(snapshot.len(), 1, "the row survived the restart");
        assert_eq!(snapshot[0].service_name, "otelcol");
        assert!(
            !snapshot[0].connected,
            "connectedness is runtime-only and never restored"
        );
    }

    /// Every `Unavailable` — a full enrolment queue, the record ceiling, an audit record that cannot
    /// be written — tells the Agent when to ask again, so it retries instead of giving up or
    /// hammering (ADR-0027 §9, ADR-0028 §5).
    /// Verifies: ADR-0026, ADR-0028, ADR-0012
    #[test]
    fn unavailable_tells_the_agent_when_to_retry() {
        let reply = unavailable(&[7; 16], "busy");
        assert_eq!(reply.instance_uid, [7; 16], "it names the Agent it answers");
        let error = reply.error_response.expect("an error");
        assert_eq!(error.r#type, ServerErrorResponseType::Unavailable as i32);
        match error.details {
            Some(opamp::proto::server_error_response::Details::RetryInfo(info)) => {
                assert_eq!(info.retry_after_nanoseconds, RETRY_AFTER.as_nanos() as u64);
            }
            other => panic!("no retry_info: {other:?}"),
        }
    }

    /// A new `instance_uid` past the record ceiling is refused `Unavailable` and leaves no record,
    /// so a peer minting fresh self-asserted UIDs (ADR-0026) cannot grow the fleet — and its
    /// in-memory map and per-Agent disk mirror — without bound. Agents already known keep reporting.
    #[test]
    fn a_new_agent_past_the_ceiling_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let state = AppState::new(dir).expect("state").with_max_agents(2);

        let a = InstanceUid::default();
        let b = InstanceUid::default();
        state.process(report(&a, 1), Transport::Http, None);
        state.process(report(&b, 1), Transport::Http, None);
        assert_eq!(state.snapshot().len(), 2, "the fleet filled to its ceiling");

        // A third, genuinely new UID: refused, and told to retry rather than give up.
        let c = InstanceUid::default();
        let processed = state.process(report(&c, 1), Transport::Http, None);
        let error = processed.reply.error_response.expect("an error response");
        assert_eq!(
            error.r#type,
            ServerErrorResponseType::Unavailable as i32,
            "a full fleet answers Unavailable, not BadRequest"
        );
        assert!(
            processed.uid.is_none(),
            "the refused report has no identity to route a config to"
        );
        assert_eq!(
            state.snapshot().len(),
            2,
            "no record was created for the refused UID"
        );

        // An Agent already in the fleet keeps reporting even at the ceiling.
        let processed = state.process(report(&a, 2), Transport::Http, None);
        assert!(
            processed.reply.error_response.is_none(),
            "a known Agent is never refused by the ceiling"
        );
        assert_eq!(state.snapshot().len(), 2);
    }

    /// ADR-0029: a restored sequence number means the next compressed heartbeat is accepted in
    /// place of a fleet-wide ReportFullState stampede.
    #[test]
    fn a_restored_agent_is_not_demanded_a_full_report() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let uid = InstanceUid::default();
        {
            let state = AppState::new(dir.clone()).expect("state");
            state.process(report(&uid, 1), Transport::WebSocket, Some(1));
            state.flush_agents();
        }
        let state = AppState::new(dir).expect("reopened state");
        let heartbeat = AgentToServer {
            instance_uid: uid.as_bytes().to_vec(),
            sequence_num: 2,
            ..Default::default()
        };
        let processed = state.process(heartbeat, Transport::WebSocket, Some(1));
        assert_eq!(
            processed.reply.flags & ServerToAgentFlags::ReportFullState as u64,
            0,
            "no gap: the restored record carries the sequence"
        );
    }

    /// ADR-0029: a heartbeat exists to change nothing, and it reaches no storage backend — the
    /// stored record still carries the durable state's write, not the heartbeat's.
    // Verifies: ADR-0029
    #[test]
    fn a_heartbeat_writes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let uid = InstanceUid::default();
        let state = AppState::new(dir.clone()).expect("state");
        state.process(report(&uid, 1), Transport::WebSocket, Some(1));
        let path = dir.join("agents").join(format!("{uid}.json"));
        let written = std::fs::read_to_string(&path).expect("the report was persisted");
        let heartbeat = AgentToServer {
            instance_uid: uid.as_bytes().to_vec(),
            sequence_num: 2,
            ..Default::default()
        };
        state.process(heartbeat, Transport::WebSocket, Some(1));
        let after = std::fs::read_to_string(&path).expect("still persisted");
        assert_eq!(written, after, "the heartbeat performed no write");
    }

    /// ADR-0029: forgetting removes the stored record with the row — nothing
    /// remembers under another name.
    #[test]
    fn forgetting_an_agent_removes_its_stored_record() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let uid = InstanceUid::default();
        let state = AppState::new(dir.clone())
            .expect("state")
            .with_stale_after(Duration::from_secs(90));
        let mut goodbye = report(&uid, 1);
        goodbye.agent_disconnect = Some(opamp::proto::AgentDisconnect {});
        state.process(goodbye, Transport::WebSocket, Some(1));
        let path = dir.join("agents").join(format!("{uid}.json"));
        assert!(path.exists(), "the record was persisted");
        assert!(
            state.forget_agent(&uid).is_ok(),
            "forgettable: disconnected"
        );
        assert!(!path.exists(), "forgetting frees the store too");
    }

    /// ADR-0029: the persisted record follows a reassigned identity — one record, one file.
    #[test]
    fn a_rekeyed_agent_moves_its_stored_record() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let uid = InstanceUid::default();
        let state = AppState::new(dir.clone()).expect("state");
        state.process(report(&uid, 1), Transport::WebSocket, Some(1));
        let mut rekey = report(&uid, 2);
        rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
        let processed = state.process(rekey, Transport::WebSocket, Some(1));
        let new_uid = processed.uid.expect("the new identity");
        assert_eq!(
            processed.reply.instance_uid,
            uid.as_bytes().to_vec(),
            "the reply is addressed to the identity the Agent asked under"
        );
        assert!(!dir.join("agents").join(format!("{uid}.json")).exists());
        assert!(dir.join("agents").join(format!("{new_uid}.json")).exists());
    }

    /// A certificate of one host does not speak for another host's Agent: the reporter is re-keyed
    /// to an identity of its own, and the Agent it claimed keeps its record.
    /// Verifies: ADR-0026, G-17
    #[test]
    fn a_host_cannot_report_for_another_hosts_agent() {
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
        let presented = |host: &str, serial: &str| Presented {
            id: CertId::new(b"CA", serial),
            host: Some(host.to_string()),
            subject_values: crate::revocation::SubjectValues::default(),
            not_after_ms: u64::MAX,
        };
        let victim = InstanceUid::default();
        let own = state.process_presented(
            {
                let mut first = AgentToServer {
                    instance_uid: victim.as_bytes().to_vec(),
                    ..Default::default()
                };
                first.capabilities = opamp::proto::AgentCapabilities::ReportsStatus as u64;
                first
            },
            Transport::Http,
            None,
            Some(&presented("h1", "01")),
        );
        assert_eq!(own.uid, Some(victim));
        assert!(own.reply.agent_identification.is_none());

        let claimed = state.process_presented(
            AgentToServer {
                instance_uid: victim.as_bytes().to_vec(),
                flags: AgentToServerFlags::RequestInstanceUid as u64,
                ..Default::default()
            },
            Transport::Http,
            None,
            Some(&presented("h2", "02")),
        );
        let new_uid = claimed.uid.expect("an identity");
        assert_ne!(new_uid, victim, "another host spoke for the Agent");
        assert_eq!(
            claimed.reply.instance_uid,
            victim.as_bytes().to_vec(),
            "the reply is addressed to the identity the reporter sent, or it routes to no one"
        );
        assert_eq!(
            claimed
                .reply
                .agent_identification
                .expect("told to adopt it")
                .new_instance_uid,
            new_uid.as_bytes().to_vec()
        );
        assert!(
            state
                .snapshot()
                .iter()
                .any(|agent| agent.instance_uid == victim.to_string()),
            "the claimed Agent's record moved"
        );

        // Having adopted it, the reporter speaks under its new identity and is re-keyed no more:
        // no further record appears for it.
        let adopted = state.process_presented(
            AgentToServer {
                instance_uid: new_uid.as_bytes().to_vec(),
                ..Default::default()
            },
            Transport::Http,
            None,
            Some(&presented("h2", "02")),
        );
        assert_eq!(adopted.uid, Some(new_uid));
        assert!(adopted.reply.agent_identification.is_none());
        assert_eq!(state.snapshot().len(), 2, "one record per Agent, no more");
    }

    /// Forgetting an Agent releases its binding: another host's report for it binds it, under the
    /// identity it reported, where before it would have been re-keyed.
    /// Verifies: ADR-0029
    #[test]
    fn forgetting_an_agent_releases_its_binding() {
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
            .with_revocations(Some(revocations.clone()));
        let presented = |host: &str| Presented {
            id: CertId::new(b"CA", host),
            host: Some(host.to_string()),
            subject_values: crate::revocation::SubjectValues::default(),
            not_after_ms: u64::MAX,
        };
        let uid = InstanceUid::default();
        let message = || AgentToServer {
            instance_uid: uid.as_bytes().to_vec(),
            capabilities: opamp::proto::AgentCapabilities::ReportsStatus as u64,
            ..Default::default()
        };
        let conn = state.connection_id();
        let first = state.process_presented(
            message(),
            Transport::WebSocket,
            Some(conn),
            Some(&presented("h1")),
        );
        assert_eq!(first.uid, Some(uid));
        state.mark_disconnected(&[uid], conn);
        assert!(state.forget_agent(&uid).is_ok(), "forgotten");
        assert_eq!(
            revocations.speaks_for("h1"),
            crate::revocation::SpeaksFor::Agents(std::collections::BTreeSet::new()),
            "released from h1"
        );

        let other = state.process_presented(
            message(),
            Transport::WebSocket,
            Some(state.connection_id()),
            Some(&presented("h2")),
        );
        assert_eq!(other.uid, Some(uid), "bound by h2, not re-keyed");
        assert!(other.reply.agent_identification.is_none());
        assert_eq!(
            revocations.speaks_for("h2"),
            crate::revocation::SpeaksFor::Agents([uid.to_string().replace('-', "")].into())
        );
    }

    /// A report refused at the record ceiling binds no `instance_uid` to the host that sent it,
    /// so it cannot speak for that Agent once there is room — nor lock out the host it belongs to.
    /// Verifies: ADR-0026, ADR-0029
    #[test]
    fn a_report_refused_at_the_ceiling_binds_nothing() {
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
            .with_max_agents(1)
            .with_revocations(Some(revocations.clone()));
        let presented = |host: &str| Presented {
            id: CertId::new(b"CA", host),
            host: Some(host.to_string()),
            subject_values: crate::revocation::SubjectValues::default(),
            not_after_ms: u64::MAX,
        };
        let first = InstanceUid::default();
        let accepted = state.process_presented(
            report(&first, 1),
            Transport::Http,
            None,
            Some(&presented("h1")),
        );
        assert!(accepted.reply.error_response.is_none());

        for flags in [0, AgentToServerFlags::RequestInstanceUid as u64] {
            let refused_uid = InstanceUid::default();
            let mut refused = report(&refused_uid, 1);
            refused.flags = flags;
            let refused =
                state.process_presented(refused, Transport::Http, None, Some(&presented("h2")));
            assert_eq!(
                refused.reply.error_response.expect("refused").r#type,
                ServerErrorResponseType::Unavailable as i32
            );
            assert_eq!(
                revocations.speaks_for("h2"),
                SpeaksFor::Agents(BTreeSet::new()),
                "a refused report bound an instance_uid (flags {flags})"
            );
        }
        assert_eq!(
            revocations.speaks_for("h1"),
            SpeaksFor::Agents([hex::encode(first.as_bytes())].into())
        );
    }

    /// An Agent bound to a direct Host that reports over a Gateway since is placed by the Gateway
    /// it arrived over: the list, its view and a rollout to it all inherit the labels of the
    /// Client Agent behind that Gateway.
    /// Verifies: ADR-0025, ADR-0029
    #[test]
    fn an_agent_bound_to_a_direct_host_and_arriving_over_a_gateway_is_placed_by_the_gateway() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let a = on_postgres(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::open(database.clone()).expect("open")),
        );
        let (gateway, client, agent) = (
            InstanceUid([0xa1; 16]),
            InstanceUid([0xa2; 16]),
            InstanceUid([0xa3; 16]),
        );
        let subject = crate::revocation::SubjectValues::default;
        report_over(&a, "g", subject(), gateway, &[]);
        assert_eq!(a.set_gateway("g", true), Ok(true));
        report_over(&a, "h1", subject(), agent, &[]);
        report_over(
            &a,
            "g",
            subject(),
            client,
            &[("supervisor.kind.collector", "true"), ("host.name", "m1")],
        );
        report_over(&a, "g", subject(), agent, &[("host.name", "m1")]);
        label(&a, &client, &[("ring", "early")]);

        let dir = tempfile::tempdir().expect("blobs");
        let b = delivering_on(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::open(database.clone()).expect("open")),
            dir.path(),
        );
        aim_at_the_early_ring(&b);
        let inherited =
            |view: &AgentView| (view.inherited_labels.clone(), view.client_agent.clone());
        let expected = (
            BTreeMap::from([("ring".to_string(), "early".to_string())]),
            client.to_string(),
        );
        let listed = b
            .fleet_view()
            .expect("fleet")
            .into_iter()
            .find(|view| view.instance_uid == agent.to_string())
            .expect("listed");
        assert_eq!(inherited(&listed), expected, "the list");
        let viewed = b.agent_view(&agent).expect("read").expect("known");
        assert_eq!(inherited(&viewed), expected, "its view");
        assert!(b.rollout_to_agent(&agent, None).is_ok(), "a rollout to it");
        assert_eq!(
            released_through(&database, &agent).as_deref(),
            Some("early")
        );
    }

    /// A record that does not decode, which every read that meets it fails, can still be
    /// forgotten through an instance that does not hold it, and is gone after, with what its
    /// assignment pinned: the Deployment it was released through may be deleted then.
    /// Verifies: ADR-0039, ADR-0029
    #[test]
    fn a_record_that_does_not_decode_can_be_forgotten() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        // The Deployment its assignment names is stored, so the store's own count decides.
        test.sql(
            "INSERT INTO deployments (name, selector, packages, configurations) \
             VALUES ('stable', '{\"channel\": \"stable\"}', '[]', '{}')",
        );
        let uid = InstanceUid([0x84; 16]);
        crate::postgres::agents::PgAgentStore::new(database.clone())
            .put(&uid, &crate::agent_store::tests::record())
            .expect("put");
        test.sql("UPDATE agents SET health = '\\xff'::bytea");
        let dir = tempfile::tempdir().expect("blobs");
        let state = delivering_on(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::open(database.clone()).expect("open")),
            dir.path(),
        );
        assert!(
            matches!(
                state.delete_deployment("stable"),
                Err(DeploymentError::Conflict(_))
            ),
            "its assignment pins the Deployment"
        );
        assert!(state.fleet_view().is_err(), "the fleet cannot be read");
        assert!(
            state.forget_agent(&uid).is_ok(),
            "it is forgotten all the same"
        );
        // Before any read of the fleet, which would take up its absence.
        assert!(
            state.delete_deployment("stable").is_ok(),
            "and pins nothing any more"
        );
        assert!(state.fleet_view().expect("readable again").is_empty());
    }

    /// Forgetting an Agent this instance on PostgreSQL does not hold collects the revision only its
    /// assignment pinned, as forgetting one it holds does.
    /// Verifies: ADR-0032, ADR-0039
    #[test]
    fn forgetting_an_agent_held_nowhere_collects_the_revision_only_it_pinned() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        // A rollout releases through a stored Deployment.
        test.store_deployments(&["stable"]);
        let body = |text: &str| crate::configs::Revision {
            body: text.to_string(),
            role: String::new(),
            service_name: "otelcol".to_string(),
        };
        let (pinned, saved) = (body("v1\n"), body("v2\n"));
        let hash = crate::configs::revision_hash(&pinned);
        let configs = crate::postgres::configs::PgConfigBackend::new(database.clone());
        configs.save("base", &pinned).expect("save");
        let uid = InstanceUid([0x83; 16]);
        let mut stored = crate::agent_store::tests::record();
        stored.assignment = Some(Assignment {
            deployment: "stable".to_string(),
            package: None,
            configurations: BTreeMap::from([("base".to_string(), hash.clone())]),
        });
        let agents = crate::postgres::agents::PgAgentStore::new(database.clone());
        agents.put(&uid, &stored).expect("put");
        agents
            .release(
                &[crate::configs::Pinned {
                    configuration: "base".to_string(),
                    hash: hash.clone(),
                    revision: pinned,
                }],
                &[(uid, stored)],
            )
            .expect("release");
        configs.save("base", &saved).expect("save again");

        let state = on_postgres(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::open(database.clone()).expect("open")),
        );
        let retained = |state: &AppState| {
            state
                .configurations()
                .get("base")
                .expect("base")
                .retained
                .contains_key(&hash)
        };
        assert!(retained(&state), "pinned while the Agent is stored");
        assert!(state.snapshot().is_empty(), "held nowhere");
        assert!(state.forget_agent(&uid).is_ok(), "silent, so forgotten");
        assert!(!retained(&state), "collected with it");
    }

    /// A queued restart pushed to every connection that named the Agent goes out only on the one
    /// that carries it: an older connection of its host that still names it, its peer likely
    /// gone, is handed none, and the restart stays queued for the newer one.
    /// Verifies: ADR-0025
    #[test]
    fn a_pushed_restart_goes_out_only_on_the_connection_that_carries_the_agent() {
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
        let late =
            state.process_presented(restartable(3), Transport::WebSocket, Some(old), Some(&host));
        assert_eq!(late.uid, Some(uid), "the older connection still names it");
        assert!(state.request_restart(&uid).is_ok(), "queued");
        assert!(
            state.restart_command_for(&uid, old).is_none(),
            "not handed to the older connection"
        );
        assert!(state.fleet.lock().expect("fleet lock")[&uid].restart_pending);
        assert!(
            state.restart_command_for(&uid, new).is_some(),
            "handed to the newer one"
        );
    }

    /// A re-key whose move of the stored record fails leaves the Agent under the identity it has:
    /// the reply assigns no new one, the record stays stored and held under the old uid, and the
    /// session is not ended as one of an Agent forgotten.
    /// Verifies: ADR-0029
    #[test]
    fn a_rekey_whose_move_fails_keeps_the_agent_under_its_identity() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let failing = hooked.failing_rekeys.clone();
        let a = on_postgres(&database, Box::new(hooked));
        let uid = InstanceUid([0x67; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));

        failing.store(true, Ordering::SeqCst);
        let mut rekey = report(&uid, 2);
        rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
        let answered = a.process(rekey, Transport::WebSocket, Some(7));
        assert_eq!(answered.uid, Some(uid), "carried under its identity");
        assert!(
            answered.reply.agent_identification.is_none(),
            "no new identity is assigned"
        );
        assert!(a.take_forgotten(7).is_empty(), "its session goes on");
        assert_eq!(view(&a, &uid).instance_uid, uid.to_string());
        assert_eq!(
            test.rows("SELECT instance_uid FROM agents"),
            [vec![Some(uid.to_string())]],
            "the record stays stored under it"
        );
    }

    /// A report that re-keyed its Agent and then finds it no longer carried, its connection
    /// marked closed meanwhile, goes on under the identity it assigned rather than start again
    /// from the message: a next round would mint a second identity and leave the first record
    /// behind. The Agent stays disconnected, as the closed connection left it.
    /// Verifies: ADR-0025, ADR-0029
    #[test]
    fn a_report_that_rekeyed_its_agent_does_not_start_again() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let of: Arc<OnceLock<std::sync::Weak<AppState>>> = Arc::new(OnceLock::new());
        let uid = InstanceUid([0x9c; 16]);
        let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let probe: Probe = {
            let (of, armed) = (of.clone(), armed.clone());
            Arc::new(move |call, _| {
                if call == "rebind" && armed.swap(false, Ordering::SeqCst) {
                    // The connection closes while the report moves the binding.
                    let a = of.get().and_then(std::sync::Weak::upgrade).expect("A");
                    a.mark_disconnected(&[uid], 7);
                }
            })
        };
        let revocations = Revocations::open(
            Box::new(ProbedLedger {
                inner: Box::new(crate::postgres::ledger::PgLedgerStore::new(
                    database.clone(),
                )),
                probe,
            }),
            Arc::new(crate::clock::SystemClock),
            Vec::new(),
        )
        .expect("revocations");
        let a = Arc::new(
            AppState::with_stores(
                Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
                Box::new(crate::postgres::configs::PgConfigBackend::new(
                    database.clone(),
                )),
                Box::new(crate::clock::SystemClock),
            )
            .expect("state")
            .with_revocations(Some(Arc::new(revocations))),
        );
        of.set(Arc::downgrade(&a)).expect("set once");
        a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let host = of_host("h9c");
        a.process_presented(report(&uid, 1), Transport::WebSocket, Some(7), Some(&host));
        armed.store(true, Ordering::SeqCst);
        let mut rekey = report(&uid, 2);
        rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
        let answered = a.process_presented(rekey, Transport::WebSocket, Some(7), Some(&host));
        let assigned = answered
            .reply
            .agent_identification
            .as_ref()
            .map(|identification| identification.new_instance_uid.clone())
            .expect("re-keyed");
        let rows = test.rows("SELECT instance_uid FROM agents");
        assert_eq!(rows.len(), 1, "one record: {rows:?}");
        assert_eq!(
            answered.uid.map(|uid| uid.as_bytes().to_vec()),
            Some(assigned)
        );
        let moved = answered.uid.expect("the new identity");
        assert!(
            !view(&a, &moved).connected,
            "left disconnected, as its closed connection left it"
        );
    }

    /// A probe that notes each call with whether the fleet lock of the state `of` names was free
    /// as it was made.
    fn lock_probe(
        of: &Arc<OnceLock<std::sync::Weak<AppState>>>,
        seen: &Arc<Mutex<Vec<(&'static str, bool)>>>,
    ) -> Probe {
        let (of, seen) = (of.clone(), seen.clone());
        Arc::new(move |call, _| {
            let free = of
                .get()
                .and_then(std::sync::Weak::upgrade)
                .is_none_or(|state| state.fleet.try_lock().is_ok());
            seen.lock().expect("seen lock").push((call, free));
        })
    }

    /// No call a report makes to the database holds the fleet lock: the creation of a new Agent's
    /// record, the binding of its `instance_uid` and the write of its Host, its take, the read of
    /// its group's Client Agents another instance carries, the write of what it reported, and a
    /// re-key it asks for with the move of its binding are each made with the lock free, so no
    /// other report waits for them.
    /// Verifies: ADR-0025, ADR-0029
    #[test]
    fn a_report_holds_the_fleet_lock_across_no_call_to_the_database() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        // Another instance carries an Agent of the same Host, so placing this one reads it.
        let b = on_postgres(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
        );
        let elsewhere = InstanceUid([0x90; 16]);
        b.process_presented(
            report(&elsewhere, 1),
            Transport::WebSocket,
            Some(1),
            Some(&of_host("h91")),
        );
        let (of, seen) = (Arc::new(OnceLock::new()), Arc::new(Mutex::new(Vec::new())));
        let probe = lock_probe(&of, &seen);
        *hooked.probe.lock().expect("hook lock") = Some(probe.clone());
        let revocations = Revocations::open(
            Box::new(ProbedLedger {
                inner: Box::new(crate::postgres::ledger::PgLedgerStore::new(
                    database.clone(),
                )),
                probe,
            }),
            Arc::new(crate::clock::SystemClock),
            Vec::new(),
        )
        .expect("revocations");
        let a = Arc::new(
            AppState::with_stores(
                Box::new(hooked),
                Box::new(crate::postgres::configs::PgConfigBackend::new(
                    database.clone(),
                )),
                Box::new(crate::clock::SystemClock),
            )
            .expect("state")
            .with_revocations(Some(Arc::new(revocations))),
        );
        of.set(Arc::downgrade(&a)).expect("set once");
        a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);

        let uid = InstanceUid([0x91; 16]);
        let host = of_host("h91");
        a.process_presented(report(&uid, 1), Transport::WebSocket, Some(7), Some(&host));
        let mut rekey = report(&uid, 2);
        rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
        let answered = a.process_presented(rekey, Transport::WebSocket, Some(7), Some(&host));
        assert!(
            answered.reply.agent_identification.is_some(),
            "re-keyed: {:?}",
            answered.reply
        );

        let seen = seen.lock().expect("seen lock").clone();
        for call in [
            "create_within",
            "get_many",
            "bind_seen",
            "write_host_act",
            "take",
            "write_report",
            "rekey",
            "rebind",
        ] {
            assert!(
                seen.iter().any(|(made, _)| *made == call),
                "{call} was made: {seen:?}"
            );
        }
        let held: Vec<&(&str, bool)> = seen.iter().filter(|(_, free)| !free).collect();
        assert!(held.is_empty(), "the fleet lock was held across {held:?}");
    }

    /// While one Agent's report waits for a call to its store, another Agent's report is answered:
    /// what the fleet lock guards is applied before and after the call, never across it.
    /// Verifies: ADR-0025
    #[test]
    fn another_agents_report_is_answered_while_one_agents_write_waits() {
        let dir = tempfile::tempdir().expect("tempdir");
        let hooked = Hooked::new(Box::new(
            crate::fs::FsAgentStore::open(dir.path()).expect("agents"),
        ));
        let (held, other) = (InstanceUid([0x92; 16]), InstanceUid([0x93; 16]));
        let (entered, entering) = std::sync::mpsc::channel::<()>();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let released = Arc::new(Mutex::new(released));
        *hooked.probe.lock().expect("hook lock") = Some(Arc::new(move |call, uid| {
            if call == "write_report" && uid == Some(&held) {
                let _ = entered.send(());
                let _ = released
                    .lock()
                    .expect("release lock")
                    .recv_timeout(Duration::from_secs(30));
            }
        }));
        let state = Arc::new(
            AppState::with_stores(
                Box::new(hooked),
                Box::new(crate::fs::FsConfigBackend::open(dir.path()).expect("configs")),
                Box::new(crate::clock::SystemClock),
            )
            .expect("state"),
        );
        let waiting = {
            let state = state.clone();
            std::thread::spawn(move || {
                state.process(report(&held, 1), Transport::WebSocket, Some(1))
            })
        };
        entering
            .recv_timeout(Duration::from_secs(30))
            .expect("the held Agent's write is under way");
        let (answered, answering) = std::sync::mpsc::channel();
        {
            let state = state.clone();
            std::thread::spawn(move || {
                let _ =
                    answered.send(state.process(report(&other, 1), Transport::WebSocket, Some(2)));
            });
        }
        let other_answer = answering.recv_timeout(Duration::from_secs(30));
        let _ = release.send(());
        let held_answer = waiting.join().expect("the held report");
        let other_answer = other_answer.expect("the other Agent is answered while the write waits");
        assert_eq!(other_answer.uid, Some(other));
        assert_eq!(held_answer.uid, Some(held));
    }

    /// A new Agent forgotten through another instance between its creation here and the end of
    /// the report that created it is created anew by a next round of that report, so what is held
    /// is stored.
    /// Verifies: ADR-0025, ADR-0029
    #[test]
    fn a_new_agent_forgotten_while_its_report_runs_is_created_anew() {
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
        let uid = InstanceUid([0x9a; 16]);
        {
            let on = Arc::downgrade(&a);
            let gone = format!("DELETE FROM agents WHERE instance_uid = '{uid}'");
            let test = test.sql_runner();
            *after_take.lock().expect("hook lock") = Some(Box::new(move || {
                test(&gone);
                on.upgrade()
                    .expect("A")
                    .take_up_agents(&HashSet::from([uid]))
                    .expect("A hears the Agent forgotten");
            }));
        }
        let answered = a.process(report(&uid, 1), Transport::WebSocket, Some(8));
        assert_eq!(answered.uid, Some(uid), "carried");
        assert!(view(&a, &uid).connected, "held");
        assert_eq!(
            test.rows(&format!(
                "SELECT instance_uid FROM agents WHERE instance_uid = '{uid}'"
            ))
            .len(),
            1,
            "stored anew"
        );
    }

    /// A restart requested while a delivery of the one queued before clears it is not lost: the
    /// request waits for the delivery, which holds the Agent's own lock across its clear, and is
    /// queued after it, so the Agent is sent it next.
    /// Verifies: ADR-0025
    #[test]
    fn a_restart_requested_while_a_delivery_clears_one_stays_queued() {
        let dir = tempfile::tempdir().expect("tempdir");
        let hooked = Hooked::new(Box::new(
            crate::fs::FsAgentStore::open(dir.path()).expect("agents"),
        ));
        let probe = hooked.probe.clone();
        let state = Arc::new(
            AppState::with_stores(
                Box::new(hooked),
                Box::new(crate::fs::FsConfigBackend::open(dir.path()).expect("configs")),
                Box::new(crate::clock::SystemClock),
            )
            .expect("state"),
        );
        let uid = InstanceUid([0x95; 16]);
        state.process(restartable(&uid, 1), Transport::WebSocket, Some(1));
        assert!(state.request_restart(&uid).is_ok(), "queued");

        let (requested, requesting) = std::sync::mpsc::channel();
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let armed = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let waits_before = state.agent_lock_waits();
        {
            let (on, finished) = (Arc::downgrade(&state), finished.clone());
            *probe.lock().expect("hook lock") = Some(Arc::new(move |call, _| {
                if call != "set_restart_pending" || !armed.swap(false, Ordering::SeqCst) {
                    return;
                }
                let state = on.upgrade().expect("state");
                let (again, done, requested) = (state.clone(), finished.clone(), requested.clone());
                std::thread::spawn(move || {
                    let request = again.request_restart(&uid).is_ok();
                    done.store(true, Ordering::SeqCst);
                    let _ = requested.send(request);
                });
                until("the request waits for the delivery or is done", || {
                    state.agent_lock_waits() > waits_before || finished.load(Ordering::SeqCst)
                });
            }));
        }
        let delivered = state.process(restartable(&uid, 2), Transport::WebSocket, Some(1));
        assert!(
            delivered.reply.command.is_some(),
            "the first restart is delivered"
        );
        assert!(
            requesting
                .recv_timeout(Duration::from_secs(30))
                .expect("the second request is answered"),
            "queued again"
        );
        assert!(
            state.restart_command_for(&uid, 1).is_some(),
            "the second restart is sent next"
        );
    }

    /// A restart requested through one instance after the carrying instance cleared the one it
    /// delivers, but before it took the clear into memory, is not lost: the take-up of its
    /// announcement waits for the delivery, which holds the Agent's own lock, and then holds the
    /// request, which is sent next.
    /// Verifies: ADR-0029, ADR-0025
    #[test]
    fn a_restart_requested_between_a_deliverys_clear_and_its_end_is_sent_next() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let after_clear = hooked.after_clear.clone();
        let a = Arc::new(on_postgres(&database, Box::new(hooked)));
        let b = Arc::new(on_postgres(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
        ));
        let uid = InstanceUid([0x99; 16]);
        a.process(restartable(&uid, 1), Transport::WebSocket, Some(1));
        assert!(a.request_restart(&uid).is_ok(), "queued");

        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let waits_before = a.agent_lock_waits();
        let (answered, answering) = std::sync::mpsc::channel();
        {
            let (on, b, finished) = (Arc::downgrade(&a), b.clone(), finished.clone());
            *after_clear.lock().expect("hook lock") = Some(Box::new(move || {
                let a = on.upgrade().expect("A");
                let (again, done) = (a.clone(), finished.clone());
                std::thread::spawn(move || {
                    let requested = b.request_restart(&uid).is_ok();
                    // The announcement, once the request committed.
                    let named = HashSet::from([uid]);
                    let taken = again.take_up_agents(&named).is_ok()
                        && again.take_up_restarts(&named).is_ok();
                    done.store(true, Ordering::SeqCst);
                    let _ = answered.send(requested && taken);
                });
                until("the take-up waits for the delivery or is done", || {
                    a.agent_lock_waits() > waits_before || finished.load(Ordering::SeqCst)
                });
            }));
        }
        assert!(
            a.restart_command_for(&uid, 1).is_some(),
            "the first restart is delivered"
        );
        assert!(
            answering
                .recv_timeout(Duration::from_secs(30))
                .expect("the request and its take-up are answered"),
            "requested and taken up"
        );
        assert!(
            a.restart_command_for(&uid, 1).is_some(),
            "the request made between the clear and the delivery's end is sent next"
        );
    }

    /// The same request taken up after a missed notification, which names no Agent, is not lost
    /// either: the catch-up reads the restart the store queues under the Agent's own lock, waits
    /// for the delivery, and holds the request, which is sent next.
    /// Verifies: ADR-0029, ADR-0025
    #[test]
    fn a_restart_requested_between_a_deliverys_clear_and_its_end_is_sent_after_a_missed_notification(
    ) {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let after_clear = hooked.after_clear.clone();
        let a = Arc::new(on_postgres(&database, Box::new(hooked)));
        let b = Arc::new(on_postgres(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
        ));
        let uid = InstanceUid([0x96; 16]);
        a.process(restartable(&uid, 1), Transport::WebSocket, Some(1));
        assert!(a.request_restart(&uid).is_ok(), "queued");

        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let waits_before = a.agent_lock_waits();
        let (answered, answering) = std::sync::mpsc::channel();
        {
            let (on, b, finished) = (Arc::downgrade(&a), b.clone(), finished.clone());
            *after_clear.lock().expect("hook lock") = Some(Box::new(move || {
                let a = on.upgrade().expect("A");
                let (again, done) = (a.clone(), finished.clone());
                std::thread::spawn(move || {
                    let requested = b.request_restart(&uid).is_ok();
                    // The announcement is missed: the instance catches up, as it does then.
                    let taken = again.catch_up().is_ok();
                    done.store(true, Ordering::SeqCst);
                    let _ = answered.send(requested && taken);
                });
                until("the take-up waits for the delivery or is done", || {
                    a.agent_lock_waits() > waits_before || finished.load(Ordering::SeqCst)
                });
            }));
        }
        assert!(
            a.restart_command_for(&uid, 1).is_some(),
            "the first restart is delivered"
        );
        assert!(
            answering
                .recv_timeout(Duration::from_secs(30))
                .expect("the request and its take-up are answered"),
            "requested and taken up"
        );
        assert!(
            a.restart_command_for(&uid, 1).is_some(),
            "the request made between the clear and the delivery's end is sent next"
        );
    }

    /// A restart requested through one instance while the instance that carries the Agent
    /// delivers the one queued before is not lost, however the two meet: where the request came
    /// before the delivery's clear, the restart sent is both; where it came after, it stays queued
    /// and is sent next, once the carrying instance takes the announcement up.
    /// Verifies: ADR-0029, ADR-0025
    #[test]
    fn a_restart_requested_through_one_instance_while_another_delivers_one_stays_until_sent() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let on = || {
            Arc::new(on_postgres(
                &database,
                Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
            ))
        };
        let (a, b) = (on(), on());
        let uid = InstanceUid([0x97; 16]);
        a.process(restartable(&uid, 1), Transport::WebSocket, Some(1));
        let queued = || {
            test.rows(&format!(
                "SELECT restart_pending::text FROM agents WHERE instance_uid = '{uid}'"
            ))[0][0]
                .clone()
                .expect("a value")
                == "true"
        };
        let (mut merged, mut after) = (0, 0);
        for round in 0..40u64 {
            assert!(a.request_restart(&uid).is_ok(), "round {round}: queued");
            let met = Arc::new(std::sync::Barrier::new(2));
            let delivering = {
                let (a, met) = (a.clone(), met.clone());
                std::thread::spawn(move || {
                    met.wait();
                    // Later by a step each round, so the request meets the delivery before its
                    // clear, during it, and after it.
                    std::thread::sleep(Duration::from_micros(round * 25));
                    a.restart_command_for(&uid, 1).is_some()
                })
            };
            let requesting = {
                let (a, b, met) = (a.clone(), b.clone(), met.clone());
                std::thread::spawn(move || {
                    met.wait();
                    let requested = b.request_restart(&uid).is_ok();
                    // The announcement reaches the carrying instance once the request committed,
                    // which takes the Agent up and then its kept restarts, as a notice does.
                    let named = HashSet::from([uid]);
                    a.take_up_agents(&named).expect("taken up");
                    a.take_up_restarts(&named).expect("taken up");
                    requested
                })
            };
            assert!(
                delivering.join().expect("delivery"),
                "round {round}: the first restart is delivered"
            );
            assert!(
                requesting.join().expect("request"),
                "round {round}: the second request is answered"
            );
            if queued() {
                after += 1;
                assert!(
                    a.restart_command_for(&uid, 1).is_some(),
                    "round {round}: the request that stayed queued is sent next"
                );
                assert!(!queued(), "round {round}: sending it cleared it");
            } else {
                merged += 1;
                assert!(
                    a.restart_command_for(&uid, 1).is_none(),
                    "round {round}: a request the delivery took is not sent twice"
                );
            }
        }
        eprintln!("{merged} requests taken by the delivery, {after} queued after it");
    }

    /// A label set while a report reads the Agent's record from the store, before it holds it, is
    /// not undone by what the read found: the label waits for the report, which holds the Agent's
    /// own lock, and is then taken into the record held.
    /// Verifies: ADR-0025
    #[test]
    fn a_label_set_while_a_report_reads_the_record_is_not_undone() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let after_get = hooked.after_get.clone();
        let a = Arc::new(on_postgres(&database, Box::new(hooked)));
        let b = on_postgres(
            &database,
            Box::new(crate::postgres::agents::PgAgentStore::new(database.clone())),
        );
        let uid = InstanceUid([0x96; 16]);
        b.process(report(&uid, 1), Transport::Http, None);
        label(&b, &uid, &[("tier", "edge")]);

        let (labelled, labelling) = std::sync::mpsc::channel();
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let waits_before = a.agent_lock_waits();
        {
            let (on, finished) = (Arc::downgrade(&a), finished.clone());
            *after_get.lock().expect("hook lock") = Some(Box::new(move || {
                let a = on.upgrade().expect("A");
                let (again, done) = (a.clone(), finished.clone());
                std::thread::spawn(move || {
                    label(&again, &uid, &[("tier", "core")]);
                    done.store(true, Ordering::SeqCst);
                    let _ = labelled.send(());
                });
                until("the label waits for the report or is done", || {
                    a.agent_lock_waits() > waits_before || finished.load(Ordering::SeqCst)
                });
            }));
        }
        a.process(report(&uid, 2), Transport::Http, None);
        labelling
            .recv_timeout(Duration::from_secs(30))
            .expect("the label is set");
        assert_eq!(view(&a, &uid).labels["tier"], "core", "the label set stays");
    }

    /// A report whose Agent leaves memory while it reads and writes the store — forgotten through
    /// another instance — starts again from what the store holds then, and the Agent is held
    /// under a record created anew.
    /// Verifies: ADR-0025, ADR-0029
    #[test]
    fn a_report_whose_agent_leaves_memory_meanwhile_starts_again() {
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
        let uid = InstanceUid([0x97; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        a.mark_disconnected(&[uid], 7);
        {
            let on = Arc::downgrade(&a);
            let gone = format!("DELETE FROM agents WHERE instance_uid = '{uid}'");
            let test = test.sql_runner();
            *after_take.lock().expect("hook lock") = Some(Box::new(move || {
                test(&gone);
                on.upgrade()
                    .expect("A")
                    .take_up_agents(&HashSet::from([uid]))
                    .expect("A hears the Agent forgotten");
            }));
        }
        let answered = a.process(report(&uid, 2), Transport::WebSocket, Some(8));
        assert_eq!(answered.uid, Some(uid), "carried");
        assert!(view(&a, &uid).connected, "held");
        assert_eq!(
            test.rows(&format!(
                "SELECT instance_uid FROM agents WHERE instance_uid = '{uid}'"
            ))
            .len(),
            1,
            "stored anew"
        );
    }

    /// A report whose Agent leaves memory in each of its rounds is answered `Unavailable`, so the
    /// Agent retries, rather than starting again without end.
    /// Verifies: ADR-0025
    #[test]
    fn a_report_whose_agent_leaves_memory_in_every_round_is_answered_unavailable() {
        let Some(test) = crate::postgres::testing::TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hooked = Hooked::new(Box::new(crate::postgres::agents::PgAgentStore::new(
            database.clone(),
        )));
        let probe = hooked.probe.clone();
        let a = Arc::new(on_postgres(&database, Box::new(hooked)));
        a.serve_under(tokio::sync::watch::channel(Some(LEASE_A.to_string())).1);
        let uid = InstanceUid([0x98; 16]);
        a.process(report(&uid, 1), Transport::WebSocket, Some(7));
        a.mark_disconnected(&[uid], 7);
        let rounds = Arc::new(AtomicU64::new(0));
        {
            let (on, rounds) = (Arc::downgrade(&a), rounds.clone());
            let other = crate::postgres::agents::PgAgentStore::new(database.clone());
            let gone = format!("DELETE FROM agents WHERE instance_uid = '{uid}'");
            let test = test.sql_runner();
            *probe.lock().expect("hook lock") = Some(Arc::new(move |call, _| {
                if call != "take" {
                    return;
                }
                rounds.fetch_add(1, Ordering::SeqCst);
                // Forgotten and heard of here, then stored again by another instance.
                test(&gone);
                on.upgrade()
                    .expect("A")
                    .take_up_agents(&HashSet::from([uid]))
                    .expect("A hears the Agent forgotten");
                other
                    .put(&uid, &crate::agent_store::tests::record())
                    .expect("stored again");
            }));
        }
        let answered = a.process(report(&uid, 2), Transport::WebSocket, Some(8));
        assert_eq!(rounds.load(Ordering::SeqCst), REPORT_ROUNDS as u64);
        assert!(
            answered
                .reply
                .error_response
                .as_ref()
                .is_some_and(|error| error.error_message.contains("retry later")),
            "answered unavailable: {:?}",
            answered.reply
        );
        assert_eq!(answered.uid, None);
    }

    /// Labels are part of the Agent's record: a re-keyed Agent keeps them, on disk too, and
    /// forgetting it removes them — it comes back unlabelled.
    /// Verifies: ADR-0029
    #[test]
    fn forgetting_an_agent_removes_its_labels_and_a_rekey_keeps_them() {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let state = AppState::new(dir.clone())
            .expect("state")
            .with_stale_after(Duration::from_secs(90));
        let uid = InstanceUid::default();
        state.process(report(&uid, 1), Transport::WebSocket, Some(1));
        label(&state, &uid, &[("rollout", "canary")]);

        let mut rekey = report(&uid, 2);
        rekey.flags = AgentToServerFlags::RequestInstanceUid as u64;
        let new_uid = state
            .process(rekey, Transport::WebSocket, Some(1))
            .uid
            .expect("the new identity");
        assert_eq!(view(&state, &new_uid).labels["rollout"], "canary");
        let stored = std::fs::read_to_string(dir.join("agents").join(format!("{new_uid}.json")))
            .expect("stored");
        assert!(stored.contains("canary"), "{stored}");

        let mut goodbye = report(&new_uid, 3);
        goodbye.agent_disconnect = Some(opamp::proto::AgentDisconnect {});
        state.process(goodbye, Transport::WebSocket, Some(1));
        assert!(state.forget_agent(&new_uid).is_ok());
        state.process(report(&new_uid, 4), Transport::WebSocket, Some(2));
        assert!(
            view(&state, &new_uid).labels.is_empty(),
            "it comes back unlabelled"
        );
        assert!(!dir.join("labels").exists(), "no label store is written");
    }
}
