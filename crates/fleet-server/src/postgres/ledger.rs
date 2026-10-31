//! The register, the revocation list and the Hosts in PostgreSQL (ADR-0029 clauses 9 and 10): the
//! PostgreSQL adapter behind [`LedgerStore`](crate::revocation::LedgerStore) — one row per
//! certificate the Server signed, one per revocation, one per Host. Every call is one transaction,
//! committed before the decision it answers is.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use tokio_postgres::GenericClient;

use super::{client, signed, sql, unsigned, Database};
use crate::revocation::{Added, CertId, Facts, Host, Issued, Ledger, LedgerStore, Revocation};

/// The ledger in `certificates`, `revocations` and `hosts`. A revocation is written as the one row
/// it adds or removes, deciding in its transaction from what the table holds, as other instances
/// write the same table; the Hosts are written as what changed since the last commit.
pub struct PgLedgerStore {
    database: Arc<Database>,
    /// The Hosts as last committed.
    written_hosts: Mutex<BTreeMap<String, Host>>,
}

impl PgLedgerStore {
    /// The adapter on `database`.
    #[must_use]
    pub fn new(database: Arc<Database>) -> Self {
        PgLedgerStore {
            database,
            written_hosts: Mutex::new(BTreeMap::new()),
        }
    }
}

impl LedgerStore for PgLedgerStore {
    fn load(&self) -> Result<Ledger, String> {
        let ledger = self.database.run(|pool| async move {
            let client = client(&pool).await?;
            read_all(&**client).await
        })?;
        ledger
            .hosts
            .clone_into(&mut self.written_hosts.lock().expect("hosts lock"));
        Ok(ledger)
    }

    fn put_issued(&self, issued: &Issued) -> Result<(), String> {
        let issued = issued.clone();
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            write_issued(&*transaction, &issued).await?;
            transaction.commit().await.map_err(sql)
        })
    }

    fn remove_issued(&self, id: &CertId) -> Result<(), String> {
        let id = id.clone();
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            transaction
                .execute(
                    "DELETE FROM certificates WHERE issuer = $1 AND serial = $2",
                    &[&id.issuer, &id.serial],
                )
                .await
                .map_err(sql)?;
            transaction.commit().await.map_err(sql)
        })
    }

    /// One transaction that holds the table against every other adder, so the bound is counted
    /// for every instance together: an entry revoking the same is answered as it stands, and a new
    /// one goes last.
    fn add_revocation(&self, entry: &Revocation, bound: usize) -> Result<Added, String> {
        let entry = entry.clone();
        let bound = i64::try_from(bound).unwrap_or(i64::MAX);
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            transaction
                .execute("LOCK TABLE revocations IN SHARE ROW EXCLUSIVE MODE", &[])
                .await
                .map_err(sql)?;
            let revoked = serde_json::to_value(&entry.revoked).expect("a revocation serializes");
            if let Some(row) = transaction
                .query_opt(
                    "SELECT id, revoked, revoked_ms FROM revocations \
                     WHERE id = $1 OR revoked::text = $2::json::text",
                    &[&entry.id, &revoked],
                )
                .await
                .map_err(sql)?
            {
                let id: String = row.get(0);
                let revoked = serde_json::from_value(row.get(1))
                    .map_err(|e| format!("revocation {id} in the database: {e}"))?;
                return Ok(Added::Present(Revocation {
                    id,
                    revoked,
                    revoked_ms: unsigned(row.get(2)),
                }));
            }
            let (count, last): (i64, Option<i32>) = {
                let row = transaction
                    .query_one("SELECT count(*), max(position) FROM revocations", &[])
                    .await
                    .map_err(sql)?;
                (row.get(0), row.get(1))
            };
            if count >= bound {
                return Ok(Added::Full);
            }
            let position = last.map_or(0, |last| last + 1);
            write_revocations(&*transaction, &[(position, entry)]).await?;
            super::notify::notify(&*transaction, super::notify::REVOCATION).await?;
            transaction.commit().await.map_err(sql)?;
            Ok(Added::Added)
        })
    }

    fn remove_revocation(&self, id: &str) -> Result<bool, String> {
        let id = id.to_string();
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            let removed = transaction
                .execute("DELETE FROM revocations WHERE id = $1", &[&id])
                .await
                .map_err(sql)?;
            if removed == 0 {
                return Ok(false);
            }
            super::notify::notify(&*transaction, super::notify::REVOCATION).await?;
            transaction.commit().await.map_err(sql)?;
            Ok(true)
        })
    }

    /// The Hosts that changed are written and the ones that are gone deleted, in one transaction.
    fn save_hosts(&self, hosts: &BTreeMap<String, Host>) -> Result<(), String> {
        let mut written = self.written_hosts.lock().expect("hosts lock");
        let changed: Vec<(String, Host)> = hosts
            .iter()
            .filter(|(host, entry)| written.get(*host) != Some(*entry))
            .map(|(host, entry)| (host.clone(), entry.clone()))
            .collect();
        let gone: Vec<String> = written
            .keys()
            .filter(|host| !hosts.contains_key(*host))
            .cloned()
            .collect();
        if changed.is_empty() && gone.is_empty() {
            return Ok(());
        }
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            transaction
                .execute("DELETE FROM hosts WHERE host = ANY($1)", &[&gone])
                .await
                .map_err(sql)?;
            for (host, entry) in &changed {
                write_host(&*transaction, host, entry).await?;
            }
            transaction.commit().await.map_err(sql)
        })?;
        hosts.clone_into(&mut written);
        Ok(())
    }
}

/// The whole ledger. A row that does not decode fails, naming it.
pub(crate) async fn read_all(client: &impl GenericClient) -> Result<Ledger, String> {
    let mut issued = Vec::new();
    for row in client
        .query(
            "SELECT issuer, serial, issuer_name, subject, key_fingerprint, not_after_ms, host, \
             subject_values, instance_uid, predecessor_issuer, predecessor_serial, issued_ms \
             FROM certificates ORDER BY issuer, serial",
            &[],
        )
        .await
        .map_err(sql)?
    {
        let id = CertId {
            issuer: row.get(0),
            serial: row.get(1),
        };
        let named = |e: String| {
            format!(
                "certificate {}/{} in the database: {e}",
                id.issuer, id.serial
            )
        };
        let predecessor = match (
            row.get::<_, Option<String>>(9),
            row.get::<_, Option<String>>(10),
        ) {
            (Some(issuer), Some(serial)) => Some(CertId { issuer, serial }),
            (None, None) => None,
            _ => return Err(named("half a predecessor".to_string())),
        };
        let subject_values = serde_json::from_value(row.get(7))
            .map_err(|e| named(format!("subject_values: {e}")))?;
        issued.push(Issued {
            facts: Facts {
                id: id.clone(),
                issuer_name: row.get(2),
                subject: String::from_utf8(row.get(3))
                    .map_err(|e| named(format!("subject: {e}")))?,
                key_fingerprint: row.get(4),
                not_after_ms: unsigned(row.get(5)),
                host: row.get(6),
                subject_values,
            },
            instance_uid: row.get(8),
            predecessor,
            issued_ms: unsigned(row.get(11)),
        });
    }
    let mut revocations = Vec::new();
    for row in client
        .query(
            "SELECT id, revoked, revoked_ms FROM revocations ORDER BY position, id",
            &[],
        )
        .await
        .map_err(sql)?
    {
        let id: String = row.get(0);
        let revoked = serde_json::from_value(row.get(1))
            .map_err(|e| format!("revocation {id} in the database: {e}"))?;
        revocations.push(Revocation {
            id,
            revoked,
            revoked_ms: unsigned(row.get(2)),
        });
    }
    let mut hosts = BTreeMap::new();
    for row in client
        .query(
            "SELECT host, gateway, instance_uids, subject, approved_ms, valid_until_ms FROM hosts",
            &[],
        )
        .await
        .map_err(sql)?
    {
        let host: String = row.get(0);
        let subject = serde_json::from_value(row.get(3))
            .map_err(|e| format!("host {host:?} in the database: subject: {e}"))?;
        hosts.insert(
            host,
            Host {
                gateway: row.get(1),
                instance_uids: row
                    .get::<_, Vec<String>>(2)
                    .into_iter()
                    .collect::<BTreeSet<_>>(),
                subject,
                approved_ms: row.get::<_, Option<i64>>(4).map(unsigned),
                valid_until_ms: row.get::<_, Option<i64>>(5).map(unsigned),
            },
        );
    }
    Ok(Ledger {
        issued,
        revocations,
        hosts,
    })
}

/// Creates or replaces one register entry inside the caller's transaction.
pub(crate) async fn write_issued(
    client: &impl GenericClient,
    issued: &Issued,
) -> Result<(), String> {
    let facts = &issued.facts;
    let subject_values =
        serde_json::to_value(&facts.subject_values).expect("subject values serialize");
    let predecessor = issued.predecessor.as_ref();
    client
        .execute(
            "INSERT INTO certificates (issuer, serial, issuer_name, subject, key_fingerprint, \
             not_after_ms, host, subject_values, instance_uid, predecessor_issuer, \
             predecessor_serial, issued_ms) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, \
             $11, $12) ON CONFLICT (issuer, serial) DO UPDATE SET \
             issuer_name = EXCLUDED.issuer_name, subject = EXCLUDED.subject, \
             key_fingerprint = EXCLUDED.key_fingerprint, not_after_ms = EXCLUDED.not_after_ms, \
             host = EXCLUDED.host, subject_values = EXCLUDED.subject_values, \
             instance_uid = EXCLUDED.instance_uid, \
             predecessor_issuer = EXCLUDED.predecessor_issuer, \
             predecessor_serial = EXCLUDED.predecessor_serial, issued_ms = EXCLUDED.issued_ms",
            &[
                &facts.id.issuer,
                &facts.id.serial,
                &facts.issuer_name,
                &facts.subject.as_bytes(),
                &facts.key_fingerprint,
                &signed(facts.not_after_ms),
                &facts.host,
                &subject_values,
                &issued.instance_uid,
                &predecessor.map(|id| id.issuer.as_str()),
                &predecessor.map(|id| id.serial.as_str()),
                &signed(issued.issued_ms),
            ],
        )
        .await
        .map_err(sql)?;
    Ok(())
}

/// Writes the entries of the list at their positions, in one statement.
pub(crate) async fn write_revocations(
    client: &impl GenericClient,
    entries: &[(i32, Revocation)],
) -> Result<(), String> {
    if entries.is_empty() {
        return Ok(());
    }
    let ids: Vec<&str> = entries.iter().map(|(_, entry)| entry.id.as_str()).collect();
    let positions: Vec<i32> = entries.iter().map(|(position, _)| *position).collect();
    let revoked: Vec<serde_json::Value> = entries
        .iter()
        .map(|(_, entry)| serde_json::to_value(&entry.revoked).expect("a revocation serializes"))
        .collect();
    let revoked_ms: Vec<i64> = entries
        .iter()
        .map(|(_, entry)| signed(entry.revoked_ms))
        .collect();
    client
        .execute(
            "INSERT INTO revocations (id, position, revoked, revoked_ms) \
             SELECT * FROM UNNEST($1::text[], $2::integer[], $3::json[], $4::bigint[]) \
             ON CONFLICT (id) DO UPDATE SET position = EXCLUDED.position, \
             revoked = EXCLUDED.revoked, revoked_ms = EXCLUDED.revoked_ms",
            &[&ids, &positions, &revoked, &revoked_ms],
        )
        .await
        .map_err(sql)?;
    Ok(())
}

/// Creates or replaces one Host inside the caller's transaction.
pub(crate) async fn write_host(
    client: &impl GenericClient,
    host: &str,
    entry: &Host,
) -> Result<(), String> {
    let uids: Vec<&str> = entry.instance_uids.iter().map(String::as_str).collect();
    let subject = serde_json::to_value(&entry.subject).expect("subject values serialize");
    client
        .execute(
            "INSERT INTO hosts (host, gateway, instance_uids, subject, approved_ms, \
             valid_until_ms) VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (host) DO UPDATE SET \
             gateway = EXCLUDED.gateway, instance_uids = EXCLUDED.instance_uids, \
             subject = EXCLUDED.subject, approved_ms = EXCLUDED.approved_ms, \
             valid_until_ms = EXCLUDED.valid_until_ms",
            &[
                &host,
                &entry.gateway,
                &uids,
                &subject,
                &entry.approved_ms.map(signed),
                &entry.valid_until_ms.map(signed),
            ],
        )
        .await
        .map_err(sql)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::FsLedgerStore;
    use crate::postgres::testing::TestDatabase;
    use crate::revocation::{Revoked, SubjectValues};

    fn issued(serial: &str, predecessor: Option<&str>) -> Issued {
        Issued {
            facts: Facts {
                id: CertId::new(b"ca", serial),
                issuer_name: "CN=ca".into(),
                // A CSR chooses its subject: a NUL is kept as it is, and fails no write.
                subject: format!("CN=edge-{serial}\0"),
                key_fingerprint: "k".into(),
                not_after_ms: 9,
                host: predecessor.is_none().then(|| "0192f0c4-host".to_string()),
                subject_values: SubjectValues {
                    organization: "Kunde\0GmbH".into(),
                    ..Default::default()
                },
            },
            instance_uid: "00".into(),
            predecessor: predecessor.map(|serial| CertId::new(b"ca", serial)),
            issued_ms: 1,
        }
    }

    fn revocation(id: &str, serial: &str) -> Revocation {
        Revocation {
            id: id.into(),
            revoked: Revoked::Certificate {
                authority: "client".into(),
                issuers: vec![CertId::new(b"ca", serial).issuer],
                serial: serial.into(),
            },
            revoked_ms: u64::MAX - 1,
        }
    }

    fn host(uids: &[&str], gateway: bool) -> Host {
        Host {
            gateway,
            instance_uids: uids.iter().map(ToString::to_string).collect(),
            subject: SubjectValues {
                country: "D\0E".into(),
                ..Default::default()
            },
            approved_ms: Some(5),
            valid_until_ms: (!gateway).then_some(7),
        }
    }

    /// The port's contract on either store: register, list and Hosts load back as written, a
    /// removed entry goes, the list keeps the order its entries were added in, an entry revoking
    /// what one on the list revokes is answered with that one, a list at its bound takes no more,
    /// and Hosts saved again are exactly the new ones.
    fn contract(open: &dyn Fn() -> Box<dyn LedgerStore>) {
        let store = open();
        assert_eq!(store.load().expect("empty"), Ledger::default());
        let (root, renewal) = (issued("0a", None), issued("0b", Some("0a")));
        store.put_issued(&root).expect("put");
        store.put_issued(&renewal).expect("put");
        let list = vec![
            revocation("r1", "0a"),
            revocation("r2", "0c"),
            revocation("r3", "0d"),
        ];
        for entry in &list {
            assert_eq!(store.add_revocation(entry, 3).expect("add"), Added::Added);
        }
        assert_eq!(
            store
                .add_revocation(&revocation("r4", "0e"), 3)
                .expect("add"),
            Added::Full,
            "a list at its bound takes no more"
        );
        assert_eq!(
            store
                .add_revocation(&revocation("r1", "0a"), 3)
                .expect("add"),
            Added::Present(list[0].clone()),
            "the same revocation again is the one on the list, even at the bound"
        );
        let hosts = BTreeMap::from([
            ("a".to_string(), host(&["01", "02"], false)),
            ("gateway/1".to_string(), host(&[], true)),
        ]);
        store.save_hosts(&hosts).expect("save");

        let reopened = open();
        let ledger = reopened.load().expect("load");
        assert_eq!(ledger.issued, [root.clone(), renewal.clone()]);
        assert_eq!(ledger.revocations, list);
        assert_eq!(ledger.hosts, hosts);

        reopened.remove_issued(&root.facts.id).expect("remove");
        assert!(reopened.remove_revocation("r2").expect("remove"));
        assert!(
            !reopened.remove_revocation("r2").expect("remove"),
            "gone already"
        );
        assert_eq!(
            reopened
                .add_revocation(&revocation("r5", "0f"), 3)
                .expect("add"),
            Added::Added
        );
        let shorter = vec![
            revocation("r1", "0a"),
            revocation("r3", "0d"),
            revocation("r5", "0f"),
        ];
        let fewer = BTreeMap::from([("a".to_string(), host(&["01"], false))]);
        reopened.save_hosts(&fewer).expect("save");
        let ledger = open().load().expect("load");
        assert_eq!(ledger.issued, [renewal]);
        assert_eq!(
            ledger.revocations, shorter,
            "the entries left, in the order they were added"
        );
        assert_eq!(ledger.hosts, fewer);
    }

    /// Verifies: ADR-0029
    #[test]
    fn the_ledger_port_contract_holds_on_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        contract(&|| Box::new(FsLedgerStore::open(dir.path()).expect("open")));
    }

    /// Verifies: ADR-0029
    #[test]
    fn the_ledger_port_contract_holds_on_postgres() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        contract(&|| Box::new(PgLedgerStore::new(database.clone())));
    }

    /// A save of the Hosts that fails part-way leaves them as they were, and the next save still
    /// writes what changed: the adapter's memory follows only what committed.
    /// Verifies: ADR-0029
    #[test]
    fn a_save_that_fails_part_way_leaves_nothing_of_itself() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let store = PgLedgerStore::new(test.open());
        let before = BTreeMap::from([("a".to_string(), host(&["01"], false))]);
        store.save_hosts(&before).expect("save");
        test.sql("ALTER TABLE hosts ADD CONSTRAINT not_b CHECK (host <> 'b')");
        let after = BTreeMap::from([
            ("a".to_string(), host(&["01", "02"], false)),
            ("b".to_string(), host(&[], false)),
        ]);
        store.save_hosts(&after).expect_err("b cannot be written");
        assert_eq!(store.load().expect("load").hosts, before);
        test.sql("ALTER TABLE hosts DROP CONSTRAINT not_b");
        store.save_hosts(&after).expect("save");
        assert_eq!(store.load().expect("load").hosts, after);
    }
}
