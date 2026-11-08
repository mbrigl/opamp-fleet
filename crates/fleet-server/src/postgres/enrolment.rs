//! The enrolment window and its queue in PostgreSQL (ADR-0023 clause 8): the PostgreSQL adapter
//! behind [`EnrolmentStore`](crate::enrolment::EnrolmentStore) — the window's end in one row, each
//! request in a row of its own. Each act is one transaction on the rows it names, written before
//! it is answered; taking a request in holds the queue against every other taker while it counts,
//! so the queue's bounds hold for every instance on the database
//! (ADR-0025
//! clause 19).

use std::sync::Arc;

use super::Cached as GenericClient;
use serde_json::Value;
use tokio_postgres::error::Severity;

use deadpool_postgres::Object;

use super::notify::{notify, ENROLMENT, WINDOW};
use super::{client, signed, sql, unsigned, Database};
use crate::enrolment::{
    Approval, Decided, Decision, DecisionError, EnrolmentStore, Persisted, PersistedRequest,
    Registrar, Signing, Took,
};
use crate::revocation::CertId;

/// A connection inside a transaction this adapter began, held across the calls that do the
/// transaction's work. Dropped before it is let go, it is closed rather than returned to the pool,
/// so the database rolls the transaction back and no other caller is handed a connection inside
/// one.
struct Held(Option<Object>);

impl Held {
    fn client(&self) -> &Object {
        self.0.as_ref().expect("held until let go")
    }

    /// The connection, back to the pool once the transaction has ended.
    fn let_go(mut self) -> Object {
        self.0.take().expect("held until let go")
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Some(object) = self.0.take() {
            drop(Object::take(object));
        }
    }
}

/// What the held request was found to be.
enum Found {
    NotWaiting,
    Revoked,
    Waiting(Value),
    /// Held by another instance, which decides it.
    Deciding,
}

/// Whether `error` ends a wait for the request's row that outlasted the session's `lock_timeout`:
/// another instance holds the request while it decides it, signing it perhaps, and how it decides
/// is not known yet.
fn held_elsewhere(error: &tokio_postgres::Error) -> bool {
    error.code() == Some(&tokio_postgres::error::SqlState::LOCK_NOT_AVAILABLE)
}

/// How the transaction that decides an approved request ended.
enum Ended {
    /// Committed, leaving the revocation list's version where the register entry written with
    /// it moved it, if one was.
    Committed(Option<u64>),
    /// The register refused the entry; the transaction was rolled back.
    Refused(String),
    /// The database server answered an error at the commit or at the statement before it: the
    /// transaction was rolled back, for certain.
    NotCommitted(String),
}

/// The statement of the decision that failed.
#[derive(Clone, Copy, Debug)]
enum At {
    /// The UPDATE that writes the decision: nothing is committed when it fails.
    Update,
    /// The COMMIT.
    Commit,
}

/// Whether an error of the decision's statement `at` ended the transaction without its decision,
/// for certain, given what the database server answered: `None` when it answered nothing, else
/// the severity it gave, where it gave one. At the UPDATE any answer of the server is certain.
/// At the COMMIT only an `ERROR` is: a `FATAL` or a `PANIC` may end a backend whose commit is
/// already durable, as a terminate during the wait for a synchronous standby does, so it is read
/// back as a lost connection is.
fn rolled_back_for_certain(server: Option<Option<Severity>>, at: At) -> bool {
    match (server, at) {
        (None, _) => false,
        (Some(_), At::Update) => true,
        (Some(severity), At::Commit) => severity == Some(Severity::Error),
    }
}

/// What an error of the decision's own statement `at` comes to: one that ended the transaction
/// without its decision for certain ([`rolled_back_for_certain`]) is a refusal; any other — a
/// lost connection, a cancelled call, a `FATAL` at the commit — leaves the outcome to be read
/// back, and is the call's error.
fn refused_by_the_server(error: tokio_postgres::Error, at: At) -> Result<Ended, String> {
    let server = error
        .as_db_error()
        .map(tokio_postgres::error::DbError::parsed_severity);
    if rolled_back_for_certain(server, at) {
        Ok(Ended::NotCommitted(sql(error)))
    } else {
        Err(sql(error))
    }
}

/// The window in `enrolment_window`, its requests in `enrolment_requests`, the rejected keys in
/// `enrolment_rejected`.
pub struct PgEnrolmentStore {
    database: Arc<Database>,
}

impl PgEnrolmentStore {
    /// The adapter on `database`.
    #[must_use]
    pub fn new(database: Arc<Database>) -> Self {
        PgEnrolmentStore { database }
    }
}

impl EnrolmentStore for PgEnrolmentStore {
    fn load(&self) -> Result<Option<Persisted>, String> {
        self.database.run(|pool| async move {
            let client = client(&pool).await?;
            read(&client).await
        })
    }

    fn save(&self, persisted: &Persisted) -> Result<(), String> {
        let persisted = persisted.clone();
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            write(&transaction, &persisted).await?;
            notify(&transaction, WINDOW, "").await?;
            transaction.commit().await.map_err(sql)
        })
    }

    /// The window's row is held for share and the queue against every other taker and every
    /// rejection while the key is looked up among the rejected and the request is counted and
    /// written; a decision or a withdrawal waits, and a reader does not.
    fn take(
        &self,
        request: &PersistedRequest,
        now_ms: u64,
        max: usize,
        max_per_bootstrap: usize,
    ) -> Result<Took, String> {
        let document = serde_json::to_value(request).expect("a request serializes");
        let id = document["id"].as_str().unwrap_or_default().to_string();
        let bootstrap = document["requester"]["bootstrap"].clone();
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            let until = transaction
                .query_opt("SELECT until_ms FROM enrolment_window FOR SHARE", &[])
                .await
                .map_err(sql)?
                .and_then(|row| row.get::<_, Option<i64>>(0))
                .map(unsigned);
            if until.is_none_or(|until| now_ms >= until) {
                return Ok(Took::Closed);
            }
            transaction
                .batch_execute("LOCK TABLE enrolment_requests IN SHARE ROW EXCLUSIVE MODE")
                .await
                .map_err(sql)?;
            if transaction
                .query_opt("SELECT 1 FROM enrolment_rejected WHERE key = $1", &[&id])
                .await
                .map_err(sql)?
                .is_some()
            {
                return Ok(Took::Rejected);
            }
            if let Some(row) = transaction
                .query_opt(
                    "SELECT request FROM enrolment_requests WHERE id = $1",
                    &[&id],
                )
                .await
                .map_err(sql)?
            {
                let held = serde_json::from_value(row.get::<_, Value>(0))
                    .map_err(|e| format!("enrolment request {id:?} does not decode: {e}"))?;
                return Ok(Took::Held(Box::new(held)));
            }
            let counts = transaction
                .query_one(
                    "SELECT count(*), count(*) FILTER (WHERE (request -> 'requester' -> \
                     'bootstrap')::jsonb = $1::jsonb), coalesce(max(position) + 1, 0) \
                     FROM enrolment_requests",
                    &[&bootstrap],
                )
                .await
                .map_err(sql)?;
            let (held, share, position): (i64, i64, i32) =
                (counts.get(0), counts.get(1), counts.get(2));
            if usize::try_from(held).unwrap_or(usize::MAX) >= max {
                return Ok(Took::Full);
            }
            if usize::try_from(share).unwrap_or(usize::MAX) >= max_per_bootstrap {
                return Ok(Took::OverShare);
            }
            transaction
                .execute(
                    "INSERT INTO enrolment_requests (id, position, request) VALUES ($1, $2, $3)",
                    &[&id, &position, &document],
                )
                .await
                .map_err(sql)?;
            notify(&transaction, ENROLMENT, &id).await?;
            transaction.commit().await.map_err(sql)?;
            Ok(Took::Added)
        })
    }

    /// The request's row is held while it is read and written, so two instances deciding one
    /// request decide it once. A wait for the row that outlasts the session's `lock_timeout`, as
    /// behind an approval that signs, is answered [`Decided::Deciding`].
    fn decide(&self, id: &str, decision: &Decision) -> Result<Decided, String> {
        let id = id.to_string();
        let decision = serde_json::to_value(decision).expect("a decision serializes");
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            let row = match transaction
                .query_opt(
                    "SELECT request FROM enrolment_requests WHERE id = $1 FOR UPDATE",
                    &[&id],
                )
                .await
            {
                Ok(row) => row,
                Err(e) if held_elsewhere(&e) => return Ok(Decided::Deciding),
                Err(e) => return Err(sql(e)),
            };
            let Some(row) = row else {
                return Ok(Decided::NotWaiting);
            };
            let mut request: Value = row.get(0);
            if request.get("decision").is_some_and(|d| !d.is_null()) {
                return Ok(Decided::NotWaiting);
            }
            request["decision"] = decision;
            transaction
                .execute(
                    "UPDATE enrolment_requests SET request = $2 WHERE id = $1",
                    &[&id, &request],
                )
                .await
                .map_err(sql)?;
            notify(&transaction, ENROLMENT, &id).await?;
            transaction.commit().await.map_err(sql)?;
            Ok(Decided::Written)
        })
    }

    /// The queue is locked as [`take`](EnrolmentStore::take) locks it, so every take after the
    /// commit finds the key rejected and none lands between the look and the write. The request's
    /// row is then held without a wait: an approval holding it would need the queue's lock for its
    /// own update, so a wait here would deadlock with it, and such a request is answered
    /// [`Decided::Deciding`] at once. A wait for the queue's lock past the session's
    /// `lock_timeout` is answered so too.
    fn reject(&self, id: &str, rejected_ms: u64) -> Result<Decided, String> {
        let id = id.to_string();
        let rejected = serde_json::to_value(Decision::Rejected).expect("a decision serializes");
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            match transaction
                .batch_execute("LOCK TABLE enrolment_requests IN SHARE ROW EXCLUSIVE MODE")
                .await
            {
                Ok(()) => {}
                Err(e) if held_elsewhere(&e) => return Ok(Decided::Deciding),
                Err(e) => return Err(sql(e)),
            }
            let row = match transaction
                .query_opt(
                    "SELECT request FROM enrolment_requests WHERE id = $1 FOR UPDATE NOWAIT",
                    &[&id],
                )
                .await
            {
                Ok(row) => row,
                Err(e) if held_elsewhere(&e) => return Ok(Decided::Deciding),
                Err(e) => return Err(sql(e)),
            };
            let decided = match row.map(|row| row.get::<_, Value>(0)) {
                Some(request) if request.get("decision").is_some_and(|d| !d.is_null()) => {
                    return Ok(Decided::NotWaiting);
                }
                Some(mut request) => {
                    request["decision"] = rejected;
                    transaction
                        .execute(
                            "UPDATE enrolment_requests SET request = $2 WHERE id = $1",
                            &[&id, &request],
                        )
                        .await
                        .map_err(sql)?;
                    Decided::Written
                }
                None => Decided::Listed,
            };
            transaction
                .execute(
                    "INSERT INTO enrolment_rejected (key, rejected_ms) VALUES ($1, $2) \
                     ON CONFLICT (key) DO NOTHING",
                    &[&id, &signed(rejected_ms)],
                )
                .await
                .map_err(sql)?;
            notify(&transaction, ENROLMENT, &id).await?;
            transaction.commit().await.map_err(sql)?;
            Ok(decided)
        })
    }

    /// The request's row is held from before it is signed until its decision is written: of two
    /// instances approving one request, the second waits for the first and then finds it decided,
    /// signing nothing. A wait for the row that outlasts the session's `lock_timeout` is answered
    /// [`Approval::Deciding`], signing nothing. A key found rejected once the row is held is
    /// answered [`Approval::NotWaiting`], signing nothing: a rejection holds the row before it
    /// writes the key, so none lands while the approval holds it. The bootstrap certificate is
    /// looked up in the list once the row is held; the register's write looks again, under its hold
    /// of the list. The register entry and the Host the signature prepared are written in the same
    /// transaction as the decision, and handed to `registrar` once it committed
    /// (ADR-0023 clause 9): a commit that fails leaves neither, and a refusal of the register rolls
    /// the decision back.
    fn approve(
        &self,
        id: &str,
        bootstrap: &CertId,
        sign: &mut dyn FnMut() -> Result<Signing, DecisionError>,
        registrar: &dyn Registrar,
    ) -> Result<Approval, String> {
        let request_id = id.to_string();
        let bootstrap = bootstrap.clone();
        let (held, found) = self.database.run(move |pool| async move {
            let held = Held(Some(client(&pool).await?));
            held.client().batch_execute("BEGIN").await.map_err(sql)?;
            let row = match held
                .client()
                .query_opt(
                    "SELECT request FROM enrolment_requests WHERE id = $1 FOR UPDATE",
                    &[&request_id],
                )
                .await
            {
                Ok(row) => row,
                Err(e) if held_elsewhere(&e) => return Ok((held, Found::Deciding)),
                Err(e) => return Err(sql(e)),
            };
            let rejected = held
                .client()
                .query_opt(
                    "SELECT 1 FROM enrolment_rejected WHERE key = $1",
                    &[&request_id],
                )
                .await
                .map_err(sql)?
                .is_some();
            let found = match row.map(|row| row.get::<_, Value>(0)) {
                // A rejected key is never signed, though an instance of 0.8.0 queued it.
                _ if rejected => Found::NotWaiting,
                Some(request) if request.get("decision").is_none_or(Value::is_null) => {
                    if super::ledger::names(&***held.client(), &bootstrap).await? {
                        held.client()
                            .execute(
                                "DELETE FROM enrolment_requests WHERE id = $1",
                                &[&request_id],
                            )
                            .await
                            .map_err(sql)?;
                        // Announced by the call that commits it, below, so a commit whose answer
                        // is lost is read again.
                        Found::Revoked
                    } else {
                        Found::Waiting(request)
                    }
                }
                _ => Found::NotWaiting,
            };
            Ok((held, found))
        })?;
        let (finish, answer) = match found {
            Found::NotWaiting => ("ROLLBACK", Ok(Approval::NotWaiting)),
            Found::Deciding => ("ROLLBACK", Ok(Approval::Deciding)),
            Found::Revoked => ("COMMIT", Ok(Approval::Revoked)),
            Found::Waiting(mut request) => match sign() {
                Err(e) => ("ROLLBACK", Ok(Approval::Refused(e))),
                Ok((decision, approved)) => {
                    request["decision"] =
                        serde_json::to_value(&decision).expect("a decision serializes");
                    let id = id.to_string();
                    let registering = approved.clone();
                    let ended = self.database.run(move |_| async move {
                        let mut version = None;
                        if let Some(approved) = &registering {
                            let registered = super::ledger::register_in(
                                &***held.client(),
                                approved.issued(),
                                &approved.bounds,
                                approved.mint(),
                                Some(&approved.bootstrap),
                            )
                            .await?;
                            if let Err(refused) = crate::revocation::refusal(registered) {
                                held.client().batch_execute("ROLLBACK").await.map_err(sql)?;
                                drop(held.let_go());
                                return Ok(Ended::Refused(refused));
                            }
                            version =
                                Some(super::ledger::read_list_version(&***held.client()).await?);
                        }
                        if let Err(e) = held
                            .client()
                            .execute(
                                "UPDATE enrolment_requests SET request = $2 WHERE id = $1",
                                &[&id, &request],
                            )
                            .await
                        {
                            return refused_by_the_server(e, At::Update);
                        }
                        notify(&***held.client(), ENROLMENT, &id).await?;
                        if let Err(e) = held.client().batch_execute("COMMIT").await {
                            return refused_by_the_server(e, At::Commit);
                        }
                        drop(held.let_go());
                        Ok(Ended::Committed(version))
                    })?;
                    let written = match ended {
                        Ended::Committed(written) => written,
                        Ended::NotCommitted(e) => return Ok(Approval::NotCommitted(e)),
                        Ended::Refused(refused) => {
                            return Ok(Approval::Refused(
                                if refused == crate::revocation::BOOTSTRAP_REVOKED {
                                    DecisionError::Revoked
                                } else {
                                    DecisionError::Sign(format!(
                                        "cannot record the issued certificate: {refused}"
                                    ))
                                },
                            ));
                        }
                    };
                    if let Some(approved) = &approved {
                        registrar.committed(approved, written);
                    }
                    return Ok(Approval::Approved(decision));
                }
            },
        };
        let id = id.to_string();
        self.database.run(move |_| async move {
            if finish == "COMMIT" {
                notify(&***held.client(), ENROLMENT, &id).await?;
            }
            held.client().batch_execute(finish).await.map_err(sql)?;
            drop(held.let_go());
            answer
        })
    }

    fn registers_with_its_decision(&self) -> bool {
        true
    }

    /// Read under a share lock on the request's row, so it waits until a transaction that holds
    /// the row, the approval whose outcome is not known among them, has ended, and answers what
    /// it left (ADR-0023 clause 9). A wait past the session's `lock_timeout` is an error.
    fn read_back(&self, id: &str) -> Result<Option<PersistedRequest>, String> {
        let id = id.to_string();
        self.database.run(move |pool| async move {
            let client = client(&pool).await?;
            client
                .query_opt(
                    "SELECT request FROM enrolment_requests WHERE id = $1 FOR SHARE",
                    &[&id],
                )
                .await
                .map_err(sql)?
                .map(|row| {
                    serde_json::from_value(row.get::<_, Value>(0))
                        .map_err(|e| format!("enrolment request {id:?} does not decode: {e}"))
                })
                .transpose()
        })
    }

    fn take_back(&self, id: &str) -> Result<(), String> {
        let id = id.to_string();
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            let taken = transaction
                .execute(
                    "UPDATE enrolment_requests SET request = (request::jsonb - 'decision')::json \
                     WHERE id = $1 AND request -> 'decision' IS NOT NULL",
                    &[&id],
                )
                .await
                .map_err(sql)?;
            if taken > 0 {
                notify(&transaction, ENROLMENT, &id).await?;
            }
            transaction.commit().await.map_err(sql)
        })
    }

    fn withdraw_bootstrap(&self, issuers: &[String], serial: &str) -> Result<usize, String> {
        let issuers = issuers.to_vec();
        let serial = serial.to_string();
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            let withdrawn = transaction
                .execute(
                    "DELETE FROM enrolment_requests WHERE request -> 'decision' IS NULL \
                     AND request -> 'requester' -> 'bootstrap' ->> 'serial' = $2 \
                     AND request -> 'requester' -> 'bootstrap' ->> 'issuer' = ANY($1)",
                    &[&issuers, &serial],
                )
                .await
                .map_err(sql)?;
            if withdrawn > 0 {
                notify(&transaction, ENROLMENT, &serial).await?;
            }
            transaction.commit().await.map_err(sql)?;
            Ok(usize::try_from(withdrawn).unwrap_or(usize::MAX))
        })
    }

    fn withdraw(&self, ids: &[String]) -> Result<(), String> {
        let ids = ids.to_vec();
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            let withdrawn = transaction
                .execute(
                    "DELETE FROM enrolment_requests WHERE id = ANY($1) \
                     AND request -> 'decision' IS NULL",
                    &[&ids],
                )
                .await
                .map_err(sql)?;
            if withdrawn > 0 {
                notify(&transaction, ENROLMENT, "").await?;
            }
            transaction.commit().await.map_err(sql)
        })
    }

    fn open_window(&self, until_ms: u64) -> Result<(), String> {
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            transaction
                .execute(
                    "INSERT INTO enrolment_window (singleton, until_ms) VALUES (true, $1) \
                     ON CONFLICT (singleton) DO UPDATE SET until_ms = EXCLUDED.until_ms",
                    &[&signed(until_ms)],
                )
                .await
                .map_err(sql)?;
            notify(&transaction, WINDOW, "").await?;
            transaction.commit().await.map_err(sql)
        })
    }

    fn close_window(&self) -> Result<(), String> {
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            transaction
                .execute(
                    "INSERT INTO enrolment_window (singleton, until_ms) VALUES (true, NULL) \
                     ON CONFLICT (singleton) DO UPDATE SET until_ms = NULL",
                    &[],
                )
                .await
                .map_err(sql)?;
            transaction
                .execute("DELETE FROM enrolment_requests", &[])
                .await
                .map_err(sql)?;
            notify(&transaction, WINDOW, "").await?;
            transaction.commit().await.map_err(sql)
        })
    }

    fn end_window(&self, until_ms: u64) -> Result<bool, String> {
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            let ended = transaction
                .execute(
                    "UPDATE enrolment_window SET until_ms = NULL WHERE until_ms = $1",
                    &[&signed(until_ms)],
                )
                .await
                .map_err(sql)?;
            if ended > 0 {
                transaction
                    .execute("DELETE FROM enrolment_requests", &[])
                    .await
                    .map_err(sql)?;
                notify(&transaction, WINDOW, "").await?;
            }
            transaction.commit().await.map_err(sql)?;
            Ok(ended > 0)
        })
    }
}

/// What was saved last, `None` when nothing was: neither a window nor a rejected key. A request
/// that does not decode fails, naming it.
pub(crate) async fn read(client: &impl GenericClient) -> Result<Option<Persisted>, String> {
    let window = client
        .query_opt("SELECT until_ms FROM enrolment_window", &[])
        .await
        .map_err(sql)?;
    let mut rejected = serde_json::Map::new();
    for row in client
        .query("SELECT key, rejected_ms FROM enrolment_rejected", &[])
        .await
        .map_err(sql)?
    {
        rejected.insert(row.get(0), unsigned(row.get(1)).into());
    }
    let Some(window) = window else {
        if rejected.is_empty() {
            return Ok(None);
        }
        return serde_json::from_value(serde_json::json!({ "rejected": rejected }))
            .map(Some)
            .map_err(|e| {
                format!("the rejected enrolment keys in the database do not decode: {e}")
            });
    };
    let mut requests = Vec::new();
    for row in client
        .query(
            "SELECT id, request FROM enrolment_requests ORDER BY position, id",
            &[],
        )
        .await
        .map_err(sql)?
    {
        requests.push(row.get::<_, Value>(1));
    }
    let mut document = serde_json::Map::new();
    if let Some(until) = window.get::<_, Option<i64>>(0) {
        document.insert("window_until_ms".to_string(), unsigned(until).into());
    }
    document.insert("requests".to_string(), Value::Array(requests));
    if !rejected.is_empty() {
        document.insert("rejected".to_string(), Value::Object(rejected));
    }
    serde_json::from_value(Value::Object(document))
        .map(Some)
        .map_err(|e| format!("the enrolment queue in the database does not decode: {e}"))
}

/// Replaces the window, its requests and the rejected keys inside the caller's transaction.
pub(crate) async fn write(
    client: &impl GenericClient,
    persisted: &Persisted,
) -> Result<(), String> {
    let document = serde_json::to_value(persisted).expect("the enrolment serializes");
    let until = document
        .get("window_until_ms")
        .and_then(Value::as_u64)
        .map(signed);
    client
        .execute(
            "INSERT INTO enrolment_window (singleton, until_ms) VALUES (true, $1) \
             ON CONFLICT (singleton) DO UPDATE SET until_ms = EXCLUDED.until_ms",
            &[&until],
        )
        .await
        .map_err(sql)?;
    client
        .execute("DELETE FROM enrolment_requests", &[])
        .await
        .map_err(sql)?;
    let requests = document
        .get("requests")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for (position, request) in requests.iter().enumerate() {
        let id = request
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        client
            .execute(
                "INSERT INTO enrolment_requests (id, position, request) VALUES ($1, $2, $3)",
                &[
                    &id,
                    &i32::try_from(position).expect("the queue is bounded"),
                    request,
                ],
            )
            .await
            .map_err(sql)?;
    }
    client
        .execute("DELETE FROM enrolment_rejected", &[])
        .await
        .map_err(sql)?;
    for (key, rejected_ms) in &persisted.rejected {
        client
            .execute(
                "INSERT INTO enrolment_rejected (key, rejected_ms) VALUES ($1, $2)",
                &[key, &signed(*rejected_ms)],
            )
            .await
            .map_err(sql)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrolment::{Decided, Decision, PersistedRequest, Took};
    use crate::fs::FsEnrolmentStore;
    use crate::postgres::testing::TestDatabase;

    fn persisted(until: Option<u64>, ids: &[&str]) -> Persisted {
        let requests: Vec<Value> = ids
            .iter()
            .map(|id| {
                // A host chooses its subject: a NUL is kept, and fails no write.
                let mut requester = crate::enrolment::tests::requester();
                requester.bootstrap_subject = "CN=edge\0".to_string();
                serde_json::json!({
                    "id": id,
                    "arrived_ms": 5,
                    "key_fingerprint": id,
                    "requester": serde_json::to_value(requester).expect("requester"),
                    "csr_pem": "-----BEGIN CERTIFICATE REQUEST-----",
                    "instance_uid": "00",
                    "decision": {"approved": "-----BEGIN CERTIFICATE-----"},
                })
            })
            .collect();
        let mut document = serde_json::json!({ "requests": requests });
        if let Some(until) = until {
            document["window_until_ms"] = until.into();
        }
        serde_json::from_value(document).expect("a persisted queue")
    }

    /// The port's contract on either store: nothing saved is `None`; what is saved is loaded back,
    /// the window's end and every request in order; a save replaces the whole.
    fn contract(open: &dyn Fn() -> Box<dyn EnrolmentStore>) {
        let store = open();
        assert_eq!(store.load().expect("nothing"), None);
        let open_window = persisted(Some(u64::MAX - 7), &["b", "a", "c"]);
        store.save(&open_window).expect("save");
        assert_eq!(open().load().expect("load"), Some(open_window));
        let closed = persisted(None, &["a"]);
        store.save(&closed).expect("save");
        assert_eq!(open().load().expect("load"), Some(closed));
        store.save(&Persisted::default()).expect("save");
        assert_eq!(open().load().expect("load"), Some(Persisted::default()));
    }

    /// One request as it persists, of the bootstrap certificate with `serial`, waiting.
    fn request(id: &str, serial: &str) -> PersistedRequest {
        let mut requester = crate::enrolment::tests::requester();
        requester.bootstrap = crate::revocation::CertId::new(b"bootstrap CA", serial);
        serde_json::from_value(serde_json::json!({
            "id": id,
            "arrived_ms": 5,
            "key_fingerprint": id,
            "requester": serde_json::to_value(requester).expect("requester"),
            "csr_pem": "-----BEGIN CERTIFICATE REQUEST-----",
            "instance_uid": "00",
        }))
        .expect("a persisted request")
    }

    /// The acts' contract on either store: a request is taken in only while a window is open and
    /// within both bounds, a request held is answered as it stands, a decision is made once and
    /// only on a request that waits, a withdrawal keeps a decided request, and a window ends only
    /// at the end it was given.
    fn acts(open: &dyn Fn() -> Box<dyn EnrolmentStore>) {
        let store = open();
        let now = 1_000;
        assert_eq!(store.take(&request("a", "1"), now, 3, 2), Ok(Took::Closed));
        store.open_window(now + 100).expect("open");
        assert_eq!(
            store.take(&request("a", "1"), now + 100, 3, 2),
            Ok(Took::Closed),
            "ended"
        );
        assert_eq!(store.take(&request("a", "1"), now, 3, 2), Ok(Took::Added));
        assert_eq!(store.take(&request("b", "1"), now, 3, 2), Ok(Took::Added));
        assert_eq!(
            store.take(&request("c", "1"), now, 3, 2),
            Ok(Took::OverShare),
            "the bootstrap certificate holds its share"
        );
        assert_eq!(store.take(&request("c", "2"), now, 3, 2), Ok(Took::Added));
        assert_eq!(store.take(&request("d", "3"), now, 3, 2), Ok(Took::Full));
        assert_eq!(
            store.take(&request("a", "1"), now, 3, 2),
            Ok(Took::Held(Box::new(request("a", "1")))),
            "a request held counts nothing"
        );

        let approved = Decision::Approved("-----BEGIN CERTIFICATE-----".to_string());
        assert_eq!(store.decide("a", &approved), Ok(Decided::Written));
        assert_eq!(
            store.decide("a", &Decision::Rejected),
            Ok(Decided::NotWaiting),
            "decided once"
        );
        assert_eq!(
            store.decide("none", &Decision::Rejected),
            Ok(Decided::NotWaiting)
        );
        let mut decided = request("a", "1");
        decided.decision = Some(approved);
        assert_eq!(
            open().take(&request("a", "1"), now, 3, 2),
            Ok(Took::Held(Box::new(decided)))
        );

        store
            .withdraw(&["a".to_string(), "b".to_string()])
            .expect("withdraw");
        let held: Vec<String> = open()
            .load()
            .expect("load")
            .expect("saved")
            .requests
            .iter()
            .map(|r| r.id.clone())
            .collect();
        assert_eq!(held, ["a", "c"], "a decided request is kept");

        assert_eq!(
            store.end_window(now + 50),
            Ok(false),
            "moved by another act"
        );
        assert!(open()
            .load()
            .expect("load")
            .expect("saved")
            .window_until_ms
            .is_some());
        assert_eq!(store.end_window(now + 100), Ok(true));
        assert_eq!(open().load().expect("load"), Some(Persisted::default()));
        store.open_window(now + 100).expect("open");
        store.take(&request("e", "1"), now, 3, 2).expect("take");
        store.close_window().expect("close");
        assert_eq!(open().load().expect("load"), Some(Persisted::default()));
    }

    /// Verifies: ADR-0023
    #[test]
    fn the_enrolment_acts_hold_on_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        acts(&|| Box::new(FsEnrolmentStore::open(dir.path())));
    }

    /// Verifies: ADR-0025
    #[test]
    fn the_enrolment_acts_hold_on_postgres() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        acts(&|| Box::new(PgEnrolmentStore::new(database.clone())));
    }

    /// Two instances take a request each into a queue with one place left, behind a barrier, and
    /// exactly one is taken while the other finds the queue full; two deciding one request decide
    /// it once.
    /// Verifies: ADR-0025
    #[test]
    fn two_takers_at_the_queues_bound_take_one_request_between_them() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let stores = [
            PgEnrolmentStore::new(database.clone()),
            PgEnrolmentStore::new(database.clone()),
        ];
        stores[0].open_window(u64::MAX / 4).expect("open");
        const ROUNDS: usize = 20;
        for round in 0..ROUNDS {
            let barrier = std::sync::Barrier::new(2);
            let answers: Vec<Took> = std::thread::scope(|scope| {
                let racers: Vec<_> = stores
                    .iter()
                    .enumerate()
                    .map(|(racer, store)| {
                        let barrier = &barrier;
                        scope.spawn(move || {
                            let request =
                                request(&format!("q{round}-{racer}"), &format!("{racer}"));
                            barrier.wait();
                            store
                                .take(&request, 1_000, round + 1, ROUNDS)
                                .expect("take")
                        })
                    })
                    .collect();
                racers
                    .into_iter()
                    .map(|r| r.join().expect("racer"))
                    .collect()
            });
            let taken = answers.iter().filter(|a| **a == Took::Added).count();
            let full = answers.iter().filter(|a| **a == Took::Full).count();
            assert_eq!((taken, full), (1, 1), "round {round}: {answers:?}");
        }
        let held = stores[0].load().expect("load").expect("saved").requests;
        assert_eq!(held.len(), ROUNDS);

        for request in &held {
            let barrier = std::sync::Barrier::new(2);
            let id = request.id.clone();
            let decided: Vec<bool> = std::thread::scope(|scope| {
                let racers: Vec<_> = stores
                    .iter()
                    .map(|store| {
                        let (barrier, id) = (&barrier, id.clone());
                        scope.spawn(move || {
                            barrier.wait();
                            store.decide(&id, &Decision::Rejected).expect("decide")
                                == Decided::Written
                        })
                    })
                    .collect();
                racers
                    .into_iter()
                    .map(|r| r.join().expect("racer"))
                    .collect()
            });
            assert_eq!(
                decided.iter().filter(|d| **d).count(),
                1,
                "{id}: {decided:?}"
            );
        }
    }

    fn bootstrap(serial: &str) -> CertId {
        CertId::new(b"bootstrap CA", serial)
    }

    /// The approvals' contract on either store: a waiting request is signed once and approved
    /// with what was signed, a request no longer waiting signs nothing, a refused signature leaves
    /// the request waiting, withdrawing a bootstrap certificate's requests keeps a decided one, and
    /// a decision taken back leaves its request waiting.
    fn approvals(open: &dyn Fn() -> Box<dyn EnrolmentStore>) {
        let store = open();
        store.open_window(10_000).expect("open");
        for (id, serial) in [("a", "1"), ("b", "2"), ("c", "1")] {
            store.take(&request(id, serial), 1_000, 8, 8).expect("take");
        }
        let signed = std::cell::Cell::new(0);
        let approved = Decision::Approved("-----BEGIN CERTIFICATE-----".to_string());
        let mut sign = || {
            signed.set(signed.get() + 1);
            Ok((approved.clone(), None))
        };
        assert_eq!(
            store.approve(
                "a",
                &bootstrap("1"),
                &mut sign,
                &crate::enrolment::tests::kept()
            ),
            Ok(Approval::Approved(approved.clone()))
        );
        assert_eq!(
            store.approve(
                "a",
                &bootstrap("1"),
                &mut sign,
                &crate::enrolment::tests::kept()
            ),
            Ok(Approval::NotWaiting)
        );
        assert_eq!(
            store.approve(
                "none",
                &bootstrap("1"),
                &mut sign,
                &crate::enrolment::tests::kept()
            ),
            Ok(Approval::NotWaiting)
        );
        assert_eq!(signed.get(), 1, "signed once");
        let mut refuse = || Err(DecisionError::Sign("refused".to_string()));
        assert_eq!(
            store.approve(
                "b",
                &bootstrap("2"),
                &mut refuse,
                &crate::enrolment::tests::kept()
            ),
            Ok(Approval::Refused(DecisionError::Sign(
                "refused".to_string()
            )))
        );
        let decisions = |store: &dyn EnrolmentStore| -> Vec<(String, bool)> {
            store
                .load()
                .expect("load")
                .expect("saved")
                .requests
                .iter()
                .map(|r| (r.id.clone(), r.decision.is_some()))
                .collect()
        };
        assert_eq!(
            decisions(&*open()),
            [("a".into(), true), ("b".into(), false), ("c".into(), false)]
        );
        let issuer = bootstrap("1").issuer;
        assert_eq!(store.withdraw_bootstrap(&[issuer], "1"), Ok(1));
        assert_eq!(
            decisions(&*open()),
            [("a".into(), true), ("b".into(), false)],
            "the decided request of that bootstrap certificate is kept"
        );
        store.take_back("a").expect("take back");
        store.take_back("none").expect("nothing to take back");
        assert_eq!(
            decisions(&*open()),
            [("a".into(), false), ("b".into(), false)],
            "a decision taken back waits again"
        );
    }

    /// Verifies: ADR-0023
    #[test]
    fn the_enrolment_approvals_hold_on_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        approvals(&|| Box::new(FsEnrolmentStore::open(dir.path())));
    }

    /// Verifies: ADR-0023
    #[test]
    fn the_enrolment_approvals_hold_on_postgres() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        approvals(&|| Box::new(PgEnrolmentStore::new(database.clone())));
    }

    /// Of two instances approving one request, one signs and records it and the other finds it
    /// decided, signing nothing: the request's row is held from before the signature until the
    /// decision is written. The other finds it still being decided instead when it waited out
    /// its `lock_timeout`, as on a database slowed by load.
    /// Verifies: ADR-0025
    #[test]
    fn two_approvers_of_one_request_sign_it_once() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let stores = [
            PgEnrolmentStore::new(database.clone()),
            PgEnrolmentStore::new(database.clone()),
        ];
        stores[0].open_window(u64::MAX / 4).expect("open");
        for round in 0..20 {
            let id = format!("p{round}");
            stores[0]
                .take(&request(&id, "1"), 1_000, 100, 100)
                .expect("take");
            let signed = std::sync::atomic::AtomicUsize::new(0);
            let barrier = std::sync::Barrier::new(2);
            let answers: Vec<Approval> = std::thread::scope(|scope| {
                let racers: Vec<_> = stores
                    .iter()
                    .map(|store| {
                        let (barrier, id, signed) = (&barrier, id.clone(), &signed);
                        scope.spawn(move || {
                            barrier.wait();
                            store
                                .approve(
                                    &id,
                                    &bootstrap("1"),
                                    &mut || {
                                        signed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                        std::thread::sleep(std::time::Duration::from_millis(20));
                                        Ok((Decision::Approved(format!("pem {id}")), None))
                                    },
                                    &crate::enrolment::tests::kept(),
                                )
                                .expect("approve")
                        })
                    })
                    .collect();
                racers
                    .into_iter()
                    .map(|r| r.join().expect("racer"))
                    .collect()
            });
            let approved = answers
                .iter()
                .filter(|a| matches!(a, Approval::Approved(_)))
                .count();
            assert_eq!(approved, 1, "round {round}: {answers:?}");
            assert!(
                answers.contains(&Approval::NotWaiting) || answers.contains(&Approval::Deciding),
                "round {round}: {answers:?}"
            );
            assert_eq!(
                signed.load(std::sync::atomic::Ordering::SeqCst),
                1,
                "round {round}: signed once"
            );
        }
    }

    /// A withdrawal of a request whose bootstrap certificate the list names announces itself in the
    /// call that commits it: where that call loses its answer, the instance is handed the
    /// notification, so it reads its queue again.
    /// Verifies: ADR-0025
    #[test]
    fn a_withdrawal_whose_commit_lost_its_answer_is_handed_on() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        // Made ready on the usual bound, so tests beside this one that slow the database fail
        // no step but the one whose answer is lost.
        let ready = test.shared();
        let setting_up = PgEnrolmentStore::new(ready.clone());
        setting_up.open_window(u64::MAX / 4).expect("open");
        setting_up
            .take(&request("a", "1"), 1_000, 8, 8)
            .expect("take");
        crate::revocation::LedgerStore::add_revocation(
            &crate::postgres::ledger::PgLedgerStore::new(ready),
            &crate::revocation::Revocation {
                id: "r1".to_string(),
                revoked: crate::revocation::Revoked::Certificate {
                    authority: "bootstrap".to_string(),
                    issuers: vec![bootstrap("1").issuer],
                    serial: "1".to_string(),
                },
                revoked_ms: 1,
            },
            100,
        )
        .expect("revoke");
        let database = test.shared_within(std::time::Duration::from_secs(3));
        let store = PgEnrolmentStore::new(database.clone());
        let (handed, handing) = std::sync::mpsc::channel();
        database.listen(move |what| {
            if let crate::postgres::notify::Heard::Notified(notice) = what {
                let _ = handed.send(notice);
            }
        });
        let gate = test.hold_commits("enrolment_requests", 0x28_0006);
        let mut sign = || Ok((Decision::Approved("pem".to_string()), None));
        let answered = store.approve(
            "a",
            &bootstrap("1"),
            &mut sign,
            &crate::enrolment::tests::kept(),
        );
        assert!(
            answered.is_err(),
            "the commit's answer is lost: {answered:?}"
        );
        assert_eq!(
            handing.recv_timeout(std::time::Duration::from_secs(30)),
            Ok(crate::postgres::notify::Notice::new(ENROLMENT, "a")),
            "handed on at once"
        );
        gate.open();
    }

    /// A request whose bootstrap certificate another instance revoked is withdrawn when it is
    /// approved, and nothing is signed, whatever the approving instance holds in memory
    /// (ADR-0040 clause 28).
    /// Verifies: ADR-0040
    #[test]
    fn an_approval_whose_bootstrap_certificate_the_list_names_signs_nothing() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let store = PgEnrolmentStore::new(database.clone());
        store.open_window(u64::MAX / 4).expect("open");
        store.take(&request("a", "1"), 1_000, 8, 8).expect("take");
        store.take(&request("b", "2"), 1_000, 8, 8).expect("take");
        let ledger = crate::postgres::ledger::PgLedgerStore::new(database.clone());
        crate::revocation::LedgerStore::add_revocation(
            &ledger,
            &crate::revocation::Revocation {
                id: "r1".to_string(),
                revoked: crate::revocation::Revoked::Certificate {
                    authority: "bootstrap".to_string(),
                    issuers: vec![bootstrap("1").issuer],
                    serial: "1".to_string(),
                },
                revoked_ms: 1,
            },
            100,
        )
        .expect("revoke");
        let signed = std::cell::Cell::new(0);
        let mut sign = || {
            signed.set(signed.get() + 1);
            Ok((Decision::Approved("pem".to_string()), None))
        };
        assert_eq!(
            store.approve(
                "a",
                &bootstrap("1"),
                &mut sign,
                &crate::enrolment::tests::kept()
            ),
            Ok(Approval::Revoked)
        );
        assert_eq!(signed.get(), 0, "nothing signed");
        let held: Vec<String> = store
            .load()
            .expect("load")
            .expect("saved")
            .requests
            .iter()
            .map(|r| r.id.clone())
            .collect();
        assert_eq!(held, ["b"], "the request is withdrawn");
        assert!(matches!(
            store.approve(
                "b",
                &bootstrap("2"),
                &mut sign,
                &crate::enrolment::tests::kept()
            ),
            Ok(Approval::Approved(_))
        ));
    }

    /// Two instances on one database approving one request: one hands it out, the other answers
    /// that it is not found, or that it is being decided when it waited out its `lock_timeout`,
    /// and the request is signed and recorded once.
    /// Verifies: ADR-0025
    #[test]
    fn two_instances_approving_one_request_record_one_certificate() {
        use crate::enrolment::tests::{request as submitted, requester, Echo, Manual};
        use crate::enrolment::{Enrolment, Submitted};
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let clock = Arc::new(Manual(std::sync::atomic::AtomicU64::new(1_000_000)));
        let a = Enrolment::open_on(
            clock.clone(),
            Box::new(PgEnrolmentStore::new(database.clone())),
        )
        .expect("A");
        a.open(600).expect("window");
        let b = Enrolment::open_on(
            clock.clone(),
            Box::new(PgEnrolmentStore::new(database.clone())),
        )
        .expect("B");
        for round in 0..10 {
            let key = format!("k{round}");
            assert_eq!(a.submit(submitted(&key), requester()), Submitted::Waiting);
            assert_eq!(b.submit(submitted(&key), requester()), Submitted::Waiting);
            let recorded = std::sync::atomic::AtomicUsize::new(0);
            let record = |_: &crate::revocation::Signed, _: &[u8], _: &CertId| {
                recorded.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(20));
                Ok(())
            };
            let barrier = std::sync::Barrier::new(2);
            let answers: Vec<Result<String, DecisionError>> = std::thread::scope(|scope| {
                let racers: Vec<_> = [&a, &b]
                    .into_iter()
                    .map(|enrolment| {
                        let (barrier, key, record) = (&barrier, key.clone(), &record);
                        scope.spawn(move || {
                            barrier.wait();
                            enrolment.approve(
                                &key,
                                &Echo,
                                "host",
                                &crate::enrolment::tests::Recording::new(record),
                                &|_| false,
                            )
                        })
                    })
                    .collect();
                racers
                    .into_iter()
                    .map(|r| r.join().expect("racer"))
                    .collect()
            });
            assert!(
                answers.iter().any(Result::is_ok),
                "round {round}: {answers:?}"
            );
            assert!(
                answers.contains(&Err(DecisionError::NotFound))
                    || answers.contains(&Err(DecisionError::Deciding)),
                "round {round}: {answers:?}"
            );
            assert_eq!(
                recorded.load(std::sync::atomic::Ordering::SeqCst),
                1,
                "round {round}"
            );
        }
    }

    /// A take that read the window open before another instance closed it does not land in the
    /// closed window: the window's row is held for share while the request is written.
    /// Verifies: ADR-0025
    #[test]
    fn a_take_does_not_land_in_a_window_closed_meanwhile() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let store = PgEnrolmentStore::new(database.clone());
        store.open_window(u64::MAX / 4).expect("open");
        // Another instance's close, held open after its writes: the window's row and the queue
        // are written, not yet committed.
        let closing = database
            .run(|pool| async move {
                let held = Held(Some(client(&pool).await?));
                held.client()
                    .batch_execute(
                        "BEGIN; UPDATE enrolment_window SET until_ms = NULL; \
                         DELETE FROM enrolment_requests",
                    )
                    .await
                    .map_err(sql)?;
                Ok(held)
            })
            .expect("closing");
        let taken = std::thread::scope(|scope| {
            let taker = scope.spawn(|| store.take(&request("late", "1"), 1_000, 8, 8));
            std::thread::sleep(std::time::Duration::from_millis(300));
            database
                .run(move |_| async move {
                    closing
                        .client()
                        .batch_execute("COMMIT")
                        .await
                        .map_err(sql)?;
                    drop(closing.let_go());
                    Ok(())
                })
                .expect("commit the close");
            taker.join().expect("taker")
        });
        assert_eq!(taken, Ok(Took::Closed));
        assert_eq!(store.load().expect("load"), Some(Persisted::default()));
    }

    /// Closing or ending the window is one transaction: a deletion of its requests that fails
    /// leaves the window open and its requests as they were.
    /// Verifies: ADR-0023
    #[test]
    fn closing_or_ending_the_window_leaves_nothing_of_itself_when_it_fails() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let store = PgEnrolmentStore::new(test.open());
        store.open_window(10_000).expect("open");
        store.take(&request("a", "1"), 1_000, 8, 8).expect("take");
        let before = store.load().expect("load");
        test.sql(
            "CREATE OR REPLACE FUNCTION fleet_test_keep() RETURNS trigger \
             LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'kept by the test'; END $$; \
             CREATE TRIGGER fleet_test_keep BEFORE DELETE ON enrolment_requests \
             FOR EACH ROW EXECUTE FUNCTION fleet_test_keep();",
        );
        store.close_window().expect_err("the deletion fails");
        assert_eq!(store.load().expect("load"), before, "close");
        store.end_window(10_000).expect_err("the deletion fails");
        assert_eq!(store.load().expect("load"), before, "end");
        test.sql("DROP TRIGGER fleet_test_keep ON enrolment_requests");
        assert_eq!(store.end_window(10_000), Ok(true));
    }

    /// A take that fails part-way leaves the queue as it was, on either store.
    fn take_fails_part_way(store: &dyn EnrolmentStore, refuse: &dyn Fn(), mend: &dyn Fn()) {
        store.open_window(10_000).expect("open");
        store.take(&request("a", "1"), 1_000, 8, 8).expect("take");
        let before = store.load().expect("load");
        refuse();
        store
            .take(&request("z", "1"), 1_000, 8, 8)
            .expect_err("z cannot be written");
        mend();
        assert_eq!(store.load().expect("load"), before);
    }

    /// Verifies: ADR-0023
    #[test]
    fn an_enrolment_take_that_fails_leaves_nothing_of_itself_on_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("enrolment.json");
        take_fails_part_way(
            &FsEnrolmentStore::open(dir.path()),
            &|| crate::fs::testing::refuse(&file),
            &|| crate::fs::testing::mend(&file),
        );
    }

    /// Verifies: ADR-0023
    #[test]
    fn an_enrolment_take_that_fails_leaves_nothing_of_itself_on_postgres() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        take_fails_part_way(
            &PgEnrolmentStore::new(test.open()),
            &|| test.refuse("enrolment_requests", "NEW.id = 'z'"),
            &|| test.mend("enrolment_requests"),
        );
    }

    /// Verifies: ADR-0023
    #[test]
    fn the_enrolment_port_contract_holds_on_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        contract(&|| Box::new(FsEnrolmentStore::open(dir.path())));
    }

    /// Verifies: ADR-0023
    #[test]
    fn the_enrolment_port_contract_holds_on_postgres() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        contract(&|| Box::new(PgEnrolmentStore::new(database.clone())));
    }

    /// A save that fails part-way leaves the window and the queue as they were, on either store.
    /// `refuse` makes the write of request `z` fail; `mend` takes that away.
    fn fails_part_way(store: &dyn EnrolmentStore, refuse: &dyn Fn(), mend: &dyn Fn()) {
        let before = persisted(Some(10), &["a"]);
        store.save(&before).expect("save");
        refuse();
        store
            .save(&persisted(Some(20), &["b", "z"]))
            .expect_err("z cannot be written");
        mend();
        assert_eq!(store.load().expect("load"), Some(before));
    }

    /// Verifies: ADR-0023
    #[test]
    fn an_enrolment_save_that_fails_part_way_leaves_nothing_of_itself_on_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("enrolment.json");
        fails_part_way(
            &FsEnrolmentStore::open(dir.path()),
            &|| crate::fs::testing::refuse(&file),
            &|| crate::fs::testing::mend(&file),
        );
    }

    /// Verifies: ADR-0023
    #[test]
    fn an_enrolment_save_that_fails_part_way_leaves_nothing_of_itself_on_postgres() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        fails_part_way(
            &PgEnrolmentStore::new(test.open()),
            &|| test.refuse("enrolment_requests", "NEW.id = 'z'"),
            &|| test.mend("enrolment_requests"),
        );
    }

    /// An approval or a rejection that meets a request another instance holds while it signs it,
    /// for longer than a wait for a lock lasts, is answered that the request is being decided, and
    /// signs nothing; the other instance's approval stands, and once it is written the request is
    /// no longer waiting.
    /// Verifies: ADR-0025
    #[test]
    fn a_decision_on_a_request_another_instance_holds_long_is_answered_deciding() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b) = (
            &PgEnrolmentStore::new(database.clone()),
            &PgEnrolmentStore::new(database.clone()),
        );
        a.open_window(u64::MAX / 4).expect("open");
        a.take(&request("p", "1"), 1_000, 100, 100).expect("take");
        let (signing, signs) = std::sync::mpsc::channel::<()>();
        let (answered, answers) = std::sync::mpsc::channel();
        let (b_approved, b_decided, b_rejected, b_listed) = std::thread::scope(|scope| {
            // A signs until B has answered, however long B waits.
            let first = scope.spawn(move || {
                a.approve(
                    "p",
                    &bootstrap("1"),
                    &mut || {
                        signing.send(()).expect("signing");
                        answers.recv().expect("B answers");
                        Ok((Decision::Approved("pem a".to_string()), None))
                    },
                    &crate::enrolment::tests::kept(),
                )
            });
            signs.recv().expect("A signs");
            let answer = (
                b.approve(
                    "p",
                    &bootstrap("1"),
                    &mut || Ok((Decision::Approved("pem b".to_string()), None)),
                    &crate::enrolment::tests::kept(),
                ),
                b.decide("p", &Decision::Rejected),
                b.reject("p", 1),
                // A key the queue does not hold is rejected beside the approval, without a wait.
                b.reject("q", 1),
            );
            answered.send(()).expect("A waits");
            assert_eq!(
                first.join().expect("A"),
                Ok(Approval::Approved(Decision::Approved("pem a".to_string())))
            );
            answer
        });
        assert_eq!(b_approved, Ok(Approval::Deciding));
        assert_eq!(b_decided, Ok(Decided::Deciding));
        assert_eq!(b_rejected, Ok(Decided::Deciding));
        assert_eq!(b_listed, Ok(Decided::Listed));
        assert_eq!(
            b.approve(
                "p",
                &bootstrap("1"),
                &mut || { Ok((Decision::Approved("pem b".to_string()), None)) },
                &crate::enrolment::tests::kept()
            ),
            Ok(Approval::NotWaiting)
        );
        assert_eq!(b.decide("p", &Decision::Rejected), Ok(Decided::NotWaiting));
        assert_eq!(
            b.reject("p", 1),
            Ok(Decided::NotWaiting),
            "the approval stands"
        );
    }

    /// A rejection of a request another instance holds while it signs it answers at once that the
    /// request is being decided, rather than wait for the row behind the queue's lock: the
    /// approval's update needs that lock, so a wait would deadlock the two, and the database would
    /// end one of them. The approval is let go once the rejection answered or waits for the row,
    /// and it stands.
    /// Verifies: ADR-0027
    #[test]
    fn a_rejection_of_a_request_held_by_an_approval_does_not_wait_for_it() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (a, b) = (
            &PgEnrolmentStore::new(database.clone()),
            &PgEnrolmentStore::new(database.clone()),
        );
        a.open_window(u64::MAX / 4).expect("open");
        a.take(&request("p", "1"), 1_000, 100, 100).expect("take");
        let tuple_waits = format!(
            "SELECT count(*) FROM pg_locks WHERE locktype = 'tuple' \
             AND relation = '{}.enrolment_requests'::regclass",
            test.schema
        );
        let (signing, signs) = std::sync::mpsc::channel::<()>();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let (approved, rejected) = std::thread::scope(|scope| {
            let first = scope.spawn(move || {
                a.approve(
                    "p",
                    &bootstrap("1"),
                    &mut || {
                        signing.send(()).expect("signing");
                        released.recv().expect("let go");
                        Ok((Decision::Approved("pem a".to_string()), None))
                    },
                    &crate::enrolment::tests::kept(),
                )
            });
            signs.recv().expect("A signs while it holds the request");
            let second = scope.spawn(move || b.reject("p", 1));
            // B has answered, or waits for the row A holds.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
            while !second.is_finished() && test.rows(&tuple_waits)[0][0].as_deref() != Some("1") {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the rejection neither answered nor waited"
                );
                std::thread::yield_now();
            }
            release.send(()).expect("A waits");
            (first.join().expect("A"), second.join().expect("B"))
        });
        assert_eq!(
            approved,
            Ok(Approval::Approved(Decision::Approved("pem a".to_string())))
        );
        assert_eq!(rejected, Ok(Decided::Deciding));
        assert_eq!(
            b.reject("p", 1),
            Ok(Decided::NotWaiting),
            "the approval stands"
        );
    }

    /// The rejected keys' contract on either store: a key is rejected with or without a window and
    /// whether the queue holds its request or not, a decided request is left as it is, a take of
    /// a rejected key is refused beyond a close and an end of the window, each key keeps when it
    /// was first rejected, and an approval of a rejected key whose request still waits, as an
    /// instance of 0.8.0 may have queued it, signs nothing.
    fn rejections(open: &dyn Fn() -> Box<dyn EnrolmentStore>) {
        let store = open();
        let now = 1_000;
        let rejected = |store: &dyn EnrolmentStore| {
            store
                .load()
                .expect("load")
                .expect("saved")
                .rejected
                .into_iter()
                .collect::<Vec<_>>()
        };
        assert_eq!(store.reject("z", 3), Ok(Decided::Listed), "with no window");
        assert_eq!(rejected(&*open()), [("z".to_string(), 3)]);
        store.open_window(now + 100).expect("open");
        assert_eq!(store.take(&request("a", "1"), now, 3, 3), Ok(Took::Added));
        assert_eq!(store.take(&request("b", "1"), now, 3, 3), Ok(Took::Added));
        let approved = Decision::Approved("-----BEGIN CERTIFICATE-----".to_string());
        assert_eq!(store.decide("b", &approved), Ok(Decided::Written));
        assert_eq!(store.reject("a", 7), Ok(Decided::Written));
        assert_eq!(
            store.reject("a", 8),
            Ok(Decided::NotWaiting),
            "decided once"
        );
        assert_eq!(
            store.reject("b", 9),
            Ok(Decided::NotWaiting),
            "an approval stands"
        );
        assert_eq!(store.reject("z", 10), Ok(Decided::Listed), "rejected again");
        let mut decided = request("a", "1");
        decided.decision = Some(Decision::Rejected);
        assert_eq!(
            open().read_back("a"),
            Ok(Some(decided)),
            "its request is decided"
        );
        for id in ["a", "z"] {
            assert_eq!(
                store.take(&request(id, "1"), now, 3, 3),
                Ok(Took::Rejected),
                "{id}"
            );
        }
        store.close_window().expect("close");
        store.open_window(now + 100).expect("open");
        assert_eq!(
            store.take(&request("a", "1"), now, 3, 3),
            Ok(Took::Rejected),
            "beyond a close"
        );
        assert_eq!(store.end_window(now + 100), Ok(true));
        assert_eq!(
            rejected(&*open()),
            [("a".to_string(), 7), ("z".to_string(), 3)],
            "beyond an end"
        );

        store.open_window(now + 100).expect("open");
        assert_eq!(store.take(&request("c", "1"), now, 3, 3), Ok(Took::Added));
        let mut persisted = store.load().expect("load").expect("saved");
        persisted.rejected.insert("c".to_string(), 11);
        store.save(&persisted).expect("save");
        let signed = std::cell::Cell::new(false);
        assert_eq!(
            store.approve(
                "c",
                &bootstrap("1"),
                &mut || {
                    signed.set(true);
                    Ok((approved.clone(), None))
                },
                &crate::enrolment::tests::kept()
            ),
            Ok(Approval::NotWaiting)
        );
        assert!(!signed.get(), "a rejected key is signed by no approval");
    }

    /// Verifies: ADR-0027
    #[test]
    fn the_rejected_keys_hold_at_the_store_on_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        rejections(&|| Box::new(FsEnrolmentStore::open(dir.path())));
    }

    /// Verifies: ADR-0027
    #[test]
    fn the_rejected_keys_hold_at_the_store_on_postgres() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        rejections(&|| Box::new(PgEnrolmentStore::new(database.clone())));
    }

    /// An approval of a key another instance rejected, whose request still waits in the database
    /// as an instance of 0.8.0 queued it, signs nothing, records nothing, and is answered as not
    /// found; an instance that reads the queue lists the request as pending no longer.
    /// Verifies: ADR-0027
    #[test]
    fn an_approval_signs_no_rejected_key_on_postgres() {
        use crate::enrolment::tests::{
            key, request as submitted, requester, Counting, Registering,
        };
        use crate::enrolment::{DecisionError, Submitted};
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let register = register_on(&database);
        let a = enrolment_on(&database);
        a.open(600).expect("open the window");
        assert_eq!(
            a.submit(submitted(&key(1)), requester()),
            Submitted::Waiting
        );
        test.sql(&format!(
            "INSERT INTO enrolment_rejected (key, rejected_ms) VALUES ('{}', 1)",
            key(1)
        ));
        let recorder = Registering::on(&register);
        assert_eq!(
            a.approve(&key(1), &Counting::default(), "host-1", &recorder, &|_| {
                false
            }),
            Err(DecisionError::NotFound)
        );
        assert!(recorder.signed.lock().expect("signed").is_empty());
        assert_eq!(count(&test, "certificates"), 0);
        let b = enrolment_on(&database);
        assert!(b.pending().is_empty(), "{:?}", b.pending());
        assert!(b.is_rejected(&key(1)));
    }

    /// A rejected key stays refused beyond its window on PostgreSQL, on every instance on the
    /// database.
    /// Verifies: ADR-0027
    #[test]
    fn a_rejected_key_stays_refused_on_postgres() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let clock = Arc::new(crate::enrolment::tests::Manual(
            std::sync::atomic::AtomicU64::new(1_000_000),
        ));
        crate::enrolment::tests::a_rejected_key_stays_refused(
            &|| {
                crate::enrolment::Enrolment::open_on(
                    clock.clone(),
                    Box::new(PgEnrolmentStore::new(database.clone())),
                )
                .expect("open")
            },
            &clock,
        );
    }

    /// The register on `database`, as an instance opens it.
    fn register_on(database: &Arc<Database>) -> crate::revocation::Revocations {
        crate::revocation::Revocations::open(
            Box::new(crate::postgres::ledger::PgLedgerStore::new(
                database.clone(),
            )),
            Arc::new(crate::clock::SystemClock),
            Vec::new(),
        )
        .expect("register")
    }

    /// An instance's window and queue on `database`, the window open.
    fn enrolment_on(database: &Arc<Database>) -> crate::enrolment::Enrolment {
        let clock = Arc::new(crate::enrolment::tests::Manual(
            std::sync::atomic::AtomicU64::new(1_000_000),
        ));
        crate::enrolment::Enrolment::open_on(
            clock,
            Box::new(PgEnrolmentStore::new(database.clone())),
        )
        .expect("enrolment")
    }

    /// How many rows `table` holds in the test's schema.
    fn count(test: &TestDatabase, table: &str) -> usize {
        test.rows(&format!("SELECT count(*) FROM {table}"))[0][0]
            .as_deref()
            .expect("a count")
            .parse()
            .expect("a number")
    }

    /// An approval whose decision fails to commit after the certificate is signed leaves neither
    /// its register entry nor its Host, in the database or in memory, and its signature is
    /// recorded as rolled back; approved again, the request is signed once more, and one
    /// certificate for its key is in the register beside one Host, the list's version moved by
    /// its entry alone and held by the approving instance (ADR-0023 clauses 9 and 10).
    /// Verifies: ADR-0023
    #[test]
    fn an_approval_whose_commit_fails_records_nothing_and_a_second_records_one_certificate() {
        use crate::enrolment::tests::{request as submitted, requester, Counting, Registering};
        use crate::enrolment::{Submitted, Undecided};
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let register = register_on(&database);
        let enrolment = enrolment_on(&database);
        enrolment.open(600).expect("window");
        assert_eq!(
            enrolment.submit(submitted("k"), requester()),
            Submitted::Waiting
        );
        let recorder = Registering::on(&register);
        let signer = Counting::default();
        test.fail_commits("enrolment_requests", 1);

        let failed = enrolment.approve("k", &signer, "host-1", &recorder, &|_| false);
        assert!(matches!(failed, Err(DecisionError::Store(_))), "{failed:?}");
        assert_eq!(
            (count(&test, "certificates"), count(&test, "hosts")),
            (0, 0),
            "a decision that did not commit left a certificate or a Host"
        );
        assert!(register.issued().is_empty(), "taken up in memory");
        assert!(register.hosts().is_empty(), "a Host taken up in memory");
        assert_eq!(
            *recorder.undecided.lock().expect("undecided"),
            [("k".to_string(), "1".to_string(), Undecided::RolledBack)]
        );
        assert_eq!(enrolment.pending().len(), 1, "the request still waits");

        let serial = enrolment
            .approve("k", &signer, "host-2", &recorder, &|_| false)
            .expect("approved again");
        assert_eq!(serial, "2");
        assert_eq!(
            test.rows("SELECT serial FROM certificates"),
            [[Some("2".to_string())]],
            "one certificate for the key"
        );
        assert_eq!(
            test.rows("SELECT host FROM hosts"),
            [[Some("host-2".to_string())]],
            "one Host"
        );
        let held: Vec<String> = register
            .issued()
            .into_iter()
            .map(|issued| issued.facts.id.serial)
            .collect();
        assert_eq!(held, ["2"]);
        assert_eq!(
            (
                register.list_version(),
                test.rows("SELECT version FROM revocation_list_version")[0][0].as_deref()
            ),
            (Some(1), Some("1")),
            "the entry moved the list's version, and the approving instance holds it"
        );
        assert_eq!(*recorder.committed.lock().expect("committed"), ["2"]);
        assert_eq!(
            enrolment.submit(submitted("k"), requester()),
            Submitted::Issued("serial 2 for csr-k".to_string())
        );
        assert_eq!(recorder.undecided.lock().expect("undecided").len(), 1);
    }

    /// Two instances approve one request, and the first one's commit fails: the second, which
    /// waited for the request's row, signs it once when the first rolls back, and the request ends
    /// decided once, with one certificate in the register (ADR-0023 clause 9, ADR-0025 clause 19).
    /// Verifies: ADR-0023
    #[test]
    fn two_instances_approving_one_request_whose_first_commit_fails_sign_it_once_more() {
        use crate::enrolment::tests::{request as submitted, requester, Counting, Registering};
        use crate::enrolment::{Submitted, Undecided};
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (register_a, register_b) = (register_on(&database), register_on(&database));
        let (a, b) = (enrolment_on(&database), enrolment_on(&database));
        a.open(600).expect("window");
        b.reload().expect("B reads the window");
        assert_eq!(a.submit(submitted("k"), requester()), Submitted::Waiting);
        assert_eq!(b.submit(submitted("k"), requester()), Submitted::Waiting);
        test.fail_commits("enrolment_requests", 1);
        let (recorder_a, recorder_b) = (Registering::on(&register_a), Registering::on(&register_b));

        /// Signs as `Counting` does, once the test lets it go.
        struct Held<'a> {
            signs: Counting,
            signing: std::sync::mpsc::Sender<()>,
            go: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
            _lifetime: std::marker::PhantomData<&'a ()>,
        }
        impl crate::fleet::CertificateSigner for Held<'_> {
            fn sign(
                &self,
                csr_pem: &str,
                host: &str,
                subject: &crate::revocation::SubjectValues,
                _use_case: crate::fleet::UseCase,
            ) -> Result<crate::revocation::Signed, String> {
                self.signing.send(()).expect("signing");
                self.go.lock().expect("go").recv().expect("let go");
                self.signs.sign(csr_pem, host, subject, _use_case)
            }
            fn check_claims(&self, csr_pem: &str, sender: &[u8]) -> Result<(), String> {
                self.signs.check_claims(csr_pem, sender)
            }
            fn renewal_proof(
                &self,
                csr_pem: &str,
            ) -> Result<Option<crate::revocation::Facts>, String> {
                self.signs.renewal_proof(csr_pem)
            }
            fn bootstrap_proof_id(&self, csr_pem: &str) -> Result<Option<CertId>, String> {
                self.signs.bootstrap_proof_id(csr_pem)
            }
        }
        let (signing, signs) = std::sync::mpsc::channel();
        let (go, gone) = std::sync::mpsc::channel();
        let held = Held {
            signs: Counting::default(),
            signing,
            go: std::sync::Mutex::new(gone),
            _lifetime: std::marker::PhantomData,
        };
        let signer_b = Counting(std::sync::atomic::AtomicU64::new(100));
        let tuple_waits = format!(
            "SELECT count(*) FROM pg_locks WHERE locktype = 'tuple' \
             AND relation = '{}.enrolment_requests'::regclass",
            test.schema
        );
        let (first, second) = std::thread::scope(|scope| {
            let first = scope.spawn(|| a.approve("k", &held, "host-a", &recorder_a, &|_| false));
            signs.recv().expect("A signs while it holds the request");
            let second =
                scope.spawn(|| b.approve("k", &signer_b, "host-b", &recorder_b, &|_| false));
            // B waits for the row A holds before A's commit fails.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
            while test.rows(&tuple_waits)[0][0].as_deref() != Some("1") {
                assert!(
                    std::time::Instant::now() < deadline,
                    "B never waited for the row"
                );
                std::thread::yield_now();
            }
            go.send(()).expect("A goes on");
            (first.join().expect("A"), second.join().expect("B"))
        });
        assert!(matches!(first, Err(DecisionError::Store(_))), "{first:?}");
        assert_eq!(
            second,
            Ok("101".to_string()),
            "B signed once, after A rolled back"
        );
        assert_eq!(*recorder_b.signed.lock().expect("signed"), ["101"]);
        assert_eq!(
            *recorder_a.undecided.lock().expect("undecided"),
            [("k".to_string(), "1".to_string(), Undecided::RolledBack)]
        );
        assert_eq!(
            test.rows("SELECT serial FROM certificates"),
            [[Some("101".to_string())]]
        );
        assert_eq!(count(&test, "hosts"), 1);
        let decided = PgEnrolmentStore::new(database.clone())
            .read_back("k")
            .expect("read")
            .expect("held")
            .decision;
        assert_eq!(
            decided,
            Some(Decision::Approved("serial 101 for csr-k".to_string()))
        );
        assert_eq!(
            a.approve("k", &Counting::default(), "host-a", &recorder_a, &|_| false),
            Err(DecisionError::NotFound),
            "decided once"
        );
    }

    /// A register entry that its write refuses in the transaction that decides the request, here
    /// because another instance revoked the bootstrap certificate while the request was signed,
    /// rolls the decision back with it: nothing is registered or decided, the request is
    /// withdrawn, and the signature is recorded as rolled back (ADR-0023 clause 9, ADR-0040
    /// clause 28).
    /// Verifies: ADR-0023
    #[test]
    fn an_approval_whose_register_entry_is_refused_rolls_its_decision_back() {
        use crate::enrolment::tests::{request as submitted, requester, Counting, Registering};
        use crate::enrolment::{Submitted, Undecided};
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let register = register_on(&database);
        let enrolment = enrolment_on(&database);
        enrolment.open(600).expect("window");
        assert_eq!(
            enrolment.submit(submitted("k"), requester()),
            Submitted::Waiting
        );
        let recorder = Registering::on(&register);

        /// Signs as `Counting` does, after another instance revoked the bootstrap certificate.
        struct Revoking<'a>(Counting, &'a Arc<Database>);
        impl crate::fleet::CertificateSigner for Revoking<'_> {
            fn sign(
                &self,
                csr_pem: &str,
                host: &str,
                subject: &crate::revocation::SubjectValues,
                _use_case: crate::fleet::UseCase,
            ) -> Result<crate::revocation::Signed, String> {
                let bootstrap = requester().bootstrap;
                crate::revocation::LedgerStore::add_revocation(
                    &crate::postgres::ledger::PgLedgerStore::new(self.1.clone()),
                    &crate::revocation::Revocation {
                        id: "r1".to_string(),
                        revoked: crate::revocation::Revoked::Certificate {
                            authority: "bootstrap".to_string(),
                            issuers: vec![bootstrap.issuer],
                            serial: bootstrap.serial,
                        },
                        revoked_ms: 1,
                    },
                    100,
                )
                .expect("revoked elsewhere");
                self.0.sign(csr_pem, host, subject, _use_case)
            }
            fn check_claims(&self, csr_pem: &str, sender: &[u8]) -> Result<(), String> {
                self.0.check_claims(csr_pem, sender)
            }
            fn renewal_proof(
                &self,
                csr_pem: &str,
            ) -> Result<Option<crate::revocation::Facts>, String> {
                self.0.renewal_proof(csr_pem)
            }
            fn bootstrap_proof_id(&self, csr_pem: &str) -> Result<Option<CertId>, String> {
                self.0.bootstrap_proof_id(csr_pem)
            }
        }
        let refused = enrolment.approve(
            "k",
            &Revoking(Counting::default(), &database),
            "host-1",
            &recorder,
            &|_| false,
        );
        assert_eq!(refused, Err(DecisionError::Revoked));
        assert_eq!(
            (count(&test, "certificates"), count(&test, "hosts")),
            (0, 0)
        );
        assert_eq!(count(&test, "enrolment_requests"), 0, "withdrawn");
        assert!(register.issued().is_empty());
        assert!(recorder.committed.lock().expect("committed").is_empty());
        assert_eq!(
            *recorder.undecided.lock().expect("undecided"),
            [("k".to_string(), "1".to_string(), Undecided::RolledBack)]
        );
    }

    /// A commit the database server itself refuses is answered as a refusal, not as an outcome
    /// the instance would have to read back (ADR-0023 clause 9).
    /// Verifies: ADR-0023
    #[test]
    fn a_commit_the_database_refuses_is_answered_as_a_refusal() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let store = PgEnrolmentStore::new(test.open());
        store.open_window(u64::MAX / 4).expect("open");
        store.take(&request("p", "1"), 1_000, 8, 8).expect("take");
        test.fail_commits("enrolment_requests", 1);
        let answer = store.approve(
            "p",
            &bootstrap("1"),
            &mut || Ok((Decision::Approved("pem".to_string()), None)),
            &crate::enrolment::tests::kept(),
        );
        assert!(
            matches!(&answer, Ok(Approval::NotCommitted(e)) if e.contains("refused by the test")),
            "{answer:?}"
        );
        assert_eq!(
            store.read_back("p").expect("read").expect("held").decision,
            None,
            "the request still waits"
        );
    }

    /// The read back of an approval whose outcome is not known waits for the transaction that
    /// holds the request to end, and answers what it committed, not what was committed before
    /// (ADR-0023 clause 9).
    /// Verifies: ADR-0023
    #[test]
    fn a_read_back_waits_for_the_transaction_that_holds_the_request() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let store = PgEnrolmentStore::new(database.clone());
        store.open_window(u64::MAX / 4).expect("open");
        store.take(&request("p", "1"), 1_000, 8, 8).expect("take");
        let mut decided = serde_json::to_value(request("p", "1")).expect("request");
        decided["decision"] =
            serde_json::to_value(Decision::Approved("pem held".to_string())).expect("decision");
        // A decision written and not yet committed, as an approval in its commit leaves it.
        let holding = database
            .run(move |pool| async move {
                let held = Held(Some(client(&pool).await?));
                held.client()
                    .batch_execute(
                        "BEGIN; SELECT request FROM enrolment_requests WHERE id = 'p' FOR UPDATE",
                    )
                    .await
                    .map_err(sql)?;
                held.client()
                    .execute(
                        "UPDATE enrolment_requests SET request = $1 WHERE id = 'p'",
                        &[&decided],
                    )
                    .await
                    .map_err(sql)?;
                Ok(held)
            })
            .expect("holding");
        let tuple_waits = format!(
            "SELECT count(*) FROM pg_locks WHERE locktype = 'tuple' \
             AND relation = '{}.enrolment_requests'::regclass",
            test.schema
        );
        let read = std::thread::scope(|scope| {
            let reader = scope.spawn(|| store.read_back("p"));
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
            while test.rows(&tuple_waits)[0][0].as_deref() != Some("1") {
                assert!(
                    std::time::Instant::now() < deadline && !reader.is_finished(),
                    "the read back did not wait for the transaction"
                );
                std::thread::yield_now();
            }
            database
                .run(move |_| async move {
                    holding
                        .client()
                        .batch_execute("COMMIT")
                        .await
                        .map_err(sql)?;
                    drop(holding.let_go());
                    Ok(())
                })
                .expect("commit");
            reader.join().expect("reader")
        });
        assert_eq!(
            read.expect("read").expect("held").decision,
            Some(Decision::Approved("pem held".to_string()))
        );
    }

    /// Only an `ERROR` the server answers at the commit is a certain rollback: a `FATAL` or a
    /// `PANIC` there, and an error the server did not answer, are read back; at the UPDATE any
    /// answer of the server is certain (ADR-0023 clause 9). A real `FATAL` after a commit durable
    /// on its own, as a terminate during the wait for a synchronous standby gives, needs a
    /// replica, so the classification is decided here from the severity alone.
    /// Verifies: ADR-0023
    #[test]
    fn only_an_error_at_the_commit_is_a_certain_rollback() {
        assert!(rolled_back_for_certain(
            Some(Some(Severity::Error)),
            At::Commit
        ));
        for severity in [Some(Severity::Fatal), Some(Severity::Panic), None] {
            assert!(
                !rolled_back_for_certain(Some(severity), At::Commit),
                "{severity:?} at the commit"
            );
            assert!(
                rolled_back_for_certain(Some(severity), At::Update),
                "{severity:?} at the UPDATE"
            );
        }
        assert!(
            !rolled_back_for_certain(None, At::Commit),
            "no answer of the server"
        );
        assert!(
            !rolled_back_for_certain(None, At::Update),
            "no answer of the server"
        );
    }
}
