//! How long the audit record on PostgreSQL goes back
//! (ADR-draft-the-servers-state-and-its-stores clause 12): once a day each instance removes from
//! its own chain, and the one that holds the prune lock from every closed chain, the longest run of
//! entries from the chain's start that are all older than `[audit] retain_days`, by the database's
//! time when each was stored.
//!
//! Before it deletes, the instance records one `audit.pruned` in its own chain for each chain it
//! prunes, naming the last entry it removes, and waits until they are written; `audit-verify` then
//! links what is left to them. Where an entry it removes is itself an `audit.pruned` whose link is
//! still needed, or names the archive as the predecessor, the link and the predecessor are recorded
//! again, so that pruning the record of a prune never cuts a chain loose.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio_postgres::GenericClient;

use super::{client, sql, Database};
use crate::audit::{Audit as _, Entry};
use crate::audit_log::{line_hash, AuditLog};

/// The lock the instance that prunes the closed chains holds for its session, by the key its schema
/// gives it.
pub(crate) const PRUNE_LOCK: &str =
    "hashtextextended('opamp-fleet-server-audit-prune:' || coalesce(current_schema(), ''), 0)";

/// How long one round of a prune may take, its wait for the `audit.pruned` entries included.
const PRUNE_TIMEOUT: Duration = Duration::from_secs(300);

/// The entries one round of a prune removes at most, across its chains: 20 statements of
/// [`DELETE_BATCH`]. A batch by the primary key takes milliseconds; even at a second each, a
/// hundred times that, a round takes 20 s, a fifteenth of [`PRUNE_TIMEOUT`], so a round fits
/// with room for a database under load, and a backlog drains over as many rounds as it needs.
pub(crate) const ROUND_ENTRIES: i64 = 100_000;

/// How long the prune waits for its `audit.pruned` entries to be written.
const WRITTEN_TIMEOUT: Duration = Duration::from_secs(30);

/// The entries one statement of a prune deletes at most: a few milliseconds' work by the primary
/// key, far inside the statement timeout every connection of the Server holds.
const DELETE_BATCH: i64 = 5000;

/// The event an entry recording a prune carries.
pub const PRUNED: &str = "audit.pruned";

/// A chain's entries up to `last_seq`, the entry whose hash is `last_hash`, removed.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Link {
    pub chain: String,
    pub last_seq: u64,
    pub last_hash: String,
}

/// The archive's end, as the first chain names it as its predecessor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Predecessor {
    pub chain: String,
    pub seq: u64,
    pub hash: String,
}

/// What one prune did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pruned {
    /// Each chain it pruned, with the last entry it removed.
    pub chains: Vec<Link>,
    /// The links still needed that it recorded again, because an entry it removed recorded them.
    pub carried: Vec<Link>,
    /// Whether it carried the predecessor on.
    pub predecessor: bool,
    /// Whether it held the prune lock, and so pruned the closed chains.
    pub closed_chains: bool,
    /// Whether it removed as many entries as one round may, so that old entries may be left for
    /// the next round.
    pub capped: bool,
}

/// The `audit.pruned` entries a prune writes, in order: each chain it prunes, then each link it
/// carries on, the predecessor on the first of them.
fn entries(chains: &[Link], carried: &[Link], predecessor: Option<&Predecessor>) -> Vec<Entry> {
    let mut entries: Vec<Entry> = chains
        .iter()
        .chain(carried)
        .map(|link| {
            Entry::new(PRUNED, "recorded")
                .with("pruned_chain", link.chain.clone())
                .with("last_seq", link.last_seq)
                .with("last_hash", link.last_hash.clone())
        })
        .collect();
    if let (Some(first), Some(predecessor)) = (entries.first_mut(), predecessor) {
        *first = first
            .clone()
            .with("predecessor_chain", predecessor.chain.clone())
            .with("predecessor_seq", predecessor.seq)
            .with("predecessor_hash", predecessor.hash.clone());
    }
    entries
}

/// The link an `audit.pruned` line records, and the predecessor it carries, if it is one.
#[must_use]
pub fn read_pruned(line: &str) -> Option<(Link, Option<Predecessor>)> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("event").and_then(Value::as_str) != Some(PRUNED) {
        return None;
    }
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
    let link = Link {
        chain: text("pruned_chain")?,
        last_seq: value.get("last_seq").and_then(Value::as_u64)?,
        last_hash: text("last_hash")?,
    };
    let predecessor = predecessor_named(&value, "predecessor_chain");
    Some((link, predecessor))
}

/// The predecessor a line names: the first chain's `audit.chain_opened` names it for its own chain,
/// an `audit.pruned` under `predecessor_chain`.
fn predecessor_named(value: &Value, chain_key: &str) -> Option<Predecessor> {
    Some(Predecessor {
        chain: value.get(chain_key).and_then(Value::as_str)?.to_string(),
        seq: value.get("predecessor_seq").and_then(Value::as_u64)?,
        hash: value
            .get("predecessor_hash")
            .and_then(Value::as_str)?
            .to_string(),
    })
}

/// Prunes the record on `database` for the instance whose chain is `own`, recording through
/// `audit`, in rounds of [`prune_round`], one right after the other until a round finds nothing
/// more to remove; answers what each round did.
///
/// # Errors
/// As [`prune_round`]: the round that fails deleted nothing, and the rounds before it stand.
pub fn prune(
    database: &Database,
    audit: Arc<AuditLog>,
    own: Option<String>,
    retain_days: u64,
) -> Result<Vec<Pruned>, String> {
    prune_in_rounds_of(database, &audit, own.as_ref(), retain_days, ROUND_ENTRIES)
}

/// [`prune`] in rounds of at most `round` entries each.
fn prune_in_rounds_of(
    database: &Database,
    audit: &Arc<AuditLog>,
    own: Option<&String>,
    retain_days: u64,
    round: i64,
) -> Result<Vec<Pruned>, String> {
    let mut rounds = Vec::new();
    loop {
        let round = prune_round_of(database, audit.clone(), own.cloned(), retain_days, round)?;
        if round.capped && round.chains.is_empty() {
            // A next round would find what this one found.
            return Err(
                "a round of the audit prune was cut off by its budget and pruned no \
                        chain, so the prune stopped — run server audit-verify --config"
                    .to_string(),
            );
        }
        let more = round.capped;
        rounds.push(round);
        if !more {
            return Ok(rounds);
        }
    }
}

/// One round of a prune: the instance's own chain, and, when it gets the prune lock, every closed
/// chain. Entries stored more than `retain_days` days ago go, at most [`ROUND_ENTRIES`] of them,
/// each chain's run from its start, from every chain in one transaction.
///
/// # Errors
/// Returns an error when the database cannot be read or written, or when the `audit.pruned`
/// entries could not be written; then nothing was deleted.
pub fn prune_round(
    database: &Database,
    audit: Arc<AuditLog>,
    own: Option<String>,
    retain_days: u64,
) -> Result<Pruned, String> {
    prune_round_of(database, audit, own, retain_days, ROUND_ENTRIES)
}

/// [`prune_round`] removing at most `budget` entries.
fn prune_round_of(
    database: &Database,
    audit: Arc<AuditLog>,
    own: Option<String>,
    retain_days: u64,
    budget: i64,
) -> Result<Pruned, String> {
    // At most 36 500 (`[audit] retain_days`), so the bits carry over unchanged.
    let days = super::signed(retain_days);
    database.run_within(PRUNE_TIMEOUT, move |pool| async move {
        // A connection of its own, taken from the pool, so that the session-level lock ends with
        // it whatever happens to this call.
        let mut lock = deadpool_postgres::Object::take(client(&pool).await?);
        let locked: bool = lock
            .query_one(&format!("SELECT pg_try_advisory_lock({PRUNE_LOCK})"), &[])
            .await
            .map_err(sql)?
            .get(0);
        let mut chains: Vec<String> = Vec::new();
        if let Some(own) = &own {
            if lock
                .query_opt(
                    "SELECT 1 FROM audit_chains WHERE id = $1::text::uuid AND closed_hash IS NULL",
                    &[own],
                )
                .await
                .map_err(sql)?
                .is_some()
            {
                chains.push(own.clone());
            }
        }
        if locked {
            for row in lock
                .query(
                    "SELECT id::text FROM audit_chains WHERE closed_hash IS NOT NULL \
                     ORDER BY opened_at, id",
                    &[],
                )
                .await
                .map_err(sql)?
            {
                chains.push(row.get(0));
            }
        }
        // The own chain may have been closed by a takeover between the two reads.
        let mut seen = BTreeSet::new();
        chains.retain(|chain| seen.insert(chain.clone()));
        let plan = plan(&*lock, &chains, days, budget).await?;
        let mut pruned = Pruned {
            closed_chains: locked,
            capped: plan.capped,
            ..Pruned::default()
        };
        if plan.chains.is_empty() {
            return Ok(pruned);
        }
        let written = entries(&plan.chains, &plan.carried, plan.predecessor.as_ref());
        let recorded = tokio::task::spawn_blocking(move || {
            written.into_iter().all(|entry| audit.record(entry).is_ok())
                && audit.drain(WRITTEN_TIMEOUT)
        })
        .await
        .map_err(|e| format!("the audit writer's wait failed: {e}"))?;
        if !recorded {
            return Err(
                "the audit.pruned entries could not be written, so nothing was pruned".to_string(),
            );
        }
        // One transaction for every chain: the old link of a chain pruned further may sit in
        // another chain pruned here, so a prune that stopped between two deletes would leave a
        // chain linked by nothing.
        let transaction = lock.transaction().await.map_err(sql)?;
        delete(&*transaction, &plan.chains).await?;
        transaction.commit().await.map_err(sql)?;
        pruned.chains = plan.chains;
        pruned.carried = plan.carried;
        pruned.predecessor = plan.predecessor.is_some();
        Ok(pruned)
    })
}

/// What a prune removes and records.
#[derive(Debug, Default)]
struct Plan {
    chains: Vec<Link>,
    carried: Vec<Link>,
    predecessor: Option<Predecessor>,
    /// Whether `budget` ran out, so that old entries may be left.
    capped: bool,
}

/// For each chain of `chains`, the longest run of entries from its start all stored more than
/// `days` days ago, cut so that the runs hold at most `budget` entries together; and the links
/// and the predecessor the entries in those runs record that are still needed once they are gone.
async fn plan(
    client: &impl GenericClient,
    chains: &[String],
    days: i64,
    mut budget: i64,
) -> Result<Plan, String> {
    let mut plan = Plan::default();
    let mut removed: Vec<Link> = Vec::new();
    for chain in chains {
        if budget == 0 {
            plan.capped = true;
            break;
        }
        let bounds = client
            .query_one(
                "SELECT min(seq), coalesce((SELECT min(seq) - 1 FROM audit_entries \
                 WHERE chain = $1::text::uuid \
                 AND recorded_at >= now() - $2::bigint * interval '24 hours'), max(seq)) \
                 FROM audit_entries WHERE chain = $1::text::uuid",
                &[chain, &days],
            )
            .await
            .map_err(sql)?;
        let (Some(first), Some(old_end)) = (
            bounds.get::<_, Option<i64>>(0),
            bounds.get::<_, Option<i64>>(1),
        ) else {
            continue;
        };
        if old_end < first {
            continue;
        }
        let end: i64 = old_end.min(first.saturating_add(budget - 1));
        let Some(row) = client
            .query_opt(
                "SELECT seq, line FROM audit_entries WHERE chain = $1::text::uuid AND seq = $2",
                &[chain, &end],
            )
            .await
            .map_err(sql)?
        else {
            // A chain with a gap where the cut falls is not one an instance wrote; verify names
            // it, and the prune leaves it as it is.
            tracing::warn!(
                chain = %chain,
                seq = end,
                "the audit chain holds no entry where the prune would cut it, so it is not \
                 pruned — run server audit-verify --config"
            );
            continue;
        };
        // Charged only for a chain that enters the plan, so a round that is capped prunes one.
        budget -= end - first + 1;
        if budget == 0 {
            plan.capped = true;
        }
        let last_seq = super::unsigned(row.get::<_, i64>(0));
        plan.chains.push(Link {
            chain: chain.clone(),
            last_seq,
            last_hash: line_hash(&row.get::<_, String>(1)),
        });
        for row in client
            .query(
                "SELECT line FROM audit_entries WHERE chain = $1::text::uuid AND seq <= $2 \
                 AND (line LIKE '%\"event\":\"audit.pruned\"%' \
                 OR line LIKE '%\"predecessor_hash\"%')",
                &[chain, &super::signed(last_seq)],
            )
            .await
            .map_err(sql)?
        {
            let line: String = row.get(0);
            if let Some((link, predecessor)) = read_pruned(&line) {
                removed.push(link);
                plan.predecessor = plan.predecessor.or(predecessor);
            } else if let Ok(value) = serde_json::from_str::<Value>(&line) {
                if value.get("event").and_then(Value::as_str) == Some("audit.chain_opened") {
                    plan.predecessor = plan.predecessor.or(predecessor_named(&value, "chain"));
                }
            }
        }
    }
    let pruning: BTreeSet<&str> = plan.chains.iter().map(|link| link.chain.as_str()).collect();
    let mut carried = BTreeSet::new();
    for link in removed {
        // A chain pruned now is linked by the entry recorded for it now.
        if pruning.contains(link.chain.as_str()) {
            continue;
        }
        let Some(row) = client
            .query_opt(
                "SELECT (SELECT min(seq) FROM audit_entries WHERE chain = c.id), c.closed_hash \
                 FROM audit_chains c WHERE c.id = $1::text::uuid",
                &[&link.chain],
            )
            .await
            .map_err(sql)?
        else {
            continue;
        };
        let first: Option<i64> = row.get(0);
        let closed: Option<String> = row.get(1);
        let needed = match first {
            Some(first) => super::unsigned(first) == link.last_seq + 1,
            None => closed.as_deref() == Some(link.last_hash.as_str()),
        };
        if needed {
            carried.insert(link);
        }
    }
    plan.carried = carried.into_iter().collect();
    Ok(plan)
}

/// Deletes each chain's entries up to the last one its link names, once that entry is still the
/// one the link was recorded for, in the transaction `client` holds. A chain another instance
/// pruned meanwhile is left as it is.
async fn delete(client: &impl GenericClient, chains: &[Link]) -> Result<(), String> {
    for link in chains {
        let seq = super::signed(link.last_seq);
        let stored: Option<String> = client
            .query_opt(
                "SELECT line FROM audit_entries WHERE chain = $1::text::uuid AND seq = $2",
                &[&link.chain, &seq],
            )
            .await
            .map_err(sql)?
            .map(|row| row.get(0));
        match stored {
            None => continue,
            Some(line) if line_hash(&line) != link.last_hash => {
                return Err(format!(
                    "chain {}, entry {}: it is not the entry the audit.pruned recorded, so the \
                     chain was not pruned — run server audit-verify --config",
                    link.chain, link.last_seq
                ));
            }
            Some(_) => {}
        }
        // In batches, so that no statement outgrows the connection's statement timeout however
        // long the run; the transaction around them keeps the prune whole.
        let first: i64 = client
            .query_one(
                "SELECT coalesce(min(seq), 1) FROM audit_entries WHERE chain = $1::text::uuid",
                &[&link.chain],
            )
            .await
            .map_err(sql)?
            .get(0);
        let mut from = first;
        while from <= seq {
            let to = seq.min(from.saturating_add(DELETE_BATCH - 1));
            client
                .execute(
                    "DELETE FROM audit_entries WHERE chain = $1::text::uuid \
                     AND seq BETWEEN $2 AND $3",
                    &[&link.chain, &from, &to],
                )
                .await
                .map_err(sql)?;
            from = to.saturating_add(1);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tokio::sync::watch;

    use super::super::testing::TestDatabase;
    use super::super::Exclusive;
    use super::*;
    use crate::audit_log::Limits;
    use crate::postgres::audit::{verify, PgAuditStore};

    const LAPSE: Duration = Duration::from_secs(30);
    const RETAIN: u64 = 365;

    /// An instance as the Server composes its record: a lease, and the writer on its chain.
    struct Writing {
        database: Arc<Database>,
        _held: watch::Sender<Option<String>>,
        audit: Arc<AuditLog>,
        chain: String,
    }

    fn shared(test: &TestDatabase) -> Arc<Database> {
        Database::open_in(&test.config(), Exclusive::Shared, Some(&test.schema)).expect("shared")
    }

    fn writing_on(database: Arc<Database>, host: &str) -> Writing {
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
            _held: held,
            audit: Arc::new(audit),
            chain,
        }
    }

    fn writing(test: &TestDatabase, host: &str) -> Writing {
        writing_on(shared(test), host)
    }

    impl Writing {
        /// `n` decisions, written.
        fn decide(&self, n: u32) {
            for n in 0..n {
                self.audit
                    .record(Entry::new("admission.admitted", "admitted").with("n", n))
                    .expect("record");
            }
            assert!(self.audit.drain(Duration::from_secs(10)), "not written");
        }

        fn prune(&self) -> Pruned {
            prune_round(
                &self.database,
                self.audit.clone(),
                Some(self.chain.clone()),
                RETAIN,
            )
            .expect("prune")
        }
    }

    /// Each entry of `chain`, by `seq`, as stored.
    fn lines(test: &TestDatabase, chain: &str) -> Vec<(u64, String)> {
        test.rows(&format!(
            "SELECT seq, line FROM audit_entries WHERE chain = '{chain}' ORDER BY seq"
        ))
        .into_iter()
        .map(|row| {
            (
                row[0].as_deref().expect("seq").parse().expect("seq"),
                row[1].clone().expect("line"),
            )
        })
        .collect()
    }

    /// Makes the entries `seqs` of `chain` older than `retain_days`, as the database stored them.
    fn age(test: &TestDatabase, chain: &str, seqs: &str) {
        test.sql(&format!(
            "UPDATE audit_entries SET recorded_at = now() - interval '400 days' \
             WHERE chain = '{chain}' AND seq IN ({seqs})"
        ));
    }

    fn age_all(test: &TestDatabase, chain: &str) {
        test.sql(&format!(
            "UPDATE audit_entries SET recorded_at = now() - interval '400 days' \
             WHERE chain = '{chain}'"
        ));
    }

    /// Every `audit.pruned` the record holds, by the chain it names.
    fn pruned_entries(test: &TestDatabase) -> Vec<Link> {
        test.rows("SELECT line FROM audit_entries WHERE line LIKE '%audit.pruned%' ORDER BY recorded_at, chain, seq")
            .into_iter()
            .filter_map(|row| read_pruned(row[0].as_deref().expect("line")).map(|(link, _)| link))
            .collect()
    }

    fn held(database: &Database) -> crate::postgres::audit::Verified {
        let verified = verify(database, None).expect("verify");
        assert!(!verified.failed, "{:?}", verified.lines);
        verified
    }

    fn the_lock(test: &TestDatabase) -> i64 {
        test.rows(&format!("SELECT {PRUNE_LOCK}"))[0][0]
            .as_deref()
            .expect("key")
            .parse()
            .expect("key")
    }

    /// Entries stored more than `retain_days` ago go, the longest run of them from the chain's
    /// start and no younger one; `audit.pruned` records the last removed entry's `seq` and hash
    /// before they go, and `audit-verify` links what is left to it.
    /// Verifies: ADR-draft-the-servers-state-and-its-stores, ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance
    #[test]
    fn entries_older_than_retain_days_are_removed_and_what_is_left_still_verifies() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let a = writing(&test, "host-a");
        a.decide(6);
        assert_eq!(
            a.prune(),
            Pruned {
                closed_chains: true,
                ..Pruned::default()
            },
            "nothing old"
        );

        // Entry 4 is young: only 1 to 3 go, though 5 is old.
        age(&test, &a.chain, "1, 2, 3, 5");
        let before = lines(&test, &a.chain);
        let pruned = a.prune();
        let removed = Link {
            chain: a.chain.clone(),
            last_seq: 3,
            last_hash: line_hash(&before[2].1),
        };
        assert_eq!(pruned.chains, vec![removed.clone()]);
        let after = lines(&test, &a.chain);
        assert_eq!(after.first().map(|(seq, _)| *seq), Some(4), "{after:#?}");
        let (_, recorded) = after.last().expect("entries");
        assert_eq!(
            read_pruned(recorded).map(|(link, _)| link),
            Some(removed),
            "{recorded}"
        );
        assert!(recorded.contains(&format!("\"chain\":\"{}\"", a.chain)));
        held(&a.database);
    }

    /// Closed chains, one closed at a shutdown and one for a lapsed lease, are pruned by one
    /// instance at a time, the one holding the prune lock: while it is held elsewhere an instance
    /// prunes its own chain alone; two that prune at once record each chain once, in
    /// `audit.pruned` entries naming both; a closed chain with no entries left keeps its record.
    /// Verifies: ADR-draft-the-servers-state-and-its-stores, ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance
    #[test]
    fn closed_chains_are_pruned_by_one_instance_at_a_time() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let shut = writing(&test, "host-b");
        shut.decide(2);
        assert!(shut.audit.close(Duration::from_secs(10)), "not closed");
        let lapsed = writing(&test, "host-c");
        lapsed.decide(3);
        let lapsed_chain = lapsed.chain.clone();
        drop(lapsed);
        test.sql(&format!(
            "UPDATE leases SET expires_at = now() WHERE instance_id = '{lapsed_chain}'"
        ));
        // Taking a lease closes the lapsed one's chain.
        let (a, d) = (writing(&test, "host-a"), writing(&test, "host-d"));
        assert!(lines(&test, &lapsed_chain)
            .last()
            .is_some_and(|(_, line)| line.contains("lease_lapsed")));
        age_all(&test, &shut.chain);
        age(&test, &lapsed_chain, "1, 2");
        let shut_end = lines(&test, &shut.chain).last().cloned().expect("closing");
        let lapsed_two = lines(&test, &lapsed_chain)[1].1.clone();

        let gate = test.gate(the_lock(&test));
        let alone = a.prune();
        assert!(!alone.closed_chains, "the lock was held elsewhere");
        assert!(alone.chains.is_empty(), "{alone:?}");
        assert_eq!(
            lines(&test, &shut.chain).len(),
            4,
            "pruned without the lock"
        );
        gate.open();

        let (da, dd) = (a.database.clone(), d.database.clone());
        let (aa, ad) = (a.audit.clone(), d.audit.clone());
        let (ca, cd) = (a.chain.clone(), d.chain.clone());
        let other = std::thread::spawn(move || prune(&dd, ad, Some(cd), RETAIN).expect("d"));
        prune(&da, aa, Some(ca), RETAIN).expect("a");
        other.join().expect("d");

        let recorded = pruned_entries(&test);
        let mut named: Vec<&Link> = recorded
            .iter()
            .filter(|link| link.chain == shut.chain || link.chain == lapsed_chain)
            .collect();
        named.sort();
        let mut expected = [
            Link {
                chain: shut.chain.clone(),
                last_seq: shut_end.0,
                last_hash: line_hash(&shut_end.1),
            },
            Link {
                chain: lapsed_chain.clone(),
                last_seq: 2,
                last_hash: line_hash(&lapsed_two),
            },
        ];
        expected.sort();
        assert_eq!(
            named,
            expected.iter().collect::<Vec<_>>(),
            "each chain once"
        );
        assert!(lines(&test, &shut.chain).is_empty());
        assert_eq!(
            test.rows(&format!(
                "SELECT closed_hash FROM audit_chains WHERE id = '{}'",
                shut.chain
            )),
            vec![vec![Some(line_hash(&shut_end.1))]],
            "a closed chain with no entries left keeps its record"
        );
        assert_eq!(lines(&test, &lapsed_chain)[0].0, 3);
        let verified = held(&a.database);
        assert!(
            verified
                .lines
                .iter()
                .any(|line| line.contains(&shut.chain) && line.contains("0 entries, closed")),
            "{:?}",
            verified.lines
        );
    }

    /// A pruned chain whose first retained entry does not follow the entry its `audit.pruned`
    /// recorded, and a closed chain with no entries left whose closing hash is not the one its
    /// `audit.pruned` recorded, fail `audit-verify`, naming the chain.
    /// Verifies: ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance
    #[test]
    fn a_pruned_chain_that_does_not_link_to_its_audit_pruned_fails_naming_it() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let shut = writing(&test, "host-b");
        shut.decide(1);
        assert!(shut.audit.close(Duration::from_secs(10)), "not closed");
        let a = writing(&test, "host-a");
        a.decide(4);
        age_all(&test, &shut.chain);
        age(&test, &a.chain, "1, 2");
        a.prune();
        held(&a.database);

        test.sql(&format!(
            "DELETE FROM audit_entries WHERE chain = '{}' AND seq = 3",
            a.chain
        ));
        let verified = verify(&a.database, None).expect("verify");
        assert!(verified.failed, "{:?}", verified.lines);
        assert!(
            verified
                .lines
                .iter()
                .any(|line| line.contains(&format!("chain {}, entry 4", a.chain))),
            "{:?}",
            verified.lines
        );

        test.sql(&format!(
            "UPDATE audit_chains SET closed_hash = '{}' WHERE id = '{}'",
            "f".repeat(64),
            shut.chain
        ));
        let verified = verify(&a.database, None).expect("verify");
        assert!(
            verified
                .lines
                .iter()
                .any(|line| line.contains("the chain breaks") && line.contains(&shut.chain)),
            "{:?}",
            verified.lines
        );
    }

    /// A predecessor carried on by an `audit.pruned` in a chain that fails names nothing: the
    /// archive the database records is then named by no chain, and `audit-verify` fails.
    /// Verifies: ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance
    #[test]
    fn a_predecessor_carried_by_a_chain_that_fails_names_nothing() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = shared(&test);
        database.offer_archive(Some((7, "ab".repeat(32))));
        let first = writing_on(database, "host-a");
        first.decide(1);
        assert!(first.audit.close(Duration::from_secs(10)), "not closed");
        let b = writing(&test, "host-b");
        age_all(&test, &first.chain);
        assert!(b.prune().predecessor, "the first chain's opening named it");
        let verified = held(&b.database);
        assert!(
            verified.lines.iter().any(|line| line.contains("entry 7")),
            "{:?}",
            verified.lines
        );

        // B's opening is altered: B fails, and the predecessor it carries is named by no chain.
        test.sql(&format!(
            "UPDATE audit_entries SET line = replace(line, 'host-b', 'host-z') \
             WHERE chain = '{}' AND seq = 1",
            b.chain
        ));
        let verified = verify(&b.database, None).expect("verify");
        assert!(verified.failed, "{:?}", verified.lines);
        assert!(
            verified
                .lines
                .iter()
                .any(|line| line.contains("no chain names it as its predecessor")),
            "a failing chain named the predecessor: {:?}",
            verified.lines
        );
    }

    /// A chain linked by an `audit.pruned` in a chain opened before it holds once that chain is
    /// found holding, whichever order the chains are walked in.
    /// Verifies: ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance
    #[test]
    fn a_chain_linked_from_an_older_chain_holds() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let a = writing(&test, "host-a");
        let closed = writing(&test, "host-x");
        closed.decide(3);
        assert!(closed.audit.close(Duration::from_secs(10)), "not closed");
        age(&test, &closed.chain, "1, 2");
        assert_eq!(a.prune().chains.len(), 1);
        held(&a.database);
    }

    /// An `audit.pruned` in a chain that fails vouches for nothing: the chain it linked fails
    /// too, naming its first retained entry.
    /// Verifies: ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance
    #[test]
    fn an_audit_pruned_in_a_chain_that_fails_links_no_other_chain() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let closed = writing(&test, "host-x");
        closed.decide(3);
        assert!(closed.audit.close(Duration::from_secs(10)), "not closed");
        let a = writing(&test, "host-a");
        age(&test, &closed.chain, "1, 2");
        a.prune();
        a.decide(1);
        held(&a.database);

        // A's opening is altered: A's walk fails at the entry after it, and so does its link.
        test.sql(&format!(
            "UPDATE audit_entries SET line = replace(line, 'host-a', 'host-z') \
             WHERE chain = '{}' AND seq = 1",
            a.chain
        ));
        let verified = verify(&a.database, None).expect("verify");
        assert!(verified.failed, "{:?}", verified.lines);
        assert!(
            verified
                .lines
                .iter()
                .any(|line| line.contains(&format!("chain {}, entry 2", a.chain))),
            "{:?}",
            verified.lines
        );
        assert!(
            verified.lines.iter().any(|line| line
                .contains(&format!("chain {}, entry 3", closed.chain))
                && line.contains("audit.pruned")),
            "the closed chain was linked by a chain that fails: {:?}",
            verified.lines
        );
    }

    /// Once the `audit.pruned` entries themselves and the first chain's `audit.chain_opened` are
    /// pruned, the links still needed — of a closed chain with no entries left, and of one that
    /// still begins after the entry recorded — and the archive's predecessor are recorded again,
    /// and `audit-verify` still holds.
    /// Verifies: ADR-draft-the-servers-state-and-its-stores, ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance
    #[test]
    fn the_links_a_pruned_audit_pruned_recorded_are_carried_on() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = shared(&test);
        let archive = (7, "ab".repeat(32));
        database.offer_archive(Some(archive.clone()));
        let first = writing_on(database, "host-a");
        first.decide(2);
        assert!(first.audit.close(Duration::from_secs(10)), "not closed");
        let second = writing(&test, "host-c");
        second.decide(2);
        assert!(second.audit.close(Duration::from_secs(10)), "not closed");
        let b = writing(&test, "host-b");
        age_all(&test, &first.chain);
        age(&test, &second.chain, "1, 2");
        let pruned = b.prune();
        assert_eq!(pruned.chains.len(), 2, "{pruned:?}");
        assert!(pruned.predecessor, "the first chain's opening named it");
        held(&b.database);

        // A year on, the record of that prune is old too; the closed chains are left this time.
        age_all(&test, &b.chain);
        let gate = test.gate(the_lock(&test));
        let again = b.prune();
        gate.open();
        assert!(!again.closed_chains);
        let mut carried: Vec<&str> = again
            .carried
            .iter()
            .map(|link| link.chain.as_str())
            .collect();
        carried.sort_unstable();
        let mut expected = vec![first.chain.as_str(), second.chain.as_str()];
        expected.sort_unstable();
        assert_eq!(carried, expected, "{again:?}");
        assert!(again.predecessor);
        assert!(
            !lines(&test, &b.chain)
                .iter()
                .any(|(_, line)| line.contains("audit.chain_opened")),
            "b's own chain was pruned"
        );
        let verified = held(&b.database);
        assert!(
            verified.lines.iter().any(|line| line.contains("entry 7")
                && line.contains(&first.chain)
                && line.contains("not checked")),
            "{:?}",
            verified.lines
        );
    }

    /// A prune that stops between two chains' deletes deletes nothing: the old link of a chain it
    /// prunes further sits in its own chain, which it prunes first, so a delete of its own chain
    /// that stood alone would leave the other chain linked by nothing, and an honest record would
    /// fail `audit-verify`.
    /// Verifies: ADR-draft-the-servers-state-and-its-stores
    #[test]
    fn a_prune_that_stops_between_two_chains_deletes_nothing() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let closed = writing(&test, "host-x");
        closed.decide(4);
        assert!(closed.audit.close(Duration::from_secs(10)), "not closed");
        let a = writing(&test, "host-a");
        age(&test, &closed.chain, "1, 2");
        let first = a.prune();
        assert_eq!(first.chains.len(), 1, "{first:?}");

        // A year on: A's own chain, which holds the link of the closed chain, and its entry 3 are
        // old, and the closed chain's delete is refused.
        age_all(&test, &a.chain);
        age(&test, &closed.chain, "3");
        test.sql(&format!(
            "CREATE FUNCTION fleet_test_refuse_delete() RETURNS trigger LANGUAGE plpgsql AS \
             $$ BEGIN RAISE EXCEPTION 'refused by the test'; END $$; \
             CREATE TRIGGER fleet_test_refuse_delete BEFORE DELETE ON audit_entries FOR EACH ROW \
             WHEN (OLD.chain = '{}') EXECUTE FUNCTION fleet_test_refuse_delete();",
            closed.chain
        ));
        let own = lines(&test, &a.chain);
        prune(&a.database, a.audit.clone(), Some(a.chain.clone()), RETAIN)
            .expect_err("the closed chain's delete was refused");
        held(&a.database);
        assert_eq!(
            lines(&test, &a.chain).first(),
            own.first(),
            "the own chain was pruned alone"
        );
        test.sql("DROP TRIGGER fleet_test_refuse_delete ON audit_entries");
    }

    /// A run too long for one statement within the statement timeout every connection holds is
    /// deleted in batches inside the one transaction: the prune ends, and the chain still verifies.
    /// The database slows each deleted row of the long chain by half a millisecond, so one
    /// statement over all of them would outlast the ten-second timeout and roll the prune back.
    /// Verifies: ADR-draft-the-servers-state-and-its-stores
    #[test]
    fn a_run_too_long_for_one_statement_is_deleted_in_batches() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        const OLD: u64 = 24_000;
        let chain = "00000000-0000-4000-8000-000000000024";
        let closing = crate::audit_log::render(
            OLD + 1,
            0,
            &Entry::new("audit.chain_closed", "recorded")
                .with("chain", chain)
                .with("reason", "shutdown"),
            &line_hash(&format!("old {OLD}")),
        );
        let a = writing(&test, "host-a");
        test.sql(&format!(
            "INSERT INTO audit_chains (id, host, opened_at, closed_at, closed_hash) \
             VALUES ('{chain}', 'host-x', now(), now(), '{}'); \
             INSERT INTO audit_entries (chain, seq, line, recorded_at) \
             SELECT '{chain}', n, 'old ' || n, now() - interval '400 days' \
             FROM generate_series(1, {OLD}) AS n; \
             INSERT INTO audit_entries (chain, seq, line) VALUES ('{chain}', {}, '{}'); \
             CREATE FUNCTION fleet_test_slow_delete() RETURNS trigger LANGUAGE plpgsql AS \
             $$ BEGIN PERFORM pg_sleep(0.0005); RETURN OLD; END $$; \
             CREATE TRIGGER fleet_test_slow_delete BEFORE DELETE ON audit_entries FOR EACH ROW \
             WHEN (OLD.chain = '{chain}') EXECUTE FUNCTION fleet_test_slow_delete();",
            line_hash(&closing),
            OLD + 1,
            closing.replace('\'', "''"),
        ));
        let pruned = a.prune();
        assert!(
            pruned
                .chains
                .iter()
                .any(|link| link.chain == chain && link.last_seq == OLD),
            "{pruned:?}"
        );
        assert_eq!(lines(&test, chain).len(), 1, "the old run is gone");
        held(&a.database);
    }

    /// A backlog larger than one round drains across rounds within one prune: each round removes
    /// at most its share of entries and leaves a record that verifies, and the next follows right
    /// away until nothing old is left. Rounds of 1000 here stand for [`ROUND_ENTRIES`].
    /// Verifies: ADR-draft-the-servers-state-and-its-stores
    #[test]
    fn a_backlog_larger_than_one_round_drains_in_rounds() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        const ROUND: i64 = 1000;
        const OLD: u64 = 2500;
        let a = writing(&test, "host-a");
        let chain = "00000000-0000-4000-8000-000000000025";
        let (mut prev, mut values) = (crate::audit_log::GENESIS.to_string(), Vec::new());
        for seq in 1..=OLD + 1 {
            let entry = match seq {
                1 => Entry::new("audit.chain_opened", "recorded"),
                _ if seq == OLD + 1 => {
                    Entry::new("audit.chain_closed", "recorded").with("reason", "shutdown")
                }
                _ => Entry::new("admission.admitted", "admitted").with("n", seq),
            };
            let line = crate::audit_log::render(seq, 0, &entry.with("chain", chain), &prev);
            prev = line_hash(&line);
            let age = if seq <= OLD {
                "now() - interval '400 days'"
            } else {
                "now()"
            };
            values.push(format!("('{chain}', {seq}, '{line}', {age})"));
        }
        test.sql(&format!(
            "INSERT INTO audit_chains (id, host, opened_at, closed_at, closed_hash) \
             VALUES ('{chain}', 'host-x', now(), now(), '{prev}'); \
             INSERT INTO audit_entries (chain, seq, line, recorded_at) VALUES {};",
            values.join(", ")
        ));
        let first_entry = |test: &TestDatabase| -> u64 {
            test.rows(&format!(
                "SELECT min(seq) FROM audit_entries WHERE chain = '{chain}'"
            ))[0][0]
                .as_deref()
                .expect("an entry")
                .parse()
                .expect("seq")
        };

        let one = prune_round_of(
            &a.database,
            a.audit.clone(),
            Some(a.chain.clone()),
            RETAIN,
            ROUND,
        )
        .expect("one round");
        assert!(one.capped, "{one:?}");
        assert_eq!(first_entry(&test), 1001, "one round's share");
        held(&a.database);

        let rounds = prune_in_rounds_of(&a.database, &a.audit, Some(&a.chain), RETAIN, ROUND)
            .expect("the rest");
        assert_eq!(rounds.len(), 2, "{rounds:?}");
        assert!(!rounds[1].capped);
        assert_eq!(first_entry(&test), OLD + 1, "the backlog drained");
        held(&a.database);
    }

    /// A chain with an entry missing where a round's budget cuts it is left as it is, and the run
    /// of rounds ends rather than repeating a round that prunes nothing.
    /// Verifies: ADR-draft-the-servers-state-and-its-stores
    #[test]
    fn a_round_that_prunes_no_chain_ends_the_prune() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        const ROUND: i64 = 1000;
        const OLD: u64 = 2500;
        let a = writing(&test, "host-a");
        let chain = "00000000-0000-4000-8000-000000000026";
        let (mut prev, mut values) = (crate::audit_log::GENESIS.to_string(), Vec::new());
        for seq in 1..=OLD + 1 {
            let entry = match seq {
                1 => Entry::new("audit.chain_opened", "recorded"),
                _ if seq == OLD + 1 => {
                    Entry::new("audit.chain_closed", "recorded").with("reason", "shutdown")
                }
                _ => Entry::new("admission.admitted", "admitted").with("n", seq),
            };
            let line = crate::audit_log::render(seq, 0, &entry.with("chain", chain), &prev);
            prev = line_hash(&line);
            let age = if seq <= OLD {
                "now() - interval '400 days'"
            } else {
                "now()"
            };
            // The entry where a round of 1000 would cut the chain is gone.
            if seq != 1000 {
                values.push(format!("('{chain}', {seq}, '{line}', {age})"));
            }
        }
        test.sql(&format!(
            "INSERT INTO audit_chains (id, host, opened_at, closed_at, closed_hash) \
             VALUES ('{chain}', 'host-x', now(), now(), '{prev}'); \
             INSERT INTO audit_entries (chain, seq, line, recorded_at) VALUES {};",
            values.join(", ")
        ));
        let (database, audit, own) = (a.database.clone(), a.audit.clone(), a.chain.clone());
        let (done, ended) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = done.send(prune_in_rounds_of(
                &database,
                &audit,
                Some(&own),
                RETAIN,
                ROUND,
            ));
        });
        let rounds = ended
            .recv_timeout(Duration::from_secs(60))
            .expect("the prune never ended")
            .expect("the prune");
        assert!(rounds.iter().all(|round| !round.capped), "{rounds:?}");
        assert_eq!(
            test.rows(&format!(
                "SELECT count(*) FROM audit_entries WHERE chain = '{chain}'"
            ))[0][0]
                .as_deref(),
            Some("2500"),
            "the chain was pruned past its missing entry"
        );
    }

    /// When its `audit.pruned` entries cannot be written, the prune deletes nothing.
    /// Verifies: ADR-draft-the-servers-state-and-its-stores
    #[test]
    fn nothing_is_pruned_before_its_audit_pruned_is_written() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let a = writing(&test, "host-a");
        a.decide(3);
        age(&test, &a.chain, "1, 2");
        test.refuse("audit_entries", "NEW.line LIKE '%audit.pruned%'");
        let err = prune(&a.database, a.audit.clone(), Some(a.chain.clone()), RETAIN)
            .expect_err("the record failed");
        assert!(err.contains("nothing was pruned"), "{err}");
        assert_eq!(lines(&test, &a.chain)[0].0, 1, "pruned without its record");
        test.mend("audit_entries");
    }

    /// Entries stored before the migration that adds `recorded_at` take the time their line
    /// carries, so a record of a release before is pruned by its own age.
    /// Verifies: ADR-draft-the-servers-state-and-its-stores
    #[test]
    fn entries_stored_before_the_migration_take_their_lines_time() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let before = &super::super::schema::MIGRATIONS[..9];
        let database = Database::connect(&test.config(), Some(&test.schema)).expect("connect");
        database.apply_schema(before).expect("the schema before");
        test.sql(
            "INSERT INTO audit_chains (id, host, opened_at) \
             VALUES ('00000000-0000-0000-0000-000000000001', 'h', now()); \
             INSERT INTO audit_entries (chain, seq, line) VALUES \
             ('00000000-0000-0000-0000-000000000001', 1, \
              '{\"event\":\"audit.chain_opened\",\"seq\":1,\"time\":\"2020-03-04T05:06:07.123Z\"}'), \
             ('00000000-0000-0000-0000-000000000001', 2, 'not an entry');",
        );
        database
            .apply_schema(super::super::schema::MIGRATIONS)
            .expect("the schema now");
        let rows = test.rows(
            "SELECT seq, to_char(recorded_at AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS.MS'), \
             recorded_at > now() - interval '1 day' FROM audit_entries ORDER BY seq",
        );
        assert_eq!(rows[0][1].as_deref(), Some("2020-03-04 05:06:07.123"));
        assert_eq!(
            rows[1][2].as_deref(),
            Some("t"),
            "a line that is no entry: {rows:?}"
        );
    }
}
