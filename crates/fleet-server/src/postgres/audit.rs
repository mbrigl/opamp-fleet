//! The audit record on PostgreSQL, one hash chain per lease
//! (ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance): the adapter
//! behind [`AuditStore`], the opening and closing of chains that a lease's take and renewal do in
//! their own transactions, and `server audit-verify --config`.
//!
//! A chain is named by the instance id of the lease that opened it, a random UUID drawn with each
//! lease. Its record and its first entry, `audit.chain_opened`, are written in the transaction that
//! takes the lease; the instance's writer appends every other entry of it. An orderly shutdown
//! closes the chain with `audit.chain_closed`; the instance that takes over a lapsed lease closes
//! that lease's chain in the transaction that removes the lease. The store refuses an entry to a
//! closed chain and to the chain of a lease that is no longer live, so an instance that was cut off
//! cannot extend its chain afterwards.

use std::path::Path;
use std::sync::Arc;

use tokio_postgres::GenericClient;

use super::{client, sql, Database, Exclusive, CALL_TIMEOUT};
use crate::audit::Entry;
use crate::audit_log::{line_hash, render, AuditStore, Chain, ChainWalk, Tail, GENESIS};
use crate::config::PostgresConfig;

/// The entries read in one round trip while a chain is walked.
const PAGE: i64 = 5000;

/// The chain of the lease held, in the database.
pub struct PgAuditStore {
    database: Arc<Database>,
    held: crate::instance::Held,
    /// The chain [`chain`](AuditStore::chain) answered last.
    chain: Option<String>,
}

impl PgAuditStore {
    /// The store on `database`, writing into the chain of the lease `held` names.
    #[must_use]
    pub fn new(database: Arc<Database>, held: crate::instance::Held) -> Self {
        PgAuditStore {
            database,
            held,
            chain: None,
        }
    }

    fn write(&mut self, seq: u64, lines: &[&str], closing: bool) -> Result<(), String> {
        let Some(chain) = self.chain.clone() else {
            return Err("no lease is held, so there is no chain to write into".to_string());
        };
        let lines: Vec<String> = lines.iter().map(|line| (*line).to_string()).collect();
        let first = super::signed(seq);
        self.database
            .run_writing(lines.len(), move |pool| async move {
                let mut client = client(&pool).await?;
                let transaction = client.transaction().await.map_err(sql)?;
                let closed = transaction
                    .query_opt(
                        "SELECT closed_hash FROM audit_chains WHERE id = $1::text::uuid \
                         FOR UPDATE",
                        &[&chain],
                    )
                    .await
                    .map_err(sql)?;
                let count = i64::try_from(lines.len()).unwrap_or(i64::MAX);
                match closed.map(|row| row.get::<_, Option<String>>(0)) {
                    None => return Err(format!("the audit chain {chain} does not exist")),
                    Some(Some(_)) => return Err(format!("the audit chain {chain} is closed")),
                    Some(None) => {}
                }
                let live = transaction
                    .query_opt(
                        "SELECT 1 FROM leases WHERE instance_id = $1::text::uuid \
                         AND expires_at > now()",
                        &[&chain],
                    )
                    .await
                    .map_err(sql)?;
                if live.is_none() {
                    return Err(format!(
                        "the lease of the audit chain {chain} is no longer held"
                    ));
                }
                let last: i64 = transaction
                    .query_one(
                        "SELECT coalesce(max(seq), 0) FROM audit_entries \
                         WHERE chain = $1::text::uuid",
                        &[&chain],
                    )
                    .await
                    .map_err(sql)?
                    .get(0);
                // A write whose answer was lost may have landed, and lines queued since joined the
                // batch: the lines already stored are the batch's first ones, and only the rest is
                // written.
                let landed = last - (first - 1);
                if !(0..=count).contains(&landed) {
                    return Err(format!(
                        "the audit chain {chain} ends at entry {last}, not at {}",
                        first - 1
                    ));
                }
                if landed > 0 {
                    let stored: Option<String> = transaction
                        .query_opt(
                            "SELECT line FROM audit_entries WHERE chain = $1::text::uuid \
                             AND seq = $2",
                            &[&chain, &last],
                        )
                        .await
                        .map_err(sql)?
                        .map(|row| row.get(0));
                    let index = usize::try_from(landed - 1).unwrap_or(usize::MAX);
                    if stored.as_deref() != lines.get(index).map(String::as_str) {
                        return Err(format!(
                            "the audit chain {chain} ends at entry {last} on a line this \
                             instance did not write there, so the database was restored or \
                             changed behind it: run server audit-verify --config to see where \
                             the chain breaks, and restart this instance so that it writes into \
                             a chain of a new lease"
                        ));
                    }
                }
                let all_last = lines.last().cloned();
                let rest = usize::try_from(landed).unwrap_or(usize::MAX);
                let lines = lines[rest..].to_vec();
                let first = first + landed;
                let seqs: Vec<i64> = (first..first + count - landed).collect();
                transaction
                    .execute(
                        "INSERT INTO audit_entries (chain, seq, line) \
                         SELECT $1::text::uuid, seq, line FROM unnest($2::bigint[], $3::text[]) \
                         AS entry (seq, line)",
                        &[&chain, &seqs, &lines],
                    )
                    .await
                    .map_err(sql)?;
                if closing {
                    let hash = all_last.map(|line| line_hash(&line)).unwrap_or_default();
                    transaction
                        .execute(
                            "UPDATE audit_chains SET closed_at = now(), closed_hash = $2 \
                             WHERE id = $1::text::uuid",
                            &[&chain, &hash],
                        )
                        .await
                        .map_err(sql)?;
                }
                transaction.commit().await.map_err(sql)
            })
    }
}

impl AuditStore for PgAuditStore {
    fn chain(&mut self) -> Chain {
        self.chain = self.held.borrow().clone();
        self.chain.clone().map_or(Chain::Unleased, Chain::Lease)
    }

    fn tail(&mut self) -> Result<Option<Tail>, String> {
        let Some(chain) = self.chain.clone() else {
            return Ok(None);
        };
        let last = self.database.run(move |pool| async move {
            let client = client(&pool).await?;
            Ok(client
                .query_opt(
                    "SELECT seq, line FROM audit_entries WHERE chain = $1::text::uuid \
                     ORDER BY seq DESC LIMIT 1",
                    &[&chain],
                )
                .await
                .map_err(sql)?
                .map(|row| (row.get::<_, i64>(0), row.get::<_, String>(1))))
        })?;
        Ok(last.map(|(seq, last_line)| Tail {
            last_line,
            last_seq: super::unsigned(seq),
            torn: false,
        }))
    }

    fn append(&mut self, seq: u64, lines: &[&str]) -> Result<(), String> {
        self.write(seq, lines, false)
    }

    fn close(&mut self, seq: u64, lines: &[&str]) -> Result<(), String> {
        self.write(seq, lines, true)
    }

    fn stored(&mut self, chain: &str, seq: u64) -> Result<Vec<String>, String> {
        let (chain, from) = (chain.to_string(), super::signed(seq));
        self.database.run(move |pool| async move {
            let client = client(&pool).await?;
            Ok(client
                .query(
                    "SELECT line FROM audit_entries WHERE chain = $1::text::uuid AND seq >= $2 \
                     ORDER BY seq",
                    &[&chain, &from],
                )
                .await
                .map_err(sql)?
                .iter()
                .map(|row| row.get(0))
                .collect())
        })
    }

    fn current_bytes(&self) -> u64 {
        0
    }

    fn rotate(&mut self, _keep: usize) -> Result<Vec<(String, String)>, String> {
        Ok(Vec::new())
    }
}

/// A line written into a chain inside a lease's transaction, emitted as the `tracing` copy once
/// that transaction commits.
#[derive(Clone, Debug)]
pub(crate) struct Written {
    seq: u64,
    event: &'static str,
    line: String,
}

impl Written {
    /// Emits the `tracing` copy of the line (ADR-0023 clause 6).
    pub(crate) fn emit(&self) {
        crate::audit_log::emit(self.seq, self.event, "recorded", &self.line);
    }
}

fn now_ms() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

/// Opens the chain of the lease `chain`, held on `host`: its record, and its first entry
/// `audit.chain_opened`, with 64 zeros as `prev`. The first chain the store ever opens names the
/// archive's last entry as its predecessor, where a move from files recorded one.
///
/// `files` is the end of the file record a Server on PostgreSQL kept under `data_dir/audit/`
/// before its record moved into the database ([`Database::offer_archive`]): with no chain and no
/// archive recorded yet, it is recorded as the archive here, as `server migrate-to-postgres`
/// records one, and named as the predecessor.
pub(crate) async fn open_chain(
    transaction: &impl GenericClient,
    chain: &str,
    host: &str,
    files: Option<&(u64, String)>,
) -> Result<Written, String> {
    let first: bool = transaction
        .query_one("SELECT NOT EXISTS (SELECT 1 FROM audit_chains)", &[])
        .await
        .map_err(sql)?
        .get(0);
    let mut entry = Entry::new("audit.chain_opened", "recorded")
        .with("chain", chain)
        .with("host", host);
    if first {
        if let Some((seq, hash)) = files {
            transaction
                .execute(
                    "INSERT INTO audit_archive (seq, hash) VALUES ($1, $2) \
                     ON CONFLICT (singleton) DO NOTHING",
                    &[&super::signed(*seq), hash],
                )
                .await
                .map_err(sql)?;
        }
        if let Some(row) = transaction
            .query_opt("SELECT seq, hash FROM audit_archive", &[])
            .await
            .map_err(sql)?
        {
            entry = entry
                .with("predecessor_seq", super::unsigned(row.get::<_, i64>(0)))
                .with("predecessor_hash", row.get::<_, String>(1));
        }
    }
    let line = render(1, now_ms(), &entry, GENESIS);
    transaction
        .execute(
            "INSERT INTO audit_chains (id, host, opened_at) VALUES ($1::text::uuid, $2, now())",
            &[&chain, &host],
        )
        .await
        .map_err(sql)?;
    transaction
        .execute(
            "INSERT INTO audit_entries (chain, seq, line) VALUES ($1::text::uuid, 1, $2)",
            &[&chain, &line],
        )
        .await
        .map_err(sql)?;
    Ok(Written {
        seq: 1,
        event: "audit.chain_opened",
        line,
    })
}

/// Closes the chain of the lapsed lease `chain` for the instance whose own chain is `closed_by`
/// (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
/// clause 18): `audit.chain_closed` with `reason` `lease_lapsed` after its last entry, and that
/// entry's hash and the time in its record. A chain already closed, or a lease that opened none, is
/// left as it is.
pub(crate) async fn close_lapsed(
    transaction: &impl GenericClient,
    chain: &str,
    closed_by: &str,
) -> Result<Option<Written>, String> {
    let open = transaction
        .query_opt(
            "SELECT closed_hash IS NULL FROM audit_chains WHERE id = $1::text::uuid FOR UPDATE",
            &[&chain],
        )
        .await
        .map_err(sql)?
        .is_some_and(|row| row.get::<_, bool>(0));
    if !open {
        return Ok(None);
    }
    let (seq, prev) = transaction
        .query_opt(
            "SELECT seq, line FROM audit_entries WHERE chain = $1::text::uuid \
             ORDER BY seq DESC LIMIT 1",
            &[&chain],
        )
        .await
        .map_err(sql)?
        .map_or((0, GENESIS.to_string()), |row| {
            (row.get::<_, i64>(0), line_hash(&row.get::<_, String>(1)))
        });
    let entry = Entry::new("audit.chain_closed", "recorded")
        .with("chain", chain)
        .with("reason", "lease_lapsed")
        .with("closed_by", closed_by);
    let line = render(super::unsigned(seq + 1), now_ms(), &entry, &prev);
    transaction
        .execute(
            "INSERT INTO audit_entries (chain, seq, line) VALUES ($1::text::uuid, $2, $3)",
            &[&chain, &(seq + 1), &line],
        )
        .await
        .map_err(sql)?;
    transaction
        .execute(
            "UPDATE audit_chains SET closed_at = now(), closed_hash = $2 WHERE id = $1::text::uuid",
            &[&chain, &line_hash(&line)],
        )
        .await
        .map_err(sql)?;
    Ok(Some(Written {
        seq: super::unsigned(seq + 1),
        event: "audit.chain_closed",
        line,
    }))
}

/// What `server audit-verify --config` found: a line for each chain and for the predecessor, and
/// whether anything failed.
#[derive(Debug, Default)]
pub struct Verified {
    pub lines: Vec<String>,
    pub failed: bool,
}

/// Walks every chain of the record on the database `config` names, with the Server's own
/// credentials and without changing anything
/// (ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance clauses 5 and 6),
/// and, with `archive`, checks that the file record there ends on the entry the first chain names
/// as its predecessor.
///
/// # Errors
/// Returns an error when the database cannot be reached or read.
pub fn verify_on(config: &PostgresConfig, archive: Option<&Path>) -> Result<Verified, String> {
    verify_in(config, None, archive)
}

/// [`verify_on`] in the schema `schema`: what the tests work in.
///
/// # Errors
/// As [`verify_on`].
#[doc(hidden)]
pub fn verify_in(
    config: &PostgresConfig,
    schema: Option<&str>,
    archive: Option<&Path>,
) -> Result<Verified, String> {
    let database = Database::connect_as(config, schema, CALL_TIMEOUT, Exclusive::Shared)?;
    verify(&database, archive)
}

/// What one snapshot of the record holds, walked: each chain with its host and what its walk
/// found, the archive the database records, and the chain that names a predecessor.
struct Snapshot {
    chains: Vec<(String, String, Result<crate::audit_log::Walked, String>)>,
    archive: Option<(u64, String)>,
    named: Option<(String, (u64, String))>,
}

/// Walks every chain on `database`; see [`verify_on`]. The chain records and every entry are read
/// in one read-only snapshot, so a chain an instance closes meanwhile is seen either open or
/// closed, never half of each.
///
/// # Errors
/// As [`verify_on`].
pub fn verify(database: &Database, archive: Option<&Path>) -> Result<Verified, String> {
    let snapshot = database.run_within(super::IMPORT_TIMEOUT, |pool| async move {
        let mut client = client(&pool).await?;
        let transaction = client
            .build_transaction()
            .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
            .read_only(true)
            .start()
            .await
            .map_err(sql)?;
        let records: Vec<(String, String, Option<String>)> = transaction
            .query(
                "SELECT id::text, host, closed_hash FROM audit_chains ORDER BY opened_at, id",
                &[],
            )
            .await
            .map_err(sql)?
            .iter()
            .map(|row| (row.get(0), row.get(1), row.get(2)))
            .collect();
        let mut chains = Vec::new();
        for (chain, host, closed_hash) in records {
            let walked = walk(&*transaction, &chain)
                .await?
                .and_then(|walk| walk.finish(closed_hash.as_deref()));
            chains.push((chain, host, walked));
        }
        let archive = transaction
            .query_opt("SELECT seq, hash FROM audit_archive", &[])
            .await
            .map_err(sql)?
            .map(|row| {
                (
                    super::unsigned(row.get::<_, i64>(0)),
                    row.get::<_, String>(1),
                )
            });
        // Read from the first entries themselves, so a chain whose walk fails still says what it
        // names.
        let mut named = None;
        for row in transaction
            .query(
                "SELECT chain::text, line FROM audit_entries WHERE seq = 1 \
                 AND line LIKE '%\"predecessor_hash\"%'",
                &[],
            )
            .await
            .map_err(sql)?
        {
            let line: String = row.get(1);
            let value: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
            if let (Some(seq), Some(hash)) = (
                value
                    .get("predecessor_seq")
                    .and_then(serde_json::Value::as_u64),
                value
                    .get("predecessor_hash")
                    .and_then(serde_json::Value::as_str),
            ) {
                named = Some((row.get::<_, String>(0), (seq, hash.to_string())));
            }
        }
        transaction.commit().await.map_err(sql)?;
        Ok(Snapshot {
            chains,
            archive,
            named,
        })
    })?;
    let mut verified = Verified::default();
    let mut open = 0;
    for (chain, host, walked) in &snapshot.chains {
        match walked {
            Ok(walked) if walked.closed => verified.lines.push(format!(
                "chain {chain} on {host}: {} entries, closed",
                walked.entries
            )),
            Ok(walked) => {
                open += 1;
                verified.lines.push(format!(
                    "chain {chain} on {host}: {} entries, open — its last entry is {} {} at {}",
                    walked.entries, walked.last_seq, walked.last_event, walked.last_time
                ));
            }
            Err(e) => {
                verified.failed = true;
                verified.lines.push(format!("the chain breaks: {e}"));
            }
        }
    }
    match (&snapshot.named, &snapshot.archive) {
        (None, Some((seq, _))) => {
            verified.failed = true;
            verified.lines.push(format!(
                "the database records an archive that ends on entry {seq}, and no chain names it \
                 as its predecessor — the first chain is gone"
            ));
        }
        (Some((chain, named)), Some(recorded)) if named != recorded => {
            verified.failed = true;
            verified.lines.push(format!(
                "chain {chain} names entry {} as its predecessor, and the database records the \
                 archive's end as entry {}",
                named.0, recorded.0
            ));
        }
        _ => {}
    }
    match (snapshot.named, archive) {
        (None, Some(dir)) => {
            verified.failed = true;
            verified.lines.push(format!(
                "no chain names a predecessor, so nothing links the record to {}",
                dir.display()
            ));
        }
        (None, None) => {}
        (Some((chain, (seq, _))), None) => verified.lines.push(format!(
            "the predecessor chain {chain} names, entry {seq} of the archive, was not checked — \
             name the archive with --archive <dir>"
        )),
        (Some((chain, (seq, hash))), Some(dir)) => match archive_end(dir) {
            Ok(Some((end_seq, end_hash))) if end_seq == seq && end_hash == hash => {
                verified.lines.push(format!(
                    "the archive {} holds, and ends on entry {seq}, the predecessor chain {chain} \
                     names",
                    dir.display()
                ));
            }
            Ok(end) => {
                verified.failed = true;
                verified.lines.push(format!(
                    "the predecessor chain {chain} names, entry {seq} with hash {hash}, is not \
                     where the archive {} ends ({})",
                    dir.display(),
                    end.map_or("it holds no entry".to_string(), |(seq, hash)| format!(
                        "entry {seq} with hash {hash}"
                    ))
                ));
            }
            Err(e) => {
                verified.failed = true;
                verified.lines.push(format!(
                    "the predecessor chain {chain} names, entry {seq}, cannot be checked: {e}"
                ));
            }
        },
    }
    verified.lines.push(format!(
        "{} chains, {open} open: {}",
        snapshot.chains.len(),
        if verified.failed {
            "the record does not hold"
        } else {
            "the record holds"
        }
    ));
    Ok(verified)
}

/// Walks the entries of `chain` in `seq` order, a page at a time, in the snapshot `transaction`
/// reads. The outer error is the database's; the inner one the chain's first entry that fails.
async fn walk(
    transaction: &impl GenericClient,
    chain: &str,
) -> Result<Result<ChainWalk, String>, String> {
    let mut walk = ChainWalk::new(chain);
    let mut after = 0i64;
    loop {
        let page = transaction
            .query(
                "SELECT seq, line FROM audit_entries WHERE chain = $1::text::uuid \
                 AND seq > $2 ORDER BY seq LIMIT $3",
                &[&chain, &after, &PAGE],
            )
            .await
            .map_err(sql)?;
        let Some(last) = page.last() else {
            return Ok(Ok(walk));
        };
        after = last.get(0);
        for row in &page {
            if let Err(e) = walk.step(super::unsigned(row.get::<_, i64>(0)), row.get(1)) {
                return Ok(Err(e));
            }
        }
    }
}

/// Offers the file record in `dir`, a Server's `data_dir/audit/` from before its record moved into
/// the database, as the archive the first chain names
/// (ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance clause 6), while
/// the database holds no chain; once it holds one, the offer would be moot, and `dir` is not read.
///
/// # Errors
/// As [`archive_end`], and when the database cannot be asked whether it holds a chain.
pub fn offer_archive_in(database: &Database, dir: &Path) -> Result<(), String> {
    let first = database.run(|pool| async move {
        let client = client(&pool).await?;
        let row = client
            .query_one("SELECT NOT EXISTS (SELECT 1 FROM audit_chains)", &[])
            .await
            .map_err(sql)?;
        Ok(row.get::<_, bool>(0))
    })?;
    if first {
        database.offer_archive(archive_end(dir)?);
    }
    Ok(())
}

/// The `seq` and the hash of the last entry of the file record in `dir`, once its whole chain
/// holds: what a move from files records as the archive's end, and what `audit-verify --archive`
/// checks the predecessor against. `None` when `dir` does not exist or holds no entry.
///
/// # Errors
/// Returns an error when the files cannot be read, when the chain breaks, and when the record ends
/// in a torn line — a crash mid-write — naming the file: an end recorded there could never be
/// verified.
pub fn archive_end(dir: &Path) -> Result<Option<(u64, String)>, String> {
    if !dir.is_dir() {
        return Ok(None);
    }
    let files = crate::fs::audit_files(dir)?;
    let mut texts = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file)
            .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        texts.push((file.display().to_string(), text));
    }
    let Some(tail) = crate::fs::audit_tail(dir)? else {
        return Ok(None);
    };
    if tail.torn {
        let newest = files
            .iter()
            .rev()
            .find(|file| std::fs::metadata(file).is_ok_and(|meta| meta.len() > 0))
            .map_or_else(
                || dir.display().to_string(),
                |file| file.display().to_string(),
            );
        return Err(format!(
            "the audit record in {} ends in a torn line, in {newest}: a crash cut it mid-write, \
             and an end recorded there could never be verified — keep a copy of the file, cut \
             the torn line from it so the record ends on its last whole entry, and try again",
            dir.display()
        ));
    }
    crate::audit_log::verify(texts.iter().flat_map(|(name, text)| {
        text.lines()
            .enumerate()
            .filter(|(_, line)| !line.trim().is_empty())
            .map(move |(index, line)| (format!("{name}:{}", index + 1), line))
    }))
    .map_err(|e| format!("the archive's chain breaks at {e}"))?;
    Ok(Some((tail.last_seq, line_hash(&tail.last_line))))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::sync::watch;

    use super::super::testing::TestDatabase;
    use super::*;
    use crate::audit::Audit as _;
    use crate::audit_log::{AuditLog, Limits};

    const LAPSE: Duration = Duration::from_secs(30);

    fn shared(test: &TestDatabase) -> Arc<Database> {
        Database::open_in(&test.config(), Exclusive::Shared, Some(&test.schema)).expect("shared")
    }

    /// An instance as the Server composes its record: a lease, and the writer on its chain.
    struct Writing {
        database: Arc<Database>,
        held: watch::Sender<Option<String>>,
        audit: AuditLog,
        chain: String,
    }

    fn writing(test: &TestDatabase, host: &str) -> Writing {
        let database = shared(test);
        let chain = database.take_lease(host, LAPSE).expect("lease").instance_id;
        let (held, watched) = watch::channel(Some(chain.clone()));
        let audit = AuditLog::start_under(
            Box::new(PgAuditStore::new(database.clone(), watched.clone())),
            Limits::default(),
            Arc::new(crate::clock::SystemClock),
            watched,
        )
        .expect("start");
        Writing {
            database,
            held,
            audit,
            chain,
        }
    }

    fn lines(test: &TestDatabase, chain: &str) -> Vec<String> {
        test.rows(&format!(
            "SELECT line FROM audit_entries WHERE chain = '{chain}' ORDER BY seq"
        ))
        .into_iter()
        .map(|row| row[0].clone().expect("line"))
        .collect()
    }

    fn lapse(test: &TestDatabase, id: &str) {
        test.sql(&format!(
            "UPDATE leases SET expires_at = now() WHERE instance_id = '{id}'"
        ));
    }

    fn admitted(n: u32) -> Entry {
        Entry::new("admission.admitted", "admitted").with("n", n)
    }

    /// Two instances on one database each write their own chain, every entry naming it, and
    /// `audit-verify` passes over both and lists both as open.
    /// Verifies: ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance
    #[test]
    fn two_instances_each_write_their_own_chain_and_both_verify() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let (a, b) = (writing(&test, "host-a"), writing(&test, "host-b"));
        for n in 0..3 {
            a.audit.record(admitted(n)).expect("record");
            b.audit.record(admitted(n)).expect("record");
        }
        assert!(a.audit.drain(Duration::from_secs(10)));
        assert!(b.audit.drain(Duration::from_secs(10)));
        for writer in [&a, &b] {
            let chain = lines(&test, &writer.chain);
            assert_eq!(chain.len(), 4, "{chain:#?}");
            assert!(chain[0].contains("audit.chain_opened"), "{}", chain[0]);
            for line in &chain {
                assert!(
                    line.contains(&format!("\"chain\":\"{}\"", writer.chain)),
                    "{line}"
                );
            }
        }
        let verified = verify(&a.database, None).expect("verify");
        assert!(!verified.failed, "{:?}", verified.lines);
        for writer in [&a, &b] {
            assert!(
                verified
                    .lines
                    .iter()
                    .any(|line| line.contains(&writer.chain) && line.contains("4 entries, open")),
                "{:?}",
                verified.lines
            );
        }
        assert!(
            verified
                .lines
                .iter()
                .any(|line| line == "2 chains, 2 open: the record holds"),
            "{:?}",
            verified.lines
        );
    }

    /// An orderly shutdown closes the chain with `audit.chain_closed`, `reason` `shutdown`, and
    /// its record holds that entry's hash.
    /// Verifies: ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance
    #[test]
    fn an_orderly_shutdown_closes_the_chain() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let a = writing(&test, "host-a");
        a.audit.record(admitted(1)).expect("record");
        assert!(
            a.audit.close(Duration::from_secs(10)),
            "the chain was not closed"
        );
        let chain = lines(&test, &a.chain);
        let last = chain.last().expect("entries");
        assert!(last.contains("audit.chain_closed"), "{last}");
        assert!(last.contains("\"reason\":\"shutdown\""), "{last}");
        assert_eq!(
            test.rows(&format!(
                "SELECT closed_hash FROM audit_chains WHERE id = '{}'",
                a.chain
            )),
            vec![vec![Some(line_hash(last))]]
        );
        let verified = verify(&a.database, None).expect("verify");
        assert!(!verified.failed, "{:?}", verified.lines);
        assert!(
            verified
                .lines
                .iter()
                .any(|line| line.contains("3 entries, closed")),
            "{:?}",
            verified.lines
        );
    }

    /// An instance killed without a shutdown leaves its chain open; the instance that takes over
    /// its lapsed lease, at a take or at a renewal, closes it with `audit.chain_closed`, `reason`
    /// `lease_lapsed` and `closed_by` its own chain, and from then on the store refuses the dead
    /// chain an entry. Two instances that try at once take it over once.
    /// Verifies: ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance, ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn the_instance_that_takes_over_a_lapsed_lease_closes_its_chain() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let dead = writing(&test, "host-a");
        dead.audit.record(admitted(1)).expect("record");
        assert!(dead.audit.drain(Duration::from_secs(10)));
        let Writing {
            database: dead_database,
            held: dead_held,
            audit,
            chain: dead_chain,
        } = dead;
        drop(audit);
        lapse(&test, &dead_chain);

        // The next take closes it.
        let (b, c) = (writing(&test, "host-b"), writing(&test, "host-c"));
        let closed_at_take = lines(&test, &dead_chain);
        assert!(
            closed_at_take
                .last()
                .is_some_and(|line| line.contains("audit.chain_closed")),
            "a take closes a lapsed lease's chain: {closed_at_take:#?}"
        );

        // Another instance killed: the two that run race for its lease at their renewals.
        let gone = writing(&test, "host-d");
        gone.audit.record(admitted(2)).expect("record");
        assert!(gone.audit.drain(Duration::from_secs(10)));
        let gone_chain = gone.chain.clone();
        drop(gone);
        lapse(&test, &gone_chain);
        let (bd, cd) = (b.database.clone(), c.database.clone());
        let (b_chain, c_chain) = (b.chain.clone(), c.chain.clone());
        let racing = std::thread::spawn(move || bd.take_over_lapsed(&b_chain, LAPSE));
        let other = cd.take_over_lapsed(&c_chain, LAPSE).expect("c");
        let one = racing.join().expect("b").expect("b");
        let mut taken: Vec<String> = one.iter().chain(other.iter()).cloned().collect();
        taken.sort();
        assert_eq!(taken, vec![gone_chain.clone()], "taken over once");
        let chain = lines(&test, &gone_chain);
        let closings: Vec<&String> = chain
            .iter()
            .filter(|line| line.contains("audit.chain_closed"))
            .collect();
        assert_eq!(closings.len(), 1, "{chain:#?}");
        let closing = closings[0];
        assert!(closing.contains("\"reason\":\"lease_lapsed\""), "{closing}");
        let by = if one.is_empty() { &c.chain } else { &b.chain };
        assert!(
            closing.contains(&format!("\"closed_by\":\"{by}\"")),
            "{closing}"
        );
        assert_eq!(
            chain.last(),
            Some(closing),
            "the closing entry ends the chain"
        );

        // The cut-off instance cannot extend its chain.
        let mut store = PgAuditStore::new(dead_database, dead_held.subscribe());
        assert_eq!(store.chain(), Chain::Lease(dead_chain.clone()));
        let err = store
            .append(4, &["{}"])
            .expect_err("a closed chain takes no entry");
        assert!(err.contains("is closed"), "{err}");

        let verified = verify(&b.database, None).expect("verify");
        assert!(!verified.failed, "{:?}", verified.lines);
    }

    /// Entries queued under a lease that lapses before they are written go into the chain of the
    /// instance's next lease, after its `audit.chain_opened`, each marked `taken_under` the old
    /// chain; the old chain, closed by the take, holds none of them, and both chains verify.
    /// Verifies: ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance
    #[test]
    fn entries_queued_when_a_lease_lapses_land_in_the_next_chain_marked_with_the_old() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let a = writing(&test, "host-a");
        a.audit.record(admitted(1)).expect("record");
        assert!(a.audit.drain(Duration::from_secs(10)));
        let old = a.chain.clone();

        // The lease lapses before the instance noticed: what it queued now cannot be written.
        lapse(&test, &old);
        a.audit.record(admitted(2)).expect("queued");
        assert!(
            !a.audit.drain(Duration::from_secs(10)),
            "a write under a lapsed lease landed"
        );
        assert_eq!(lines(&test, &old).len(), 2, "the old chain took an entry");

        // The instance takes its next lease, which closes the old chain.
        let new = a
            .database
            .take_lease("host-a", LAPSE)
            .expect("new lease")
            .instance_id;
        a.held.send_replace(Some(new.clone()));
        assert!(
            a.audit.drain(Duration::from_secs(10)),
            "the kept entry was not written"
        );
        let chain = lines(&test, &new);
        assert!(chain[0].contains("audit.chain_opened"), "{chain:#?}");
        let moved = &chain[1];
        assert!(moved.contains("\"n\":2"), "{moved}");
        assert!(
            moved.contains(&format!("\"taken_under\":\"{old}\"")),
            "{moved}"
        );
        assert!(moved.contains(&format!("\"chain\":\"{new}\"")), "{moved}");
        let old_chain = lines(&test, &old);
        assert_eq!(old_chain.len(), 3, "{old_chain:#?}");
        assert!(old_chain[2].contains("lease_lapsed"), "{}", old_chain[2]);
        assert!(!old_chain.iter().any(|line| line.contains("\"n\":2")));

        // Decided under the new lease: no mark.
        a.audit.record(admitted(3)).expect("record");
        assert!(a.audit.drain(Duration::from_secs(10)));
        let chain = lines(&test, &new);
        let last = chain.last().expect("entries");
        assert!(
            last.contains("\"n\":3") && !last.contains("taken_under"),
            "{last}"
        );

        let verified = verify(&a.database, None).expect("verify");
        assert!(!verified.failed, "{:?}", verified.lines);
    }

    /// An entry removed from the middle of a chain fails `audit-verify`, which names the chain and
    /// the entry that no longer follows; the other chains are still walked.
    /// Verifies: ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance
    #[test]
    fn an_entry_removed_from_the_middle_of_a_chain_fails_naming_it() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let (a, b) = (writing(&test, "host-a"), writing(&test, "host-b"));
        for n in 0..4 {
            a.audit.record(admitted(n)).expect("record");
        }
        b.audit.record(admitted(0)).expect("record");
        assert!(a.audit.drain(Duration::from_secs(10)));
        assert!(b.audit.drain(Duration::from_secs(10)));
        test.sql(&format!(
            "DELETE FROM audit_entries WHERE chain = '{}' AND seq = 3",
            a.chain
        ));
        let verified = verify(&a.database, None).expect("verify");
        assert!(verified.failed, "{:?}", verified.lines);
        assert!(
            verified.lines.iter().any(|line| line
                .contains(&format!("chain {}, entry 4:", a.chain))
                && line.contains("missing")),
            "{:?}",
            verified.lines
        );
        assert!(
            verified
                .lines
                .iter()
                .any(|line| line.contains(&b.chain) && line.contains("open")),
            "{:?}",
            verified.lines
        );
    }

    /// The audit files of `root`, written as a Server on the filesystem record writes them.
    fn file_record(root: &Path, entries: u32) {
        let audit = AuditLog::start(
            Box::new(crate::fs::FsAuditStore::open(root).expect("open")),
            Limits::default(),
            Arc::new(crate::clock::SystemClock),
        )
        .expect("audit");
        for n in 0..entries {
            audit.record(admitted(n)).expect("record");
        }
        assert!(audit.drain(Duration::from_secs(10)));
    }

    /// A Server that was on PostgreSQL before its record moved into the database kept it in
    /// `data_dir/audit/`: at its first start the take that opens the first chain records those
    /// files' last entry as the archive and names it as the predecessor, so `audit-verify
    /// --archive` checks the link. A later chain names none, and an archive recorded already is
    /// kept.
    /// Verifies: ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance
    #[test]
    fn the_first_chain_names_the_files_a_server_on_postgresql_wrote_before() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        file_record(dir.path(), 3);
        let archive = dir.path().join(crate::fs::AUDIT);
        let end = archive_end(&archive).expect("tail").expect("entries");
        assert_eq!(end.0, 3);

        let database = shared(&test);
        database.offer_archive(Some(end.clone()));
        let first = database.take_lease("host-a", LAPSE).expect("lease");
        assert_eq!(
            test.rows("SELECT seq::text, hash FROM audit_archive"),
            vec![vec![Some("3".to_string()), Some(end.1.clone())]]
        );
        let opened = lines(&test, &first.instance_id).remove(0);
        assert!(opened.contains("\"predecessor_seq\":3"), "{opened}");
        assert!(opened.contains(&end.1), "{opened}");
        let verified = verify(&database, Some(&archive)).expect("verify");
        assert!(!verified.failed, "{:?}", verified.lines);

        // Offered again by a later start, over files that went on: nothing more is recorded.
        file_record(dir.path(), 1);
        database.offer_archive(archive_end(&archive).expect("tail"));
        let later = database.take_lease("host-a", LAPSE).expect("lease");
        assert!(!lines(&test, &later.instance_id)[0].contains("predecessor"));
        assert_eq!(
            test.rows("SELECT seq::text FROM audit_archive"),
            vec![vec![Some("3".to_string())]]
        );
    }

    /// A first start with an empty or no `data_dir/audit/` records no archive and names no
    /// predecessor.
    /// Verifies: ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance
    #[test]
    fn a_first_start_without_audit_files_records_no_archive() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            archive_end(&dir.path().join("missing")).expect("missing"),
            None
        );
        std::fs::create_dir(dir.path().join("empty")).expect("mkdir");
        assert_eq!(archive_end(&dir.path().join("empty")).expect("empty"), None);
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = shared(&test);
        database.offer_archive(archive_end(&dir.path().join("empty")).expect("empty"));
        let first = database.take_lease("host-a", LAPSE).expect("lease");
        assert!(test.rows("SELECT seq FROM audit_archive").is_empty());
        assert!(!lines(&test, &first.instance_id)[0].contains("predecessor"));
    }

    /// A store whose first append lands in the database and then answers an error, as a commit
    /// whose answer was lost does. It says when the lines landed, and answers only once the test
    /// lets it: a gate, so what the test does in between happens while the answer is awaited.
    struct LostAnswer {
        inner: PgAuditStore,
        landed: Option<std::sync::mpsc::Sender<()>>,
        gate: std::sync::mpsc::Receiver<()>,
    }

    impl AuditStore for LostAnswer {
        fn chain(&mut self) -> Chain {
            self.inner.chain()
        }
        fn tail(&mut self) -> Result<Option<Tail>, String> {
            self.inner.tail()
        }
        fn append(&mut self, seq: u64, lines: &[&str]) -> Result<(), String> {
            self.inner.append(seq, lines)?;
            let Some(landed) = self.landed.take() else {
                return Ok(());
            };
            landed.send(()).expect("the test listens");
            self.gate
                .recv_timeout(Duration::from_secs(10))
                .expect("the test opens the gate");
            Err("the commit's answer was lost".to_string())
        }
        fn close(&mut self, seq: u64, lines: &[&str]) -> Result<(), String> {
            self.inner.close(seq, lines)
        }
        fn stored(&mut self, chain: &str, seq: u64) -> Result<Vec<String>, String> {
            self.inner.stored(chain, seq)
        }
        fn current_bytes(&self) -> u64 {
            0
        }
        fn rotate(&mut self, _keep: usize) -> Result<Vec<(String, String)>, String> {
            Ok(Vec::new())
        }
    }

    /// An instance whose writer writes through [`LostAnswer`]; the channels say when the first
    /// write landed and let its answer go.
    fn losing(
        test: &TestDatabase,
    ) -> (
        Writing,
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::Sender<()>,
    ) {
        let database = shared(test);
        let chain = database
            .take_lease("host-a", LAPSE)
            .expect("lease")
            .instance_id;
        let (held, watched) = watch::channel(Some(chain.clone()));
        let (landed, has_landed) = std::sync::mpsc::channel();
        let (open, gate) = std::sync::mpsc::channel();
        let audit = AuditLog::start_under(
            Box::new(LostAnswer {
                inner: PgAuditStore::new(database.clone(), watched.clone()),
                landed: Some(landed),
                gate,
            }),
            Limits::default(),
            Arc::new(crate::clock::SystemClock),
            watched,
        )
        .expect("start");
        (
            Writing {
                database,
                held,
                audit,
                chain,
            },
            has_landed,
            open,
        )
    }

    fn count(lines: &[String], needle: &str) -> usize {
        lines.iter().filter(|line| line.contains(needle)).count()
    }

    /// A write that landed although its answer was lost is recognised as landed: alone, it is not
    /// written again, and with an entry queued while the answer was awaited, only that entry is
    /// written after it. The writer recovers, and each decision is in the chain once.
    /// Verifies: ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance
    #[test]
    fn a_write_whose_answer_was_lost_is_not_written_again() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        // Alone.
        let (a, landed, open) = losing(&test);
        a.audit.record(admitted(1)).expect("record");
        landed
            .recv_timeout(Duration::from_secs(10))
            .expect("landed");
        open.send(()).expect("open");
        assert!(
            a.audit.drain(Duration::from_secs(10)),
            "the writer did not recover"
        );
        let chain = lines(&test, &a.chain);
        assert_eq!(count(&chain, "\"n\":1"), 1, "{chain:#?}");

        // With an entry queued while the answer was awaited.
        let (b, landed, open) = losing(&test);
        b.audit.record(admitted(1)).expect("record");
        landed
            .recv_timeout(Duration::from_secs(10))
            .expect("landed");
        b.audit
            .record(admitted(2))
            .expect("queued while the answer is awaited");
        open.send(()).expect("open");
        assert!(
            b.audit.drain(Duration::from_secs(10)),
            "the writer stayed failing"
        );
        let chain = lines(&test, &b.chain);
        assert_eq!(count(&chain, "\"n\":1"), 1, "{chain:#?}");
        assert_eq!(count(&chain, "\"n\":2"), 1, "{chain:#?}");
        let verified = verify(&b.database, None).expect("verify");
        assert!(!verified.failed, "{:?}", verified.lines);
    }

    /// A write into a chain whose answer was lost, and then a new lease: the lines that landed in
    /// the old chain stay there alone, and only the rest is written into the new one, so no
    /// decision is recorded twice.
    /// Verifies: ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance
    #[test]
    fn a_landed_write_is_not_written_again_into_the_next_chain() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let (a, landed, open) = losing(&test);
        a.audit.record(admitted(1)).expect("record");
        landed
            .recv_timeout(Duration::from_secs(10))
            .expect("landed");
        lapse(&test, &a.chain);
        let next = a
            .database
            .take_lease("host-a", LAPSE)
            .expect("new lease")
            .instance_id;
        a.held.send_replace(Some(next.clone()));
        open.send(()).expect("open");
        assert!(
            a.audit.drain(Duration::from_secs(10)),
            "the writer stayed failing"
        );
        a.audit.record(admitted(2)).expect("record");
        assert!(a.audit.drain(Duration::from_secs(10)));
        let (old, new) = (lines(&test, &a.chain), lines(&test, &next));
        assert_eq!(count(&old, "\"n\":1"), 1, "{old:#?}");
        assert_eq!(count(&new, "\"n\":1"), 0, "written twice: {new:#?}");
        assert_eq!(count(&new, "\"n\":2"), 1, "{new:#?}");
        let verified = verify(&a.database, None).expect("verify");
        assert!(!verified.failed, "{:?}", verified.lines);
    }

    /// A writer that reaches its chain while a take is closing it waits for the take's
    /// transaction, on the chain's row, and then finds the chain closed: it never writes past the
    /// closing entry. The take's transaction is held open by the test until the writer is seen
    /// waiting.
    /// Verifies: ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance
    #[test]
    fn a_writer_waits_for_a_closing_take_and_then_finds_the_chain_closed() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let a = writing(&test, "host-a");
        let chain = a.chain.clone();
        let closer = shared(&test);
        let (locked, is_locked) = std::sync::mpsc::channel();
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let closing = {
            let chain = chain.clone();
            std::thread::spawn(move || {
                closer.run_within(Duration::from_secs(30), move |pool| async move {
                    let mut client = client(&pool).await?;
                    let transaction = client.transaction().await.map_err(sql)?;
                    close_lapsed(&*transaction, &chain, "test-closer")
                        .await?
                        .expect("an open chain");
                    locked.send(()).expect("the test listens");
                    released.await.map_err(|e| e.to_string())?;
                    transaction.commit().await.map_err(sql)
                })
            })
        };
        is_locked
            .recv_timeout(Duration::from_secs(10))
            .expect("the take closed the chain in its transaction");
        let (_held, watched) = watch::channel(Some(chain.clone()));
        let database = a.database.clone();
        let writer = std::thread::spawn(move || {
            let mut store = PgAuditStore::new(database, watched);
            store.chain();
            let line = render(2, 0, &admitted(1).with("chain", chain.as_str()), "unused");
            store.append(2, &[&line])
        });
        let waiting = || {
            test.rows(
                "SELECT count(*) FROM pg_stat_activity WHERE wait_event_type = 'Lock' \
                 AND query LIKE 'SELECT closed_hash FROM audit_chains%FOR UPDATE'",
            )[0][0]
                .as_deref()
                != Some("0")
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(4);
        while !waiting() {
            assert!(
                std::time::Instant::now() < deadline,
                "the writer did not wait for the chain's row"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        release.send(()).expect("release");
        closing.join().expect("closer").expect("the take committed");
        let err = writer
            .join()
            .expect("writer")
            .expect_err("a chain closed meanwhile takes no entry");
        assert!(err.contains("is closed"), "{err}");
        let chain = lines(&test, &a.chain);
        assert!(
            chain
                .last()
                .expect("entries")
                .contains("audit.chain_closed"),
            "{chain:#?}"
        );
    }

    /// An archive that ends in a torn line, a crash mid-write, is refused, naming the file, rather
    /// than recorded as an end that could never verify; one whose chain breaks is refused too.
    /// Verifies: ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance
    #[test]
    fn an_archive_that_ends_in_a_torn_line_is_refused_naming_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        file_record(dir.path(), 3);
        let archive = dir.path().join(crate::fs::AUDIT);
        assert!(archive_end(&archive).expect("whole").is_some());
        let file = crate::fs::audit_files(&archive)
            .expect("files")
            .pop()
            .expect("a file");
        let mut text = std::fs::read_to_string(&file).expect("read");
        let whole = text.clone();
        text.push_str("{\"seq\":4,\"pr");
        std::fs::write(&file, &text).expect("write");
        let err = archive_end(&archive).expect_err("a torn end");
        assert!(
            err.contains("torn") && err.contains(&file.display().to_string()),
            "{err}"
        );

        let broken = whole.replacen("\"n\":1", "\"n\":7", 1);
        std::fs::write(&file, broken).expect("write");
        let err = archive_end(&archive).expect_err("a broken chain");
        assert!(err.contains("chain breaks"), "{err}");
    }

    /// An archive the file store repaired after a crash (the torn line kept, then
    /// `audit.chain_broken` chained on it) is taken as the archive and its link verifies; only a
    /// torn line still at the end is refused.
    /// Verifies: ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance
    #[test]
    fn an_archive_the_file_store_repaired_after_a_crash_is_linked() {
        let dir = tempfile::tempdir().expect("tempdir");
        file_record(dir.path(), 3);
        let archive = dir.path().join(crate::fs::AUDIT);
        let file = crate::fs::audit_files(&archive)
            .expect("files")
            .pop()
            .expect("a file");
        // A crash mid-write: the last entry is cut short.
        let text = std::fs::read_to_string(&file).expect("read");
        std::fs::write(&file, &text[..text.len() - 20]).expect("tear");
        assert!(archive_end(&archive).expect_err("torn").contains("torn"));
        // The file store's next start repairs it.
        file_record(dir.path(), 1);
        let repaired = std::fs::read_to_string(&file).expect("read");
        assert!(repaired.contains("audit.chain_broken"), "{repaired}");
        let end = archive_end(&archive)
            .expect("a repaired archive")
            .expect("entries");
        assert_eq!(end.0, 4, "two whole entries, the broken mark and one more");

        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = shared(&test);
        database.offer_archive(Some(end.clone()));
        let first = database.take_lease("host-a", LAPSE).expect("lease");
        assert!(lines(&test, &first.instance_id)[0].contains(&end.1));
        let verified = verify(&database, Some(&archive)).expect("verify");
        assert!(!verified.failed, "{:?}", verified.lines);
    }

    /// A batch whose first lines are stored, but stored differently than this instance wrote
    /// them, is refused, and the refusal says what to do: check the record, and restart onto a
    /// new lease.
    /// Verifies: ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance
    #[test]
    fn a_line_stored_where_this_instance_wrote_another_is_refused_naming_the_next_step() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = shared(&test);
        let chain = database
            .take_lease("host-a", LAPSE)
            .expect("lease")
            .instance_id;
        let (_held, watched) = watch::channel(Some(chain.clone()));
        let mut store = PgAuditStore::new(database, watched);
        store.chain();
        let line = render(1, 0, &admitted(1).with("chain", chain.as_str()), GENESIS);
        let err = store
            .append(1, &[&line])
            .expect_err("entry 1 is chain_opened");
        assert!(
            err.contains("did not write there")
                && err.contains("server audit-verify --config")
                && err.contains("restart this instance"),
            "{err}"
        );
    }

    /// The archive is read and offered only while the database holds no chain: a later start
    /// neither reads `data_dir/audit/` nor stops on it, and the first chain names nothing new.
    /// Verifies: ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance
    #[test]
    fn the_archive_is_offered_only_before_the_first_chain() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        file_record(dir.path(), 3);
        let archive = dir.path().join(crate::fs::AUDIT);
        let file = crate::fs::audit_files(&archive)
            .expect("files")
            .pop()
            .expect("a file");
        let whole = std::fs::read_to_string(&file).expect("read");
        std::fs::write(&file, format!("{whole}{{\"seq\":4,\"pr")).expect("tear");
        let database = shared(&test);
        let err = offer_archive_in(&database, &archive).expect_err("no chain yet: it is read");
        assert!(err.contains("torn"), "{err}");

        std::fs::write(&file, &whole).expect("mend");
        offer_archive_in(&database, &archive).expect("offer");
        assert!(database.archive_offered().is_some());
        database.take_lease("host-a", LAPSE).expect("lease");

        std::fs::write(&file, format!("{whole}{{\"seq\":4,\"pr")).expect("tear");
        database.offer_archive(None);
        offer_archive_in(&database, &archive).expect("a chain exists: it is not read");
        assert_eq!(database.archive_offered(), None);
    }

    /// An archive the database records that no chain names as its predecessor fails
    /// `audit-verify`, with or without `--archive`: the first chain is gone.
    /// Verifies: ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance
    #[test]
    fn an_archive_no_chain_names_fails_verification() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = shared(&test);
        database.take_lease("host-a", LAPSE).expect("lease");
        test.sql("INSERT INTO audit_archive (seq, hash) VALUES (3, 'abc')");
        let verified = verify(&database, None).expect("verify");
        assert!(verified.failed, "{:?}", verified.lines);
        assert!(
            verified
                .lines
                .iter()
                .any(|line| line.contains("no chain names it")),
            "{:?}",
            verified.lines
        );
    }
}
