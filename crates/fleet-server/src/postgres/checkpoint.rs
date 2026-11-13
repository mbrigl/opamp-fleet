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

use tokio_postgres::NoTls;

use super::{client, snapshot, sql, Database, Tls};
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
    /// The connection that holds the signer's lock, while this instance holds it.
    lock: Option<tokio_postgres::Client>,
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

    /// Whether this instance holds the signer's lock: kept where its connection still answers,
    /// taken where no other instance holds it.
    fn locked(&mut self) -> Result<bool, String> {
        if let Some(lock) = self.lock.take() {
            let answers = self
                .database
                .bridge
                .run(super::bounded(LOCK_BOUND, async move {
                    lock.simple_query("SELECT 1").await.map_err(sql)?;
                    Ok::<_, String>(lock)
                }));
            match answers {
                Ok(lock) => {
                    self.lock = Some(lock);
                    return Ok(true);
                }
                Err(e) => tracing::warn!(
                    database = %self.database.name,
                    error = %e,
                    "the connection that held the lock of the audit's signer ended; the lock \
                     passes to the next instance that tries"
                ),
            }
        }
        let (pg, tls) = (self.database.pg.clone(), self.database.tls.clone());
        let (lock, held) = self
            .database
            .bridge
            .run(super::bounded(LOCK_BOUND, async move {
                let lock = connect(&pg, &tls).await?;
                let held: bool = lock
                    .query_one(&format!("SELECT pg_try_advisory_lock({SIGNER_LOCK})"), &[])
                    .await
                    .map_err(sql)?
                    .get(0);
                Ok::<_, String>((lock, held))
            }))
            .map_err(|e| format!("cannot reach the {}: {e}", self.database.name))?;
        if held {
            self.lock = Some(lock);
        }
        Ok(held)
    }
}

impl CheckpointSource for Checkpoints {
    fn prepare(&mut self) -> Result<Option<Prepared>, String> {
        if self.held.borrow().is_none() {
            // No live lease: the lock, if this instance held it, passes on.
            self.lock = None;
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

/// One connection outside the pool, its task on the current runtime, whose session and socket
/// count it dead after [`LOCK_DEAD_AFTER`] without an answer: keepalives after 10 s idle, every
/// 5 s, 3 of them unanswered, and a TCP user timeout of the same 30 s.
async fn connect(pg: &tokio_postgres::Config, tls: &Tls) -> Result<tokio_postgres::Client, String> {
    let mut pg = pg.clone();
    pg.keepalives(true)
        .keepalives_idle(Duration::from_secs(10))
        .keepalives_interval(Duration::from_secs(5))
        .keepalives_retries(3)
        .tcp_user_timeout(LOCK_DEAD_AFTER);
    let client = connect_plain(&pg, tls).await?;
    // The database's side: its session ends as soon, and the lock with it.
    client
        .batch_execute(&format!(
            "SET tcp_keepalives_idle = 10; SET tcp_keepalives_interval = 5; \
             SET tcp_keepalives_count = 3; SET tcp_user_timeout = {}",
            LOCK_DEAD_AFTER.as_millis()
        ))
        .await
        .map_err(sql)?;
    Ok(client)
}

async fn connect_plain(
    pg: &tokio_postgres::Config,
    tls: &Tls,
) -> Result<tokio_postgres::Client, String> {
    let ended = |result: Result<(), tokio_postgres::Error>| {
        if let Err(e) = result {
            tracing::debug!(error = %e, "the connection of the audit signer's lock ended");
        }
    };
    match tls {
        Tls::Rustls(tls) => {
            let (client, connection) = pg.connect(tls.clone()).await.map_err(sql)?;
            tokio::spawn(async move { ended(connection.await) });
            Ok(client)
        }
        Tls::Plain => {
            let (client, connection) = pg.connect(NoTls).await.map_err(sql)?;
            tokio::spawn(async move { ended(connection.await) });
            Ok(client)
        }
    }
}
