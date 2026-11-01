//! The lease every instance on one database holds while it serves
//! (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
//! clauses 1 to 3).
//!
//! A lease is a row of `leases` whose `expires_at` the database sets from its own clock, so every
//! instance reads one time. An instance takes one under a new instance id before it opens either
//! listener, renews it while it serves, and releases it at an orderly shutdown. Leases are taken
//! one at a time, under a lock for the transaction, so two instances that start together each see
//! the other's.

use std::time::{Duration, Instant};

use super::{client, sql, Database, Exclusive, EXCLUSIVE_LOCK};

/// The lock a lease is taken under.
const LEASES_LOCK: &str = "hashtextextended('opamp-fleet-server-leases:' || \
                           coalesce(current_schema(), ''), 0)";

/// A lease this instance took.
#[derive(Clone, Debug)]
pub struct Taken {
    /// The instance id the lease is held under: a random UUID, not persisted.
    pub instance_id: String,
    /// When the request that took it was sent: the instance measures its lease from here, which is
    /// no later than the database's own time of it.
    pub sent: Instant,
    /// The leases of instances that were gone, which this one took over: every lease that had
    /// lapsed, and the live lease of an instance that had to be alone and whose lock is free.
    pub taken_over: Vec<String>,
}

/// A lease this instance renewed.
#[derive(Clone, Copy, Debug)]
pub struct Renewal {
    /// When the request that renewed it was sent.
    pub sent: Instant,
    /// The revocation list's version as the database held it at the renewal
    /// (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    /// clause 10).
    pub list_version: u64,
}

/// A new instance id: a random UUID (version 4), as text. The Server makes one at startup, before
/// its first lease, so that its log names it from then on, and one for each new lease after a lost
/// one.
///
/// # Errors
/// Returns an error when the system gives no randomness.
pub fn new_instance_id() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes)
        .map_err(|_| "no randomness for an instance id".to_string())?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = hex::encode(bytes);
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

impl Database {
    /// Takes a lease under a new instance id, lapsing `lapse` after now, for `host`.
    ///
    /// An instance that must be alone fails when another live lease exists, naming the key that
    /// makes it alone and the number of live leases; it holds the exclusive lock already, so a
    /// live lease of an instance that had to be alone belongs to a process that is gone, and it
    /// takes that over. An instance that shares the database fails while another instance holds it
    /// alone; a live lease of one that had to be alone and whose lock is free it takes over too.
    /// A lapsed lease it takes over as well, removing it; [`Taken::taken_over`] names each.
    ///
    /// # Errors
    /// Returns an error naming the database when it cannot be reached, and the refusals above.
    pub fn take_lease(&self, host: &str, lapse: Duration) -> Result<Taken, String> {
        self.take_lease_within(new_instance_id()?, host, lapse, super::CALL_TIMEOUT)
    }

    /// [`take_lease`](Self::take_lease) under `instance_id`, one [`new_instance_id`] made, waiting
    /// at most `bound` for the answer.
    ///
    /// # Errors
    /// As [`take_lease`](Self::take_lease), and when the database does not answer within `bound`.
    pub fn take_lease_within(
        &self,
        instance_id: String,
        host: &str,
        lapse: Duration,
        bound: Duration,
    ) -> Result<Taken, String> {
        let exclusive = self.exclusive().clone();
        let host = host.to_string();
        let sent = Instant::now();
        let (id, taken_over) = self.run_within(bound, move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            transaction
                .execute(&format!("SELECT pg_advisory_xact_lock({LEASES_LOCK})"), &[])
                .await
                .map_err(sql)?;
            let live = transaction
                .query(
                    "SELECT instance_id::text, must_be_alone FROM leases WHERE expires_at > now()",
                    &[],
                )
                .await
                .map_err(sql)?;
            let alone: Vec<String> = live
                .iter()
                .filter(|row| row.get::<_, bool>(1))
                .map(|row| row.get(0))
                .collect();
            let shared = live.len() - alone.len();
            match &exclusive {
                Exclusive::Alone(because) => {
                    if shared > 0 {
                        return Err(format!(
                            "this instance must be alone on the database because of {because}, \
                             and {shared} other instance{} hold{} a live lease — stop {} first, \
                             or keep the blobs in S3 and no CA key in a file",
                            if shared == 1 { "" } else { "s" },
                            if shared == 1 { "s" } else { "" },
                            if shared == 1 { "it" } else { "them" },
                        ));
                    }
                }
                Exclusive::Shared => {
                    let free: bool = transaction
                        .query_one(
                            &format!("SELECT pg_try_advisory_xact_lock({EXCLUSIVE_LOCK})"),
                            &[],
                        )
                        .await
                        .map_err(sql)?
                        .get(0);
                    if !free {
                        return Err(
                            "another instance holds the database alone, because it keeps its \
                             blobs as files ([blobs] store = \"fs\") or a CA key in a file \
                             ([client_ca] key_file, [enrolment] bootstrap_ca_key_file) — stop \
                             it first"
                                .to_string(),
                        );
                    }
                }
            }
            let taken_over: Vec<String> = transaction
                .query(
                    "DELETE FROM leases WHERE expires_at <= now() OR instance_id::text = ANY($1) \
                     RETURNING instance_id::text",
                    &[&alone],
                )
                .await
                .map_err(sql)?
                .iter()
                .map(|row| row.get(0))
                .collect();
            transaction
                .execute(
                    "INSERT INTO leases \
                     (instance_id, host, must_be_alone, taken_at, renewed_at, expires_at) \
                     VALUES ($1::text::uuid, $2, $3, now(), now(), \
                     now() + make_interval(secs => $4))",
                    &[
                        &instance_id,
                        &host,
                        &exclusive.alone(),
                        &lapse.as_secs_f64(),
                    ],
                )
                .await
                .map_err(sql)?;
            transaction.commit().await.map_err(sql)?;
            Ok((instance_id, taken_over))
        })?;
        Ok(Taken {
            instance_id: id,
            sent,
            taken_over,
        })
    }

    /// Renews the lease of `instance_id` to lapse `lapse` after now, waiting at most `bound` for
    /// the answer. Answers when the request was sent and the revocation list's version, read in the
    /// same statement, or `None` when the lease is no longer live, lapsed or taken over.
    ///
    /// # Errors
    /// Returns an error naming the database when it does not answer within `bound`.
    pub fn renew_lease(
        &self,
        instance_id: &str,
        lapse: Duration,
        bound: Duration,
    ) -> Result<Option<Renewal>, String> {
        let id = instance_id.to_string();
        let sent = Instant::now();
        let (renewed, version) = self.run_within(bound, move |pool| async move {
            let client = client(&pool).await?;
            let row = client
                .query_one(
                    "WITH renewed AS (UPDATE leases SET renewed_at = now(), \
                     expires_at = now() + make_interval(secs => $2) \
                     WHERE instance_id = $1::text::uuid AND expires_at > now() RETURNING 1) \
                     SELECT (SELECT count(*) FROM renewed), \
                     (SELECT version FROM revocation_list_version)",
                    &[&id, &lapse.as_secs_f64()],
                )
                .await
                .map_err(sql)?;
            Ok((row.get::<_, i64>(0), row.get::<_, i64>(1)))
        })?;
        Ok((renewed == 1).then_some(Renewal {
            sent,
            list_version: super::unsigned(version),
        }))
    }

    /// Releases the lease of `instance_id`, at an orderly shutdown.
    ///
    /// # Errors
    /// Returns an error naming the database when it cannot be reached.
    pub fn release_lease(&self, instance_id: &str) -> Result<(), String> {
        let id = instance_id.to_string();
        self.run(move |pool| async move {
            let client = client(&pool).await?;
            client
                .execute(
                    "DELETE FROM leases WHERE instance_id = $1::text::uuid",
                    &[&id],
                )
                .await
                .map_err(sql)?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::TestDatabase;
    use super::*;

    const LAPSE: Duration = Duration::from_secs(30);

    fn shared(test: &TestDatabase) -> std::sync::Arc<Database> {
        Database::open_in(&test.config(), Exclusive::Shared, Some(&test.schema)).expect("shared")
    }

    fn alone(test: &TestDatabase, because: &str) -> Result<std::sync::Arc<Database>, String> {
        Database::open_in(
            &test.config(),
            Exclusive::Alone(because.to_string()),
            Some(&test.schema),
        )
    }

    /// An instance id is a version 4 UUID, and two are never the same.
    #[test]
    fn an_instance_id_is_a_random_uuid() {
        let (a, b) = (new_instance_id().expect("a"), new_instance_id().expect("b"));
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "4");
    }

    /// Two instances that share the database each take a lease of their own and renew it; a lease
    /// that lapsed is renewed no more, and a released one is gone.
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn instances_that_share_the_database_each_hold_a_lease() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let (a, b) = (shared(&test), shared(&test));
        let first = a.take_lease("host-a", LAPSE).expect("a's lease");
        let second = b.take_lease("host-b", LAPSE).expect("b's lease");
        assert_ne!(first.instance_id, second.instance_id);
        assert!(a
            .renew_lease(&first.instance_id, LAPSE, LAPSE)
            .expect("renew")
            .is_some());
        test.sql(&format!(
            "UPDATE leases SET expires_at = now() WHERE instance_id = '{}'",
            second.instance_id
        ));
        assert!(
            b.renew_lease(&second.instance_id, LAPSE, LAPSE)
                .expect("asked")
                .is_none(),
            "a lapsed lease is renewed no more"
        );
        a.release_lease(&first.instance_id).expect("release");
        assert!(test
            .rows(&format!(
                "SELECT 1 FROM leases WHERE instance_id = '{}'",
                first.instance_id
            ))
            .is_empty());
    }

    /// An instance that must be alone fails startup while another live lease exists, naming the
    /// key and the number of leases; an instance that shares the database fails while one holds it
    /// alone; one that had to be alone and is gone has its lease taken over.
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn an_instance_that_must_be_alone_is_alone() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let other = shared(&test);
        let lease = other.take_lease("host-a", LAPSE).expect("lease");
        let lonely = alone(&test, "[blobs] store = \"fs\"").expect("the lock is free");
        let err = lonely
            .take_lease("host-b", LAPSE)
            .expect_err("another lease is live");
        assert!(err.contains("[blobs] store = \"fs\""), "{err}");
        assert!(err.contains("1 other instance holds a live lease"), "{err}");
        other.release_lease(&lease.instance_id).expect("release");
        let held = lonely.take_lease("host-b", LAPSE).expect("alone now");

        let err = shared(&test)
            .take_lease("host-c", LAPSE)
            .expect_err("held alone");
        assert!(err.contains("holds the database alone"), "{err}");
        let err = alone(&test, "[client_ca] key_file")
            .map(|_| ())
            .expect_err("the lock is held");
        assert!(err.contains("[client_ca] key_file"), "{err}");

        // The process is gone and its lease still live: the lock went with its session.
        drop(lonely);
        let mut taken = None;
        for _ in 0..50 {
            if let Ok(lease) = shared(&test).take_lease("host-c", LAPSE) {
                taken = Some(lease);
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let taken = taken.expect("the lock was not released");
        assert_eq!(taken.taken_over, [held.instance_id]);
    }

    /// A lease that lapsed is taken over by the next instance that takes one, which names it; a
    /// live lease of an instance that shares the database is not.
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn a_lapsed_lease_is_taken_over_and_named() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let (a, b, c) = (shared(&test), shared(&test), shared(&test));
        let lapsed = a.take_lease("host-a", LAPSE).expect("a's lease");
        let live = b.take_lease("host-b", LAPSE).expect("b's lease");
        test.sql(&format!(
            "UPDATE leases SET expires_at = now() WHERE instance_id = '{}'",
            lapsed.instance_id
        ));
        let taken = c.take_lease("host-c", LAPSE).expect("c's lease");
        assert_eq!(taken.taken_over, [lapsed.instance_id]);
        assert!(!taken.taken_over.contains(&live.instance_id));
    }

    /// What a log writes, collected.
    #[derive(Clone, Default)]
    struct Collected(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Collected {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("log").extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// The lines `take` logs.
    fn logged(take: impl FnOnce() -> Taken) -> (Taken, Vec<String>) {
        let collected = Collected::default();
        let writer = collected.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_writer(move || writer.clone())
            .finish();
        let taken = tracing::subscriber::with_default(subscriber, take);
        let lines = String::from_utf8(collected.0.lock().expect("log").clone())
            .expect("utf-8")
            .lines()
            .map(str::to_string)
            .collect();
        (taken, lines)
    }

    /// An instance that takes over the lapsed lease of another logs its own instance id and the
    /// lapsed one as a takeover; one that takes a new lease after losing its own logs the lapsed
    /// one as its own, never as an instance that is gone
    /// (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    /// clauses 1 and 3).
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn a_take_names_a_lapsed_lease_as_a_takeover_or_as_its_own() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let times = crate::config::LeaseTimes {
            renew: Duration::from_secs(10),
            lapse: LAPSE,
        };
        let lapse = |id: &str| {
            test.sql(&format!(
                "UPDATE leases SET expires_at = now() WHERE instance_id = '{id}'"
            ));
        };
        let (a, b) = (shared(&test), shared(&test));
        let gone = a.take_lease("host-a", LAPSE).expect("a's lease");
        lapse(&gone.instance_id);
        let (taken, lines) = logged(|| {
            crate::instance::take_in_time(&b, times, "host-b", new_instance_id().expect("id"), None)
                .expect("b's lease")
        });
        let takeover = lines
            .iter()
            .find(|line| line.contains("took over the lease"))
            .unwrap_or_else(|| panic!("no takeover logged: {lines:?}"));
        assert!(
            takeover.contains(&format!("instance_id={}", taken.instance_id)),
            "{takeover}"
        );
        assert!(
            takeover.contains(&format!("taken_over={}", gone.instance_id)),
            "{takeover}"
        );

        // b loses its lease and takes a new one: its own lapsed lease is no takeover.
        lapse(&taken.instance_id);
        let (again, lines) = logged(|| {
            crate::instance::take_in_time(
                &b,
                times,
                "host-b",
                new_instance_id().expect("id"),
                Some(&taken.instance_id),
            )
            .expect("b's new lease")
        });
        assert_eq!(again.taken_over, std::slice::from_ref(&taken.instance_id));
        assert!(
            !lines.iter().any(|line| line.contains("took over")),
            "{lines:?}"
        );
        let own = lines
            .iter()
            .find(|line| line.contains("own lapsed lease"))
            .unwrap_or_else(|| panic!("the own lapsed lease is not logged: {lines:?}"));
        assert!(
            own.contains(&format!("lapsed={}", taken.instance_id)),
            "{own}"
        );
    }
}
