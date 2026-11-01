//! Agent records in PostgreSQL (ADR-0029 clauses 4 and 21): the PostgreSQL adapter behind
//! [`AgentStore`](crate::agent_store::AgentStore) — one row per Agent, its scalars as columns, the
//! wire-typed fields as their protobuf bytes, its labels, and its assignment's Configurations in
//! rows of their own. A rollout's revisions and assignments commit together or not at all. Each
//! call writes the columns it names and no other, and announces in its transaction what another
//! instance carrying the Agent takes up: a label change, a restart request, a release, a forget.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use opamp::uid::InstanceUid;
use prost::Message;
use tokio_postgres::GenericClient;

use super::notify::{notify, ASSIGNMENT, CONFIGURATION, FORGOTTEN, LABELS, RESTART};
use super::{client, signed, sql, unsigned, Database};
use crate::agent_store::{AgentStore, Creation, PersistedAgent, Pin, Presence};
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
    assignments: Mutex<HashMap<InstanceUid, Assignment>>,
}

impl PgAgentStore {
    /// The adapter on `database`.
    #[must_use]
    pub fn new(database: Arc<Database>) -> Self {
        PgAgentStore {
            database,
            assignments: Mutex::new(HashMap::new()),
        }
    }

    /// Notes what the committed records now pin.
    fn stored<'a>(
        &self,
        records: impl IntoIterator<Item = (&'a InstanceUid, Option<&'a Assignment>)>,
    ) {
        let mut assignments = self.assignments.lock().expect("assignments lock");
        for (uid, assignment) in records {
            match assignment {
                Some(assignment) => assignments.insert(*uid, assignment.clone()),
                None => assignments.remove(uid),
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
        let records = self.database.run(|pool| async move {
            let client = client(&pool).await?;
            read_all(&**client).await
        })?;
        *self.assignments.lock().expect("assignments lock") = records
            .iter()
            .filter_map(|(uid, record)| Some((*uid, record.assignment.clone()?)))
            .collect();
        Ok(records)
    }

    fn get(&self, uid: &InstanceUid) -> Result<Option<PersistedAgent>, String> {
        let key = uid.to_string();
        let mut found = self.database.run(move |pool| async move {
            let client = client(&pool).await?;
            read(&**client, Some(&key)).await
        })?;
        let record = found.remove(uid);
        self.stored([(uid, record.as_ref().and_then(|r| r.assignment.as_ref()))]);
        Ok(record)
    }

    fn is_shared(&self) -> bool {
        true
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
                if write_reported(&*transaction, &uid, &record).await? {
                    break false;
                }
                if insert(&*transaction, &uid, &record, "DO NOTHING").await? {
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

    /// The columns a report decides, of a row that is there; nothing where none is.
    fn write_report(&self, uid: &InstanceUid, record: &PersistedAgent) -> Result<bool, String> {
        let (uid, record) = (*uid, record.clone());
        self.database.run(move |pool| async move {
            let client = client(&pool).await?;
            write_reported(&**client, &uid, &record).await
        })
    }

    /// One transaction under a lock only creations take, so two instances creating at once count
    /// each other's rows; a report's write of a record that exists waits for none of it.
    fn create_within(
        &self,
        uid: &InstanceUid,
        record: &PersistedAgent,
        max_records: usize,
    ) -> Result<Creation, String> {
        let (uid, record) = (*uid, record.clone());
        let assignment = record.assignment.clone();
        let max = i64::try_from(max_records).unwrap_or(i64::MAX);
        let creation = self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            transaction
                .execute(&format!("SELECT pg_advisory_xact_lock({CREATE_LOCK})"), &[])
                .await
                .map_err(sql)?;
            let key = uid.to_string();
            let stored = transaction
                .query_one(
                    "SELECT count(*), count(*) FILTER (WHERE instance_uid = $1) FROM agents",
                    &[&key],
                )
                .await
                .map_err(sql)?;
            let (count, there): (i64, i64) = (stored.get(0), stored.get(1));
            if there > 0 {
                let found = read(&*transaction, Some(&key))
                    .await?
                    .remove(&uid)
                    .ok_or_else(|| format!("agent {key} was counted, and is not there"))?;
                return Ok(Creation::Found(Box::new(found)));
            }
            if count >= max {
                return Ok(Creation::Full);
            }
            insert(&*transaction, &uid, &record, "DO NOTHING").await?;
            transaction.commit().await.map_err(sql)?;
            Ok(Creation::Created)
        })?;
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
                notify(&*transaction, LABELS, &key).await?;
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
                notify(&*transaction, RESTART, &key).await?;
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
                notify(&*transaction, FORGOTTEN, &uid.to_string()).await?;
            }
            transaction.commit().await.map_err(sql)
        })?;
        self.stored([(&uid, None)]);
        Ok(())
    }

    /// One transaction: the rows locked in the order of their `instance_uid`, byte by byte as the
    /// release orders them, then the batch written in one statement. A disconnect older than the
    /// presence stored is not written: the Agent connected again since, written at that connect.
    fn write_presence(
        &self,
        holder: &str,
        presence: &[(InstanceUid, Presence)],
    ) -> Result<(), String> {
        if presence.is_empty() {
            return Ok(());
        }
        let holder = holder.to_string();
        let mut batch: Vec<(String, bool, i64)> = presence
            .iter()
            .map(|(uid, presence)| {
                (
                    uid.to_string(),
                    presence.connected,
                    signed(presence.last_seen_ms),
                )
            })
            .collect();
        batch.sort_by(|(a, ..), (b, ..)| a.cmp(b));
        let written = batch.len();
        self.database.run_writing(written, move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            let uids: Vec<&str> = batch.iter().map(|(uid, ..)| uid.as_str()).collect();
            let connected: Vec<bool> = batch.iter().map(|(_, connected, _)| *connected).collect();
            let last_seen: Vec<i64> = batch.iter().map(|(.., last_seen)| *last_seen).collect();
            transaction
                .execute(
                    "SELECT 1 FROM agents WHERE instance_uid = ANY($1) \
                     ORDER BY instance_uid COLLATE \"C\" FOR UPDATE",
                    &[&uids],
                )
                .await
                .map_err(sql)?;
            transaction
                .execute(
                    "UPDATE agents AS a SET \
                     last_seen_ms = GREATEST(a.last_seen_ms, p.last_seen_ms), \
                     connected = CASE WHEN p.connected THEN true \
                         WHEN (a.holder IS NULL OR a.holder = $4::text::uuid) \
                             AND a.last_seen_ms <= p.last_seen_ms THEN false \
                         ELSE a.connected END, \
                     holder = CASE WHEN p.connected THEN $4::text::uuid \
                         WHEN (a.holder IS NULL OR a.holder = $4::text::uuid) \
                             AND a.last_seen_ms <= p.last_seen_ms THEN NULL \
                         ELSE a.holder END \
                     FROM unnest($1::text[], $2::boolean[], $3::bigint[]) \
                         AS p(instance_uid, connected, last_seen_ms) \
                     WHERE a.instance_uid = p.instance_uid",
                    &[&uids, &connected, &last_seen, &holder],
                )
                .await
                .map_err(sql)?;
            transaction.commit().await.map_err(sql)
        })
    }

    /// One transaction: the row under its lock, its presence read as live only while its holder's
    /// lease is, and the row removed and announced when that lets it go.
    fn remove_if_silent(
        &self,
        uid: &InstanceUid,
        silent_before_ms: u64,
    ) -> Result<Option<bool>, String> {
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
                return Ok(Some(false));
            }
            transaction
                .execute("DELETE FROM agents WHERE instance_uid = $1", &[&key])
                .await
                .map_err(sql)?;
            notify(&*transaction, FORGOTTEN, &key).await?;
            transaction.commit().await.map_err(sql)?;
            Ok(Some(true))
        })?;
        if forgotten == Some(true) {
            self.stored([(uid, None)]);
        }
        Ok(forgotten)
    }

    /// One transaction: the record under its new key, and the old key gone.
    fn rekey(
        &self,
        old: &InstanceUid,
        new: &InstanceUid,
        record: &PersistedAgent,
    ) -> Result<(), String> {
        let (old, new, record) = (*old, *new, record.clone());
        let assignment = record.assignment.clone();
        self.transaction(move |mut client| async move {
            let transaction = client.transaction().await.map_err(sql)?;
            transaction
                .execute(
                    "DELETE FROM agents WHERE instance_uid = $1",
                    &[&old.to_string()],
                )
                .await
                .map_err(sql)?;
            write(&*transaction, &new, &record).await?;
            transaction.commit().await.map_err(sql)
        })?;
        self.stored([(&old, None), (&new, assignment.as_ref())]);
        Ok(())
    }

    /// One transaction: the revisions and the assignments that pin them commit together or not at
    /// all (ADR-0029 clause 21). Each record's assignment is written alone, in the order of the
    /// `instance_uid`, so two releases over the same Agents lock their rows in one order; a record
    /// forgotten meanwhile is written nothing.
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
                super::configs::retain(&*transaction, &revisions).await?;
                let pinned: BTreeSet<&str> = revisions
                    .iter()
                    .map(|pinned| pinned.configuration.as_str())
                    .collect();
                for configuration in pinned {
                    notify(&*transaction, CONFIGURATION, configuration).await?;
                }
                let mut wrote = BTreeSet::new();
                for (key, assignment) in records {
                    if write_assignment(&*transaction, &key, assignment.as_ref()).await? {
                        notify(&*transaction, ASSIGNMENT, &key).await?;
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
            .values()
            .filter(|assignment| pin.pinned_by(assignment))
            .count()
    }

    fn referenced_revisions(&self, name: &str) -> BTreeSet<String> {
        self.assignments
            .lock()
            .expect("assignments lock")
            .values()
            .filter_map(|assignment| assignment.configurations.get(name).cloned())
            .collect()
    }
}

/// The lock the creation of a record is counted under, by the key its schema gives it.
const CREATE_LOCK: &str =
    "hashtextextended('opamp-fleet-server-agents:' || coalesce(current_schema(), ''), 0)";

/// The columns of a row, in the order [`read_all`] selects and [`write`] inserts them.
const COLUMNS: &str = "instance_uid, sequence_num, capabilities, transport, last_seen_ms, \
     restart_pending, effective_config, description, health, remote_config_status, \
     connection_settings_status, package_statuses, available_components, labels, host, \
     deployment, package_agent_type, package_version";

/// A wire-typed field as its protobuf bytes, the encoding whose compatibility rules the Baseline
/// already governs (ADR-0029 clause 4).
fn encode<M: Message>(message: Option<&M>) -> Option<Vec<u8>> {
    message.map(Message::encode_to_vec)
}

fn decode<M: Message + Default>(bytes: Option<Vec<u8>>, what: &str) -> Result<Option<M>, String> {
    bytes
        .map(|bytes| {
            M::decode(bytes.as_slice()).map_err(|e| format!("{what} does not decode: {e}"))
        })
        .transpose()
}

/// Every Agent record. A row that does not decode fails, naming its `instance_uid`
/// (ADR-0029 clause 4).
pub(crate) async fn read_all(
    client: &impl GenericClient,
) -> Result<HashMap<InstanceUid, PersistedAgent>, String> {
    read(client, None).await
}

/// The record of `only`, or of every Agent where it names none.
async fn read(
    client: &impl GenericClient,
    only: Option<&str>,
) -> Result<HashMap<InstanceUid, PersistedAgent>, String> {
    let (filter, params): (&str, Vec<&(dyn tokio_postgres::types::ToSql + Sync)>) = match &only {
        Some(uid) => (" WHERE instance_uid = $1", vec![uid]),
        None => ("", Vec::new()),
    };
    let mut released: HashMap<String, BTreeMap<String, String>> = HashMap::new();
    for row in client
        .query(
            &format!(
                "SELECT instance_uid, configuration, revision FROM agent_assignment_configurations{filter}"
            ),
            &params,
        )
        .await
        .map_err(sql)?
    {
        released
            .entry(row.get(0))
            .or_default()
            .insert(row.get(1), row.get(2));
    }
    let mut records = HashMap::new();
    for row in client
        .query(&format!("SELECT {COLUMNS} FROM agents{filter}"), &params)
        .await
        .map_err(sql)?
    {
        let key: String = row.get(0);
        let named = |e: String| format!("agent {key} in the database: {e}");
        let uid =
            InstanceUid::parse(&key).ok_or_else(|| named("not an Instance UID".to_string()))?;
        let labels: BTreeMap<String, String> =
            serde_json::from_value(row.get(13)).map_err(|e| named(format!("labels: {e}")))?;
        let configurations = released.remove(&key).unwrap_or_default();
        let deployment: Option<String> = row.get(15);
        let package = match (
            row.get::<_, Option<String>>(16),
            row.get::<_, Option<String>>(17),
        ) {
            (Some(agent_type), Some(version)) => Some(
                PackageId::load(&agent_type, &version)
                    .map_err(|e| named(format!("invalid package in the assignment: {e}")))?,
            ),
            (None, None) => None,
            _ => return Err(named("half a package in the assignment".to_string())),
        };
        let assignment = match deployment {
            Some(deployment) => Some(Assignment {
                deployment,
                package,
                configurations,
            }),
            None if package.is_none() && configurations.is_empty() => None,
            None => return Err(named("an assignment without its Deployment".to_string())),
        };
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
    if let Some(key) = released.keys().next() {
        return Err(format!(
            "an assignment in the database names agent {key}, which is not there"
        ));
    }
    Ok(records)
}

/// The columns a report decides (ADR-draft-the-fleet-record-of-agents-and-hosts clause 2): every
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

/// Writes the columns a report decides of one Agent record, inside the caller's transaction,
/// `last_seen_ms` only forward. Answers whether there was a record.
async fn write_reported(
    client: &impl GenericClient,
    uid: &InstanceUid,
    record: &PersistedAgent,
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
    let update = format!("UPDATE agents SET {sets} WHERE instance_uid = $1");
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
    Ok(client
        .execute(
            &update,
            &[
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
            ],
        )
        .await
        .map_err(sql)?
        > 0)
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
/// record's, inside the caller's transaction: what an import and a re-key write.
pub(crate) async fn write(
    client: &impl GenericClient,
    uid: &InstanceUid,
    record: &PersistedAgent,
) -> Result<(), String> {
    let updates = COLUMNS
        .split(", ")
        .skip(1)
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
    let key = uid.to_string();
    let labels = serde_json::to_value(&record.labels).expect("labels serialize");
    let assignment = record.assignment.as_ref();
    let package = assignment.and_then(|assignment| assignment.package.as_ref());
    let written = client
        .execute(
            &format!(
                "INSERT INTO agents ({COLUMNS}) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, \
                 $11, $12, $13, $14, $15, $16, $17, $18) ON CONFLICT (instance_uid) {on_conflict}"
            ),
            &[
                &key,
                &signed(record.sequence_num),
                &signed(record.capabilities),
                &record.transport.as_str(),
                &signed(record.last_seen_ms),
                &record.restart_pending,
                &record.effective_config.as_deref().map(str::as_bytes),
                &encode(record.description.as_ref()),
                &encode(record.health.as_ref()),
                &encode(record.remote_config_status.as_ref()),
                &encode(record.connection_settings_status.as_ref()),
                &encode(record.package_statuses.as_ref()),
                &encode(record.available_components.as_ref()),
                &labels,
                &record.host,
                &assignment.map(|assignment| assignment.deployment.as_str()),
                &package.map(|id| id.agent_type.as_str()),
                &package.map(|id| id.version.as_str()),
            ],
        )
        .await
        .map_err(sql)?
        > 0;
    if written {
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

    /// The port's contract on either store: what is written is loaded back whole, wire types and
    /// all; a record without an assignment stays without one; a re-key moves the record; the
    /// integrity queries follow what is stored; and a rollout's revisions and assignments are
    /// both present.
    fn contract(open: Open<'_>) {
        let (store, configs) = open();
        assert!(store.load().expect("empty").is_empty());
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
        let loaded = open().0.load().expect("load");
        assert!(loaded[&one] == full, "the record round-trips whole");
        assert!(loaded[&two] == bare, "absent stays absent, u64::MAX stays");

        store.rekey(&two, &three, &bare).expect("rekey");
        let loaded = open().0.load().expect("load");
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
        let loaded = reopened.load().expect("load");
        assert!(loaded[&three] == released);
        assert_eq!(configs.load().expect("revisions"), [base], "both present");
        assert_eq!(reopened.pinning(Pin::Configuration("base")), 2);
        assert_eq!(reopened.pinning(Pin::Deployment("stable")), 2);
        assert_eq!(
            reopened.referenced_revisions("base"),
            BTreeSet::from(["0123abcd".to_string(), revision_hash(&pinned)])
        );

        // The acts that decide the labels and a queued restart write them, and one Agent is read
        // as it is stored.
        let labels = BTreeMap::from([("rollout".to_string(), "canary".to_string())]);
        assert!(store.set_labels(&three, &labels).expect("labels"));
        assert_eq!(store.set_restart_pending(&three, false), Ok(Some(true)));
        assert_eq!(store.set_restart_pending(&three, false), Ok(Some(false)));
        assert_eq!(store.set_restart_pending(&three, true), Ok(Some(false)));
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
        assert_eq!(open().0.load().expect("load").len(), 1);
    }

    /// A report's write on PostgreSQL leaves what the acts wrote through another instance: the
    /// labels, a queued restart and the assignment stay as they are stored, while what the report
    /// decides is written; a record that is not there is not written.
    /// Verifies: ADR-draft-the-fleet-record-of-agents-and-hosts, ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
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
        assert_eq!(a.write_report(&uid, &report), Ok(true));
        let stored = a.get(&uid).expect("get").expect("stored");
        assert_eq!(stored.labels, labels, "the labels stay as B set them");
        assert!(stored.restart_pending, "the restart stays queued");
        assert_eq!(stored.assignment, None, "the release stays");
        assert_eq!(stored.health, None, "what the report decides is written");
        assert_eq!(stored.last_seen_ms, report.last_seen_ms);

        // A record forgotten meanwhile is not written again.
        b.remove(&uid).expect("forget");
        assert_eq!(a.write_report(&uid, &report), Ok(false));
        assert_eq!(a.get(&uid), Ok(None), "nothing created");
    }

    /// A release on PostgreSQL writes the assignment of each record alone: the labels, the queued
    /// restart and what a report decided stay as they are stored, whatever the released record
    /// carries of them.
    /// Verifies: ADR-draft-the-fleet-record-of-agents-and-hosts
    #[test]
    fn a_release_on_postgres_writes_the_assignment_alone() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let store = PgAgentStore::new(test.open());
        let uid = InstanceUid::default();
        let stored = record();
        store.put(&uid, &stored).expect("put");
        let assignment = Assignment {
            deployment: "canary".to_string(),
            package: None,
            configurations: BTreeMap::from([("edge".to_string(), "4567cdef".to_string())]),
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

    /// Verifies: ADR-0029
    #[test]
    fn the_agent_port_contract_holds_on_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        contract(&|| {
            (
                Box::new(FsAgentStore::open(dir.path()).expect("open")),
                Box::new(FsConfigBackend::open(dir.path()).expect("open")),
            )
        });
    }

    /// Verifies: ADR-0029
    #[test]
    fn the_agent_port_contract_holds_on_postgres() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        contract(&|| {
            (
                Box::new(PgAgentStore::new(database.clone())),
                Box::new(super::super::configs::PgConfigBackend::new(
                    database.clone(),
                )),
            )
        });
    }

    /// A rollout whose last record cannot be written leaves none of its assignments behind, on
    /// either store, and what the store answers about pins stays as it was (ADR-0029 clause 21):
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

    /// Verifies: ADR-0029
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
    /// Verifies: ADR-0029
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
    /// then the two would deadlock.
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn two_releases_over_the_same_agents_in_opposite_orders_both_commit() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (low, high) = (InstanceUid([1; 16]), InstanceUid([2; 16]));
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
             FOR EACH ROW EXECUTE FUNCTION fleet_test_wait();",
        );
        let sessions = |condition: &str| {
            test.rows(&format!(
                "SELECT 1 FROM pg_stat_activity WHERE pid <> pg_backend_pid() AND {condition}"
            ))
            .len()
        };
        let at_gate = || sessions("wait_event = 'PgSleep' AND query LIKE 'UPDATE agents%'");
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
                || sessions("wait_event_type = 'Lock' AND query LIKE 'UPDATE agents%'") == 1
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
    /// the holder, `last_seen_ms` never moves back, and a disconnect written by an instance that is
    /// not the holder leaves the holder's presence. Forgetting decides from it: an Agent a live
    /// lease holds and that was heard from recently is kept; one whose holder holds no live lease
    /// reads as disconnected and goes, announced; one silent past the bound goes too.
    /// Verifies: ADR-draft-the-fleet-record-of-agents-and-hosts, ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
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
        store
            .write_presence(
                &a,
                &[
                    (two, at(5_000, true)),
                    (one, at(5_000, true)),
                    (three, at(5_000, true)),
                ],
            )
            .expect("batch");
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
            .write_presence(&a, &[(one, at(4_000, true))])
            .expect("an older one");
        assert_eq!(presence(one)[0][2].as_deref(), Some("5000"), "never back");
        let other = "00000000-0000-4000-8000-000000000001";
        store
            .write_presence(other, &[(one, at(6_000, false))])
            .expect("not the holder's");
        assert_eq!(
            presence(one),
            [[Some("true".into()), Some(a.clone()), Some("6000".into())]],
            "the holder's presence stays"
        );

        assert_eq!(
            store.remove_if_silent(&one, 1_000),
            Ok(Some(false)),
            "still reporting"
        );
        assert_eq!(
            store.remove_if_silent(&one, 7_000),
            Ok(Some(true)),
            "silent"
        );
        assert_eq!(store.remove_if_silent(&one, 7_000), Ok(None), "gone");
        test.sql(&format!("DELETE FROM leases WHERE instance_id = '{a}'"));
        assert_eq!(
            store.remove_if_silent(&two, 1_000),
            Ok(Some(true)),
            "a holder without a live lease holds no connection"
        );
        store
            .write_presence(&a, &[(three, at(4_000, false))])
            .expect("a disconnect older than the presence stored");
        assert_eq!(
            presence(three),
            [[Some("true".into()), Some(a.clone()), Some("5000".into())]],
            "a connect written since stays"
        );
        store
            .write_presence(&a, &[(three, at(6_000, false))])
            .expect("the holder's disconnect");
        assert_eq!(
            presence(three),
            [[Some("false".into()), None, Some("6000".into())]]
        );
        assert_eq!(
            store.remove_if_silent(&three, 1_000),
            Ok(Some(true)),
            "disconnected"
        );
        assert!(store.load().expect("load").is_empty());
    }

    /// Two presence batches over the same two Agents, given them in opposite orders, both commit:
    /// each locks the rows in the order of their `instance_uid` before it writes them, as the
    /// gated release test above shows for a release.
    /// Verifies: ADR-draft-the-fleet-record-of-agents-and-hosts
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
        let sessions = |condition: &str| {
            test.rows(&format!(
                "SELECT 1 FROM pg_stat_activity WHERE pid <> pg_backend_pid() AND {condition}"
            ))
            .len()
        };
        let at_gate = || sessions("wait_event = 'PgSleep'");
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
            at_gate() == 2 || sessions("wait_event_type = 'Lock'") == 1
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
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
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

    /// A row that does not decode fails startup naming its `instance_uid` (ADR-0029 clause 4).
    /// Verifies: ADR-0029
    #[test]
    fn an_agent_row_that_does_not_decode_fails_the_load_naming_it() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let store = PgAgentStore::new(test.open());
        let uid = InstanceUid::default();
        store.put(&uid, &record()).expect("put");
        test.sql("UPDATE agents SET health = '\\xff'::bytea");
        let err = store.load().expect_err("not protobuf");
        assert!(err.contains(&uid.to_string()), "{err}");
        assert!(err.contains("health"), "{err}");
    }
}
