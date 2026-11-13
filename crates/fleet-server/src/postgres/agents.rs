//! Agent records in PostgreSQL (ADR-0023 clauses 4 and 21): the PostgreSQL adapter behind
//! [`AgentStore`](crate::agent_store::AgentStore) — one row per Agent, its scalars as columns, the
//! wire-typed fields as their protobuf bytes, its labels, and its assignment's Configurations in
//! rows of their own. A rollout's revisions and assignments commit together or not at all. Each
//! call writes the columns it names and no other, and announces in its transaction what another
//! instance carrying the Agent takes up: a label change, a restart request, a release, a forget.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use super::Cached as GenericClient;
use opamp::uid::InstanceUid;
use prost::Message;

use super::notify::{
    follow_commit, notify, will_follow, ARRIVED, ASSIGNMENT, CONFIGURATION, FORGOTTEN, LABELS,
    RESTART, TAKEN,
};
use super::{client, creating_client, signed, snapshot, sql, unsigned, Database};
use crate::agent_store::{
    AgentPage, AgentStore, Creation, Holder, PageQuery, PersistedAgent, Pin, Presence, Silenced,
    StoredAgent, Taken,
};
use crate::configs::Pinned;
use crate::fleet::{Assignment, Transport};
use crate::packages::PackageId;

/// The Agents in `agents` and `agent_assignment_configurations`. The integrity queries are
/// answered from the assignments this adapter last read or committed, as the filesystem adapter
/// answers them: the port gives them no way to fail, and an answer that a lost database turned
/// into "nothing pins it" would let a pinned revision go.
pub struct PgAgentStore {
    database: Arc<Database>,
    /// Every stored record's assignment, as committed.
    assignments: Mutex<Pins>,
    /// The creations of this instance under way, at most half the pool: the creations of every
    /// instance wait for the count's row, which a Server of 0.8.0 holds across its whole creation,
    /// and those past the bound wait here, each on its own thread, rather than on a connection of
    /// the pool, which every other call needs.
    creating: Creating,
    /// Run between a read's rows and its taking them into [`Pins`], in tests.
    #[cfg(test)]
    after_read: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

/// The assignments the adapter holds, and when this instance last committed each one, so a read
/// that began before a commit of this instance never takes the older assignment over it: a fleet
/// read never shrinks what pins what below what this instance itself committed.
#[derive(Default)]
struct Pins {
    assignments: HashMap<InstanceUid, Assignment>,
    /// Counts the commits of this instance that changed an assignment.
    commits: u64,
    /// The count at which this instance last committed the assignment of each record. Every count
    /// above where the oldest read in flight began is kept, since it may decide for that read; one
    /// at or below it is dropped, and with no read in flight none is kept. What an Agent's re-keys
    /// or many releases add therefore leaves once every read that began before it has ended, and
    /// every read ends within its call's bound.
    committed: HashMap<InstanceUid, u64>,
    /// Where each read in flight began, and how many began there.
    in_flight: BTreeMap<u64, usize>,
}

impl Pins {
    /// Notes that this instance committed the assignment of `uid` at `commit`, for the reads in
    /// flight; with none in flight there is nothing to note it for.
    fn committed(&mut self, uid: InstanceUid, commit: u64) {
        if !self.in_flight.is_empty() {
            self.committed.insert(uid, commit);
        }
    }

    /// Ends the read that began at `commit`, and drops each count that no read still in flight
    /// can be decided by: those at or below where the oldest of them began.
    fn ended(&mut self, commit: u64) {
        if let Some(count) = self.in_flight.get_mut(&commit) {
            *count -= 1;
            if *count == 0 {
                self.in_flight.remove(&commit);
            }
        }
        match self.in_flight.keys().next().copied() {
            None => self.committed = HashMap::new(),
            Some(oldest) => self.committed.retain(|_, noted| *noted > oldest),
        }
    }
}

/// A read in flight, from where it began until it took up what it read or failed.
struct Began<'a> {
    pins: &'a Mutex<Pins>,
    commit: u64,
}

impl Drop for Began<'_> {
    fn drop(&mut self) {
        self.pins
            .lock()
            .expect("assignments lock")
            .ended(self.commit);
    }
}

/// Which records a read read.
enum Read<'a> {
    /// Every record: one not found is not stored.
    All,
    /// These: one not found is not stored.
    Only(&'a [InstanceUid]),
}

impl PgAgentStore {
    /// The adapter on `database`.
    #[must_use]
    pub fn new(database: Arc<Database>) -> Self {
        PgAgentStore {
            creating: Creating::of(database.pool_size()),
            database,
            assignments: Mutex::new(Pins::default()),
            #[cfg(test)]
            after_read: Mutex::new(None),
        }
    }

    /// The adapter on `database`, holding what the stored assignments pin, read alone: what a
    /// deletion and a collection ask of the pins is answered from them, and no record is loaded
    /// (ADR-0023 clause 3).
    ///
    /// # Errors
    /// Returns an error when the assignments cannot be read, or one names a Package that does not
    /// parse.
    pub fn open(database: Arc<Database>) -> Result<Self, String> {
        let store = Self::new(database);
        let assignments = store.database.run(|pool| async move {
            let client = client(&pool).await?;
            read_assignments(&client).await
        })?;
        store
            .assignments
            .lock()
            .expect("assignments lock")
            .assignments = assignments;
        Ok(store)
    }

    /// Notes what the records this instance committed now pin.
    fn stored<'a>(
        &self,
        records: impl IntoIterator<Item = (&'a InstanceUid, Option<&'a Assignment>)>,
    ) {
        let mut pins = self.assignments.lock().expect("assignments lock");
        pins.commits += 1;
        let commit = pins.commits;
        for (uid, assignment) in records {
            pins.committed(*uid, commit);
            match assignment {
                Some(assignment) => pins.assignments.insert(*uid, assignment.clone()),
                None => pins.assignments.remove(uid),
            };
        }
    }

    /// Where a read begins: what it reads is taken over no commit of this instance made since.
    fn began(&self) -> Began<'_> {
        let mut pins = self.assignments.lock().expect("assignments lock");
        let commit = pins.commits;
        *pins.in_flight.entry(commit).or_default() += 1;
        Began {
            pins: &self.assignments,
            commit,
        }
    }

    /// Takes what a read that began at `began` found into what pins what, for each record it
    /// read that this instance committed nothing of since.
    fn refresh<'a>(
        &self,
        began: Began<'_>,
        read: Read<'_>,
        found: impl Fn(&InstanceUid) -> Option<&'a Assignment>,
        keys: impl Iterator<Item = &'a InstanceUid>,
    ) {
        #[cfg(test)]
        {
            let after = self.after_read.lock().expect("hook lock").take();
            if let Some(after) = after {
                after();
            }
        }
        let mut pins = self.assignments.lock().expect("assignments lock");
        let uids: std::collections::HashSet<InstanceUid> = match read {
            Read::All => keys
                .copied()
                .chain(pins.assignments.keys().copied())
                .collect(),
            Read::Only(uids) => uids.iter().copied().collect(),
        };
        for uid in uids {
            if pins
                .committed
                .get(&uid)
                .is_some_and(|commit| *commit > began.commit)
            {
                continue;
            }
            match found(&uid) {
                Some(assignment) => pins.assignments.insert(uid, assignment.clone()),
                None => pins.assignments.remove(&uid),
            };
        }
    }

    /// Runs `writes` in one transaction.
    fn transaction<F>(
        &self,
        writes: impl FnOnce(deadpool_postgres::Object) -> F + Send + 'static,
    ) -> Result<(), String>
    where
        F: std::future::Future<Output = Result<(), String>> + Send + 'static,
    {
        self.database
            .run(move |pool| async move { writes(client(&pool).await?).await })
    }
}

impl AgentStore for PgAgentStore {
    fn load(&self) -> Result<HashMap<InstanceUid, PersistedAgent>, String> {
        let began = self.began();
        let records = self.database.run(|pool| async move {
            let client = client(&pool).await?;
            read_all(&client).await
        })?;
        self.refresh(
            began,
            Read::All,
            |uid| {
                records
                    .get(uid)
                    .and_then(|record| record.assignment.as_ref())
            },
            records.keys(),
        );
        Ok(records)
    }

    fn get(&self, uid: &InstanceUid) -> Result<Option<PersistedAgent>, String> {
        Ok(self.get_many(std::slice::from_ref(uid))?.remove(uid))
    }

    /// One query for the rows and their Configurations, however many records it reads.
    fn get_many(
        &self,
        uids: &[InstanceUid],
    ) -> Result<HashMap<InstanceUid, PersistedAgent>, String> {
        let began = self.began();
        let keys: Vec<String> = uids.iter().map(ToString::to_string).collect();
        let found = self.database.run(move |pool| async move {
            let client = client(&pool).await?;
            read(&client, Some(&keys)).await
        })?;
        self.refresh(
            began,
            Read::Only(uids),
            |uid| found.get(uid).and_then(|record| record.assignment.as_ref()),
            found.keys(),
        );
        Ok(found)
    }

    /// The rows and their presence, then their Configurations, in one snapshot of the database.
    fn query(
        &self,
        only: Option<&[InstanceUid]>,
    ) -> Result<Vec<(InstanceUid, StoredAgent)>, String> {
        let began = self.began();
        let keys: Option<Vec<String>> =
            only.map(|uids| uids.iter().map(ToString::to_string).collect());
        let (records, mut presence) = self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = snapshot(&mut client).await?;
            let records = read(&transaction, keys.as_deref()).await?;
            let (filter, params): (&str, Vec<&(dyn tokio_postgres::types::ToSql + Sync)>) =
                match &keys {
                    Some(keys) => (" AND a.instance_uid = ANY($1)", vec![keys]),
                    None => ("", Vec::new()),
                };
            let presence: HashMap<String, (bool, Holder)> = transaction
                .query(
                    &format!(
                        "SELECT a.instance_uid, a.connected, l.instance_id::text, l.host \
                         FROM agents AS a JOIN leases AS l ON l.instance_id = a.holder \
                         AND l.expires_at > now(){filter}"
                    ),
                    &params,
                )
                .await
                .map_err(sql)?
                .into_iter()
                .map(|row| {
                    (
                        row.get(0),
                        (
                            row.get(1),
                            Holder {
                                instance_id: row.get(2),
                                host: row.get(3),
                            },
                        ),
                    )
                })
                .collect();
            transaction.commit().await.map_err(sql)?;
            Ok((records, presence))
        })?;
        self.refresh(
            began,
            only.map_or(Read::All, Read::Only),
            |uid| {
                records
                    .get(uid)
                    .and_then(|record| record.assignment.as_ref())
            },
            records.keys(),
        );
        let mut all: Vec<(InstanceUid, StoredAgent)> = records
            .into_iter()
            .map(|(uid, record)| {
                let (connected, holder) = presence
                    .remove(&uid.to_string())
                    .map_or((false, None), |(connected, holder)| {
                        (connected, Some(holder))
                    });
                (
                    uid,
                    StoredAgent {
                        record,
                        connected,
                        holder,
                    },
                )
            })
            .collect();
        all.sort_by_key(|(uid, _)| uid.0);
        Ok(all)
    }

    fn is_shared(&self) -> bool {
        true
    }

    /// The rows written without their derived columns derived again first, then one
    /// `SELECT DISTINCT` over those columns for each.
    fn reported_values(&self, limit: usize) -> Result<crate::agent_store::ReportedValues, String> {
        super::agent_page::reported_values(&self.database, limit)
    }

    /// The rows written without their derived columns derived again first, then the count and
    /// the page in one snapshot of the database.
    fn query_page(&self, query: &PageQuery<'_>) -> Result<AgentPage, String> {
        let began = self.began();
        let page = super::agent_page::query_page(&self.database, query)?;
        let uids: Vec<InstanceUid> = page.agents.iter().map(|agent| agent.uid).collect();
        let found: HashMap<InstanceUid, Option<&Assignment>> = page
            .agents
            .iter()
            .map(|agent| (agent.uid, agent.stored.record.assignment.as_ref()))
            .collect();
        self.refresh(
            began,
            Read::Only(&uids),
            |uid| found.get(uid).copied().flatten(),
            uids.iter(),
        );
        Ok(page)
    }

    /// A new record whole; of one that exists, the columns a report decides alone, so a label
    /// change, a restart request or a release committed through another instance stays.
    fn put(&self, uid: &InstanceUid, record: &PersistedAgent) -> Result<(), String> {
        let (uid, record) = (*uid, record.clone());
        let created = self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            // One another instance created in between the two is written to again, the columns a
            // report decides alone.
            let created = loop {
                if write_reported(&transaction, &uid, &record, None).await? {
                    break false;
                }
                if insert(&transaction, &uid, &record, "DO NOTHING").await? {
                    break true;
                }
            };
            transaction.commit().await.map_err(sql)?;
            Ok(created.then_some(record.assignment))
        })?;
        if let Some(assignment) = created {
            self.stored([(&uid, assignment.as_ref())]);
        }
        Ok(())
    }

    /// The columns a report decides, of a row that is there; nothing where none is. A Client Agent
    /// first arriving over a Gateway's Host is announced right after the commit, named by the Host
    /// ([`follow_commit`]).
    fn write_report(
        &self,
        uid: &InstanceUid,
        record: &PersistedAgent,
        arrived: Option<&str>,
    ) -> Result<bool, String> {
        let (uid, record) = (*uid, record.clone());
        let arrived = arrived.map(str::to_string);
        self.database.run(move |pool| async move {
            let client = client(&pool).await?;
            // Counted before the write commits, so a write whose answer is lost has its instance
            // read again what the notice names.
            if let Some(host) = &arrived {
                will_follow(ARRIVED, host);
            }
            let written = write_reported(&client, &uid, &record, None).await?;
            if written && arrived.is_some() {
                follow_commit(client, &pool);
            }
            Ok(written)
        })
    }

    /// [`write_report`](AgentStore::write_report), of a row whose holder is `holder`, or none and
    /// whose count of takes is still `take`, and whose sequence number is not higher than the
    /// record's.
    fn write_report_unless_held_elsewhere(
        &self,
        uid: &InstanceUid,
        record: &PersistedAgent,
        holder: Option<&str>,
        take: Option<u64>,
    ) -> Result<bool, String> {
        let (uid, record) = (*uid, record.clone());
        let holder = holder.map(str::to_string);
        self.database.run(move |pool| async move {
            let client = client(&pool).await?;
            write_reported(
                &client,
                &uid,
                &record,
                Some(Held {
                    holder: holder.as_deref(),
                    take: take.map(signed),
                }),
            )
            .await
        })
    }

    /// One statement that locks the row the triggers on `agents` keep the count in and inserts
    /// only while the count is below `max_records`, so creations on every instance count each
    /// other's rows one after another, the ceiling is decided without reading every row, and no
    /// creation waits for a client round trip of another (ADR-0023 clause 22). Its transaction
    /// commits without waiting for the flush, so neither does any for a flush of another
    /// (ADR-0023 clause 21). A report's write of a record that exists waits for none of it.
    fn create_within(
        &self,
        uid: &InstanceUid,
        record: &PersistedAgent,
        max_records: usize,
    ) -> Result<Creation, String> {
        let (uid, record) = (*uid, record.clone());
        let assignment = record.assignment.clone();
        let max = i64::try_from(max_records).unwrap_or(i64::MAX);
        let creating = self.creating.enter();
        let creation = self.database.run(move |pool| async move {
            // A creation waits for a connection up to its call's bound (ADR-0023 clause 24).
            let mut client = creating_client(&pool).await?;
            let key = uid.to_string();
            // One transaction that commits without waiting for its flush, so the count's row is
            // let go at once (ADR-0023 clause 21); the record's Configurations, where it names
            // any, are written in it.
            let transaction = client.transaction().await.map_err(sql)?;
            transaction
                .batch_execute(ASYNCHRONOUS_COMMIT)
                .await
                .map_err(sql)?;
            let created = insert_as(&transaction, &uid, &record, "DO NOTHING", Some(max)).await?;
            transaction.commit().await.map_err(sql)?;
            if created {
                return Ok(Creation::Created);
            }
            // Stored already, or the count is at the ceiling.
            Ok(
                match read(&client, Some(std::slice::from_ref(&key)))
                    .await?
                    .remove(&uid)
                {
                    Some(found) => Creation::Found(Box::new(found)),
                    None => Creation::Full,
                },
            )
        });
        drop(creating);
        let creation = creation?;
        match &creation {
            Creation::Created => self.stored([(&uid, assignment.as_ref())]),
            Creation::Found(found) => self.stored([(&uid, found.assignment.as_ref())]),
            Creation::Full => {}
        }
        Ok(creation)
    }

    fn set_labels(
        &self,
        uid: &InstanceUid,
        labels: &BTreeMap<String, String>,
    ) -> Result<bool, String> {
        let key = uid.to_string();
        let labels = serde_json::to_value(labels).expect("labels serialize");
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            let set = transaction
                .execute(
                    "UPDATE agents SET labels = $2 WHERE instance_uid = $1",
                    &[&key, &labels],
                )
                .await
                .map_err(sql)?
                > 0;
            if set {
                notify(&transaction, LABELS, &key).await?;
            }
            transaction.commit().await.map_err(sql)?;
            Ok(set)
        })
    }

    /// Reads what is queued under the row's lock and writes it only when it changes; a request is
    /// announced, a clear is not, since no other instance acts on it.
    fn set_restart_pending(
        &self,
        uid: &InstanceUid,
        pending: bool,
    ) -> Result<Option<bool>, String> {
        let key = uid.to_string();
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            let Some(row) = transaction
                .query_opt(
                    "SELECT restart_pending FROM agents WHERE instance_uid = $1 FOR UPDATE",
                    &[&key],
                )
                .await
                .map_err(sql)?
            else {
                return Ok(None);
            };
            let was: bool = row.get(0);
            if was != pending {
                transaction
                    .execute(
                        "UPDATE agents SET restart_pending = $2 WHERE instance_uid = $1",
                        &[&key, &pending],
                    )
                    .await
                    .map_err(sql)?;
            }
            if pending {
                notify(&transaction, RESTART, &key).await?;
            }
            transaction.commit().await.map_err(sql)?;
            Ok(Some(was))
        })
    }

    fn remove(&self, uid: &InstanceUid) -> Result<(), String> {
        let uid = *uid;
        self.transaction(move |mut client| async move {
            let transaction = client.transaction().await.map_err(sql)?;
            let removed = transaction
                .execute(
                    "DELETE FROM agents WHERE instance_uid = $1",
                    &[&uid.to_string()],
                )
                .await
                .map_err(sql)?;
            if removed > 0 {
                notify(&transaction, FORGOTTEN, &uid.to_string()).await?;
            }
            transaction.commit().await.map_err(sql)
        })?;
        self.stored([(&uid, None)]);
        Ok(())
    }

    /// One transaction: the rows locked in the order of their `instance_uid`, byte by byte as the
    /// release orders them, then the batch written in one statement. A connected Agent is named
    /// anew only where the row names no instance with a live lease, and where it comes with a count
    /// only while the row still holds it: another instance took it after this one read the batch. A disconnect whose row holds another count of takes than it comes with
    /// is not written: the Agent was taken since, by this instance or another, and the take wrote
    /// its presence.
    fn write_presence(
        &self,
        holder: &str,
        presence: &[(InstanceUid, Presence, Option<u64>)],
    ) -> Result<HashMap<InstanceUid, u64>, String> {
        if presence.is_empty() {
            return Ok(HashMap::new());
        }
        let holder = holder.to_string();
        let mut batch: Vec<(String, bool, i64, Option<i64>)> = presence
            .iter()
            .map(|(uid, presence, take)| {
                (
                    uid.to_string(),
                    presence.connected,
                    signed(presence.last_seen_ms),
                    take.map(signed),
                )
            })
            .collect();
        batch.sort_by(|(a, ..), (b, ..)| a.cmp(b));
        let written = batch.len();
        let rows = self.database.run_writing(written, move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            let uids: Vec<&str> = batch.iter().map(|(uid, ..)| uid.as_str()).collect();
            let connected: Vec<bool> = batch.iter().map(|(_, connected, ..)| *connected).collect();
            let last_seen: Vec<i64> = batch
                .iter()
                .map(|(_, _, last_seen, _)| *last_seen)
                .collect();
            let takes: Vec<Option<i64>> = batch.iter().map(|(.., take)| *take).collect();
            transaction
                .execute(
                    "SELECT 1 FROM agents WHERE instance_uid = ANY($1) \
                     ORDER BY instance_uid COLLATE \"C\" FOR UPDATE",
                    &[&uids],
                )
                .await
                .map_err(sql)?;
            let rows = transaction
                .query(
                    "UPDATE agents AS a SET \
                     last_seen_ms = GREATEST(a.last_seen_ms, p.last_seen_ms), \
                     connected = CASE WHEN p.connected THEN true \
                         WHEN a.holder = $4::text::uuid AND a.takes = p.take THEN false \
                         ELSE a.connected END, \
                     holder = CASE WHEN p.connected THEN $4::text::uuid \
                         WHEN a.holder = $4::text::uuid AND a.takes = p.take THEN NULL \
                         ELSE a.holder END, \
                     takes = CASE WHEN p.connected AND a.holder IS DISTINCT FROM $4::text::uuid \
                         THEN a.takes + 1 ELSE a.takes END \
                     FROM unnest($1::text[], $2::boolean[], $3::bigint[], $5::bigint[]) \
                         AS p(instance_uid, connected, last_seen_ms, take) \
                     WHERE a.instance_uid = p.instance_uid \
                         AND (NOT p.connected OR a.holder = $4::text::uuid \
                             OR ((p.take IS NULL OR a.takes = p.take) \
                                 AND (a.holder IS NULL \
                                     OR NOT EXISTS (SELECT 1 FROM leases AS l \
                                         WHERE l.instance_id = a.holder \
                                             AND l.expires_at > now())))) \
                     RETURNING a.instance_uid, p.connected, a.takes",
                    &[&uids, &connected, &last_seen, &holder, &takes],
                )
                .await
                .map_err(sql)?;
            transaction.commit().await.map_err(sql)?;
            Ok(rows)
        })?;
        rows.into_iter()
            .filter(|row| row.get::<_, bool>(1))
            .map(|row| {
                let key: String = row.get(0);
                let uid = InstanceUid::parse(&key)
                    .ok_or_else(|| format!("agents: {key} is not an instance_uid"))?;
                Ok((uid, unsigned(row.get(2))))
            })
            .collect()
    }

    /// One transaction: the row under its lock and the holder it names read, then the row written,
    /// its count of takes raised by one, and the take announced where it replaced another holder,
    /// whether or not that one's lease is live: an instance that serves under a new lease still
    /// hears of it. The changes of holder are kept as the database's times of the latest eleven,
    /// and those within a minute counted, so every instance counts on one clock.
    fn take(
        &self,
        uid: &InstanceUid,
        holder: &str,
        last_seen_ms: u64,
    ) -> Result<Option<Taken>, String> {
        let (key, holder, last_seen) = (uid.to_string(), holder.to_string(), signed(last_seen_ms));
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            let Some(row) = transaction
                .query_opt(
                    "SELECT a.holder::text, a.holder IS DISTINCT FROM $2::text::uuid, \
                         (SELECT count(*) FROM unnest(a.holder_changes) AS t \
                             WHERE t > now() - interval '1 minute') \
                     FROM agents AS a WHERE a.instance_uid = $1 FOR UPDATE OF a",
                    &[&key, &holder],
                )
                .await
                .map_err(sql)?
            else {
                return Ok(None);
            };
            let (stored, other, recent): (Option<String>, bool, i64) =
                (row.get(0), row.get(1), row.get(2));
            let from = stored.filter(|_| other);
            let changed = from.is_some();
            let take: i64 = transaction
                .query_one(
                    "UPDATE agents SET connected = true, holder = $2::text::uuid, \
                         last_seen_ms = GREATEST(last_seen_ms, $3), takes = takes + 1, \
                         holder_changes = CASE WHEN $4 THEN ARRAY(\
                             SELECT t FROM unnest(holder_changes) AS t ORDER BY t DESC LIMIT 10) \
                             || now() ELSE holder_changes END \
                     WHERE instance_uid = $1 RETURNING takes",
                    &[&key, &holder, &last_seen, &changed],
                )
                .await
                .map_err(sql)?
                .get(0);
            if changed {
                notify(&transaction, TAKEN, &key).await?;
            }
            transaction.commit().await.map_err(sql)?;
            Ok(Some(Taken {
                from,
                likely_duplicate: changed && recent == 10,
                take: unsigned(take),
            }))
        })
    }

    fn holders(
        &self,
        uids: &[InstanceUid],
    ) -> Result<HashMap<InstanceUid, Option<String>>, String> {
        let keys: Vec<String> = uids.iter().map(ToString::to_string).collect();
        let rows = self.database.run(move |pool| async move {
            let client = client(&pool).await?;
            client
                .query(
                    "SELECT instance_uid, holder::text FROM agents WHERE instance_uid = ANY($1)",
                    &[&keys],
                )
                .await
                .map_err(sql)
        })?;
        rows.into_iter()
            .map(|row| {
                let key: String = row.get(0);
                let uid = InstanceUid::parse(&key)
                    .ok_or_else(|| format!("agents: {key} is not an instance_uid"))?;
                Ok((uid, row.get(1)))
            })
            .collect()
    }

    fn takes(
        &self,
        uids: &[InstanceUid],
    ) -> Result<HashMap<InstanceUid, (Option<String>, u64)>, String> {
        let keys: Vec<String> = uids.iter().map(ToString::to_string).collect();
        let rows = self.database.run(move |pool| async move {
            let client = client(&pool).await?;
            client
                // Under a share lock, taken in the order the batches lock, so a take still in
                // flight is waited for and its count read.
                .query(
                    "SELECT instance_uid, holder::text, takes FROM agents \
                     WHERE instance_uid = ANY($1) \
                     ORDER BY instance_uid COLLATE \"C\" FOR SHARE",
                    &[&keys],
                )
                .await
                .map_err(sql)
        })?;
        rows.into_iter()
            .map(|row| {
                let key: String = row.get(0);
                let uid = InstanceUid::parse(&key)
                    .ok_or_else(|| format!("agents: {key} is not an instance_uid"))?;
                Ok((uid, (row.get(1), unsigned(row.get(2)))))
            })
            .collect()
    }

    /// One transaction: the row under its lock, its presence read as live only while its holder's
    /// lease is, and the row removed and announced when that lets it go.
    fn remove_if_silent(
        &self,
        uid: &InstanceUid,
        silent_before_ms: u64,
    ) -> Result<Option<Silenced>, String> {
        let key = uid.to_string();
        let silent_before = signed(silent_before_ms);
        let forgotten = self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            let Some(row) = transaction
                .query_opt(
                    "SELECT a.connected AND EXISTS (SELECT 1 FROM leases AS l \
                         WHERE l.instance_id = a.holder AND l.expires_at > now()), \
                     a.last_seen_ms FROM agents AS a WHERE a.instance_uid = $1 FOR UPDATE OF a",
                    &[&key],
                )
                .await
                .map_err(sql)?
            else {
                return Ok(None);
            };
            let (connected, last_seen): (bool, i64) = (row.get(0), row.get(1));
            if connected && unsigned(last_seen) >= unsigned(silent_before) {
                return Ok(Some(Silenced::Reporting));
            }
            // What the stored assignment names, under the row's lock: no release changes it
            // before the row goes.
            let configurations = transaction
                .query(
                    "SELECT configuration FROM agent_assignment_configurations \
                     WHERE instance_uid = $1 ORDER BY configuration",
                    &[&key],
                )
                .await
                .map_err(sql)?
                .iter()
                .map(|row| row.get(0))
                .collect();
            transaction
                .execute("DELETE FROM agents WHERE instance_uid = $1", &[&key])
                .await
                .map_err(sql)?;
            notify(&transaction, FORGOTTEN, &key).await?;
            transaction.commit().await.map_err(sql)?;
            Ok(Some(Silenced::Forgotten(configurations)))
        })?;
        if matches!(forgotten, Some(Silenced::Forgotten(_))) {
            self.stored([(uid, None)]);
        }
        Ok(forgotten)
    }

    /// The old row under a share lock first, which waits for a move still in flight and then
    /// finds it gone, and the new row read after it, by a statement that sees that move's commit.
    fn moved(&self, old: &InstanceUid, new: &InstanceUid) -> Result<bool, String> {
        let (old, new) = (old.to_string(), new.to_string());
        self.database.run(move |pool| async move {
            let client = client(&pool).await?;
            let old_stored = client
                .query_opt(
                    "SELECT 1 FROM agents WHERE instance_uid = $1 FOR SHARE",
                    &[&old],
                )
                .await
                .map_err(sql)?
                .is_some();
            let new_stored = client
                .query_opt("SELECT 1 FROM agents WHERE instance_uid = $1", &[&new])
                .await
                .map_err(sql)?
                .is_some();
            Ok(new_stored && !old_stored)
        })
    }

    /// One transaction: the row as it is stored moved to its new key, its assignment's
    /// Configurations with it, and nothing of `record` written, so a label change or a release
    /// another instance committed stays, the row locked before it is copied so one committed
    /// during the move does too; the old key announced forgotten, so an instance that
    /// loaded it drops it; a record no longer stored is moved nowhere.
    fn rekey(
        &self,
        old: &InstanceUid,
        new: &InstanceUid,
        record: &PersistedAgent,
    ) -> Result<(), String> {
        let _ = record;
        let (old, new) = (*old, *new);
        let moved = self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            let (old, new) = (old.to_string(), new.to_string());
            // The row locked first, so the copy is of its latest committed version and a write
            // another instance makes meanwhile waits, then finds no row, rather than being lost
            // between the copy and the delete.
            transaction
                .execute(
                    "SELECT 1 FROM agents WHERE instance_uid = $1 FOR UPDATE",
                    &[&old],
                )
                .await
                .map_err(sql)?;
            // The row copied whole, its key replaced, so no column is named and a column added
            // later moves with it.
            let moved = transaction
                .execute(
                    "INSERT INTO agents SELECT moved.* FROM agents AS a, \
                     jsonb_populate_record(a, jsonb_build_object('instance_uid', $2::text)) \
                         AS moved \
                     WHERE a.instance_uid = $1",
                    &[&old, &new],
                )
                .await
                .map_err(sql)?
                > 0;
            if moved {
                transaction
                    .execute(
                        "UPDATE agent_assignment_configurations SET instance_uid = $2 \
                         WHERE instance_uid = $1",
                        &[&old, &new],
                    )
                    .await
                    .map_err(sql)?;
                transaction
                    .execute("DELETE FROM agents WHERE instance_uid = $1", &[&old])
                    .await
                    .map_err(sql)?;
                notify(&transaction, FORGOTTEN, &old).await?;
            }
            transaction.commit().await.map_err(sql)?;
            Ok(moved)
        })?;
        if moved {
            let mut pins = self.assignments.lock().expect("assignments lock");
            pins.commits += 1;
            let commit = pins.commits;
            pins.committed(old, commit);
            pins.committed(new, commit);
            if let Some(assignment) = pins.assignments.remove(&old) {
                pins.assignments.insert(new, assignment);
            }
        }
        Ok(())
    }

    /// One transaction: the revisions and the assignments that pin them commit together or not at
    /// all (ADR-0023 clause 21). Each record's assignment is written alone, in the order of the
    /// `instance_uid`, so two releases over the same Agents lock their rows in one order; a record
    /// forgotten meanwhile is written nothing. The row of each Package an assignment pins is held
    /// `FOR KEY SHARE` until the commit, as each pinned Configuration's is, so a write to the
    /// Package that counts the assignments pinning it waits for these.
    fn release(
        &self,
        revisions: &[Pinned],
        agents: &[(InstanceUid, PersistedAgent)],
    ) -> Result<Vec<InstanceUid>, String> {
        let revisions = revisions.to_vec();
        let mut records: Vec<(String, Option<Assignment>)> = agents
            .iter()
            .map(|(uid, record)| (uid.to_string(), record.assignment.clone()))
            .collect();
        records.sort_by(|(a, _), (b, _)| a.cmp(b));
        let written = revisions.len() + records.len();
        let wrote: BTreeSet<String> =
            self.database.run_writing(written, move |pool| async move {
                let mut client = client(&pool).await?;
                let transaction = client.transaction().await.map_err(sql)?;
                super::configs::retain(&transaction, &revisions).await?;
                let packages: BTreeSet<&PackageId> = records
                    .iter()
                    .filter_map(|(_, assignment)| assignment.as_ref()?.package.as_ref())
                    .collect();
                for id in packages {
                    hold_pinned_package(&transaction, id).await?;
                }
                let deployments: BTreeSet<&str> = records
                    .iter()
                    .filter_map(|(_, assignment)| Some(assignment.as_ref()?.deployment.as_str()))
                    .collect();
                for name in deployments {
                    hold_released_through(&transaction, name).await?;
                }
                let pinned: BTreeSet<&str> = revisions
                    .iter()
                    .map(|pinned| pinned.configuration.as_str())
                    .collect();
                for configuration in pinned {
                    notify(&transaction, CONFIGURATION, configuration).await?;
                }
                let mut wrote = BTreeSet::new();
                for (key, assignment) in records {
                    if write_assignment(&transaction, &key, assignment.as_ref()).await? {
                        notify(&transaction, ASSIGNMENT, &key).await?;
                        wrote.insert(key);
                    }
                }
                transaction.commit().await.map_err(sql)?;
                Ok(wrote)
            })?;
        let wrote: Vec<&(InstanceUid, PersistedAgent)> = agents
            .iter()
            .filter(|(uid, _)| wrote.contains(&uid.to_string()))
            .collect();
        self.stored(
            wrote
                .iter()
                .map(|(uid, record)| (uid, record.assignment.as_ref())),
        );
        Ok(wrote.into_iter().map(|(uid, _)| *uid).collect())
    }

    fn pinning(&self, pin: Pin<'_>) -> usize {
        self.assignments
            .lock()
            .expect("assignments lock")
            .assignments
            .values()
            .filter(|assignment| pin.pinned_by(assignment))
            .count()
    }

    fn referenced_revisions(&self, name: &str) -> BTreeSet<String> {
        self.assignments
            .lock()
            .expect("assignments lock")
            .assignments
            .values()
            .filter_map(|assignment| assignment.configurations.get(name).cloned())
            .collect()
    }
}

/// Holds the row of the Package `id` `FOR KEY SHARE` inside a rollout's transaction: a write to
/// the Package, which holds it `FOR UPDATE` and then counts the assignments that pin it, waits for
/// the rollout's commit, while other rollouts that pin it go on beside it. A Package that is not
/// stored is an error, as a Configuration that is not is.
async fn hold_pinned_package(client: &impl GenericClient, id: &PackageId) -> Result<(), String> {
    client
        .query_opt(
            "SELECT 1 FROM packages WHERE agent_type = $1 AND version = $2 FOR KEY SHARE",
            &[&id.agent_type, &id.version],
        )
        .await
        .map_err(sql)?
        .map(drop)
        .ok_or_else(|| format!("no package set {id}"))
}

/// Holds the row of the Deployment `name` `FOR KEY SHARE` inside a rollout's transaction, as
/// [`hold_pinned_package`] holds a Package's: its deletion, which counts the assignments that name
/// it, waits for the rollout's commit. A Deployment that is not stored is an error.
async fn hold_released_through(client: &impl GenericClient, name: &str) -> Result<(), String> {
    client
        .query_opt(
            "SELECT 1 FROM deployments WHERE name = $1 FOR KEY SHARE",
            &[&name],
        )
        .await
        .map_err(sql)?
        .map(drop)
        .ok_or_else(|| format!("no deployment {name:?}"))
}

/// What a creation's transaction runs first: its commit does not wait for the database to flush
/// it, and no other transaction on the connection is affected (ADR-0023 clause 21).
const ASYNCHRONOUS_COMMIT: &str = "SET LOCAL synchronous_commit = off";

/// A count of the creations under way, bounded by half the pool, so the other half serves every
/// other call while creations wait for the count's row (ADR-0023 clause 24).
struct Creating {
    at_most: usize,
    under_way: Mutex<usize>,
    freed: std::sync::Condvar,
}

impl Creating {
    /// Half of a pool of `pool_size` connections, at least one.
    fn of(pool_size: usize) -> Self {
        Creating {
            at_most: (pool_size / 2).max(1),
            under_way: Mutex::new(0),
            freed: std::sync::Condvar::new(),
        }
    }

    /// Waits until fewer than its bound are under way, and counts this one until the answer is
    /// dropped.
    fn enter(&self) -> Entered<'_> {
        let mut under_way = self
            .under_way
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *under_way >= self.at_most {
            under_way = self
                .freed
                .wait(under_way)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        *under_way += 1;
        Entered(self)
    }
}

/// One creation under way.
struct Entered<'a>(&'a Creating);

impl Drop for Entered<'_> {
    fn drop(&mut self) {
        *self
            .0
            .under_way
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) -= 1;
        self.0.freed.notify_one();
    }
}

/// The columns of a row, in the order [`read_all`] selects and [`write`] inserts them.
const COLUMNS: &str = "instance_uid, sequence_num, capabilities, transport, last_seen_ms, \
     restart_pending, effective_config, description, health, remote_config_status, \
     connection_settings_status, package_statuses, available_components, labels, host, \
     deployment, package_agent_type, package_version";

/// The assignment's Configurations of the `agents` row a query selects, by name to revision, in
/// the statement that reads the row: one statement reads one committed state whatever the
/// transaction's isolation, so no rollout that commits meanwhile is read in part.
const CONFIGURATIONS: &str = "(SELECT json_object_agg(c.configuration, c.revision) \
     FROM agent_assignment_configurations AS c WHERE c.instance_uid = agents.instance_uid)";

/// A wire-typed field as its protobuf bytes, the encoding whose compatibility rules the Baseline
/// already governs (ADR-0023 clause 4).
fn encode<M: Message>(message: Option<&M>) -> Option<Vec<u8>> {
    message.map(Message::encode_to_vec)
}

pub(super) fn decode<M: Message + Default>(
    bytes: Option<Vec<u8>>,
    what: &str,
) -> Result<Option<M>, String> {
    bytes
        .map(|bytes| {
            M::decode(bytes.as_slice()).map_err(|e| format!("{what} does not decode: {e}"))
        })
        .transpose()
}

/// Every Agent record. A row that does not decode fails, naming its `instance_uid`
/// (ADR-0023 clause 4).
pub(crate) async fn read_all(
    client: &impl GenericClient,
) -> Result<HashMap<InstanceUid, PersistedAgent>, String> {
    read(client, None).await
}

/// The records of the keys `only` names, or of every Agent where it names none, in one statement.
pub(super) async fn read(
    client: &impl GenericClient,
    only: Option<&[String]>,
) -> Result<HashMap<InstanceUid, PersistedAgent>, String> {
    let (filter, params): (&str, Vec<&(dyn tokio_postgres::types::ToSql + Sync)>) = match &only {
        Some(uids) => (" WHERE instance_uid = ANY($1)", vec![uids]),
        None => ("", Vec::new()),
    };
    let mut records = HashMap::new();
    for row in client
        .query(
            &format!("SELECT {COLUMNS}, {CONFIGURATIONS} FROM agents{filter}"),
            &params,
        )
        .await
        .map_err(sql)?
    {
        let key: String = row.get(0);
        let named = |e: String| format!("agent {key} in the database: {e}");
        let uid =
            InstanceUid::parse(&key).ok_or_else(|| named("not an Instance UID".to_string()))?;
        let labels: BTreeMap<String, String> =
            serde_json::from_value(row.get(13)).map_err(|e| named(format!("labels: {e}")))?;
        let assignment = assignment_of(
            row.get(15),
            row.get(16),
            row.get(17),
            configurations_of(row.get(18)).map_err(named)?,
        )
        .map_err(named)?;
        let record = PersistedAgent {
            sequence_num: unsigned(row.get(1)),
            capabilities: unsigned(row.get(2)),
            transport: Transport::parse(row.get(3)),
            last_seen_ms: unsigned(row.get(4)),
            restart_pending: row.get(5),
            effective_config: row
                .get::<_, Option<Vec<u8>>>(6)
                .map(String::from_utf8)
                .transpose()
                .map_err(|e| named(format!("effective_config: {e}")))?,
            description: decode(row.get(7), "description").map_err(named)?,
            health: decode(row.get(8), "health").map_err(named)?,
            remote_config_status: decode(row.get(9), "remote_config_status").map_err(named)?,
            connection_settings_status: decode(row.get(10), "connection_settings_status")
                .map_err(named)?,
            package_statuses: decode(row.get(11), "package_statuses").map_err(named)?,
            available_components: decode(row.get(12), "available_components").map_err(named)?,
            assignment,
            labels,
            host: row.get(14),
        };
        records.insert(uid, record);
    }
    Ok(records)
}

/// The assignment's Configurations of a row, as [`CONFIGURATIONS`] selects them.
fn configurations_of(
    configurations: Option<serde_json::Value>,
) -> Result<BTreeMap<String, String>, String> {
    configurations
        .map(serde_json::from_value)
        .transpose()
        .map(Option::unwrap_or_default)
        .map_err(|e| format!("the assignment's Configurations: {e}"))
}

/// The assignment the assignment's columns of one row and its Configurations make, held to the
/// rules its write met ([`Assignment::check`]).
fn assignment_of(
    deployment: Option<String>,
    agent_type: Option<String>,
    version: Option<String>,
    configurations: BTreeMap<String, String>,
) -> Result<Option<Assignment>, String> {
    let package = match (agent_type, version) {
        (Some(agent_type), Some(version)) => Some(
            PackageId::load(&agent_type, &version)
                .map_err(|e| format!("invalid package in the assignment: {e}"))?,
        ),
        (None, None) => None,
        _ => return Err("half a package in the assignment".to_string()),
    };
    match deployment {
        Some(deployment) => {
            let assignment = Assignment {
                deployment,
                package,
                configurations,
            };
            assignment.check()?;
            Ok(Some(assignment))
        }
        None if package.is_none() && configurations.is_empty() => Ok(None),
        None => Err("an assignment without its Deployment".to_string()),
    }
}

/// Every stored assignment, read alone: the assignment's columns of each row and their
/// Configurations, no record.
async fn read_assignments(
    client: &impl GenericClient,
) -> Result<HashMap<InstanceUid, Assignment>, String> {
    let mut assignments = HashMap::new();
    for row in client
        .query(
            &format!(
                "SELECT instance_uid, deployment, package_agent_type, package_version, \
                 {CONFIGURATIONS} FROM agents"
            ),
            &[],
        )
        .await
        .map_err(sql)?
    {
        let key: String = row.get(0);
        let named = |e: String| format!("agent {key} in the database: {e}");
        let uid =
            InstanceUid::parse(&key).ok_or_else(|| named("not an Instance UID".to_string()))?;
        let assignment = assignment_of(
            row.get(1),
            row.get(2),
            row.get(3),
            configurations_of(row.get(4)).map_err(named)?,
        )
        .map_err(named)?;
        if let Some(assignment) = assignment {
            assignments.insert(uid, assignment);
        }
    }
    Ok(assignments)
}

/// The columns a report decides (ADR-0030 clause 2): every
/// column but the key, the labels, `restart_pending` and the assignment's.
const REPORTED: [&str; 12] = [
    "sequence_num",
    "capabilities",
    "transport",
    "last_seen_ms",
    "effective_config",
    "description",
    "health",
    "remote_config_status",
    "connection_settings_status",
    "package_statuses",
    "available_components",
    "host",
];

/// The instance a shutdown flush writes for, and the count of takes it was last answered.
struct Held<'a> {
    holder: Option<&'a str>,
    take: Option<i64>,
}

/// Writes the columns a report decides of one Agent record, inside the caller's transaction,
/// `last_seen_ms` only forward. Where `unless_held_elsewhere` is given, only of a row whose holder
/// is the one it names, or of one no instance holds whose count of takes is the one it names, and
/// whose sequence number is not higher than the record's. Answers whether it wrote a record.
async fn write_reported(
    client: &impl GenericClient,
    uid: &InstanceUid,
    record: &PersistedAgent,
    unless_held_elsewhere: Option<Held<'_>>,
) -> Result<bool, String> {
    let key = uid.to_string();
    // `last_seen_ms` never moves back, as a presence batch writes it.
    let sets = REPORTED
        .iter()
        .enumerate()
        .map(|(i, column)| match *column {
            "last_seen_ms" => format!("{column} = GREATEST({column}, ${})", i + 2),
            _ => format!("{column} = ${}", i + 2),
        })
        .collect::<Vec<_>>()
        .join(", ");
    // A row no instance holds is written only where no instance took the Agent since this one
    // was last answered its count of takes: another instance that carried the Agent since clears
    // the holder at its disconnect, and what it wrote stays. A row whose sequence number is higher
    // holds a newer report another instance wrote while this one still held the row, and stays
    // too; an Agent that started again and counts from the start misses this one flush, and the
    // instance that carries it, with no count answered, writes its record again at its next
    // report. No clock is compared.
    let held = match unless_held_elsewhere {
        Some(_) => format!(
            " AND (holder = ${}::text::uuid OR (holder IS NULL AND takes = ${})) \
             AND sequence_num <= $2",
            REPORTED.len() + 2,
            REPORTED.len() + 3
        ),
        None => String::new(),
    };
    // The derived columns and their digest in the same statement, the digest over the values bound.
    let first = REPORTED.len()
        + 2
        + if unless_held_elsewhere.is_some() {
            2
        } else {
            0
        };
    let derived_sets = derived_sets(first, |column| {
        Some(
            match column {
                "instance_uid" => "$1::text",
                "capabilities" => "$3::bigint",
                "transport" => "$4::text",
                "description" => "$7::bytea",
                "health" => "$8::bytea",
                "remote_config_status" => "$9::bytea",
                "package_statuses" => "$11::bytea",
                _ => return None,
            }
            .to_string(),
        )
    });
    let update = format!("UPDATE agents SET {sets}, {derived_sets} WHERE instance_uid = $1{held}");
    let (sequence_num, capabilities, last_seen_ms) = (
        signed(record.sequence_num),
        signed(record.capabilities),
        signed(record.last_seen_ms),
    );
    let (description, health, remote_config_status) = (
        encode(record.description.as_ref()),
        encode(record.health.as_ref()),
        encode(record.remote_config_status.as_ref()),
    );
    let (connection_settings_status, package_statuses, available_components) = (
        encode(record.connection_settings_status.as_ref()),
        encode(record.package_statuses.as_ref()),
        encode(record.available_components.as_ref()),
    );
    let effective_config = record.effective_config.as_deref().map(str::as_bytes);
    let transport = record.transport.as_str();
    let (holder, take) = unless_held_elsewhere
        .as_ref()
        .map_or((None, None), |held| (held.holder, held.take));
    let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = vec![
        &key,
        &sequence_num,
        &capabilities,
        &transport,
        &last_seen_ms,
        &effective_config,
        &description,
        &health,
        &remote_config_status,
        &connection_settings_status,
        &package_statuses,
        &available_components,
        &record.host,
    ];
    if unless_held_elsewhere.is_some() {
        params.push(&holder);
        params.push(&take);
    }
    let derived = derived_of(uid, record);
    params.extend(
        derived
            .iter()
            .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync)),
    );
    Ok(client.execute(&update, &params).await.map_err(sql)? > 0)
}

/// The values of the columns the list of Agents reads, derived from `record`, which every write
/// of it writes in the same statement
/// (ADR-0031
/// clause 8).
fn derived_of(
    uid: &InstanceUid,
    record: &PersistedAgent,
) -> Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> {
    super::agent_page::derived_params(&crate::listing::derive(&crate::agent_store::source_of(
        uid, record,
    )))
}

/// `SET` of the derived columns, their values bound from `$first` on, and of their digest over
/// the values the statement binds for the columns it is the digest of, `column` naming each.
fn derived_sets(first: usize, column: impl Fn(&str) -> Option<String>) -> String {
    let mut sets: Vec<String> = super::agent_page::DERIVED_COLUMNS
        .iter()
        .enumerate()
        .map(|(i, name)| format!("{name} = ${}", first + i))
        .collect();
    sets.push(format!(
        "derived_from = {}",
        super::agent_page::stamp_of(column)
    ));
    sets.join(", ")
}

/// Writes the assignment of one Agent record alone, its Configurations exactly the assignment's,
/// inside the caller's transaction. Answers whether there was a record.
async fn write_assignment(
    client: &impl GenericClient,
    key: &str,
    assignment: Option<&Assignment>,
) -> Result<bool, String> {
    let package = assignment.and_then(|assignment| assignment.package.as_ref());
    let updated = client
        .execute(
            "UPDATE agents SET deployment = $2, package_agent_type = $3, package_version = $4 \
             WHERE instance_uid = $1",
            &[
                &key,
                &assignment.map(|assignment| assignment.deployment.as_str()),
                &package.map(|id| id.agent_type.as_str()),
                &package.map(|id| id.version.as_str()),
            ],
        )
        .await
        .map_err(sql)?;
    if updated == 0 {
        return Ok(false);
    }
    write_configurations(client, key, assignment).await?;
    Ok(true)
}

/// Replaces the assignment's Configurations of one Agent record with `assignment`'s.
async fn write_configurations(
    client: &impl GenericClient,
    key: &str,
    assignment: Option<&Assignment>,
) -> Result<(), String> {
    client
        .execute(
            "DELETE FROM agent_assignment_configurations WHERE instance_uid = $1",
            &[&key],
        )
        .await
        .map_err(sql)?;
    for (name, revision) in assignment
        .map(|assignment| &assignment.configurations)
        .into_iter()
        .flatten()
    {
        client
            .execute(
                "INSERT INTO agent_assignment_configurations (instance_uid, configuration, \
                 revision) VALUES ($1, $2, $3)",
                &[&key, name, revision],
            )
            .await
            .map_err(sql)?;
    }
    Ok(())
}

/// Creates or replaces one Agent record whole, its assignment's Configurations exactly the
/// record's, inside the caller's transaction: what an import writes.
pub(crate) async fn write(
    client: &impl GenericClient,
    uid: &InstanceUid,
    record: &PersistedAgent,
) -> Result<(), String> {
    let updates = COLUMNS
        .split(", ")
        .skip(1)
        .chain(super::agent_page::DERIVED_COLUMNS)
        .chain(["derived_from"])
        .map(|column| format!("{column} = EXCLUDED.{column}"))
        .collect::<Vec<_>>()
        .join(", ");
    insert(client, uid, record, &format!("DO UPDATE SET {updates}")).await?;
    Ok(())
}

/// Inserts one Agent record whole, `on_conflict` saying what a row of its key already there does,
/// and its assignment's Configurations where the row was written. Answers whether it was.
async fn insert(
    client: &impl GenericClient,
    uid: &InstanceUid,
    record: &PersistedAgent,
    on_conflict: &str,
) -> Result<bool, String> {
    insert_as(client, uid, record, on_conflict, None).await
}

/// The types of the columns [`insert`] writes, in the order of its parameters: what a row
/// selected rather than given as values names, so each parameter takes its column's type.
const INSERTED_TYPES: [&str; 28] = [
    "text", "bigint", "bigint", "text", "bigint", "boolean", "bytea", "bytea", "bytea", "bytea",
    "bytea", "bytea", "bytea", "json", "text", "text", "text", "text", "text", "bigint", "text",
    "text", "text", "text", "json", "text", "boolean", "text",
];

/// [`insert`], or with `within` the creation of a record only while the count of the records,
/// kept by the triggers of migration 14, is below `within`: one statement that locks the count's
/// row, so creations of every instance are counted one after another and none waits for a round
/// trip of another (ADR-0023 clause 22).
async fn insert_as(
    client: &impl GenericClient,
    uid: &InstanceUid,
    record: &PersistedAgent,
    on_conflict: &str,
    within: Option<i64>,
) -> Result<bool, String> {
    let key = uid.to_string();
    let labels = serde_json::to_value(&record.labels).expect("labels serialize");
    let assignment = record.assignment.as_ref();
    let package = assignment.and_then(|assignment| assignment.package.as_ref());
    let derived = derived_of(uid, record);
    let derived_names = super::agent_page::DERIVED_COLUMNS.join(", ");
    let derived_values = (19..19 + derived.len())
        .map(|n| format!("${n}"))
        .collect::<Vec<_>>()
        .join(", ");
    let stamp = super::agent_page::stamp_of(|column| {
        Some(
            match column {
                "instance_uid" => "$1::text",
                "capabilities" => "$3::bigint",
                "transport" => "$4::text",
                "description" => "$8::bytea",
                "health" => "$9::bytea",
                "remote_config_status" => "$10::bytea",
                "package_statuses" => "$12::bytea",
                _ => return None,
            }
            .to_string(),
        )
    });
    let mut values: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = vec![
        Box::new(key.clone()),
        Box::new(signed(record.sequence_num)),
        Box::new(signed(record.capabilities)),
        Box::new(record.transport.as_str()),
        Box::new(signed(record.last_seen_ms)),
        Box::new(record.restart_pending),
        Box::new(
            record
                .effective_config
                .as_ref()
                .map(|text| text.as_bytes().to_vec()),
        ),
        Box::new(encode(record.description.as_ref())),
        Box::new(encode(record.health.as_ref())),
        Box::new(encode(record.remote_config_status.as_ref())),
        Box::new(encode(record.connection_settings_status.as_ref())),
        Box::new(encode(record.package_statuses.as_ref())),
        Box::new(encode(record.available_components.as_ref())),
        Box::new(labels),
        Box::new(record.host.clone()),
        Box::new(assignment.map(|assignment| assignment.deployment.clone())),
        Box::new(package.map(|id| id.agent_type.clone())),
        Box::new(package.map(|id| id.version.clone())),
    ];
    values.extend(derived);
    let rows = match within {
        None => format!(
            "VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, \
             $18, {derived_values}, {stamp})"
        ),
        Some(within) => {
            let typed: Vec<String> = INSERTED_TYPES
                .iter()
                .enumerate()
                .map(|(i, kind)| format!("${}::{kind}", i + 1))
                .collect();
            let bound = typed.len() + 1;
            values.push(Box::new(within));
            format!(
                "SELECT {}, {stamp} FROM agent_count WHERE agents < ${bound} FOR UPDATE",
                typed.join(", ")
            )
        }
    };
    let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = values
        .iter()
        .map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync))
        .collect();
    let written = client
        .execute(
            &format!(
                "INSERT INTO agents ({COLUMNS}, {derived_names}, derived_from) {rows} \
                 ON CONFLICT (instance_uid) {on_conflict}"
            ),
            &params,
        )
        .await
        .map_err(sql)?
        > 0;
    // A record created anew has no Configurations to replace: none is written for one whose
    // assignment names none.
    let configurations = assignment.is_some_and(|assignment| !assignment.configurations.is_empty());
    if written && (within.is_none() || configurations) {
        write_configurations(client, &key, assignment).await?;
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_store::tests::record;
    use crate::agent_store::Presence;
    use crate::configs::{revision_hash, Configuration, Revision};
    use crate::fs::{FsAgentStore, FsConfigBackend};
    use crate::postgres::testing::TestDatabase;

    /// The two stores a rollout writes into, opened on one backing store.
    type Open<'a> = &'a dyn Fn() -> (Box<dyn AgentStore>, Box<dyn crate::configs::ConfigBackend>);

    fn revision(body: &str) -> Revision {
        Revision {
            body: body.to_string(),
            role: String::new(),
            service_name: "otelcol".to_string(),
        }
    }

    fn pinned_of(revision: &Revision) -> Pinned {
        Pinned {
            configuration: "base".to_string(),
            hash: revision_hash(revision),
            revision: revision.clone(),
        }
    }

    /// How the contract reads every record of a store: through `load` where the Server holds the
    /// fleet in memory, through `query` where it reads it from the database.
    type ReadAll<'a> = &'a dyn Fn(&dyn AgentStore) -> HashMap<InstanceUid, PersistedAgent>;

    /// The port's contract on either store: what is written is loaded back whole, wire types and
    /// all; a record without an assignment stays without one; a re-key moves the record; the
    /// integrity queries follow what is stored; and a rollout's revisions and assignments are
    /// both present.
    fn contract(open: Open<'_>, all: ReadAll<'_>) {
        let (store, configs) = open();
        assert!(all(&*store).is_empty());
        let (one, two, three) = (
            InstanceUid::default(),
            InstanceUid::default(),
            InstanceUid::default(),
        );
        let mut full = record();
        // An Agent reports what it likes: a NUL is kept as it is, and fails no write.
        full.effective_config = Some("receivers: {}\n# \0 a NUL an Agent sent\n".to_string());
        let mut bare = record();
        bare.assignment = None;
        bare.labels.clear();
        bare.host = None;
        bare.description = None;
        bare.effective_config = None;
        bare.sequence_num = u64::MAX;
        store.put(&one, &full).expect("put");
        store.put(&two, &bare).expect("put");
        let loaded = all(&*open().0);
        assert!(loaded[&one] == full, "the record round-trips whole");
        assert!(loaded[&two] == bare, "absent stays absent, u64::MAX stays");

        store.rekey(&two, &three, &bare).expect("rekey");
        let loaded = all(&*open().0);
        assert!(loaded.contains_key(&three) && !loaded.contains_key(&two));

        let pinned = revision("v1\n");
        let base = Configuration {
            name: "base".to_string(),
            saved: pinned.clone(),
            retained: BTreeMap::from([(revision_hash(&pinned), pinned.clone())]),
        };
        let mut released = bare.clone();
        released.assignment = Some(Assignment {
            deployment: "stable".to_string(),
            package: None,
            configurations: BTreeMap::from([("base".to_string(), revision_hash(&pinned))]),
        });
        configs.save("base", &pinned).expect("save");
        store
            .release(&[pinned_of(&pinned)], &[(three, released.clone())])
            .expect("release");
        let (reopened, configs) = open();
        let loaded = all(&*reopened);
        assert!(loaded[&three] == released);
        assert_eq!(configs.load().expect("revisions"), [base], "both present");
        assert_eq!(reopened.pinning(Pin::Configuration("base")), 2);
        assert_eq!(reopened.pinning(Pin::Deployment("stable")), 2);
        assert_eq!(
            reopened.referenced_revisions("base"),
            BTreeSet::from(["0123abcd".repeat(8), revision_hash(&pinned)])
        );

        // The acts that decide the labels and a queued restart write them, and one Agent is read
        // as it is stored.
        let labels = BTreeMap::from([("rollout".to_string(), "canary".to_string())]);
        assert!(store.set_labels(&three, &labels).expect("labels"));
        assert_eq!(store.set_restart_pending(&three, false), Ok(Some(true)));
        assert_eq!(store.set_restart_pending(&three, false), Ok(Some(false)));
        assert_eq!(store.set_restart_pending(&three, true), Ok(Some(false)));
        // A restart taken for a send that failed is put back, and the next take finds it queued
        // (ADR-0045
        // clause 8).
        assert_eq!(store.set_restart_pending(&three, false), Ok(Some(true)));
        assert_eq!(store.set_restart_pending(&three, true), Ok(Some(false)));
        assert_eq!(
            open().0.get(&three).map(|r| r.map(|r| r.restart_pending)),
            Ok(Some(true))
        );
        let got = open().0.get(&three).expect("get").expect("stored");
        assert_eq!(got.labels, labels);
        assert!(got.restart_pending);
        assert_eq!(got.assignment, released.assignment);
        let absent = InstanceUid::default();
        assert_eq!(open().0.get(&absent), Ok(None));
        assert_eq!(store.set_labels(&absent, &labels), Ok(false));
        assert_eq!(store.set_restart_pending(&absent, true), Ok(None));
        assert!(
            open().0.get(&absent).expect("get").is_none(),
            "nothing created"
        );

        store.remove(&one).expect("remove");
        store.remove(&one).expect("removing the absent is fine");
        assert_eq!(store.pinning(Pin::Configuration("base")), 1);
        assert_eq!(all(&*open().0).len(), 1);

        // The read the Server makes of the Agents behind a Gateway: those that arrived over its
        // Host, and no other.
        let behind = [InstanceUid([0x0b; 16]), InstanceUid([0x0c; 16])];
        let mut gatewayed = record();
        gatewayed.host = Some("gateway".to_string());
        gatewayed
            .description
            .get_or_insert_with(Default::default)
            .non_identifying_attributes
            .extend([
                opamp::attributes::string_attr("zone", "b"),
                // Taken, and no value a Selector matches: no pair to suggest.
                opamp::proto::KeyValue {
                    key: "port".to_string(),
                    value: Some(opamp::proto::AnyValue {
                        value: Some(opamp::proto::any_value::Value::IntValue(8)),
                    }),
                },
            ]);
        for uid in &behind {
            store.put(uid, &gatewayed).expect("put");
        }
        let filter = crate::listing::AgentFilter {
            arrived_over: Some("gateway".to_string()),
            ..crate::listing::AgentFilter::default()
        };
        let page = open()
            .0
            .query_page(&crate::agent_store::PageQuery {
                filter: &filter,
                sort: crate::listing::AgentSort::Name,
                descending: false,
                after: None,
                limit: 10,
                seen_before_ms: 0,
            })
            .expect("page");
        let found: Vec<InstanceUid> = page.agents.iter().map(|agent| agent.uid).collect();
        assert_eq!(
            found, behind,
            "the Agents that arrived over the Host, by name and UID"
        );
        assert_eq!(page.total, 2);
        assert!(page
            .agents
            .iter()
            .all(|agent| agent.stored.record == gatewayed));

        // What the UI suggests: the distinct reported pairs and types, each sorted, bounded.
        let derived: Vec<crate::listing::Derived> = all(&*open().0)
            .iter()
            .map(|(uid, record)| {
                crate::listing::derive(&crate::agent_store::source_of(uid, record))
            })
            .collect();
        let expected = crate::agent_store::ReportedValues::of(&derived, 1000);
        assert!(expected.pairs.len() > 1 && !expected.service_names.is_empty());
        assert_eq!(open().0.reported_values(1000).expect("values"), expected);
        let one = open().0.reported_values(1).expect("values");
        assert_eq!(
            (one.pairs.as_slice(), one.service_names.as_slice()),
            (&expected.pairs[..1], &expected.service_names[..1])
        );
    }

    /// An instance that does not hold an Agent another instance re-released counts its old pins
    /// until it reads the whole fleet again, and its forget of that Agent answers the
    /// Configurations the stored assignment named as it removed the record, not what it read.
    /// Verifies: ADR-0045, ADR-0033
    #[test]
    fn a_forget_answers_what_the_stored_assignment_named() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        test.store_deployments(&["stable", "canary"]);
        let (a, b) = (
            PgAgentStore::new(database.clone()),
            PgAgentStore::new(database),
        );
        let uid = InstanceUid::default();
        a.put(&uid, &record()).expect("created");
        b.load().expect("B reads the fleet");
        assert_eq!(b.pinning(Pin::Configuration("base")), 1);

        let mut re_released = record();
        re_released.assignment = Some(Assignment {
            deployment: "canary".to_string(),
            package: None,
            configurations: BTreeMap::from([("other".to_string(), "4567cdef".repeat(8))]),
        });
        a.release(&[], &[(uid, re_released)])
            .expect("re-released through A");
        assert_eq!(
            b.pinning(Pin::Configuration("base")),
            1,
            "B still counts what it read"
        );

        assert_eq!(
            b.remove_if_silent(&uid, u64::MAX / 2),
            Ok(Some(Silenced::Forgotten(vec!["other".to_string()]))),
            "what the stored assignment named, not what B read"
        );
    }

    /// A report's write on PostgreSQL leaves what the acts wrote through another instance: the
    /// labels, a queued restart and the assignment stay as they are stored, while what the report
    /// decides is written; a record that is not there is not written.
    /// Verifies: ADR-0030, ADR-0045
    #[test]
    fn a_report_written_on_postgres_keeps_what_the_acts_wrote() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b) = (
            PgAgentStore::new(database.clone()),
            PgAgentStore::new(database),
        );
        let uid = InstanceUid::default();
        let held = record();
        a.put(&uid, &held).expect("created whole");
        assert!(
            a.get(&uid).expect("get") == Some(held.clone()),
            "written whole"
        );

        let labels = BTreeMap::from([("tier".to_string(), "edge".to_string())]);
        assert!(b.set_labels(&uid, &labels).expect("labels"));
        assert_eq!(b.set_restart_pending(&uid, false), Ok(Some(true)));
        assert_eq!(b.set_restart_pending(&uid, true), Ok(Some(false)));
        b.release(
            &[],
            &[(
                uid,
                PersistedAgent {
                    assignment: None,
                    ..held.clone()
                },
            )],
        )
        .expect("release");

        // A still holds the record as it was, and writes a report from it.
        let mut report = held.clone();
        report.restart_pending = false;
        report.health = None;
        report.last_seen_ms += 30_000;
        assert_eq!(a.write_report(&uid, &report, None), Ok(true));
        let stored = a.get(&uid).expect("get").expect("stored");
        assert_eq!(stored.labels, labels, "the labels stay as B set them");
        assert!(stored.restart_pending, "the restart stays queued");
        assert_eq!(stored.assignment, None, "the release stays");
        assert_eq!(stored.health, None, "what the report decides is written");
        assert_eq!(stored.last_seen_ms, report.last_seen_ms);

        // A record forgotten meanwhile is not written again.
        b.remove(&uid).expect("forget");
        assert_eq!(a.write_report(&uid, &report, None), Ok(false));
        assert_eq!(a.get(&uid), Ok(None), "nothing created");
    }

    /// A label change through B that holds the row while A re-keys it is moved with the row: the
    /// re-key waits for it, and copies the row as B committed it, so B's labels stay under the new
    /// key. A trigger holds B's write open, mid-transaction, until the test lets it go, once A
    /// waits for B.
    /// Verifies: ADR-0030
    #[test]
    fn a_label_change_committed_during_a_rekey_moves_with_the_row() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b) = (
            PgAgentStore::new(database.clone()),
            PgAgentStore::new(database),
        );
        let (old, new) = (InstanceUid::default(), InstanceUid::default());
        a.put(&old, &record()).expect("put");
        let key = i64::from(u32::from_be_bytes([new.0[12], new.0[13], new.0[14], new.0[15]]) >> 1);
        test.sql(&format!(
            "CREATE FUNCTION fleet_test_gate() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
                 PERFORM pg_advisory_lock_shared({key}); \
                 PERFORM pg_advisory_unlock_shared({key}); RETURN NULL; END $$; \
             CREATE TRIGGER fleet_test_gate AFTER UPDATE OF labels ON agents \
                 FOR EACH ROW EXECUTE FUNCTION fleet_test_gate();"
        ));
        let gate = test.gate(key);
        let one = |query: &str| test.rows(query).first().and_then(|row| row[0].clone());
        let until = |query: &str| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                if let Some(found) = one(query) {
                    break found;
                }
                assert!(std::time::Instant::now() < deadline, "never: {query}");
            }
        };

        let labelled = std::thread::spawn(move || {
            b.set_labels(
                &old,
                &BTreeMap::from([("tier".to_string(), "core".to_string())]),
            )
        });
        // B has updated the row, and waits at the gate with the row locked.
        let b_pid = until(&format!(
            "SELECT pid::text FROM pg_locks WHERE locktype = 'advisory' AND NOT granted \
             AND objid::text = '{key}'"
        ));
        let rekeyed = std::thread::spawn(move || a.rekey(&old, &new, &record()));
        until(&format!(
            "SELECT pid::text FROM pg_stat_activity WHERE {b_pid} = ANY(pg_blocking_pids(pid))"
        ));
        gate.open();
        assert_eq!(labelled.join().expect("B"), Ok(true));
        assert_eq!(rekeyed.join().expect("A"), Ok(()));
        assert_eq!(
            test.rows("SELECT instance_uid, labels::text FROM agents"),
            [vec![
                Some(new.to_string()),
                Some(r#"{"tier":"core"}"#.to_string())
            ]],
            "B's labels moved with the row"
        );
    }

    /// A re-key on PostgreSQL moves every column of the row, one the code names nowhere included.
    /// Verifies: ADR-0030
    #[test]
    fn a_rekey_on_postgres_moves_every_column() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let store = PgAgentStore::new(test.open());
        let (old, new) = (InstanceUid::default(), InstanceUid::default());
        store.put(&old, &record()).expect("put");
        test.sql(&format!(
            "ALTER TABLE agents ADD COLUMN later text; \
             UPDATE agents SET later = 'kept', connected = true WHERE instance_uid = '{old}'"
        ));
        store.rekey(&old, &new, &record()).expect("rekey");
        assert_eq!(
            test.rows("SELECT instance_uid, later, connected::text FROM agents"),
            [vec![
                Some(new.to_string()),
                Some("kept".to_string()),
                Some("true".to_string())
            ]]
        );
    }

    /// A release on PostgreSQL writes the assignment of each record alone: the labels, the queued
    /// restart and what a report decided stay as they are stored, whatever the released record
    /// carries of them.
    /// Verifies: ADR-0030
    #[test]
    fn a_release_on_postgres_writes_the_assignment_alone() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let store = PgAgentStore::new(test.open());
        // A rollout releases through a stored Deployment.
        test.store_deployments(&["canary"]);
        let uid = InstanceUid::default();
        let stored = record();
        store.put(&uid, &stored).expect("put");
        let assignment = Assignment {
            deployment: "canary".to_string(),
            package: None,
            configurations: BTreeMap::from([("edge".to_string(), "4567cdef".repeat(8))]),
        };
        let stale = PersistedAgent {
            sequence_num: 1,
            health: None,
            effective_config: Some("stale: true".to_string()),
            host: None,
            restart_pending: false,
            labels: BTreeMap::new(),
            assignment: Some(assignment.clone()),
            ..record()
        };
        assert_eq!(store.release(&[], &[(uid, stale)]), Ok(vec![uid]));
        let released = store.get(&uid).expect("get");
        assert!(
            released
                == Some(PersistedAgent {
                    assignment: Some(assignment),
                    ..stored
                }),
            "{released:?}"
        );
    }

    /// Verifies: ADR-0023, ADR-0031
    #[test]
    fn the_agent_port_contract_holds_on_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        contract(
            &|| {
                (
                    Box::new(FsAgentStore::open(dir.path()).expect("open")),
                    Box::new(FsConfigBackend::open(dir.path()).expect("open")),
                )
            },
            &|store| store.load().expect("load"),
        );
    }

    /// Verifies: ADR-0023, ADR-0031
    #[test]
    fn the_agent_port_contract_holds_on_postgres() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        // A rollout releases through a stored Deployment.
        test.store_deployments(&["stable", "canary"]);
        contract(
            &|| {
                (
                    Box::new(PgAgentStore::open(database.clone()).expect("open")),
                    Box::new(super::super::configs::PgConfigBackend::new(
                        database.clone(),
                    )),
                )
            },
            &|store| {
                let all = store.query(None).expect("query");
                assert!(
                    all.windows(2).all(|pair| pair[0].0 .0 < pair[1].0 .0),
                    "in the order of the instance_uid"
                );
                all.into_iter()
                    .map(|(uid, stored)| (uid, stored.record))
                    .collect()
            },
        );
    }

    /// A stored assignment is loaded by the rules its write met, on either store. `plant` stores
    /// the assignment of `uid` as each case names it — its Deployment, its one Configuration and
    /// that one's revision — and every read in `reads` is refused in the words of
    /// [`Assignment::check`], naming the record. Whether a revision is still retained is not
    /// asked, so the well-formed hash `put` pins is retained by no Configuration.
    fn assignments_are_held_to_the_write_rules(
        store: &dyn AgentStore,
        plant: &dyn Fn(&InstanceUid, &str, &str, &str),
        reads: &dyn Fn() -> Vec<Result<(), String>>,
    ) {
        let hash = "0".repeat(64);
        let cases = [
            ("Stable", "base", hash.as_str()),
            ("stable", "Base", hash.as_str()),
            ("stable", "base", "0123abcd"),
        ];
        for (deployment, configuration, revision) in cases {
            let uid = InstanceUid::default();
            let mut held = record();
            held.assignment = Some(Assignment {
                deployment: "stable".to_string(),
                package: None,
                configurations: BTreeMap::from([("base".to_string(), hash.clone())]),
            });
            store.put(&uid, &held).expect("put");
            assert!(reads().iter().all(Result::is_ok), "a well-formed one loads");
            plant(&uid, deployment, configuration, revision);
            let core = Assignment {
                deployment: deployment.to_string(),
                package: None,
                configurations: BTreeMap::from([(configuration.to_string(), revision.to_string())]),
            }
            .check()
            .expect_err("the core refuses it");
            for read in reads() {
                let err = read.expect_err("refused");
                assert!(err.ends_with(&core), "{err}");
                assert!(err.contains(&uid.to_string()), "the record is named: {err}");
            }
            store.remove(&uid).expect("remove");
        }
    }

    /// Verifies: ADR-0023, ADR-0033
    #[test]
    fn stored_assignments_are_held_to_the_write_rules_on_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        let open = || FsAgentStore::open(dir.path()).expect("open");
        assignments_are_held_to_the_write_rules(
            &open(),
            &|uid, deployment, configuration, revision| {
                let file = dir.path().join(format!("agents/{uid}.json"));
                let mut envelope: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&file).expect("read")).expect("json");
                envelope["assignment"] = serde_json::json!({
                    "deployment": deployment,
                    "configurations": {configuration: revision},
                });
                std::fs::write(&file, envelope.to_string()).expect("write");
            },
            &|| vec![open().load().map(|_| ())],
        );
    }

    /// Opening the adapter reads the assignments alone, and the load reads every record: both
    /// are refused.
    /// Verifies: ADR-0023, ADR-0033
    #[test]
    fn stored_assignments_are_held_to_the_write_rules_on_postgres() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        assignments_are_held_to_the_write_rules(
            &PgAgentStore::new(database.clone()),
            &|uid, deployment, configuration, revision| {
                test.sql(&format!(
                    "UPDATE agents SET deployment = '{deployment}' \
                     WHERE instance_uid = '{uid}'; \
                     UPDATE agent_assignment_configurations SET \
                     configuration = '{configuration}', revision = '{revision}' \
                     WHERE instance_uid = '{uid}'"
                ));
            },
            &|| {
                vec![
                    PgAgentStore::open(database.clone()).map(|_| ()),
                    PgAgentStore::new(database.clone()).load().map(|_| ()),
                ]
            },
        );
    }

    /// The count of the Agents' records follows every row added and removed, whichever call adds
    /// or removes it, and the ceiling is decided from it and not from the rows: a count that says
    /// full refuses a new Agent though fewer rows are stored.
    /// Verifies: ADR-0023, ADR-0030
    #[test]
    fn the_count_of_agents_follows_every_addition_and_removal_and_decides_the_ceiling() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let store = PgAgentStore::open(database.clone()).expect("open");
        let counted = || {
            let rows = test.rows("SELECT (SELECT agents FROM agent_count), count(*) FROM agents");
            let (kept, stored) = (rows[0][0].clone(), rows[0][1].clone());
            assert_eq!(kept, stored, "the count is the rows'");
            kept.expect("a count").parse::<usize>().expect("a number")
        };
        assert_eq!(counted(), 0);
        let uids = [1u8, 2, 3].map(|byte| InstanceUid([byte; 16]));
        for uid in &uids[..2] {
            assert!(matches!(
                store.create_within(uid, &record(), 10).expect("create"),
                Creation::Created
            ));
        }
        assert!(matches!(
            store
                .create_within(&uids[0], &record(), 10)
                .expect("create"),
            Creation::Found(_)
        ));
        assert_eq!(counted(), 2, "a record created, not one found");
        store.put(&uids[2], &record()).expect("put");
        assert_eq!(counted(), 3, "a record put");
        let moved = InstanceUid([4; 16]);
        store.rekey(&uids[2], &moved, &record()).expect("rekey");
        assert_eq!(counted(), 3, "a record moved");
        store.remove(&moved).expect("remove");
        assert_eq!(counted(), 2, "a record removed");

        test.sql("UPDATE agent_count SET agents = 10");
        assert!(
            matches!(
                store.create_within(&moved, &record(), 10).expect("create"),
                Creation::Full
            ),
            "decided by the count"
        );
        test.sql("UPDATE agent_count SET agents = 2");
        assert!(matches!(
            store.create_within(&moved, &record(), 3).expect("create"),
            Creation::Created
        ));
        assert_eq!(counted(), 3);
    }

    /// The count follows the Agents' records of its own schema whatever the search path of the
    /// session that writes them: the triggers' functions are pinned to the schema the migration
    /// ran in, not to the search path it ran under. Here that path starts with `"$user"` and names
    /// a schema created only afterwards, which a function keeping the path would count in.
    /// Verifies: ADR-0023
    #[test]
    fn the_count_is_kept_in_its_own_schema_whatever_the_writing_session_names_first() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let decoy = format!("fleet_test_decoy_{}", hex::encode(InstanceUid::default().0));
        let database = crate::postgres::Database::open_in(
            &test.config(),
            crate::postgres::Exclusive::Alone("the test".to_string()),
            Some(&format!("\"$user\",{decoy},{}", test.schema)),
        )
        .expect("open under a path naming $user first");
        let store = PgAgentStore::open(database.clone()).expect("open");
        store.put(&InstanceUid([1; 16]), &record()).expect("put");
        let copied = InstanceUid([2; 16]);
        test.sql(&format!(
            "CREATE SCHEMA {decoy};
             CREATE TABLE {decoy}.agent_count (
                 one boolean PRIMARY KEY DEFAULT true, agents bigint NOT NULL);
             INSERT INTO {decoy}.agent_count (agents) VALUES (100);
             CREATE TEMP TABLE copied AS SELECT * FROM agents;
             UPDATE copied SET instance_uid = '{copied}';
             SELECT set_config('search_path', '{decoy},' || current_schema(), false);
             INSERT INTO agents SELECT * FROM copied;
             DELETE FROM agents WHERE instance_uid = '{copied}';
             INSERT INTO agents SELECT * FROM copied;"
        ));
        let decoyed = test.rows(&format!("SELECT agents FROM {decoy}.agent_count"));
        test.sql(&format!("DROP SCHEMA {decoy} CASCADE"));
        let rows = test.rows("SELECT (SELECT agents FROM agent_count), count(*) FROM agents");
        assert_eq!(rows[0][0], rows[0][1], "the count is the rows'");
        assert_eq!(rows[0][1].as_deref(), Some("2"));
        assert_eq!(
            decoyed[0][0].as_deref(),
            Some("100"),
            "the decoy is untouched"
        );
    }

    /// A creation commits without waiting for its flush, and the calls made after it on the same
    /// connections commit synchronously again: a take, a report's write and a binding after it
    /// wait for the flush, of the creation's too.
    /// Verifies: ADR-0023
    #[test]
    fn a_creation_commits_without_its_flush_and_no_other_call_does() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        test.sql(
            "CREATE TABLE commits_seen (statement text, setting text);
             CREATE FUNCTION commit_seen() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN
                 INSERT INTO commits_seen VALUES (TG_OP, current_setting('synchronous_commit'));
                 RETURN NULL;
             END $$;
             CREATE TRIGGER commit_seen AFTER INSERT OR UPDATE ON agents
                 FOR EACH ROW EXECUTE FUNCTION commit_seen();",
        );
        let store = PgAgentStore::open(database.clone()).expect("open");
        let mut bare = record();
        bare.assignment = None;
        let uids: Vec<InstanceUid> = (0..20u8).map(|n| InstanceUid([0x50 + n; 16])).collect();
        for uid in &uids {
            store.create_within(uid, &bare, 1_000).expect("create");
        }
        for uid in &uids {
            store
                .take(uid, "00000000-0000-7000-8000-00000000000a", 5)
                .expect("take");
            let mut reported = bare.clone();
            reported.sequence_num = 9;
            store.write_report(uid, &reported, None).expect("write");
        }
        let seen = |statement: &str| -> Vec<String> {
            let mut settings: Vec<String> = test
                .rows(&format!(
                    "SELECT DISTINCT setting FROM commits_seen WHERE statement = '{statement}'"
                ))
                .into_iter()
                .flatten()
                .flatten()
                .collect();
            settings.sort();
            settings
        };
        assert_eq!(seen("INSERT"), ["off"], "a creation waits for no flush");
        assert_eq!(seen("UPDATE"), ["on"], "a take and a write wait for theirs");
    }

    /// The pool holds the connections `[database] pool_size` names, and the store creates half of
    /// them at once.
    /// Verifies: ADR-0023
    #[test]
    fn the_pool_holds_what_pool_size_names_and_half_of_it_creates() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        for (size, creating) in [(2, 1), (6, 3), (40, 20)] {
            let mut config = test.config();
            config.pool_size = size;
            let database = Database::open_in(
                &config,
                super::super::Exclusive::Alone("the test".to_string()),
                Some(&test.schema),
            )
            .expect("open");
            assert_eq!(database.pool_size(), size, "the pool");
            let store = PgAgentStore::new(database.clone());
            assert_eq!(store.creating.at_most, creating, "the creations at once");
            drop(store);
            drop(database);
        }
    }

    /// While every pooled connection is busy for longer than the five seconds any other call waits
    /// for one, a creation waits on and creates the record once a connection is free, and a read
    /// fails after its five seconds: a new Agent is not answered `Unavailable` while a connecting
    /// fleet holds the pool for a few seconds (ADR-0023 clause 24).
    /// Verifies: ADR-0023
    #[test]
    fn a_creation_waits_for_a_pooled_connection_longer_than_any_other_call() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let mut config = test.config();
        config.pool_size = 2;
        let database = Database::open_in(
            &config,
            super::super::Exclusive::Alone("the test".to_string()),
            Some(&test.schema),
        )
        .expect("open");
        let store = Arc::new(PgAgentStore::open(database.clone()).expect("open"));
        // Both connections of the pool, held past the five seconds and well within the call's
        // bound.
        use std::time::Duration;
        const BUSY: Duration = Duration::from_secs(7);
        let (held, holding) = std::sync::mpsc::channel();
        let busy = {
            let database = database.clone();
            std::thread::spawn(move || {
                database.run(move |pool| async move {
                    let one = client(&pool).await?;
                    let other = client(&pool).await?;
                    let _ = held.send(());
                    tokio::time::sleep(BUSY).await;
                    drop((one, other));
                    Ok(())
                })
            })
        };
        holding
            .recv_timeout(Duration::from_secs(10))
            .expect("the pool is held");
        let started = std::time::Instant::now();
        let creating = {
            let store = store.clone();
            std::thread::spawn(move || {
                let mut bare = record();
                bare.assignment = None;
                store.create_within(&InstanceUid([0x61; 16]), &bare, 100)
            })
        };
        let read = store.get(&InstanceUid([0x62; 16]));
        assert!(
            read.as_ref()
                .is_err_and(|e| e.contains("waiting for a slot")),
            "a read waits five seconds for a connection: {read:?}"
        );
        assert!(
            started.elapsed() < BUSY,
            "the read gave up before the pool was free: {:?}",
            started.elapsed()
        );
        let created = creating.join().expect("the creation");
        assert!(
            matches!(created, Ok(Creation::Created)),
            "the creation waited for a connection: {created:?}"
        );
        assert!(started.elapsed() >= BUSY - Duration::from_secs(1));
        busy.join().expect("the holder").expect("held");
    }

    /// Creations that wait for the count's row, which a creation elsewhere holds, hold no
    /// connection of the pool meanwhile, so the other calls of the instance are answered.
    /// Verifies: ADR-0023
    #[test]
    fn creations_waiting_for_their_lock_leave_the_pool_to_the_other_calls() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let store = Arc::new(PgAgentStore::open(database.clone()).expect("open"));
        let gate = test.hold("SELECT agents FROM agent_count FOR UPDATE;");
        // More than the whole pool.
        let waiting: Vec<_> = (0..24u8)
            .map(|n| {
                let store = store.clone();
                std::thread::spawn(move || {
                    store.create_within(&InstanceUid([0x40 + n; 16]), &record(), 100)
                })
            })
            .collect();
        // However many creations wait, a read is answered within the pool's wait.
        for _ in 0..3 {
            store
                .get(&InstanceUid([0x3f; 16]))
                .expect("a read while creations wait");
        }
        gate.open();
        for creation in waiting {
            assert!(matches!(
                creation.join().expect("a creation").expect("created"),
                Creation::Created
            ));
        }
    }

    /// A read that began before this instance committed a release never takes the older
    /// assignment over it: the release pins what it pins after the query and the read of the
    /// records that began before it, so a deletion it pins stays refused. A read that began after
    /// it takes up what another instance committed.
    /// Verifies: ADR-0033, ADR-0023
    #[test]
    fn a_read_that_began_before_a_release_does_not_undo_what_it_pins() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        // A rollout releases through a stored Deployment.
        test.store_deployments(&["stable"]);
        let store = Arc::new(PgAgentStore::open(database.clone()).expect("open"));
        let uid = InstanceUid([5; 16]);
        let mut bare = record();
        bare.assignment = None;
        store.put(&uid, &bare).expect("put");
        let released = PersistedAgent {
            assignment: Some(Assignment {
                deployment: "stable".to_string(),
                package: None,
                configurations: BTreeMap::new(),
            }),
            ..bare.clone()
        };
        let release_in_between = |store: &Arc<PgAgentStore>, record: PersistedAgent| {
            let releasing = store.clone();
            *store.after_read.lock().expect("hook") = Some(Box::new(move || {
                assert_eq!(releasing.release(&[], &[(uid, record)]), Ok(vec![uid]));
            }));
        };

        release_in_between(&store, released.clone());
        store.query(None).expect("query");
        assert_eq!(
            store.pinning(Pin::Deployment("stable")),
            1,
            "after the query"
        );

        release_in_between(&store, bare.clone());
        store.get_many(&[uid]).expect("get_many");
        assert_eq!(
            store.pinning(Pin::Deployment("stable")),
            0,
            "after the read"
        );

        release_in_between(&store, released);
        store.query(Some(&[uid])).expect("query");
        assert_eq!(
            store.pinning(Pin::Deployment("stable")),
            1,
            "after the query of one"
        );

        // Another instance's release is taken up by a read that begins after it.
        PgAgentStore::new(database)
            .release(&[], &[(uid, bare)])
            .expect("release elsewhere");
        store.query(None).expect("query");
        assert_eq!(store.pinning(Pin::Deployment("stable")), 0, "taken up");
    }

    /// What the adapter keeps of its commits for the reads in flight does not grow with an Agent's
    /// re-keys or releases while no read is in flight, and leaves with the last read in flight.
    /// Verifies: ADR-0023
    #[test]
    fn the_commits_kept_for_reads_in_flight_do_not_grow_without_one() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let store = Arc::new(PgAgentStore::open(test.open()).expect("open"));
        let kept = |store: &PgAgentStore| store.assignments.lock().expect("pins").committed.len();
        let mut uid = InstanceUid([6; 16]);
        store.put(&uid, &record()).expect("put");
        for n in 0..50u8 {
            let next = InstanceUid([7, n, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
            store.rekey(&uid, &next, &record()).expect("rekey");
            uid = next;
        }
        assert_eq!(kept(&store), 0, "no read in flight, nothing kept");

        let during = store.clone();
        let seen = Arc::new(Mutex::new(0));
        let noted = seen.clone();
        let last = uid;
        *store.after_read.lock().expect("hook") = Some(Box::new(move || {
            during
                .rekey(&last, &InstanceUid([8; 16]), &record())
                .expect("rekey");
            *noted.lock().expect("seen") = kept(&during);
        }));
        store.query(None).expect("query");
        assert_eq!(
            *seen.lock().expect("seen"),
            2,
            "kept for the read in flight"
        );
        assert_eq!(kept(&store), 0, "and gone with it");
    }

    /// Of two reads in flight, the one that ends first leaves what the other is decided by: a
    /// release committed while both were in flight is still noted when the older read takes up its
    /// snapshot, so it does not take the older assignment over the release.
    /// Verifies: ADR-0033, ADR-0023
    #[test]
    fn a_read_that_ends_first_leaves_what_an_older_read_in_flight_is_decided_by() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let store = PgAgentStore::open(test.open()).expect("open");
        // A rollout releases through a stored Deployment.
        test.store_deployments(&["stable"]);
        let (agent, other) = (InstanceUid([9; 16]), InstanceUid([10; 16]));
        let mut bare = record();
        bare.assignment = None;
        store.put(&agent, &bare).expect("put");
        store.put(&other, &bare).expect("put");
        // The older read has taken its snapshot: the Agent with no assignment.
        let older = store.began();
        let released = PersistedAgent {
            assignment: Some(Assignment {
                deployment: "stable".to_string(),
                package: None,
                configurations: BTreeMap::new(),
            }),
            ..bare
        };
        assert_eq!(store.release(&[], &[(agent, released)]), Ok(vec![agent]));
        // A read begins after the release and is still in flight; a short one begins after it and
        // ends before both.
        let middle = store.began();
        store.get_many(&[other]).expect("read");
        store.refresh(older, Read::Only(&[agent]), |_| None, std::iter::empty());
        drop(middle);
        assert_eq!(
            store.pinning(Pin::Deployment("stable")),
            1,
            "the older read did not take its snapshot over the release"
        );
    }

    /// Under reads that always overlap, what is kept of the commits is only what the oldest read
    /// in flight can be decided by: an Agent that re-keys on every report does not grow it.
    /// Verifies: ADR-0023
    #[test]
    fn under_overlapping_reads_the_commits_kept_follow_the_oldest_read_in_flight() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let store = PgAgentStore::open(test.open()).expect("open");
        let kept = |store: &PgAgentStore| store.assignments.lock().expect("pins").committed.len();
        let mut uid = InstanceUid([11; 16]);
        store.put(&uid, &record()).expect("put");
        let mut in_flight = store.began();
        for n in 0..50u8 {
            let next_read = store.began();
            let next = InstanceUid([12, n, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
            store.rekey(&uid, &next, &record()).expect("rekey");
            uid = next;
            // The older read ends; one is in flight at every moment.
            drop(std::mem::replace(&mut in_flight, next_read));
        }
        assert_eq!(
            kept(&store),
            2,
            "the last re-key's, which the read in flight may need"
        );
        drop(in_flight);
        assert_eq!(kept(&store), 0);
    }

    /// The query reads each record with its stored presence: connected only while the instance
    /// its presence names holds a live lease. Opening the adapter reads the stored assignments
    /// alone, so what pins what is answered without a record loaded.
    /// Verifies: ADR-0030, ADR-0023
    #[test]
    fn the_query_reads_the_stored_presence_and_opening_reads_the_assignments_alone() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let a = database
            .take_lease("a", std::time::Duration::from_secs(60))
            .expect("lease")
            .instance_id;
        let store = PgAgentStore::new(database.clone());
        let (held, idle) = (InstanceUid([1; 16]), InstanceUid([2; 16]));
        store.put(&held, &record()).expect("put");
        let mut bare = record();
        bare.assignment = None;
        store.put(&idle, &bare).expect("put");
        store
            .take(&held, &a, 5_000)
            .expect("take")
            .expect("a record");
        let connected = |store: &PgAgentStore| -> Vec<(InstanceUid, bool)> {
            store
                .query(None)
                .expect("query")
                .into_iter()
                .map(|(uid, stored)| (uid, stored.connected))
                .collect()
        };
        assert_eq!(connected(&store), [(held, true), (idle, false)]);
        test.sql(&format!(
            "UPDATE leases SET expires_at = now() - interval '1 second' WHERE instance_id = '{a}'"
        ));
        assert_eq!(
            connected(&store),
            [(held, false), (idle, false)],
            "a holder without a live lease holds no connection"
        );

        let opened = PgAgentStore::open(database).expect("open");
        assert_eq!(opened.pinning(Pin::Deployment("stable")), 1);
        assert_eq!(
            opened.referenced_revisions("base"),
            BTreeSet::from(["0123abcd".repeat(8)])
        );
    }

    /// What a read found of the Agent's assignment: its Deployment and its revision of `base`.
    fn found(assignment: Option<Assignment>) -> Option<(String, String)> {
        assignment.map(|assignment| {
            (
                assignment.deployment,
                assignment.configurations["base"].clone(),
            )
        })
    }

    fn pair(deployment: &str, revision: &str) -> (String, String) {
        (deployment.to_string(), revision.to_string())
    }

    /// Runs `read` on a thread of its own, and its answer within 30 seconds.
    fn answer_of<T: Send + 'static>(
        label: &str,
        read: impl FnOnce() -> T + Send + 'static,
    ) -> impl FnOnce() -> T {
        let (sent, answer) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sent.send(read());
        });
        let label = label.to_string();
        move || {
            answer
                .recv_timeout(std::time::Duration::from_secs(30))
                .unwrap_or_else(|_| panic!("{label}: no answer"))
        }
    }

    /// A read of an Agent reads its row and its Configurations from one committed state: a
    /// rollout that commits while the read waits for the row is read whole or not at all, by
    /// `get_many`, `load`, `query` and opening the adapter, never one rollout's revisions with
    /// another's Deployment. The rollout commits from a session that holds `agents` against reads
    /// until the read waits for it. A read in one statement waits before it reads anything, and
    /// reads what the rollout committed; `query` reads in a snapshot taken before it waits, and
    /// reads what was there before.
    /// Verifies: ADR-0030, ADR-0045
    #[test]
    fn an_agent_is_read_in_one_snapshot_whatever_commits_while_it_waits() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let store = Arc::new(PgAgentStore::new(database.clone()));
        let uid = InstanceUid([7; 16]);
        store.put(&uid, &record()).expect("put");
        let mut states = [
            pair("stable", &"0123abcd".repeat(8)),
            pair("canary", &"4567cdef".repeat(8)),
        ];
        // Each race rolls the Agent over to the other state.
        let mut race =
            |label: &str,
             reads_after: bool,
             read: Box<dyn FnOnce() -> Option<(String, String)> + Send>| {
                let [before, after] = states.clone();
                let held = test.hold_reads(
                    "agents",
                    &format!(
                        "UPDATE agents SET deployment = '{}'; \
                     UPDATE agent_assignment_configurations SET revision = '{}'",
                        after.0, after.1
                    ),
                );
                let answer = answer_of(label, read);
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                while test.sessions(
                    "wait_event_type = 'Lock' AND query LIKE 'SELECT instance_uid%FROM agents%'",
                ) == 0
                {
                    assert!(std::time::Instant::now() < deadline, "{label}: never waits");
                }
                held.open();
                let expected = if reads_after { &after } else { &before };
                assert_eq!(answer().as_ref(), Some(expected), "{label}");
                states = [after, before];
            };

        let reading = store.clone();
        race(
            "get_many",
            true,
            Box::new(move || {
                found(
                    reading
                        .get_many(&[uid])
                        .expect("get_many")
                        .remove(&uid)
                        .and_then(|record| record.assignment),
                )
            }),
        );
        let reading = store.clone();
        race(
            "load",
            true,
            Box::new(move || {
                found(
                    reading
                        .load()
                        .expect("load")
                        .remove(&uid)
                        .and_then(|record| record.assignment),
                )
            }),
        );
        let reading = store.clone();
        race(
            "query",
            false,
            Box::new(move || {
                found(
                    reading
                        .query(None)
                        .expect("query")
                        .pop()
                        .and_then(|(_, stored)| stored.record.assignment),
                )
            }),
        );
        race(
            "open",
            true,
            Box::new(move || {
                let opened = PgAgentStore::open(database).expect("open");
                let deployment = ["stable", "canary"]
                    .into_iter()
                    .find(|name| opened.pinning(Pin::Deployment(name)) == 1)?;
                let revisions = opened.referenced_revisions("base");
                let [revision] =
                    <[String; 1]>::try_from(revisions.into_iter().collect::<Vec<_>>()).ok()?;
                Some(pair(deployment, &revision))
            }),
        );
    }

    /// A creation that finds the Agent stored reads it from one committed state: a rollout that
    /// commits while the read is part-way, its Configurations held at a gate, is not read in part.
    /// The creation counts the rows of `agents` first, so the gate stands in the Configurations,
    /// behind a view in place of their table.
    /// Verifies: ADR-0030, ADR-0045
    #[test]
    fn a_creation_that_finds_the_agent_reads_it_in_one_snapshot() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let store = Arc::new(PgAgentStore::new(test.open()));
        let uid = InstanceUid([8; 16]);
        store.put(&uid, &record()).expect("put");
        let key = 0x2900_2900;
        test.sql(&format!(
            "ALTER TABLE agent_assignment_configurations RENAME TO held_configurations; \
             CREATE FUNCTION fleet_test_held() RETURNS boolean LANGUAGE plpgsql AS $$ BEGIN \
                 PERFORM pg_advisory_lock_shared({key}); \
                 PERFORM pg_advisory_unlock_shared({key}); RETURN true; END $$; \
             CREATE VIEW agent_assignment_configurations AS \
                 SELECT * FROM held_configurations WHERE fleet_test_held();"
        ));
        let gate = test.gate(key);
        let creating = store.clone();
        let answer = answer_of("create_within", move || {
            match creating.create_within(&uid, &record(), 10).expect("create") {
                Creation::Found(record) => found(record.assignment),
                _ => None,
            }
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !test.waits_for(key) {
            assert!(std::time::Instant::now() < deadline, "never waits");
        }
        test.sql(&format!(
            "UPDATE agents SET deployment = 'canary'; \
             UPDATE held_configurations SET revision = '{}'",
            "4567cdef".repeat(8)
        ));
        gate.open();
        assert_eq!(answer(), Some(pair("stable", &"0123abcd".repeat(8))));
    }

    /// A record created, a report written and a record imported each carry their derived columns
    /// and the digest of what they derive from, written in the statement that writes the record,
    /// so no row a Server of this release writes needs deriving again.
    /// Verifies: ADR-0031
    #[test]
    fn every_write_of_a_record_writes_its_derived_columns_with_it() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let store = PgAgentStore::new(database.clone());
        let stale = || {
            test.once(&format!(
                "SELECT count(*)::text FROM agents WHERE derived_from IS DISTINCT FROM {}",
                crate::postgres::schema::stamp!()
            ))
        };
        let mut full = record();
        full.package_statuses = Some(opamp::proto::PackageStatuses {
            error_message: "a package".to_string(),
            ..Default::default()
        });
        assert!(
            full.health.is_some() && full.description.is_some() && full.package_statuses.is_some()
        );
        let (created, imported) = (InstanceUid([0x31; 16]), InstanceUid([0x32; 16]));
        store.put(&created, &full).expect("create");
        assert_eq!(stale(), "0", "created");
        let mut reported = full.clone();
        reported.health = None;
        reported.capabilities ^= 1;
        assert!(store
            .write_report(&created, &reported, None)
            .expect("report"));
        assert_eq!(stale(), "0", "reported");
        let import = |uid: InstanceUid, record: PersistedAgent| {
            database
                .run(move |pool| async move {
                    let client = client(&pool).await?;
                    write(&client, &uid, &record).await
                })
                .expect("import");
        };
        import(imported, full.clone());
        assert_eq!(stale(), "0", "imported");
        let mut over = full.clone();
        over.health = None;
        import(created, over);
        assert_eq!(stale(), "0", "imported over a record");
        let names = test.once(&format!(
            "SELECT display_name || '/' || search_text FROM agents WHERE instance_uid = '{created}'"
        ));
        assert!(names.contains(&created.to_string()), "{names}");
    }

    /// A page names the holder of an Agent only while its lease is live: `holder=<id>` lists the
    /// Agent and `holder=none` does not, `connected=true` lists it; once the lease has lapsed,
    /// without being released, `holder=none` lists it, and neither `holder=<id>` nor
    /// `connected=true` does. The read of the Agents behind a Gateway's Host says the same.
    /// Verifies: ADR-0031
    #[test]
    fn a_page_names_a_holder_only_while_its_lease_is_live() {
        use crate::listing::{AgentFilter, AgentSort, HolderFilter};
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let a = database
            .take_lease("host-a", std::time::Duration::from_secs(60))
            .expect("lease")
            .instance_id;
        let store = PgAgentStore::new(database.clone());
        let held = InstanceUid([7; 16]);
        store.put(&held, &record()).expect("put");
        store
            .take(&held, &a, 5_000)
            .expect("take")
            .expect("a record");
        let host = record().host;
        let page = |filter: AgentFilter| {
            store
                .query_page(&crate::agent_store::PageQuery {
                    filter: &filter,
                    sort: AgentSort::Name,
                    descending: false,
                    after: None,
                    limit: 10,
                    seen_before_ms: 0,
                })
                .expect("page")
                .agents
        };
        let uids = |filter: AgentFilter| -> Vec<InstanceUid> {
            page(filter).iter().map(|agent| agent.uid).collect()
        };
        let holder = |holder: HolderFilter| AgentFilter {
            holder: Some(holder),
            ..AgentFilter::default()
        };
        let connected = AgentFilter {
            connected: Some(true),
            ..AgentFilter::default()
        };
        let behind = AgentFilter {
            arrived_over: host.clone(),
            ..AgentFilter::default()
        };
        assert_eq!(uids(holder(HolderFilter::Instance(a.clone()))), [held]);
        assert_eq!(uids(holder(HolderFilter::None)), []);
        assert_eq!(uids(connected.clone()), [held]);
        let found = page(behind.clone());
        assert_eq!(
            found[0]
                .stored
                .holder
                .as_ref()
                .map(|h| (h.instance_id.as_str(), h.host.as_str())),
            Some((a.as_str(), "host-a"))
        );
        assert!(found[0].stored.connected);
        test.sql(&format!(
            "UPDATE leases SET expires_at = now() - interval '1 second' WHERE instance_id = '{a}'"
        ));
        assert_eq!(uids(holder(HolderFilter::Instance(a.clone()))), []);
        assert_eq!(
            uids(holder(HolderFilter::None)),
            [held],
            "a lapsed lease holds nothing"
        );
        assert_eq!(uids(connected), []);
        let found = page(behind);
        assert_eq!(found[0].stored.holder, None);
        assert!(!found[0].stored.connected);
    }

    /// A rollout whose last record cannot be written leaves none of its assignments behind, on
    /// either store, and what the store answers about pins stays as it was (ADR-0023 clause 21):
    /// the records already written are put back, or never committed. `refuse(uid)` makes the
    /// write of that Agent's record fail; `mend` takes that away. Answers the retained revisions
    /// the store holds afterwards, which the caller judges: none with PostgreSQL, at most one
    /// nothing references with files.
    fn a_rollout_that_fails(
        open: Open<'_>,
        refuse: &dyn Fn(&InstanceUid),
        mend: &dyn Fn(&InstanceUid),
    ) -> Vec<String> {
        let (store, configs) = open();
        let (one, two) = (InstanceUid::default(), InstanceUid::default());
        let mut before = record();
        before.assignment = None;
        store.put(&one, &before).expect("put");
        store.put(&two, &before).expect("put");
        store.load().expect("load");

        let pinned = revision("v1\n");
        let base = Configuration {
            name: "base".to_string(),
            saved: pinned.clone(),
            retained: BTreeMap::from([(revision_hash(&pinned), pinned.clone())]),
        };
        let mut released = record();
        released.assignment = Some(Assignment {
            deployment: "stable".to_string(),
            package: None,
            configurations: BTreeMap::from([("base".to_string(), revision_hash(&pinned))]),
        });
        configs.save("base", &base.saved).expect("save");
        refuse(&two);
        store
            .release(
                &[pinned_of(&pinned)],
                &[(one, released.clone()), (two, released)],
            )
            .expect_err("the second record cannot be written");
        mend(&two);

        let loaded = open().0.load().expect("load");
        assert!(loaded[&one] == before, "no assignment");
        assert!(loaded[&two] == before, "no assignment");
        assert_eq!(store.pinning(Pin::Deployment("stable")), 0);
        assert_eq!(store.pinning(Pin::Configuration("base")), 0);
        configs
            .load()
            .expect("load")
            .into_iter()
            .flat_map(|config| config.retained.into_keys())
            .collect()
    }

    /// Verifies: ADR-0023
    #[test]
    fn a_rollout_that_fails_leaves_no_assignment_on_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = |uid: &InstanceUid| dir.path().join(format!("agents/{uid}.json"));
        let left = a_rollout_that_fails(
            &|| {
                (
                    Box::new(FsAgentStore::open(dir.path()).expect("open")),
                    Box::new(FsConfigBackend::open(dir.path()).expect("open")),
                )
            },
            &|uid| crate::fs::testing::refuse(&file(uid)),
            &|uid| crate::fs::testing::mend(&file(uid)),
        );
        // The revisions are written first, and nothing references them (the port's contract).
        assert!(left.len() <= 1, "{left:?}");
    }

    /// With PostgreSQL the revisions and the assignments commit together or not at all.
    /// Verifies: ADR-0023
    #[test]
    fn a_rollout_that_fails_leaves_neither_revisions_nor_assignments_on_postgres() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let left = a_rollout_that_fails(
            &|| {
                (
                    Box::new(PgAgentStore::new(database.clone())),
                    Box::new(super::super::configs::PgConfigBackend::new(
                        database.clone(),
                    )),
                )
            },
            &|uid| test.refuse("agents", &format!("NEW.instance_uid = '{uid}'")),
            &|_| test.mend("agents"),
        );
        assert!(left.is_empty(), "no revision: {left:?}");
    }

    /// Two releases over the same two Agents, given them in opposite orders, both commit: each
    /// writes the rows in the order of their `instance_uid`, so the second waits for the first
    /// rather than holding the row the first needs next. The first is held at a gate in the
    /// database after its first row; the gate opens once the second waits for that row's lock, or
    /// has reached the gate itself, which only a release that writes in the order given does, and
    /// then the two would deadlock. Each update sets its session's `lock_timeout` aside before it
    /// waits for a row, so the second, parked behind the first, outlasts a loaded host's delays in
    /// opening the gate; the session's `statement_timeout` still bounds it.
    /// Verifies: ADR-0045
    #[test]
    fn two_releases_over_the_same_agents_in_opposite_orders_both_commit() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (low, high) = (InstanceUid([1; 16]), InstanceUid([2; 16]));
        // A rollout releases through a stored Deployment.
        test.store_deployments(&["first", "second"]);
        let mut bare = record();
        bare.assignment = None;
        let store = PgAgentStore::new(database.clone());
        store.put(&low, &bare).expect("put");
        store.put(&high, &bare).expect("put");
        test.sql(
            "CREATE TABLE fleet_test_gate (open boolean NOT NULL); \
             INSERT INTO fleet_test_gate VALUES (false); \
             CREATE FUNCTION fleet_test_wait() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
             WHILE NOT (SELECT open FROM fleet_test_gate) LOOP PERFORM pg_sleep(0.01); \
             END LOOP; RETURN NEW; END $$; \
             CREATE TRIGGER fleet_test_wait AFTER UPDATE ON agents \
             FOR EACH ROW EXECUTE FUNCTION fleet_test_wait(); \
             CREATE FUNCTION fleet_test_lock_untimed() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN PERFORM set_config('lock_timeout', '0', true); RETURN NULL; END $$; \
             CREATE TRIGGER fleet_test_lock_untimed BEFORE UPDATE ON agents \
             FOR EACH STATEMENT EXECUTE FUNCTION fleet_test_lock_untimed();",
        );
        let at_gate = || test.sessions("wait_event = 'PgSleep' AND query LIKE 'UPDATE agents%'");
        let until = |what: &str, done: &dyn Fn() -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while !done() {
                assert!(std::time::Instant::now() < deadline, "{what}");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        let release = |deployment: &str, order: [InstanceUid; 2]| {
            let mut released = bare.clone();
            released.assignment = Some(Assignment {
                deployment: deployment.to_string(),
                package: None,
                configurations: BTreeMap::new(),
            });
            let database = database.clone();
            std::thread::spawn(move || {
                PgAgentStore::new(database).release(&[], &order.map(|uid| (uid, released.clone())))
            })
        };

        let first = release("first", [high, low]);
        until("the first reaches the gate", &|| at_gate() == 1);
        let second = release("second", [low, high]);
        until("the second waits for the row, or reaches the gate", &|| {
            at_gate() == 2
                || test.sessions("wait_event_type = 'Lock' AND query LIKE 'UPDATE agents%'") == 1
        });
        test.sql("UPDATE fleet_test_gate SET open = true");
        first
            .join()
            .expect("the first's thread")
            .expect("the first commits");
        second
            .join()
            .expect("the second's thread")
            .expect("the second commits");
        assert_eq!(
            test.rows("SELECT DISTINCT deployment FROM agents"),
            [[Some("second".to_string())]],
            "the second, which waited, wrote last"
        );
    }

    /// A presence batch writes where each Agent is connected and by whom: a connected Agent names
    /// the holder, which counts as a take whose count the batch answers, `last_seen_ms` never moves
    /// back, a disconnect written by an instance that is not the holder leaves the holder's
    /// presence, and one under an earlier take leaves the presence a take wrote since. Forgetting decides from it: an Agent a live
    /// lease holds and that was heard from recently is kept; one whose holder holds no live lease
    /// reads as disconnected and goes, announced; one silent past the bound goes too.
    /// Verifies: ADR-0030, ADR-0045
    #[test]
    fn a_presence_batch_names_the_holder_and_forgetting_decides_from_it() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let a = database
            .take_lease("a", std::time::Duration::from_secs(60))
            .expect("lease")
            .instance_id;
        let store = PgAgentStore::new(database.clone());
        let (one, two, three) = (
            InstanceUid([1; 16]),
            InstanceUid([2; 16]),
            InstanceUid([3; 16]),
        );
        for uid in [one, two, three] {
            store.put(&uid, &record()).expect("put");
        }
        let at = |last_seen_ms: u64, connected: bool| Presence {
            connected,
            last_seen_ms,
        };
        let takes = store
            .write_presence(
                &a,
                &[
                    (two, at(5_000, true), None),
                    (one, at(5_000, true), None),
                    (three, at(5_000, true), None),
                ],
            )
            .expect("batch");
        assert_eq!(
            takes,
            HashMap::from([(one, 1), (two, 1), (three, 1)]),
            "named anew, each counted as a take"
        );
        let presence = |uid: InstanceUid| {
            test.rows(&format!(
                "SELECT connected::text, holder::text, last_seen_ms::text FROM agents \
                 WHERE instance_uid = '{uid}'"
            ))
        };
        assert_eq!(
            presence(one),
            [[Some("true".into()), Some(a.clone()), Some("5000".into())]]
        );
        store
            .write_presence(&a, &[(one, at(4_000, true), None)])
            .expect("an older one");
        assert_eq!(presence(one)[0][2].as_deref(), Some("5000"), "never back");
        let other = "00000000-0000-4000-8000-000000000001";
        store
            .write_presence(other, &[(one, at(6_000, false), Some(1))])
            .expect("not the holder's");
        assert_eq!(
            presence(one),
            [[Some("true".into()), Some(a.clone()), Some("6000".into())]],
            "the holder's presence stays"
        );

        assert_eq!(
            store.remove_if_silent(&one, 1_000),
            Ok(Some(Silenced::Reporting)),
            "still reporting"
        );
        assert_eq!(
            store.remove_if_silent(&one, 7_000),
            Ok(Some(Silenced::Forgotten(vec!["base".to_string()]))),
            "silent, with what its assignment named"
        );
        assert_eq!(store.remove_if_silent(&one, 7_000), Ok(None), "gone");
        test.sql(&format!("DELETE FROM leases WHERE instance_id = '{a}'"));
        assert!(
            matches!(
                store.remove_if_silent(&two, 1_000),
                Ok(Some(Silenced::Forgotten(_)))
            ),
            "a holder without a live lease holds no connection"
        );
        let taken = store
            .take(&three, &a, 5_500)
            .expect("take")
            .expect("a record");
        assert_eq!(taken.take, 2);
        store
            .write_presence(&a, &[(three, at(6_000, false), Some(1))])
            .expect("a disconnect under the earlier take");
        assert_eq!(
            presence(three),
            [[Some("true".into()), Some(a.clone()), Some("6000".into())]],
            "a take made since stays"
        );
        store
            .write_presence(&a, &[(three, at(7_000, false), Some(2))])
            .expect("the holder's disconnect");
        assert_eq!(
            presence(three),
            [[Some("false".into()), None, Some("7000".into())]]
        );
        assert!(
            matches!(
                store.remove_if_silent(&three, 1_000),
                Ok(Some(Silenced::Forgotten(_)))
            ),
            "disconnected"
        );
        assert!(store.load().expect("load").is_empty());
    }

    /// The newest connection takes an Agent: a take names its instance as the holder and answers
    /// the one it replaced. A presence batch of the instance it was taken from, read before the
    /// take and written after it, leaves the row as the take wrote it, also once the taker's lease
    /// has lapsed, since a take was counted after its own; a batch that was answered no count
    /// names its own instance over a row that names none, or an instance without a live lease, and
    /// one answered a count over such a row only while it still holds that count, never over a
    /// live holder.
    /// Verifies: ADR-0045
    #[test]
    fn a_batch_read_before_another_instance_took_the_agent_does_not_undo_the_take() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let lapse = std::time::Duration::from_secs(60);
        let a = database.take_lease("a", lapse).expect("lease").instance_id;
        let b = database.take_lease("b", lapse).expect("lease").instance_id;
        let store = PgAgentStore::new(database.clone());
        let (one, two) = (InstanceUid([1; 16]), InstanceUid([2; 16]));
        for uid in [one, two] {
            store.put(&uid, &record()).expect("put");
        }
        let presence = |uid: InstanceUid| {
            test.rows(&format!(
                "SELECT coalesce(holder::text, 'none') || ' ' || connected || ' ' || last_seen_ms \
                 FROM agents WHERE instance_uid = '{uid}'"
            ))
        };
        let connected = |last_seen_ms: u64| Presence {
            connected: true,
            last_seen_ms,
        };
        assert_eq!(
            store.take(&one, &a, 1_000),
            Ok(Some(Taken {
                take: 1,
                ..Taken::default()
            }))
        );
        // A's batch, due before B's take, holds the Agent connected.
        let due = [(one, connected(3_000), Some(1))];
        assert_eq!(
            store.take(&one, &b, 2_000),
            Ok(Some(Taken {
                from: Some(a.clone()),
                likely_duplicate: false,
                take: 2,
            }))
        );
        store.write_presence(&a, &due).expect("A's batch");
        assert_eq!(
            presence(one),
            [[Some(format!("{b} true 2000"))]],
            "the take stays"
        );
        assert_eq!(
            store.take(&InstanceUid([9; 16]), &a, 1_000),
            Ok(None),
            "no record"
        );

        store
            .write_presence(&a, &[(two, connected(3_000), None)])
            .expect("a row that names none");
        assert_eq!(presence(two), [[Some(format!("{a} true 3000"))]]);
        test.sql(&format!("DELETE FROM leases WHERE instance_id = '{b}'"));
        store.write_presence(&a, &due).expect("A's batch again");
        assert_eq!(
            presence(one),
            [[Some(format!("{b} true 2000"))]],
            "a take was counted since A's"
        );
        store
            .write_presence(&a, &[(one, connected(3_000), None)])
            .expect("A's batch answered no count");
        assert_eq!(
            presence(one),
            [[Some(format!("{a} true 3000"))]],
            "a holder without a live lease holds no connection"
        );
        assert_eq!(
            store.takes(&[one, two, InstanceUid([9; 16])]),
            Ok(HashMap::from([
                (one, (Some(a.clone()), 3)),
                (two, (Some(a.clone()), 1)),
            ])),
            "the holders and counts, none of a record not stored"
        );
        test.sql(&format!(
            "UPDATE agents SET holder = NULL, connected = false WHERE instance_uid = '{two}'"
        ));
        store
            .write_presence(&a, &[(two, connected(4_000), Some(1))])
            .expect("A's batch under the count it holds");
        assert_eq!(
            presence(two),
            [[Some(format!("{a} true 4000"))]],
            "a row that still holds the count is taken back"
        );
        store
            .write_presence(&b, &[(two, connected(5_000), Some(1))])
            .expect("B's batch under a count taken since");
        assert_eq!(
            presence(two),
            [[Some(format!("{a} true 4000"))]],
            "not under a count a take raised since"
        );
        test.sql(&format!(
            "INSERT INTO leases (instance_id, host, must_be_alone, taken_at, renewed_at, \
             expires_at) VALUES ('{b}', 'b', false, now(), now(), now() + interval '1 hour'); \
             UPDATE agents SET holder = '{b}'::uuid WHERE instance_uid = '{two}'"
        ));
        store
            .write_presence(&a, &[(two, connected(6_000), Some(2))])
            .expect("A's batch under its count, over a live holder");
        assert_eq!(
            presence(two),
            [[Some(format!("{b} true 4000"))]],
            "a live holder is never undone, whatever the count"
        );
        let gone = "00000000-0000-7000-8000-0000000000ff";
        test.sql(&format!(
            "UPDATE agents SET holder = '{gone}'::uuid WHERE instance_uid = '{two}'"
        ));
        store
            .write_presence(&a, &[(two, connected(7_000), Some(2))])
            .expect("A's batch under its count, over a holder without a live lease");
        assert_eq!(
            presence(two),
            [[Some(format!("{a} true 7000"))]],
            "a holder without a live lease is taken back under the count"
        );
    }

    /// A disconnect is written where the row still holds the count of takes it was carried under,
    /// and nowhere else, whatever the instances' clocks say: one written after another instance
    /// whose clock runs ahead took the Agent and this one took it back is written although the
    /// stored `last_seen_ms` is newer than it; one read before the Agent connected again within
    /// the same millisecond is not written, although its `last_seen_ms` is the stored one. A batch
    /// that names its instance anew answers the count it raised, and a disconnect under that count
    /// is written.
    /// Verifies: ADR-0030, ADR-0045
    #[test]
    fn a_disconnect_is_written_under_the_take_it_was_carried_by_whatever_the_clocks_say() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let lapse = std::time::Duration::from_secs(60);
        let a = database.take_lease("a", lapse).expect("lease").instance_id;
        let b = database.take_lease("b", lapse).expect("lease").instance_id;
        let store = PgAgentStore::new(database.clone());
        let (one, two) = (InstanceUid([1; 16]), InstanceUid([2; 16]));
        for uid in [one, two] {
            store.put(&uid, &record()).expect("put");
        }
        let presence = |uid: InstanceUid| {
            test.rows(&format!(
                "SELECT coalesce(holder::text, 'none') || ' ' || connected || ' ' || takes \
                 FROM agents WHERE instance_uid = '{uid}'"
            ))
        };
        let take = |uid: &InstanceUid, holder: &str, last_seen_ms: u64| {
            store
                .take(uid, holder, last_seen_ms)
                .expect("take")
                .expect("a record")
                .take
        };
        let disconnected = |last_seen_ms: u64| Presence {
            connected: false,
            last_seen_ms,
        };

        take(&one, &a, 1_000);
        take(&one, &b, 2_000);
        // B's clock runs a minute and a half ahead of A's.
        test.sql(&format!(
            "UPDATE agents SET last_seen_ms = 92000 WHERE instance_uid = '{one}'"
        ));
        let back = take(&one, &a, 3_000);
        store
            .write_presence(&a, &[(one, disconnected(4_000), Some(back))])
            .expect("A's disconnect");
        assert_eq!(
            presence(one),
            [[Some(format!("none false {back}"))]],
            "written, though the stored last_seen_ms is newer"
        );

        let first = take(&two, &a, 5_000);
        // The batch reads the disconnect, then the Agent connects again within the millisecond.
        let due = [(two, disconnected(5_000), Some(first))];
        let again = take(&two, &a, 5_000);
        store.write_presence(&a, &due).expect("A's batch");
        assert_eq!(
            presence(two),
            [[Some(format!("{a} true {again}"))]],
            "the connect made since stays"
        );

        let named = store
            .write_presence(
                &a,
                &[(
                    one,
                    Presence {
                        connected: true,
                        last_seen_ms: 6_000,
                    },
                    None,
                )],
            )
            .expect("A names itself anew");
        assert_eq!(named, HashMap::from([(one, back + 1)]));
        store
            .write_presence(&a, &[(one, disconnected(7_000), Some(back + 1))])
            .expect("A's disconnect");
        assert_eq!(presence(one), [[Some(format!("none false {}", back + 1))]]);
    }

    /// A report's write that records a Client Agent first arriving over a Gateway's Host announces
    /// it, named by the Host, after the write commits: where that notice is lost, as when the
    /// instance ends between the commit and the `NOTIFY`, another instance hears nothing, and a
    /// write that names no arrival announces nothing.
    /// Verifies: ADR-0045
    #[test]
    fn a_client_agent_arriving_over_a_gateway_is_announced_after_its_commit() {
        use super::super::notify::{Heard, Notice};
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let open = || {
            Database::open_in(
                &test.config(),
                super::super::Exclusive::Shared,
                Some(&test.schema),
            )
            .expect("open")
        };
        let (database, listener) = (open(), open());
        let (heard, hearing) = std::sync::mpsc::channel();
        listener.listen(move |what| {
            let _ = heard.send(what);
        });
        let within = std::time::Duration::from_secs(10);
        assert_eq!(hearing.recv_timeout(within), Ok(Heard::Listening));
        let store = PgAgentStore::new(database.clone());
        let uid = InstanceUid([0x5a; 16]);
        store.put(&uid, &record()).expect("put");

        database.afterwards().drop_all(true);
        assert_eq!(store.write_report(&uid, &record(), Some("g")), Ok(true));
        assert_eq!(store.write_report(&uid, &record(), None), Ok(true));
        assert!(
            hearing
                .recv_timeout(std::time::Duration::from_secs(1))
                .is_err(),
            "nothing is announced in the write's commit"
        );
        database.afterwards().drop_all(false);
        assert_eq!(store.write_report(&uid, &record(), None), Ok(true));
        assert_eq!(store.write_report(&uid, &record(), Some("g")), Ok(true));
        assert_eq!(
            hearing.recv_timeout(within),
            Ok(Heard::Notified(Notice::new(ARRIVED, "g"))),
            "the arrival is announced after its commit, and nothing before it"
        );
    }

    /// A take that replaces another holder is announced, whether or not that holder's lease is
    /// live, and one that replaces none is not.
    /// Verifies: ADR-0045
    #[test]
    fn a_take_that_replaces_another_holder_is_announced_whatever_its_lease() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let open = || {
            Database::open_in(
                &test.config(),
                super::super::Exclusive::Shared,
                Some(&test.schema),
            )
            .expect("open")
        };
        let (database, listener) = (open(), open());
        let (heard, hearing) = std::sync::mpsc::channel();
        listener.listen(move |what| {
            let _ = heard.send(what);
        });
        let within = std::time::Duration::from_secs(10);
        assert_eq!(
            hearing.recv_timeout(within),
            Ok(super::super::notify::Heard::Listening)
        );
        // Neither holder has a lease.
        let (a, b) = (
            "00000000-0000-7000-8000-00000000000a",
            "00000000-0000-7000-8000-00000000000b",
        );
        let store = PgAgentStore::new(database.clone());
        let (one, two) = (InstanceUid([1; 16]), InstanceUid([2; 16]));
        for uid in [one, two] {
            store.put(&uid, &record()).expect("put");
        }
        store.take(&one, a, 1_000).expect("take").expect("a record");
        store.take(&two, a, 1_000).expect("take").expect("a record");
        store.take(&two, b, 2_000).expect("take").expect("a record");
        assert_eq!(
            hearing.recv_timeout(within),
            Ok(super::super::notify::Heard::Notified(
                super::super::notify::Notice::new(TAKEN, &two.to_string())
            )),
            "the take from A, and before it nothing"
        );
    }

    /// The take that makes an Agent's holder change for the eleventh time within a minute, and no
    /// other, says it is likely a duplicate; a change more than a minute old counts no more, and a
    /// take that keeps the holder, or replaces none, is no change.
    /// Verifies: ADR-0045
    #[test]
    fn the_eleventh_change_of_holder_within_a_minute_is_a_likely_duplicate() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b) = (
            "00000000-0000-7000-8000-00000000000a",
            "00000000-0000-7000-8000-00000000000b",
        );
        let store = PgAgentStore::new(database.clone());
        let uid = InstanceUid([1; 16]);
        store.put(&uid, &record()).expect("put");
        let duplicate = |holder: &str| {
            store
                .take(&uid, holder, 1_000)
                .expect("take")
                .expect("a record")
                .likely_duplicate
        };
        assert!(!duplicate(a), "replaces none");
        assert!(!duplicate(a), "keeps the holder");
        let changes: Vec<bool> = (1..=12)
            .map(|change| duplicate(if change % 2 == 1 { b } else { a }))
            .collect();
        let mut expected = vec![false; 12];
        expected[10] = true;
        assert_eq!(changes, expected, "the eleventh change alone");

        test.sql(&format!(
            "UPDATE agents SET holder_changes = ARRAY(SELECT t - interval '61 seconds' \
             FROM unnest(holder_changes) AS t ORDER BY t DESC LIMIT 10) \
             WHERE instance_uid = '{uid}'"
        ));
        let changes: Vec<bool> = (1..=11)
            .map(|change| duplicate(if change % 2 == 1 { b } else { a }))
            .collect();
        assert_eq!(
            changes.iter().position(|likely| *likely),
            Some(10),
            "the changes over a minute ago count no more"
        );
    }

    /// Two presence batches over the same two Agents, given them in opposite orders, both commit:
    /// each locks the rows in the order of their `instance_uid` before it writes them, as the
    /// gated release test above shows for a release.
    /// Verifies: ADR-0030
    #[test]
    fn two_presence_batches_over_the_same_agents_in_opposite_orders_both_commit() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (low, high) = (InstanceUid([1; 16]), InstanceUid([2; 16]));
        let store = PgAgentStore::new(database.clone());
        store.put(&low, &record()).expect("put");
        store.put(&high, &record()).expect("put");
        // A fleet large enough that the database finds each row of a batch by its key, in the
        // order the batch names them, rather than scanning the table in one order for both.
        test.sql(
            "INSERT INTO agents (instance_uid, sequence_num, capabilities, transport, \
             last_seen_ms, restart_pending, labels) SELECT 'filler-' || n, 0, 0, 'http', 0, false, \
             '{}' FROM generate_series(1, 5000) AS n; ANALYZE agents",
        );
        test.sql(
            "CREATE TABLE fleet_test_gate (open boolean NOT NULL); \
             INSERT INTO fleet_test_gate VALUES (false); \
             CREATE FUNCTION fleet_test_wait() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
             WHILE NOT (SELECT open FROM fleet_test_gate) LOOP PERFORM pg_sleep(0.01); \
             END LOOP; RETURN NEW; END $$; \
             CREATE TRIGGER fleet_test_wait BEFORE UPDATE ON agents \
             FOR EACH ROW EXECUTE FUNCTION fleet_test_wait();",
        );
        let at_gate = || test.sessions("wait_event = 'PgSleep'");
        let until = |what: &str, done: &dyn Fn() -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while !done() {
                assert!(std::time::Instant::now() < deadline, "{what}");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        let batch = |holder: &'static str, order: [InstanceUid; 2]| {
            let database = database.clone();
            std::thread::spawn(move || {
                PgAgentStore::new(database).write_presence(
                    holder,
                    &order.map(|uid| {
                        (
                            uid,
                            Presence {
                                connected: true,
                                last_seen_ms: 9_000,
                            },
                            None,
                        )
                    }),
                )
            })
        };
        let (first_holder, second_holder) = (
            "00000000-0000-4000-8000-000000000001",
            "00000000-0000-4000-8000-000000000002",
        );
        let first = batch(first_holder, [high, low]);
        until("the first reaches the gate", &|| at_gate() == 1);
        let second = batch(second_holder, [low, high]);
        until("the second waits for a row, or reaches the gate", &|| {
            at_gate() == 2 || test.sessions("wait_event_type = 'Lock'") == 1
        });
        test.sql("UPDATE fleet_test_gate SET open = true");
        first
            .join()
            .expect("the first's thread")
            .expect("the first commits");
        second
            .join()
            .expect("the second's thread")
            .expect("the second commits");
        assert_eq!(
            test.rows("SELECT DISTINCT holder::text FROM agents WHERE holder IS NOT NULL"),
            [[Some(second_holder.to_string())]],
            "the second, which waited, wrote last"
        );
    }

    /// The ceiling on records holds for every instance on the store: the store counts its rows as
    /// it creates one, so a record past it is not created through any of them, and one stored
    /// already passes none.
    /// Verifies: ADR-0045
    #[test]
    fn the_records_are_bounded_where_they_are_stored() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b) = (
            PgAgentStore::new(database.clone()),
            PgAgentStore::new(database),
        );
        let (one, two, three) = (
            InstanceUid([1; 16]),
            InstanceUid([2; 16]),
            InstanceUid([3; 16]),
        );
        assert_eq!(a.create_within(&one, &record(), 2), Ok(Creation::Created));
        assert_eq!(b.create_within(&two, &record(), 2), Ok(Creation::Created));
        assert_eq!(
            a.create_within(&three, &record(), 2),
            Ok(Creation::Full),
            "past the ceiling"
        );
        assert_eq!(
            b.create_within(&three, &record(), 2),
            Ok(Creation::Full),
            "on either instance"
        );
        // Stored already, and labelled through A since: B is answered what is stored, and
        // writes nothing of what it was given.
        let labels = BTreeMap::from([("tier".to_string(), "edge".to_string())]);
        assert!(a.set_labels(&one, &labels).expect("labels"));
        let stored = PersistedAgent { labels, ..record() };
        let mut given = record();
        given.health = None;
        assert_eq!(
            b.create_within(&one, &given, 2),
            Ok(Creation::Found(Box::new(stored.clone()))),
            "stored already"
        );
        assert_eq!(a.load().expect("load").len(), 2);
        assert!(a.get(&one).expect("get") == Some(stored), "created whole");
    }

    /// A row that does not decode fails the read that meets it, naming its `instance_uid`: the
    /// load, the query, the query of it and the read of it. Opening the adapter, which reads the
    /// assignments alone, does not meet it (ADR-0023 clause 4).
    /// Verifies: ADR-0023
    #[test]
    fn an_agent_row_that_does_not_decode_fails_the_read_that_meets_it_naming_it() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let store = PgAgentStore::new(database.clone());
        let uid = InstanceUid::default();
        store.put(&uid, &record()).expect("put");
        test.sql("UPDATE agents SET health = '\\xff'::bytea");
        let errors = [
            store.load().map(|_| ()).expect_err("load"),
            store.query(None).map(|_| ()).expect_err("query"),
            store
                .query(Some(&[uid]))
                .map(|_| ())
                .expect_err("query of it"),
            store.get(&uid).map(|_| ()).expect_err("get"),
        ];
        for err in errors {
            assert!(err.contains(&uid.to_string()), "{err}");
            assert!(err.contains("health"), "{err}");
        }
        assert!(
            PgAgentStore::open(database).is_ok(),
            "opening meets no record"
        );
    }

    /// Two creations of new Agents on two instances at once, with one place left under the
    /// ceiling, are both in flight when the first commits: each waits for the row the count is
    /// kept in, the database lets one through, and the other counts the record the first created
    /// and is refused.
    /// Verifies: ADR-0045, ADR-0023
    #[test]
    fn two_creations_at_once_with_one_place_left_create_one_record() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let (a_database, b_database) = (test.shared(), test.shared());
        let a = PgAgentStore::new(a_database);
        a.create_within(&InstanceUid([1; 16]), &record(), 2)
            .expect("the first record");
        let b = PgAgentStore::new(b_database);
        // The wait for the count's row outlasts a loaded host's delays in opening the hold; the
        // session's `statement_timeout` still bounds it.
        test.sql(
            "CREATE FUNCTION fleet_test_lock_untimed() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN PERFORM set_config('lock_timeout', '0', true); RETURN NULL; END $$; \
             CREATE TRIGGER fleet_test_lock_untimed BEFORE INSERT ON agents \
             FOR EACH STATEMENT EXECUTE FUNCTION fleet_test_lock_untimed();",
        );
        // Both creations are under way before either may count.
        let hold = test.hold("SELECT agents FROM agent_count FOR UPDATE;");
        let (a, b) = (Arc::new(a), Arc::new(b));
        let create = |store: &Arc<PgAgentStore>, byte: u8| {
            let store = store.clone();
            std::thread::spawn(move || store.create_within(&InstanceUid([byte; 16]), &record(), 2))
        };
        let (on_a, on_b) = (create(&a, 2), create(&b, 3));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while test.sessions("wait_event_type = 'Lock' AND query LIKE 'INSERT INTO agents%'") < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "both creations wait for the count's row"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        hold.open();
        let mut answers = [
            on_a.join().expect("A's thread").expect("A's creation"),
            on_b.join().expect("B's thread").expect("B's creation"),
        ]
        .map(|creation| match creation {
            Creation::Created => "created",
            Creation::Full => "full",
            Creation::Found(_) => "found",
        });
        answers.sort_unstable();
        assert_eq!(answers, ["created", "full"], "one is refused");
        assert_eq!(
            test.rows("SELECT count(*), (SELECT agents FROM agent_count) FROM agents"),
            [[Some("2".to_string()), Some("2".to_string())]],
            "the ceiling holds, and the count is the rows'"
        );
    }

    /// A release and a presence batch over the same Agents at once lock the rows in one order,
    /// whichever comes first: the second waits for the first's commit rather than deadlocking, and
    /// both writes stay, each in the columns it names. The first is held at a gate the test opens
    /// once the second waits for a row.
    /// Verifies: ADR-0045, ADR-0030
    #[test]
    fn a_release_and_a_presence_batch_over_the_same_agents_at_once_both_commit() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let (releasing, writing) = (test.shared(), test.shared());
        let (low, high) = (InstanceUid([1; 16]), InstanceUid([2; 16]));
        // A rollout releases through a stored Deployment.
        test.store_deployments(&["first", "second"]);
        let mut bare = record();
        bare.assignment = None;
        let store = PgAgentStore::new(releasing.clone());
        store.put(&low, &bare).expect("put");
        store.put(&high, &bare).expect("put");
        test.sql(
            "CREATE TABLE fleet_test_gate (open boolean NOT NULL); \
             INSERT INTO fleet_test_gate VALUES (false); \
             CREATE FUNCTION fleet_test_wait() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
             WHILE NOT (SELECT open FROM fleet_test_gate) LOOP PERFORM pg_sleep(0.01); \
             END LOOP; RETURN NEW; END $$; \
             CREATE TRIGGER fleet_test_wait AFTER UPDATE ON agents \
             FOR EACH ROW EXECUTE FUNCTION fleet_test_wait(); \
             CREATE FUNCTION fleet_test_lock_untimed() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN PERFORM set_config('lock_timeout', '0', true); RETURN NULL; END $$; \
             CREATE TRIGGER fleet_test_lock_untimed BEFORE UPDATE ON agents \
             FOR EACH STATEMENT EXECUTE FUNCTION fleet_test_lock_untimed();",
        );
        let at_gate = || test.sessions("wait_event = 'PgSleep'");
        let waiting = |query: &str| {
            test.sessions(&format!(
                "wait_event_type = 'Lock' AND query LIKE '{query}%'"
            ))
        };
        let until = |what: &str, done: &dyn Fn() -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while !done() {
                assert!(std::time::Instant::now() < deadline, "{what}");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        let release = |deployment: &str, order: [InstanceUid; 2]| {
            let mut released = bare.clone();
            released.assignment = Some(Assignment {
                deployment: deployment.to_string(),
                package: None,
                configurations: BTreeMap::new(),
            });
            let database = releasing.clone();
            std::thread::spawn(move || {
                PgAgentStore::new(database).release(&[], &order.map(|uid| (uid, released.clone())))
            })
        };
        let batch = |holder: &'static str, order: [InstanceUid; 2]| {
            let database = writing.clone();
            std::thread::spawn(move || {
                PgAgentStore::new(database).write_presence(
                    holder,
                    &order.map(|uid| {
                        (
                            uid,
                            Presence {
                                connected: true,
                                last_seen_ms: 9_000,
                            },
                            None,
                        )
                    }),
                )
            })
        };
        let stored = || {
            test.rows(
                "SELECT deployment, holder::text, connected::text FROM agents \
                 ORDER BY instance_uid COLLATE \"C\"",
            )
        };
        let (first_holder, second_holder) = (
            "00000000-0000-4000-8000-000000000001",
            "00000000-0000-4000-8000-000000000002",
        );

        // The release first: it holds the first row at the gate, and the batch waits for it.
        let released = release("first", [high, low]);
        until("the release reaches the gate", &|| at_gate() == 1);
        let written = batch(first_holder, [low, high]);
        until("the batch waits for a row", &|| {
            waiting("SELECT 1 FROM agents") == 1
        });
        test.sql("UPDATE fleet_test_gate SET open = true");
        assert_eq!(
            released.join().expect("the release's thread"),
            Ok(vec![high, low]),
            "the release commits"
        );
        assert_eq!(
            written
                .join()
                .expect("the batch's thread")
                .expect("the batch commits")
                .len(),
            2
        );
        let both = |deployment: &str, holder: &str| {
            vec![
                vec![
                    Some(deployment.to_string()),
                    Some(holder.to_string()),
                    Some("true".to_string()),
                ];
                2
            ]
        };
        assert_eq!(stored(), both("first", first_holder), "both writes stay");

        // The batch first: it holds both rows at the gate, and the release waits for them.
        test.sql("UPDATE fleet_test_gate SET open = false");
        let written = batch(second_holder, [high, low]);
        until("the batch reaches the gate", &|| at_gate() == 1);
        let released = release("second", [low, high]);
        until("the release waits for a row", &|| {
            waiting("UPDATE agents") == 1
        });
        test.sql("UPDATE fleet_test_gate SET open = true");
        assert_eq!(
            written
                .join()
                .expect("the batch's thread")
                .expect("the batch commits")
                .len(),
            2
        );
        assert_eq!(
            released.join().expect("the release's thread"),
            Ok(vec![low, high]),
            "the release commits"
        );
        assert_eq!(stored(), both("second", second_holder), "both writes stay");
    }

    /// Two threads read the Agents while this instance's release commits between their reading
    /// the rows and their taking them up: neither takes the count of what pins the Deployment
    /// below the release's, the one that answers first included, whose end must not drop what
    /// the other is still decided by.
    /// Verifies: ADR-0033, ADR-0023
    #[test]
    fn two_reads_on_their_own_threads_under_a_commit_keep_what_it_pins() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        test.store_deployments(&["stable"]);
        let store = Arc::new(PgAgentStore::open(database).expect("open"));
        let uid = InstanceUid([13; 16]);
        let mut bare = record();
        bare.assignment = None;
        store.put(&uid, &bare).expect("put");
        // Each read, on its own thread, stops between its rows and taking them up until it is
        // let go.
        let read = |store: &Arc<PgAgentStore>| {
            let (arrived, arrives) = std::sync::mpsc::channel::<()>();
            let (go, goes) = std::sync::mpsc::channel::<()>();
            *store.after_read.lock().expect("hook") = Some(Box::new(move || {
                arrived.send(()).expect("arrived");
                goes.recv().expect("go");
            }));
            let reading = store.clone();
            let thread = std::thread::spawn(move || reading.query(None).map(|_| ()));
            arrives.recv().expect("the read has read the rows");
            (go, thread)
        };
        let (go_one, one) = read(&store);
        let (go_two, two) = read(&store);
        let released = PersistedAgent {
            assignment: Some(Assignment {
                deployment: "stable".to_string(),
                package: None,
                configurations: BTreeMap::new(),
            }),
            ..bare
        };
        assert_eq!(store.release(&[], &[(uid, released)]), Ok(vec![uid]));
        assert_eq!(store.pinning(Pin::Deployment("stable")), 1, "the release");
        go_one.send(()).expect("let the first go");
        one.join().expect("the first's thread").expect("the first");
        assert_eq!(
            store.pinning(Pin::Deployment("stable")),
            1,
            "after the first answered"
        );
        go_two.send(()).expect("let the second go");
        two.join()
            .expect("the second's thread")
            .expect("the second");
        assert_eq!(
            store.pinning(Pin::Deployment("stable")),
            1,
            "after the second answered"
        );
    }
}
