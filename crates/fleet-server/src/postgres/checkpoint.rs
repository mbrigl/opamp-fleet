//! The checkpoints of the audit record on PostgreSQL
//! (ADR-0041 clause 2): which
//! instance signs, and what its checkpoint names.
//!
//! The instance that holds a session-level advisory lock signs. It holds the lock on one
//! connection of its own, outside the pool, which it checks at every interval; a lock that ends with
//! its connection passes to the next instance that tries. An instance signs only while its lease
//! is live. A checkpoint names the head of every open chain, and of every closed chain no
//! checkpoint named closed yet; the transaction that writes it records which closed chains it
//! named, and the verifier carries their closed heads forward, so each is named once.

use std::sync::Arc;
use std::time::Duration;

use super::{client, snapshot, sql, Database, SessionLock};
use crate::audit_log::line_hash;
use crate::checkpoint::{CheckpointSource, Head, Prepared, Tip};

/// The lock the instance that signs holds for its session, by the key its schema gives it.
pub(crate) const SIGNER_LOCK: &str =
    "hashtextextended('opamp-fleet-server-audit-checkpoint:' || coalesce(current_schema(), ''), 0)";

/// How long the signer waits for its lock's connection to answer.
const LOCK_BOUND: Duration = Duration::from_secs(10);

/// How long the lock's connection may go unanswered, on either end, before it counts as dead: the
/// keepalives and the TCP user timeout of its session and of its socket. A partitioned signer's
/// lock is freed within about this long, well inside a minute.
pub(crate) const LOCK_DEAD_AFTER: Duration = Duration::from_secs(30);

/// The source of the checkpoints on one database, for the instance whose lease `held` names.
pub struct Checkpoints {
    database: Arc<Database>,
    held: crate::instance::Held,
    /// The signer's lock, on a session of its own, while this instance holds it.
    lock: Option<SessionLock>,
}

impl Checkpoints {
    /// The source on `database` for the instance whose lease `held` names.
    #[must_use]
    pub fn new(database: Arc<Database>, held: crate::instance::Held) -> Self {
        Checkpoints {
            database,
            held,
            lock: None,
        }
    }

    /// Whether this instance holds the signer's lock: kept where its session still holds it and
    /// answers, taken where no other instance holds it.
    fn locked(&mut self) -> Result<bool, String> {
        if let Some(lock) = self.lock.take() {
            let (lock, holds) = self.database.bridge.run(async move {
                let holds = lock.holds(LOCK_BOUND).await;
                (lock, holds)
            });
            if holds {
                self.lock = Some(lock);
                return Ok(true);
            }
            tracing::warn!(
                database = %self.database.name,
                "the session that held the lock of the audit's signer ended or does not answer; \
                 the lock passes to the next instance that tries"
            );
        }
        let (pg, tls) = (self.database.pg.clone(), self.database.tls.clone());
        let lock = self
            .database
            .bridge
            .run(super::bounded(LOCK_BOUND, async move {
                let (pg, setup) = dead_after(&pg);
                let lock = SessionLock::open(&pg, &tls, SIGNER_LOCK, &setup, |_, why| {
                    tracing::debug!(error = %why, "the session of the audit signer's lock ended");
                })
                .await?;
                Ok::<_, String>(lock.try_lock().await?.then_some(lock))
            }))
            .map_err(|e| format!("cannot reach the {}: {e}", self.database.name))?;
        let held = lock.is_some();
        self.lock = lock;
        Ok(held)
    }

    /// Lets go of the signer's lock, if this instance holds it, before it answers: the next
    /// instance that tries has it at once.
    fn let_go(&mut self) {
        if let Some(lock) = self.lock.take() {
            let let_go =
                self.database.bridge.run(super::bounded(
                    LOCK_BOUND,
                    async move { lock.unlock().await },
                ));
            if let Err(e) = let_go {
                tracing::warn!(
                    database = %self.database.name,
                    error = %e,
                    "the lock of the audit's signer was not let go; it goes when its session ends"
                );
            }
        }
    }
}

impl CheckpointSource for Checkpoints {
    fn prepare(&mut self) -> Result<Option<Prepared>, String> {
        if self.held.borrow().is_none() {
            // No live lease: the lock, if this instance held it, passes on.
            self.let_go();
            return Ok(None);
        }
        if !self.locked()? {
            return Ok(None);
        }
        let (tip, chains) = self.database.run(|pool| async move {
            let mut client = client(&pool).await?;
            let transaction = snapshot(&mut client).await?;
            let newest = transaction
                .query_one("SELECT seq, hash FROM audit_checkpoint_head", &[])
                .await
                .map_err(sql)?;
            let (seq, hash) = (newest.get::<_, i64>(0), newest.get::<_, String>(1));
            let tip = if seq == 0 {
                None
            } else {
                let line = transaction
                    .query_opt(
                        "SELECT line FROM audit_entries WHERE checkpoint_seq = $1",
                        &[&seq],
                    )
                    .await
                    .map_err(sql)?
                    .map(|row| row.get::<_, String>(0));
                Some(Tip {
                    checkpoint_seq: super::unsigned(seq),
                    hash,
                    line,
                })
            };
            let chains: Vec<Head> = transaction
                .query(
                    "SELECT c.id::text, c.closed_hash IS NOT NULL, e.seq, e.line \
                     FROM audit_chains c CROSS JOIN LATERAL \
                     (SELECT seq, line FROM audit_entries WHERE chain = c.id \
                      ORDER BY seq DESC LIMIT 1) e \
                     WHERE c.closed_hash IS NULL OR c.named_closed_by IS NULL \
                     ORDER BY c.id",
                    &[],
                )
                .await
                .map_err(sql)?
                .iter()
                .map(|row| Head {
                    chain: row.get(0),
                    closed: row.get(1),
                    seq: super::unsigned(row.get::<_, i64>(2)),
                    hash: line_hash(&row.get::<_, String>(3)),
                })
                .collect();
            transaction.commit().await.map_err(sql)?;
            Ok((tip, chains))
        })?;
        // A closed chain a checkpoint named closed is not named again: the verifier carries it.
        Ok(Some(Prepared { tip, heads: chains }))
    }
}

/// The settings of the lock's session, `pg` and what runs on the session first, under which its
/// session and socket count it dead after [`LOCK_DEAD_AFTER`] without an answer: keepalives after
/// 10 s idle, every 5 s, 3 of them unanswered, and a TCP user timeout of the same 30 s, on either
/// end, so the database's session ends as soon, and the lock with it.
fn dead_after(pg: &tokio_postgres::Config) -> (tokio_postgres::Config, String) {
    let mut pg = pg.clone();
    pg.keepalives(true)
        .keepalives_idle(Duration::from_secs(10))
        .keepalives_interval(Duration::from_secs(5))
        .keepalives_retries(3)
        .tcp_user_timeout(LOCK_DEAD_AFTER);
    let setup = format!(
        "SET tcp_keepalives_idle = 10; SET tcp_keepalives_interval = 5; \
         SET tcp_keepalives_count = 3; SET tcp_user_timeout = {}",
        LOCK_DEAD_AFTER.as_millis()
    );
    (pg, setup)
}

#[cfg(test)]
mod tests {
    use tokio::sync::watch;

    use super::super::testing::TestDatabase;
    use super::*;

    /// The source of an instance that holds a lease, and what ends its lease.
    fn signing(test: &TestDatabase) -> (Checkpoints, watch::Sender<Option<String>>) {
        let (lease, held) = watch::channel(Some("the lease".to_string()));
        (Checkpoints::new(test.open(), held), lease)
    }

    /// The signer's lock, by its key in the test's schema, as a gate takes it.
    fn the_lock(test: &TestDatabase) -> i64 {
        test.rows(&format!("SELECT {SIGNER_LOCK}"))[0][0]
            .as_deref()
            .expect("key")
            .parse()
            .expect("a bigint")
    }

    /// A signer whose lease ends lets go of its lock before it answers, so another instance takes
    /// it at once rather than once the database has ended the session.
    /// Verifies: ADR-0041
    #[test]
    fn a_signer_whose_lease_ends_lets_go_of_its_lock_at_once() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let (mut signer, lease) = signing(&test);
        assert!(signer.prepare().expect("prepare").is_some(), "it signs");
        assert!(!test.lock_is_free(SIGNER_LOCK), "held while it signs");
        lease.send_replace(None);
        assert!(
            signer.prepare().expect("prepare").is_none(),
            "no lease, no checkpoint"
        );
        assert!(
            test.lock_is_free(SIGNER_LOCK),
            "the lock stayed with the session"
        );
    }

    /// A signer whose session the database ends learns at its next interval that its lock is gone:
    /// it signs nothing while another session holds the lock, and signs again once it can take it.
    /// Verifies: ADR-0041
    #[test]
    fn a_signer_whose_session_the_database_ends_learns_its_lock_is_gone() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let (mut signer, _lease) = signing(&test);
        assert!(signer.prepare().expect("prepare").is_some(), "it signs");
        test.end_the_holder_of(SIGNER_LOCK);
        let elsewhere = test.gate(the_lock(&test));
        assert!(
            signer.prepare().expect("prepare").is_none(),
            "signed without the lock"
        );
        elsewhere.open();
        assert!(
            signer.prepare().expect("prepare").is_some(),
            "it signs again"
        );
    }
}
