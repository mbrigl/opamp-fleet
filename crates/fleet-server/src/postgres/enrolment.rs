//! The enrolment window and its queue in PostgreSQL (ADR-0029 clause 8): the PostgreSQL adapter
//! behind [`EnrolmentStore`](crate::enrolment::EnrolmentStore) — the window's end in one row, each
//! request in a row of its own. Each act is one transaction on the rows it names, written before
//! it is answered; taking a request in holds the queue against every other taker while it counts,
//! so the queue's bounds hold for every instance on the database
//! (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
//! clause 19).

use std::sync::Arc;

use serde_json::Value;
use tokio_postgres::GenericClient;

use super::{client, signed, sql, unsigned, Database};
use crate::enrolment::{Decision, EnrolmentStore, Persisted, PersistedRequest, Took};

/// The window in `enrolment_window`, its requests in `enrolment_requests`.
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
            read(&**client).await
        })
    }

    fn save(&self, persisted: &Persisted) -> Result<(), String> {
        let persisted = persisted.clone();
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            write(&*transaction, &persisted).await?;
            transaction.commit().await.map_err(sql)
        })
    }

    /// The window's row is held for share and the queue against every other taker while the
    /// request is counted and written; a decision or a withdrawal waits, and a reader does not.
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
            transaction.commit().await.map_err(sql)?;
            Ok(Took::Added)
        })
    }

    /// The request's row is held while it is read and written, so two instances deciding one
    /// request decide it once.
    fn decide(&self, id: &str, decision: &Decision) -> Result<bool, String> {
        let id = id.to_string();
        let decision = serde_json::to_value(decision).expect("a decision serializes");
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            let Some(row) = transaction
                .query_opt(
                    "SELECT request FROM enrolment_requests WHERE id = $1 FOR UPDATE",
                    &[&id],
                )
                .await
                .map_err(sql)?
            else {
                return Ok(false);
            };
            let mut request: Value = row.get(0);
            if request.get("decision").is_some_and(|d| !d.is_null()) {
                return Ok(false);
            }
            request["decision"] = decision;
            transaction
                .execute(
                    "UPDATE enrolment_requests SET request = $2 WHERE id = $1",
                    &[&id, &request],
                )
                .await
                .map_err(sql)?;
            transaction.commit().await.map_err(sql)?;
            Ok(true)
        })
    }

    fn withdraw(&self, ids: &[String]) -> Result<(), String> {
        let ids = ids.to_vec();
        self.database.run(move |pool| async move {
            let client = client(&pool).await?;
            client
                .execute(
                    "DELETE FROM enrolment_requests WHERE id = ANY($1) \
                     AND request -> 'decision' IS NULL",
                    &[&ids],
                )
                .await
                .map_err(sql)?;
            Ok(())
        })
    }

    fn open_window(&self, until_ms: u64) -> Result<(), String> {
        self.database.run(move |pool| async move {
            let client = client(&pool).await?;
            client
                .execute(
                    "INSERT INTO enrolment_window (singleton, until_ms) VALUES (true, $1) \
                     ON CONFLICT (singleton) DO UPDATE SET until_ms = EXCLUDED.until_ms",
                    &[&signed(until_ms)],
                )
                .await
                .map_err(sql)?;
            Ok(())
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
            }
            transaction.commit().await.map_err(sql)?;
            Ok(ended > 0)
        })
    }
}

/// What was saved last, `None` when nothing was. A request that does not decode fails, naming it.
pub(crate) async fn read(client: &impl GenericClient) -> Result<Option<Persisted>, String> {
    let Some(window) = client
        .query_opt("SELECT until_ms FROM enrolment_window", &[])
        .await
        .map_err(sql)?
    else {
        return Ok(None);
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
    serde_json::from_value(Value::Object(document))
        .map(Some)
        .map_err(|e| format!("the enrolment queue in the database does not decode: {e}"))
}

/// Replaces the window and its requests inside the caller's transaction.
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrolment::{Decision, PersistedRequest, Took};
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
        assert_eq!(store.decide("a", &approved), Ok(true));
        assert_eq!(
            store.decide("a", &Decision::Rejected),
            Ok(false),
            "decided once"
        );
        assert_eq!(store.decide("none", &Decision::Rejected), Ok(false));
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

    /// Verifies: ADR-0029
    #[test]
    fn the_enrolment_acts_hold_on_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        acts(&|| Box::new(FsEnrolmentStore::open(dir.path())));
    }

    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
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
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
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

        let barrier = std::sync::Barrier::new(2);
        let id = held[0].id.clone();
        let decided: Vec<bool> = std::thread::scope(|scope| {
            let racers: Vec<_> = stores
                .iter()
                .map(|store| {
                    let (barrier, id) = (&barrier, id.clone());
                    scope.spawn(move || {
                        barrier.wait();
                        store.decide(&id, &Decision::Rejected).expect("decide")
                    })
                })
                .collect();
            racers
                .into_iter()
                .map(|r| r.join().expect("racer"))
                .collect()
        });
        assert_eq!(decided.iter().filter(|d| **d).count(), 1, "{decided:?}");
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

    /// Verifies: ADR-0029
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

    /// Verifies: ADR-0029
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

    /// Verifies: ADR-0029
    #[test]
    fn the_enrolment_port_contract_holds_on_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        contract(&|| Box::new(FsEnrolmentStore::open(dir.path())));
    }

    /// Verifies: ADR-0029
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

    /// Verifies: ADR-0029
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

    /// Verifies: ADR-0029
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
}
