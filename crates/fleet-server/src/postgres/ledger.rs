//! The register, the revocation list and the Hosts in PostgreSQL (ADR-0029 clauses 9 and 10): the
//! PostgreSQL adapter behind [`LedgerStore`](crate::revocation::LedgerStore) — one row per
//! certificate the Server signed, one per revocation, one per Host. Every call is one transaction,
//! committed before the decision it answers is.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use tokio_postgres::GenericClient;

use super::notify::{notify, CERTIFICATE, HOST};
use super::{client, signed, sql, unsigned, Database};
use crate::revocation::{
    register_full, Added, Bound, CertId, Facts, Host, HostAct, Issued, Ledger, LedgerStore,
    RegisterBounds, Registered, Revocation, CHAIN_FULL,
};

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
            notify(&*transaction, CERTIFICATE, &issued.facts.id.serial).await?;
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
            notify(&*transaction, CERTIFICATE, &id.serial).await?;
            transaction.commit().await.map_err(sql)
        })
    }

    /// One transaction that holds the register against every other writer and the list against
    /// every adder, then decides from what both hold: the register's size, the chain the entry
    /// renews walked to its root, whether a certificate on it is revoked, and how many entries
    /// that root already has below it.
    fn register_issued(
        &self,
        issued: &Issued,
        bounds: &RegisterBounds,
    ) -> Result<Registered, String> {
        self.register_minting(issued, bounds, None, None)
    }

    /// [`register_issued`](LedgerStore::register_issued), and the Host an approval mints written in
    /// the same transaction when the store has no row of it, else its valid until moved forward:
    /// a certificate is registered with its Host's row or not at all. An approval's bootstrap
    /// certificate is looked up in the list the transaction holds, so a revocation another
    /// instance committed first refuses it.
    fn register_minting(
        &self,
        issued: &Issued,
        bounds: &RegisterBounds,
        mint: Option<(&str, &Host)>,
        bootstrap: Option<&CertId>,
    ) -> Result<Registered, String> {
        let issued = issued.clone();
        let bounds = *bounds;
        let mint = mint.map(|(host, entry)| (host.to_string(), entry.clone()));
        let bootstrap = bootstrap.cloned();
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            transaction
                .batch_execute(
                    "LOCK TABLE certificates IN SHARE ROW EXCLUSIVE MODE; \
                     LOCK TABLE revocations IN SHARE MODE",
                )
                .await
                .map_err(sql)?;
            let id = &issued.facts.id;
            let held: i64 = transaction
                .query_one(
                    "SELECT count(*) FROM certificates WHERE NOT (issuer = $1 AND serial = $2)",
                    &[&id.issuer, &id.serial],
                )
                .await
                .map_err(sql)?
                .get(0);
            let held = usize::try_from(held).unwrap_or(usize::MAX);
            if held >= bounds.size {
                return Ok(Registered::Full(register_full(held)));
            }
            if let Some(bootstrap) = &bootstrap {
                if names(&*transaction, bootstrap).await? {
                    return Ok(Registered::BootstrapRevoked);
                }
            }
            if let Some(predecessor) = &issued.predecessor {
                let decided = decide_renewal(&*transaction, id, predecessor, &bounds).await?;
                if decided != Registered::Registered {
                    return Ok(decided);
                }
            }
            write_issued(&*transaction, &issued).await?;
            notify(&*transaction, CERTIFICATE, &issued.facts.id.serial).await?;
            if let Some((host, entry)) = &mint {
                if write_seen(&*transaction, host, entry).await? {
                    notify(&*transaction, HOST, host).await?;
                }
            }
            transaction.commit().await.map_err(sql)?;
            Ok(Registered::Registered)
        })
    }

    /// One transaction that holds the table against every other adder, so the bound is counted
    /// for every instance together: an entry of the same id is answered as it stands, or widened
    /// in place when the new one names more issuers, and a new one goes last.
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
            if let Some(row) = transaction
                .query_opt(
                    "SELECT id, revoked, revoked_ms FROM revocations WHERE id = $1",
                    &[&entry.id],
                )
                .await
                .map_err(sql)?
            {
                let id: String = row.get(0);
                let revoked = serde_json::from_value(row.get(1))
                    .map_err(|e| format!("revocation {id} in the database: {e}"))?;
                let held = Revocation {
                    id,
                    revoked,
                    revoked_ms: unsigned(row.get(2)),
                };
                let Some(wider) = crate::revocation::widened(&held, &entry) else {
                    return Ok(Added::Present(held));
                };
                let revoked =
                    serde_json::to_value(&wider.revoked).expect("a revocation serializes");
                transaction
                    .execute(
                        "UPDATE revocations SET revoked = $2 WHERE id = $1",
                        &[&wider.id, &revoked],
                    )
                    .await
                    .map_err(sql)?;
                super::notify::notify(&*transaction, super::notify::REVOCATION, &wider.id).await?;
                transaction.commit().await.map_err(sql)?;
                return Ok(Added::Widened(wider));
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
            let id = entry.id.clone();
            write_revocations(&*transaction, &[(position, entry)]).await?;
            super::notify::notify(&*transaction, super::notify::REVOCATION, &id).await?;
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
            super::notify::notify(&*transaction, super::notify::REVOCATION, &id).await?;
            transaction.commit().await.map_err(sql)?;
            Ok(true)
        })
    }

    fn bind(&self, instance_uid: &str, host: &str) -> Result<Bound, String> {
        self.bind_seen(instance_uid, host, &Host::default())
    }

    /// One row per binding, its `instance_uid` the key: a binding another Host holds is left as it
    /// is and answered with that Host, and nothing is written. The Host's row is made first, as
    /// `row` holds it, when the store has none, so no binding stands without its Host's row, and
    /// that row is held for the
    /// transaction: a Gateway mark committed before it is seen, and one that waits for it releases
    /// what it bound. A Host whose row is marked as a Gateway is bound none (ADR-0024 clause 16).
    /// The hold is one that conflicts with itself, so two binders of one Host wait for each other
    /// rather than both holding a share of the row they then write. Until no instance of the
    /// release before runs, an `instance_uid` another Host's array lists, as that release binds it,
    /// is answered as that Host's too, and a new binding goes into the Host's array as well; a
    /// Gateway's array binds nothing.
    fn bind_seen(&self, instance_uid: &str, host: &str, row: &Host) -> Result<Bound, String> {
        let (instance_uid, host, row) = (instance_uid.to_string(), host.to_string(), row.clone());
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            let made = insert_host_row(&*transaction, &host, &row).await?;
            let gateway: bool = transaction
                .query_one(
                    "SELECT gateway FROM hosts WHERE host = $1 FOR NO KEY UPDATE",
                    &[&host],
                )
                .await
                .map_err(sql)?
                .get(0);
            if gateway {
                return Ok(Bound::Gateway);
            }
            if let Some(row) = transaction
                .query_opt(
                    "SELECT host FROM hosts WHERE NOT gateway AND host <> $2 \
                     AND $1 = ANY(instance_uids) \
                     AND NOT EXISTS (SELECT 1 FROM host_bindings WHERE instance_uid = $1) \
                     ORDER BY host LIMIT 1",
                    &[&instance_uid, &host],
                )
                .await
                .map_err(sql)?
            {
                return Ok(Bound::Taken(row.get(0)));
            }
            let inserted = transaction
                .execute(
                    "INSERT INTO host_bindings (instance_uid, host) VALUES ($1, $2) \
                     ON CONFLICT (instance_uid) DO NOTHING",
                    &[&instance_uid, &host],
                )
                .await
                .map_err(sql)?;
            let holder: String = transaction
                .query_one(
                    "SELECT host FROM host_bindings WHERE instance_uid = $1",
                    &[&instance_uid],
                )
                .await
                .map_err(sql)?
                .get(0);
            if holder != host {
                // Refused: the transaction is dropped, so the row made for it is not kept either.
                return Ok(Bound::Taken(holder));
            }
            if inserted > 0 {
                append_to_array(&*transaction, &instance_uid, &host).await?;
            }
            if made || inserted > 0 {
                notify(&*transaction, HOST, &host).await?;
            }
            transaction.commit().await.map_err(sql)?;
            Ok(Bound::Bound)
        })
    }

    fn unbind(&self, instance_uid: &str) -> Result<Option<String>, String> {
        let instance_uid = instance_uid.to_string();
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            let host = unbind_in(&*transaction, &instance_uid).await?;
            if host.is_some() {
                notify(&*transaction, HOST, host.as_deref().unwrap_or_default()).await?;
            }
            transaction.commit().await.map_err(sql)?;
            Ok(host)
        })
    }

    /// One transaction: the binding of `old` is removed and `new` bound to its Host, so no other
    /// instance sees the Host speaking for neither.
    fn rebind(&self, old: &str, new: &str) -> Result<Option<(String, Bound)>, String> {
        let (old, new) = (old.to_string(), new.to_string());
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            let Some(host) = unbind_in(&*transaction, &old).await? else {
                return Ok(None);
            };
            transaction
                .execute(
                    "INSERT INTO host_bindings (instance_uid, host) VALUES ($1, $2) \
                     ON CONFLICT (instance_uid) DO NOTHING",
                    &[&new, &host],
                )
                .await
                .map_err(sql)?;
            let holder: String = transaction
                .query_one(
                    "SELECT host FROM host_bindings WHERE instance_uid = $1",
                    &[&new],
                )
                .await
                .map_err(sql)?
                .get(0);
            if holder == host {
                append_to_array(&*transaction, &new, &host).await?;
            }
            notify(&*transaction, HOST, &host).await?;
            transaction.commit().await.map_err(sql)?;
            let bound = if holder == host {
                Bound::Bound
            } else {
                Bound::Taken(holder)
            };
            Ok(Some((host, bound)))
        })
    }

    fn unbind_host(&self, host: &str) -> Result<(), String> {
        let host = host.to_string();
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            let removed = transaction
                .execute("DELETE FROM host_bindings WHERE host = $1", &[&host])
                .await
                .map_err(sql)?;
            if removed > 0 {
                notify(&*transaction, HOST, &host).await?;
            }
            transaction.commit().await.map_err(sql)
        })
    }

    fn writes_hosts_by_act(&self) -> bool {
        true
    }

    /// One transaction on the rows the act names, each field decided from what is stored, so no
    /// instance writes back another instance's change from a stale copy.
    fn write_host_act(&self, act: &HostAct) -> Result<(), String> {
        let act = act.clone();
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            let changed = match &act {
                HostAct::Seen { host, entry } => write_seen(&*transaction, host, entry).await?,
                HostAct::Extended(untils) => {
                    let mut changed = false;
                    for (host, until) in untils {
                        changed |= transaction
                            .execute(
                                "UPDATE hosts SET valid_until_ms = \
                                 GREATEST(valid_until_ms, $2) WHERE host = $1 \
                                 AND (valid_until_ms IS NULL OR valid_until_ms < $2)",
                                &[host, &signed(*until)],
                            )
                            .await
                            .map_err(sql)?
                            > 0;
                    }
                    changed
                }
                HostAct::Gateway { host, gateway } => {
                    let subject = serde_json::to_value(crate::revocation::SubjectValues::default())
                        .expect("subject values serialize");
                    transaction
                        .execute(
                            "INSERT INTO hosts (host, gateway, instance_uids, subject) \
                             VALUES ($1, $2, '{}', $3) \
                             ON CONFLICT (host) DO UPDATE SET gateway = EXCLUDED.gateway",
                            &[host, gateway, &subject],
                        )
                        .await
                        .map_err(sql)?;
                    if *gateway {
                        transaction
                            .execute("DELETE FROM host_bindings WHERE host = $1", &[host])
                            .await
                            .map_err(sql)?;
                        transaction
                            .execute(
                                "UPDATE hosts SET instance_uids = '{}' WHERE host = $1",
                                &[host],
                            )
                            .await
                            .map_err(sql)?;
                    }
                    true
                }
                HostAct::Forgotten { hosts, now_ms } => {
                    let gone: Vec<String> = transaction
                        .query(
                            "DELETE FROM hosts WHERE host = ANY($1) AND NOT gateway \
                             AND valid_until_ms <= $2 RETURNING host",
                            &[hosts, &signed(*now_ms)],
                        )
                        .await
                        .map_err(sql)?
                        .iter()
                        .map(|row| row.get(0))
                        .collect();
                    transaction
                        .execute("DELETE FROM host_bindings WHERE host = ANY($1)", &[&gone])
                        .await
                        .map_err(sql)?;
                    !gone.is_empty()
                }
            };
            if changed {
                notify(&*transaction, HOST, "").await?;
            }
            transaction.commit().await.map_err(sql)
        })
    }

    /// Writes each Host that changed since the last commit, and removes each one that is gone with
    /// its bindings. The `instance_uids` a Host's row keeps are those this instance holds, for a
    /// release before the bindings' table; the bindings are read from that table.
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
            transaction
                .execute("DELETE FROM host_bindings WHERE host = ANY($1)", &[&gone])
                .await
                .map_err(sql)?;
            for (host, entry) in &changed {
                write_host(&*transaction, host, entry).await?;
            }
            notify(&*transaction, HOST, "").await?;
            transaction.commit().await.map_err(sql)
        })?;
        hosts.clone_into(&mut written);
        Ok(())
    }
}

/// Removes the binding of `instance_uid` inside the caller's transaction: the Host it was bound to,
/// `None` when it was bound to none. The Hosts' arrays, which a release before the bindings' table
/// binds by, lose it too, so neither release finds it bound once it is released.
async fn unbind_in(
    client: &impl GenericClient,
    instance_uid: &str,
) -> Result<Option<String>, String> {
    let host = client
        .query_opt(
            "DELETE FROM host_bindings WHERE instance_uid = $1 RETURNING host",
            &[&instance_uid],
        )
        .await
        .map_err(sql)?
        .map(|row| row.get(0));
    client
        .execute(
            "UPDATE hosts SET instance_uids = array_remove(instance_uids, $1) \
             WHERE $1 = ANY(instance_uids)",
            &[&instance_uid],
        )
        .await
        .map_err(sql)?;
    Ok(host)
}

/// Makes `host`'s row as `entry` holds it when the store has none, its array empty: what binds the
/// Host is its rows in `host_bindings`, each listed in the array by the act that binds it. Answers
/// whether it made the row.
async fn insert_host_row(
    client: &impl GenericClient,
    host: &str,
    entry: &Host,
) -> Result<bool, String> {
    let subject = serde_json::to_value(&entry.subject).expect("subject values serialize");
    let made = client
        .execute(
            "INSERT INTO hosts (host, gateway, instance_uids, subject, approved_ms, \
             valid_until_ms) VALUES ($1, $2, '{}', $3, $4, $5) \
             ON CONFLICT (host) DO NOTHING",
            &[
                &host,
                &entry.gateway,
                &subject,
                &entry.approved_ms.map(signed),
                &entry.valid_until_ms.map(signed),
            ],
        )
        .await
        .map_err(sql)?;
    Ok(made > 0)
}

/// A Host an approval or a first report makes: its row as `entry` holds it when the store has
/// none, else only its valid until moved forward to `entry`'s. Answers whether a row changed.
async fn write_seen(client: &impl GenericClient, host: &str, entry: &Host) -> Result<bool, String> {
    if insert_host_row(client, host, entry).await? {
        return Ok(true);
    }
    let Some(until) = entry.valid_until_ms else {
        return Ok(false);
    };
    let moved = client
        .execute(
            "UPDATE hosts SET valid_until_ms = GREATEST(valid_until_ms, $2) WHERE host = $1 \
             AND (valid_until_ms IS NULL OR valid_until_ms < $2)",
            &[&host, &signed(until)],
        )
        .await
        .map_err(sql)?;
    Ok(moved > 0)
}

/// Lists `instance_uid` in `host`'s array inside the caller's transaction, as a release before the
/// bindings' table binds by it; a Host with no row yet takes it with the row its act writes.
async fn append_to_array(
    client: &impl GenericClient,
    instance_uid: &str,
    host: &str,
) -> Result<(), String> {
    client
        .execute(
            "UPDATE hosts SET instance_uids = array_append(instance_uids, $1) \
             WHERE host = $2 AND NOT $1 = ANY(instance_uids)",
            &[&instance_uid, &host],
        )
        .await
        .map_err(sql)?;
    Ok(())
}

/// Binds every `instance_uid` `entry` speaks for to `host`, in a database that holds no binding of
/// them yet: what an import of a data directory writes. A Gateway's Host is bound none
/// (ADR-0024 clause 16), whatever its file lists.
pub(crate) async fn write_bindings(
    client: &impl GenericClient,
    host: &str,
    entry: &Host,
) -> Result<(), String> {
    if entry.gateway {
        return Ok(());
    }
    let uids: Vec<&str> = entry.instance_uids.iter().map(String::as_str).collect();
    client
        .execute(
            "INSERT INTO host_bindings (instance_uid, host) SELECT unnest($1::text[]), $2",
            &[&uids, &host],
        )
        .await
        .map_err(sql)?;
    Ok(())
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
    // A Host speaks for the `instance_uid`s bound to it in their own table, whatever its row's
    // array says.
    for entry in hosts.values_mut() {
        entry.instance_uids.clear();
    }
    for row in client
        .query("SELECT instance_uid, host FROM host_bindings", &[])
        .await
        .map_err(sql)?
    {
        let (uid, host): (String, String) = (row.get(0), row.get(1));
        match hosts.get_mut(&host) {
            Some(entry) => {
                entry.instance_uids.insert(uid);
            }
            // A binding whose Host has no row still holds its `instance_uid` against every other
            // Host; it is named, for an operator to release.
            None => tracing::warn!(
                instance_uid = %uid,
                host = %host,
                "an instance_uid is bound to a Host the database holds no row of"
            ),
        }
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

/// The chain a renewal of `predecessor` continues, walked to its root in the register by one
/// recursive query, with whether a revocation names a certificate on it, and the entries that root
/// already has below it: `Registered` when neither refuses the renewal. The walk is bounded by the
/// register's size, which also ends a cycle a damaged table could hold.
async fn decide_renewal(
    client: &impl GenericClient,
    id: &CertId,
    predecessor: &CertId,
    bounds: &RegisterBounds,
) -> Result<Registered, String> {
    let row = client
        .query_one(
            "WITH RECURSIVE up (issuer, serial, depth) AS ( \
                 SELECT $1::text, $2::text, 0 \
                 UNION ALL \
                 SELECT c.predecessor_issuer, c.predecessor_serial, up.depth + 1 \
                 FROM certificates c JOIN up ON c.issuer = up.issuer AND c.serial = up.serial \
                 WHERE c.predecessor_issuer IS NOT NULL \
                   AND up.depth < (SELECT count(*) FROM certificates) \
             ), root AS ( \
                 SELECT issuer, serial FROM up ORDER BY depth DESC LIMIT 1 \
             ), down (issuer, serial) AS ( \
                 SELECT issuer, serial FROM root \
                 UNION \
                 SELECT c.issuer, c.serial \
                 FROM certificates c JOIN down \
                   ON c.predecessor_issuer = down.issuer AND c.predecessor_serial = down.serial \
             ) \
             SELECT \
                 EXISTS ( \
                     SELECT 1 FROM up, revocations r, \
                         json_array_elements_text(r.revoked -> 'certificate' -> 'issuers') i \
                     WHERE r.revoked -> 'certificate' ->> 'serial' = up.serial \
                       AND i = up.issuer \
                 ), \
                 (SELECT count(*) FROM down JOIN certificates c USING (issuer, serial) \
                  WHERE NOT (c.issuer = $3 AND c.serial = $4))",
            &[
                &predecessor.issuer,
                &predecessor.serial,
                &id.issuer,
                &id.serial,
            ],
        )
        .await
        .map_err(sql)?;
    if row.get::<_, bool>(0) {
        return Ok(Registered::Revoked);
    }
    let below = usize::try_from(row.get::<_, i64>(1)).unwrap_or(usize::MAX);
    if below >= bounds.per_root {
        return Ok(Registered::Full(CHAIN_FULL.to_string()));
    }
    Ok(Registered::Registered)
}

/// Whether a revocation in the list names the certificate `id`, by its serial under one of the
/// issuers it names.
pub(crate) async fn names(client: &impl GenericClient, id: &CertId) -> Result<bool, String> {
    Ok(client
        .query_one(
            "SELECT EXISTS ( \
                 SELECT 1 FROM revocations r, \
                     json_array_elements_text(r.revoked -> 'certificate' -> 'issuers') i \
                 WHERE r.revoked -> 'certificate' ->> 'serial' = $2 AND i = $1 \
             )",
            &[&id.issuer, &id.serial],
        )
        .await
        .map_err(sql)?
        .get(0))
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
    use crate::postgres::Exclusive;
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
    /// what one on the list revokes is answered with that one, one naming more issuers widens it
    /// in place, a list at its bound takes no more, and Hosts saved again are exactly the new ones.
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
        // The serial again under the CA file of a rotation: one more issuer beside the first.
        let mut both = revocation("r1", "0a");
        let Revoked::Certificate { issuers, .. } = &mut both.revoked;
        issuers.push(CertId::new(b"next ca", "0a").issuer);
        both.revoked_ms = 9;
        let mut wider = both.clone();
        wider.revoked_ms = list[0].revoked_ms;
        assert_eq!(
            store.add_revocation(&both, 3).expect("add"),
            Added::Widened(wider.clone()),
            "a revocation under more issuers widens the entry, even at the bound"
        );
        assert_eq!(
            store
                .add_revocation(&revocation("r1", "0a"), 3)
                .expect("add"),
            Added::Present(wider.clone()),
            "one under fewer issuers changes nothing"
        );
        let list = vec![wider.clone(), list[1].clone(), list[2].clone()];
        let hosts = BTreeMap::from([
            ("a".to_string(), host(&["01", "02"], false)),
            ("gateway/1".to_string(), host(&[], true)),
        ]);
        for uid in ["01", "02"] {
            assert_eq!(store.bind(uid, "a").expect("bind"), Bound::Bound);
        }
        store.save_hosts(&hosts).expect("save");
        assert_eq!(
            store.bind("01", "b").expect("bind"),
            Bound::Taken("a".into()),
            "an instance_uid one Host holds is not bound to another"
        );
        assert_eq!(store.bind("01", "a").expect("bind"), Bound::Bound);

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
        let shorter = vec![wider, revocation("r3", "0d"), revocation("r5", "0f")];
        let fewer = BTreeMap::from([("a".to_string(), host(&["01"], false))]);
        assert_eq!(
            reopened.unbind("02").expect("unbind"),
            Some("a".to_string()),
            "a release answers the Host the store held the instance_uid for"
        );
        reopened.save_hosts(&fewer).expect("save");
        let ledger = open().load().expect("load");
        assert_eq!(ledger.issued, [renewal]);
        assert_eq!(
            ledger.revocations, shorter,
            "the entries left, in the order they were added"
        );
        assert_eq!(ledger.hosts, fewer);

        // A Gateway's Host is bound none, whatever its record lists (ADR-0024 clause 16): the next
        // Host that reports an instance_uid it lists binds it. A Host that is gone takes its
        // bindings with it.
        let store = open();
        store.load().expect("load");
        let with_gateway = BTreeMap::from([
            ("a".to_string(), host(&["01"], false)),
            ("gateway/1".to_string(), host(&["09"], true)),
        ]);
        store.save_hosts(&with_gateway).expect("save");
        assert_eq!(
            store.bind("09", "c").expect("bind"),
            Bound::Bound,
            "an instance_uid a Gateway's Host lists is bound by the next Host that reports it"
        );
        let without_a = BTreeMap::from([("gateway/1".to_string(), host(&["09"], true))]);
        store.save_hosts(&without_a).expect("save");
        assert_eq!(
            store.bind("01", "b").expect("bind"),
            Bound::Bound,
            "a Host that is gone holds no instance_uid"
        );
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

    /// A register write's contract on either store: it decides from what is stored, and a write it
    /// refuses leaves the register as it was. The register's size counts the other entries, an
    /// entry written again counting once; a renewal whose chain holds a revoked certificate is
    /// refused, the chain walked past a root the register never held; and a chain at its bound
    /// below its root takes no more.
    fn register_contract(open: &dyn Fn() -> Box<dyn LedgerStore>) {
        let store = open();
        store.load().expect("empty");
        let bounds = RegisterBounds {
            size: 3,
            per_root: 2,
        };
        let registered = |issued: &Issued| store.register_issued(issued, &bounds).expect("write");
        // 0a renews 09, which the register never held: 09 is the root of the chain.
        let (root, renewal) = (issued("0a", Some("09")), issued("0b", Some("0a")));
        assert_eq!(registered(&root), Registered::Registered);
        assert_eq!(registered(&renewal), Registered::Registered);
        assert_eq!(
            registered(&renewal),
            Registered::Registered,
            "an entry written again counts once"
        );
        assert_eq!(
            registered(&issued("0c", Some("0b"))),
            Registered::Full("this certificate's chain has been renewed too often".into()),
            "two below the root 09 already"
        );
        assert_eq!(registered(&issued("1a", None)), Registered::Registered);
        assert_eq!(
            registered(&issued("2a", None)),
            Registered::Full("the register holds 3 certificates already".into())
        );
        let wider = RegisterBounds {
            size: 10,
            per_root: 10,
        };
        assert_eq!(
            store
                .add_revocation(&revocation("r9", &CertId::new(b"ca", "09").serial), 10)
                .expect("revoke"),
            Added::Added
        );
        assert_eq!(
            store
                .register_issued(&issued("0c", Some("0b")), &wider)
                .expect("write"),
            Registered::Revoked,
            "the root two renewals up is revoked"
        );
        assert_eq!(
            store
                .register_issued(&issued("1b", Some("1a")), &wider)
                .expect("write"),
            Registered::Registered,
            "another chain is not"
        );
        let ids: BTreeSet<CertId> = open()
            .load()
            .expect("load")
            .issued
            .into_iter()
            .map(|entry| entry.facts.id)
            .collect();
        let expected: BTreeSet<CertId> = ["0a", "0b", "1a", "1b"]
            .into_iter()
            .map(|serial| CertId::new(b"ca", serial))
            .collect();
        assert_eq!(ids, expected, "nothing refused was written");
    }

    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn the_register_write_decides_from_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        register_contract(&|| Box::new(FsLedgerStore::open(dir.path()).expect("open")));
    }

    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn the_register_write_decides_from_postgres() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        register_contract(&|| Box::new(PgLedgerStore::new(database.clone())));
    }

    /// Two instances registering at once count the register's size together: in every round two
    /// writers race for the one place left, behind a barrier, and exactly one is written while the
    /// other finds the register full.
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn two_writers_at_the_registers_bound_write_one_entry_between_them() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let stores = [
            Arc::new(PgLedgerStore::new(database.clone())),
            Arc::new(PgLedgerStore::new(database.clone())),
        ];
        const ROUNDS: usize = 20;
        for round in 0..ROUNDS {
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let answers: Vec<Registered> = std::thread::scope(|scope| {
                let racers: Vec<_> = stores
                    .iter()
                    .enumerate()
                    .map(|(racer, store)| {
                        let barrier = barrier.clone();
                        scope.spawn(move || {
                            let entry = issued(&format!("{round:02x}{racer:02x}"), None);
                            let bounds = RegisterBounds {
                                size: round + 1,
                                per_root: ROUNDS,
                            };
                            barrier.wait();
                            store.register_issued(&entry, &bounds).expect("write")
                        })
                    })
                    .collect();
                racers
                    .into_iter()
                    .map(|racer| racer.join().expect("racer"))
                    .collect()
            });
            let written = answers
                .iter()
                .filter(|answer| **answer == Registered::Registered)
                .count();
            assert_eq!(written, 1, "round {round}: {answers:?}");
        }
        assert_eq!(stores[0].load().expect("load").issued.len(), ROUNDS);
    }

    /// An instance whose view of the list is stale cannot register the renewal of a certificate
    /// another instance revoked: B never hears of the revocation through A, and the register write
    /// refuses the renewal from what the database holds.
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn a_stale_instance_cannot_register_the_renewal_of_a_revoked_certificate() {
        use crate::revocation::{Authority, Revocations, RENEWS_REVOKED};
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        struct Now;
        impl crate::fleet::Clock for Now {
            fn now_ms(&self) -> u64 {
                1
            }
        }
        let instance = || {
            Revocations::open(
                Box::new(PgLedgerStore::new(database.clone())),
                Arc::new(Now),
                vec![Authority {
                    role: "client".into(),
                    subject: CertId::new(b"ca", "00").issuer,
                    name: "CN=ca".into(),
                }],
            )
            .expect("open")
        };
        let a = instance();
        a.record(issued("0a", None).facts, b"\x01", None)
            .expect("enrol");
        let b = instance();
        a.revoke_certificate("client", "0a").expect("revoke");
        let mut renewal = issued("0b", Some("0a")).facts;
        renewal.host = None;
        let refused = b
            .record(renewal, b"\x01", Some(CertId::new(b"ca", "0a")))
            .expect_err("refused");
        assert_eq!(refused, RENEWS_REVOKED);
        assert_eq!(
            PgLedgerStore::new(database.clone())
                .load()
                .expect("load")
                .issued
                .len(),
            1,
            "the renewal was not written"
        );
    }

    /// Two instances adding at once count the bound together: in every round two adders race for
    /// the one place left, behind a barrier, and exactly one is added while the other finds the
    /// list full, so the list never grows past its bound and no two entries share a place.
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn two_adders_at_the_bound_add_one_entry_between_them() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let stores = [
            Arc::new(PgLedgerStore::new(database.clone())),
            Arc::new(PgLedgerStore::new(database.clone())),
        ];
        const ROUNDS: usize = 20;
        for round in 0..ROUNDS {
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let answers: Vec<Added> = std::thread::scope(|scope| {
                let racers: Vec<_> = stores
                    .iter()
                    .enumerate()
                    .map(|(racer, store)| {
                        let barrier = barrier.clone();
                        scope.spawn(move || {
                            let entry = revocation(
                                &format!("r{round}-{racer}"),
                                &format!("{round:02x}{racer:02x}"),
                            );
                            barrier.wait();
                            store.add_revocation(&entry, round + 1).expect("add")
                        })
                    })
                    .collect();
                racers
                    .into_iter()
                    .map(|racer| racer.join().expect("racer"))
                    .collect()
            });
            let added = answers
                .iter()
                .filter(|answer| **answer == Added::Added)
                .count();
            let full = answers
                .iter()
                .filter(|answer| **answer == Added::Full)
                .count();
            assert_eq!((added, full), (1, 1), "round {round}: {answers:?}");
        }
        assert_eq!(stores[0].load().expect("load").revocations.len(), ROUNDS);
        let positions = database
            .run(|pool| async move {
                let client = client(&pool).await?;
                let row = client
                    .query_one("SELECT count(DISTINCT position) FROM revocations", &[])
                    .await
                    .map_err(sql)?;
                Ok(row.get::<_, i64>(0))
            })
            .expect("positions");
        assert_eq!(
            positions,
            i64::try_from(ROUNDS).expect("rounds"),
            "one place each"
        );
    }

    /// The bindings move out of the Hosts' arrays of a database a release before them wrote: each
    /// `instance_uid` an array holds is bound to its Host. An `instance_uid` two Hosts' arrays hold
    /// fails the migration, naming it, and changes nothing.
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn the_bindings_move_out_of_the_hosts_arrays() {
        use crate::postgres::schema::{MIGRATIONS, SCHEMA_TABLE};
        for (hosts, moved) in [
            (
                BTreeMap::from([
                    ("a".to_string(), host(&["01", "02"], false)),
                    ("b".to_string(), host(&["03"], false)),
                ]),
                true,
            ),
            (
                BTreeMap::from([
                    ("a".to_string(), host(&["01"], false)),
                    ("b".to_string(), host(&["01"], false)),
                ]),
                false,
            ),
        ] {
            let Some(test) = TestDatabase::named() else {
                return;
            };
            let database = test.open();
            PgLedgerStore::new(database.clone())
                .save_hosts(&hosts)
                .expect("save");
            // The database as the release before the bindings left it.
            test.sql(&format!(
                "DROP TABLE host_bindings; DROP INDEX certificates_predecessor; \
                 DELETE FROM {SCHEMA_TABLE} WHERE version = 4"
            ));
            let applied = database.apply_schema(MIGRATIONS);
            if moved {
                assert_eq!(applied.expect("migrate").versions, [4]);
                assert_eq!(
                    PgLedgerStore::new(database.clone())
                        .load()
                        .expect("load")
                        .hosts,
                    hosts
                );
            } else {
                let error = applied.expect_err("two Hosts hold 01");
                assert!(
                    error.contains("the instance_uid 01 is bound to the Hosts a, b"),
                    "{error}"
                );
                assert!(
                    test.rows("SELECT 1 WHERE to_regclass('host_bindings') IS NOT NULL")
                        .is_empty(),
                    "nothing changed"
                );
            }
        }
    }

    /// Two instances binding one `instance_uid` at once, each to a Host of its own, bind it once:
    /// in every round two binders race behind a barrier, and exactly one binds while the other is
    /// answered with the Host that holds it.
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn two_binders_of_one_instance_uid_bind_it_once() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let stores = [
            Arc::new(PgLedgerStore::new(database.clone())),
            Arc::new(PgLedgerStore::new(database.clone())),
        ];
        for round in 0..20 {
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let uid = format!("{round:02x}");
            let answers: Vec<Bound> = std::thread::scope(|scope| {
                let racers: Vec<_> = stores
                    .iter()
                    .enumerate()
                    .map(|(racer, store)| {
                        let (barrier, uid) = (barrier.clone(), uid.clone());
                        scope.spawn(move || {
                            barrier.wait();
                            store.bind(&uid, &format!("host-{racer}")).expect("bind")
                        })
                    })
                    .collect();
                racers
                    .into_iter()
                    .map(|racer| racer.join().expect("racer"))
                    .collect()
            });
            assert_eq!(
                answers.iter().filter(|a| **a == Bound::Bound).count(),
                1,
                "round {round}: {answers:?}"
            );
        }
    }

    /// Two instances binding two `instance_uid`s to one Host at once both bind theirs: the row
    /// each holds while it binds is a hold that waits for the other, not a share both take and
    /// then write, which the database would break as a deadlock
    /// (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    /// clause 19).
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn two_binders_of_one_host_bind_both_without_a_deadlock() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let stores = [
            Arc::new(PgLedgerStore::new(database.clone())),
            Arc::new(PgLedgerStore::new(database.clone())),
        ];
        stores[0]
            .save_hosts(&BTreeMap::from([("h".to_string(), host(&[], false))]))
            .expect("save");
        for round in 0..20 {
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let answers: Vec<Result<Bound, String>> = std::thread::scope(|scope| {
                let racers: Vec<_> = stores
                    .iter()
                    .enumerate()
                    .map(|(racer, store)| {
                        let barrier = barrier.clone();
                        scope.spawn(move || {
                            barrier.wait();
                            store.bind(&format!("{round:02x}{racer}"), "h")
                        })
                    })
                    .collect();
                racers
                    .into_iter()
                    .map(|racer| racer.join().expect("racer"))
                    .collect()
            });
            assert_eq!(
                answers,
                [Ok(Bound::Bound), Ok(Bound::Bound)],
                "round {round}"
            );
        }
    }

    /// A Gateway's mark and a binding to its Host made at once never leave a binding to it: the
    /// binding waits for the mark and sees it, or the mark waits for the binding and releases it
    /// (ADR-0024 clause 16). The Host has no row before either.
    /// Verifies: ADR-0024
    #[test]
    fn a_gateway_mark_and_a_binding_at_once_leave_the_gateway_bound_none() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (marker, binder) = (
            PgLedgerStore::new(database.clone()),
            PgLedgerStore::new(database.clone()),
        );
        for round in 0..20 {
            let gateway = format!("gateway/{round}");
            let barrier = std::sync::Barrier::new(2);
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    barrier.wait();
                    marker
                        .write_host_act(&HostAct::Gateway {
                            host: gateway.clone(),
                            gateway: true,
                        })
                        .expect("mark");
                });
                scope.spawn(|| {
                    barrier.wait();
                    binder
                        .bind(&format!("{round:02x}"), &gateway)
                        .expect("bind");
                });
            });
        }
        assert_eq!(
            test.rows(
                "SELECT b.instance_uid FROM host_bindings b JOIN hosts h ON h.host = b.host \
                 WHERE h.gateway"
            ),
            Vec::<Vec<Option<String>>>::new()
        );
    }

    /// A binding is made with its Host's row, in one write, so no binding stands without the Host
    /// it binds to; one another Host holds writes nothing, the row it would have made included
    /// (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    /// clause 19).
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn a_binding_is_made_with_its_hosts_row() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let store = PgLedgerStore::new(database.clone());
        let row = Host {
            subject: SubjectValues {
                country: "AT".into(),
                ..Default::default()
            },
            valid_until_ms: Some(42),
            ..Host::default()
        };
        assert_eq!(store.bind_seen("01", "h1", &row), Ok(Bound::Bound));
        assert_eq!(
            store.bind_seen("01", "h2", &row),
            Ok(Bound::Taken("h1".to_string()))
        );
        let hosts = PgLedgerStore::new(database.clone())
            .load()
            .expect("load")
            .hosts;
        let mut bound = row.clone();
        bound.instance_uids.insert("01".to_string());
        assert_eq!(hosts, BTreeMap::from([("h1".to_string(), bound)]));
    }

    /// A certificate an approval mints a Host for is registered with the Host's row or not at all:
    /// the row is made in the register's own write, with the approval, and a refused write makes
    /// neither (ADR-0024 clause 15).
    /// Verifies: ADR-0024
    #[test]
    fn an_approvals_host_is_written_with_its_register_entry_or_not_at_all() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let store = PgLedgerStore::new(database.clone());
        let minted = Host {
            approved_ms: Some(3),
            valid_until_ms: Some(9),
            ..host(&[], false)
        };
        let full = RegisterBounds {
            size: 0,
            per_root: 8,
        };
        let entry = issued("0a", None);
        assert!(matches!(
            store.register_minting(&entry, &full, Some(("h", &minted)), None),
            Ok(Registered::Full(_))
        ));
        let ledger = store.load().expect("load");
        assert!(
            ledger.issued.is_empty() && ledger.hosts.is_empty(),
            "{ledger:?}"
        );

        let open = RegisterBounds {
            size: 8,
            per_root: 8,
        };
        assert_eq!(
            store.register_minting(&entry, &open, Some(("h", &minted)), None),
            Ok(Registered::Registered)
        );
        let ledger = store.load().expect("load");
        assert_eq!(ledger.issued, [entry]);
        assert_eq!(ledger.hosts, BTreeMap::from([("h".to_string(), minted)]));
    }

    /// An approval whose bootstrap certificate the list names registers nothing, and neither
    /// does its Host: the register's write reads the list it holds, so a revocation another
    /// instance committed first wins (ADR-0021 clause 28).
    /// Verifies: ADR-0021
    #[test]
    fn an_approval_of_a_revoked_bootstrap_certificate_registers_nothing() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let store = PgLedgerStore::new(test.open());
        let bootstrap = CertId::new(b"bootstrap CA", "b5");
        store
            .add_revocation(
                &Revocation {
                    id: "r1".to_string(),
                    revoked: Revoked::Certificate {
                        authority: "bootstrap".to_string(),
                        issuers: vec![bootstrap.issuer.clone()],
                        serial: bootstrap.serial.clone(),
                    },
                    revoked_ms: 1,
                },
                100,
            )
            .expect("revoke");
        let open = RegisterBounds {
            size: 8,
            per_root: 8,
        };
        let minted = host(&[], false);
        let entry = issued("0a", None);
        assert_eq!(
            store.register_minting(&entry, &open, Some(("h", &minted)), Some(&bootstrap)),
            Ok(Registered::BootstrapRevoked)
        );
        let ledger = store.load().expect("load");
        assert!(
            ledger.issued.is_empty() && ledger.hosts.is_empty(),
            "{ledger:?}"
        );
        assert_eq!(
            store.register_minting(
                &entry,
                &open,
                Some(("h", &minted)),
                Some(&CertId::new(b"bootstrap CA", "b6"))
            ),
            Ok(Registered::Registered)
        );
    }

    /// An instance whose view of the Hosts is stale cannot bind an `instance_uid` another
    /// instance bound to another Host: B never hears of A's binding, and its report is refused
    /// naming the Host that holds the Agent, by which B decides from then on.
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn a_stale_instance_cannot_bind_an_agent_another_host_holds() {
        use crate::revocation::{bound_elsewhere, Revocations};
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        struct Now;
        impl crate::fleet::Clock for Now {
            fn now_ms(&self) -> u64 {
                1
            }
        }
        let instance = || {
            Revocations::open(
                Box::new(PgLedgerStore::new(database.clone())),
                Arc::new(Now),
                Vec::new(),
            )
            .expect("open")
        };
        let (a, b) = (instance(), instance());
        let subject = SubjectValues::default();
        a.check_report("h1", &subject, 1_000, b"\x01\x02")
            .expect("A binds it to h1");
        let refused = b
            .check_report("h2", &subject, 1_000, b"\x01\x02")
            .expect_err("B is refused");
        assert_eq!(refused, bound_elsewhere("0102", "h1"));
        assert_eq!(
            b.may_report("h2", b"\x01\x02"),
            Err("this certificate belongs to another host than that Agent's".to_string()),
            "B holds the binding that holds"
        );
    }

    /// The instances on one schema, each opened over its own adapter, at a clock that never moves.
    fn instances_on(database: &Arc<Database>) -> impl Fn() -> crate::revocation::Revocations + '_ {
        struct Now;
        impl crate::fleet::Clock for Now {
            fn now_ms(&self) -> u64 {
                1
            }
        }
        move || {
            crate::revocation::Revocations::open(
                Box::new(PgLedgerStore::new(database.clone())),
                Arc::new(Now),
                Vec::new(),
            )
            .expect("open")
        }
    }

    /// Instance B, on a database handle of its own, so it hears what the instances on `database`
    /// announce, following the notifications on the runtime it answers.
    fn follower(
        test: &TestDatabase,
    ) -> (Arc<crate::revocation::Revocations>, tokio::runtime::Runtime) {
        let database = Database::open_in(&test.config(), Exclusive::Shared, Some(&test.schema))
            .expect("B's database");
        let b = Arc::new(instances_on(&database)());
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        {
            let _entered = runtime.enter();
            let state = crate::fleet::AppState::on(&crate::Records::Postgres(database.clone()))
                .expect("B's state")
                .with_revocations(Some(b.clone()));
            crate::instance::follow(&database, Arc::new(state));
        }
        (b, runtime)
    }

    /// A release and a re-key hold for every instance, whatever the instance that makes them holds
    /// in memory: B never heard of A's bindings, and what it releases and moves is what a third
    /// instance finds stored; a Gateway's mark leaves its Host bound none in the store
    /// (ADR-0024 clauses 10 and 16).
    /// Verifies: ADR-0024
    #[test]
    fn a_release_a_rekey_and_a_gateway_mark_hold_for_every_instance() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let instance = instances_on(&database);
        let (a, b) = (instance(), instance());
        let subject = SubjectValues::default();
        for uid in [b"\x01", b"\x02", b"\x03"] {
            a.check_report("h1", &subject, 1_000, uid)
                .expect("A binds it to h1");
        }
        a.check_report("g", &subject, 1_000, b"\x04")
            .expect("A binds it to g");

        b.release(b"\x01")
            .expect("B releases what it never heard of");
        b.rebind(b"\x02", b"\x05")
            .expect("B moves what it never heard of");
        assert!(a.set_gateway("g", true).expect("mark"));

        let c = instance();
        c.check_report("h2", &subject, 1_000, b"\x01")
            .expect("a released instance_uid is bound by the next Host that reports it");
        c.check_report("h2", &subject, 1_000, b"\x02")
            .expect("a re-keyed instance_uid is free");
        assert_eq!(
            c.speaks_for("h1"),
            crate::revocation::SpeaksFor::Agents(["03", "05"].map(String::from).into()),
            "the new instance_uid is bound to the Host the old one was"
        );
        assert!(c.check_report("h2", &subject, 1_000, b"\x05").is_err());
        c.check_report("h2", &subject, 1_000, b"\x04")
            .expect("a Gateway's Host is bound none");
    }

    /// A stale instance re-keys only what the store binds: after another instance released the
    /// old `instance_uid`, a re-key on an instance that never heard of the release binds nothing,
    /// and the new `instance_uid` is bound by the next Host that reports it (ADR-0024 clause 10).
    /// Verifies: ADR-0024
    #[test]
    fn a_stale_rekey_binds_nothing_another_instance_released() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let instance = instances_on(&database);
        let subject = SubjectValues::default();
        let a = instance();
        a.check_report("h1", &subject, 1_000, b"\x01")
            .expect("A binds it to h1");
        instance().release(b"\x01").expect("B releases it");
        a.rebind(b"\x01", b"\x02")
            .expect("A, stale, re-keys what the store no longer binds");
        instance()
            .check_report("h2", &subject, 1_000, b"\x02")
            .expect("the new instance_uid is free for the next Host that reports it");
    }

    /// What one instance binds, releases and moves reaches every other that follows the
    /// notifications, without a restart: B admits by A's release, A's re-key and A's binding once
    /// it hears of them (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    /// clause 19).
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn a_binding_another_instance_changes_reaches_an_instance_that_follows() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let instance = instances_on(&database);
        let a = instance();
        let subject = SubjectValues::default();
        a.check_report("h1", &subject, 1_000, b"\x01")
            .expect("bind");
        a.check_report("h1", &subject, 1_000, b"\x02")
            .expect("bind");
        let (b, _runtime) = follower(&test);
        let eventually = |condition: &dyn Fn() -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                if condition() {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            false
        };
        assert!(b.may_report("h2", b"\x01").is_err(), "B holds A's binding");

        a.release(b"\x01").expect("release");
        assert!(
            eventually(&|| b.may_report("h2", b"\x01").is_ok()),
            "B hears of the release"
        );
        a.rebind(b"\x02", b"\x03").expect("re-key");
        assert!(
            eventually(
                &|| b.may_report("h2", b"\x02").is_ok() && b.may_report("h2", b"\x03").is_err()
            ),
            "B hears of the re-key"
        );
        assert!(b.may_report("h2", b"\x04").is_ok(), "nobody holds 04 yet");
        a.check_report("h1", &subject, 1_000, b"\x04")
            .expect("bind");
        assert!(
            eventually(&|| b.may_report("h2", b"\x04").is_err()),
            "B hears of the binding"
        );
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
        store.bind("01", "a").expect("bind");
        store.save_hosts(&before).expect("save");
        test.sql("ALTER TABLE hosts ADD CONSTRAINT not_b CHECK (host <> 'b')");
        let after = BTreeMap::from([
            ("a".to_string(), host(&["01", "02"], false)),
            ("b".to_string(), host(&[], false)),
        ]);
        store.save_hosts(&after).expect_err("b cannot be written");
        assert_eq!(store.load().expect("load").hosts, before);
        test.sql("ALTER TABLE hosts DROP CONSTRAINT not_b");
        store.bind("02", "a").expect("bind");
        store.save_hosts(&after).expect("save");
        assert_eq!(store.load().expect("load").hosts, after);
    }

    /// An `instance_uid` another Host's array lists, and no binding row holds, is answered as that
    /// Host's while the release before the bindings' table may still run beside this one: it binds
    /// by the arrays alone.
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn an_instance_uid_only_an_array_lists_is_that_hosts() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let store = PgLedgerStore::new(test.open());
        store
            .save_hosts(&BTreeMap::from([("a".to_string(), host(&["01"], false))]))
            .expect("save");
        assert!(test
            .rows("SELECT 1 FROM host_bindings WHERE instance_uid = '01'")
            .is_empty());
        assert_eq!(store.bind("01", "b"), Ok(Bound::Taken("a".to_string())));
        assert_eq!(store.bind("01", "a"), Ok(Bound::Bound));
    }

    /// A Gateway's Host is bound none when the bindings leave the arrays: an `instance_uid` a
    /// Gateway's array and a Host's array both list moves to the Host, and the migration passes
    /// (ADR-0024 clause 16).
    /// Verifies: ADR-0024
    #[test]
    fn the_bindings_leave_a_gateways_array_behind() {
        use crate::postgres::schema::{MIGRATIONS, SCHEMA_TABLE};
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        PgLedgerStore::new(database.clone())
            .save_hosts(&BTreeMap::from([
                ("g".to_string(), host(&["01"], true)),
                ("h".to_string(), host(&["01"], false)),
            ]))
            .expect("save");
        test.sql(&format!(
            "DROP TABLE host_bindings; DROP INDEX certificates_predecessor; \
             DELETE FROM {SCHEMA_TABLE} WHERE version = 4"
        ));
        assert_eq!(
            database.apply_schema(MIGRATIONS).expect("migrate").versions,
            [4]
        );
        assert_eq!(
            test.rows("SELECT instance_uid, host FROM host_bindings"),
            [vec![Some("01".to_string()), Some("h".to_string())]]
        );
    }

    /// An import binds none of the `instance_uid`s a Gateway's file lists (ADR-0024 clause 16).
    /// Verifies: ADR-0024
    #[test]
    fn an_import_binds_nothing_to_a_gateways_host() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        database
            .run(|pool| async move {
                let client = client(&pool).await?;
                write_bindings(&**client, "g", &host(&["01"], true)).await?;
                write_bindings(&**client, "h", &host(&["02"], false)).await
            })
            .expect("write");
        assert_eq!(
            test.rows("SELECT instance_uid, host FROM host_bindings"),
            [vec![Some("02".to_string()), Some("h".to_string())]]
        );
    }

    /// A register entry another instance writes reaches an instance that follows, without a
    /// restart, and a revocation of its chain's root revokes it there: B learns of A's renewal from
    /// its notification alone, and then refuses it once the root is revoked
    /// (ADR-draft-admission-by-client-certificate-and-revocation clause 28).
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn a_renewal_another_instance_registers_is_revoked_with_its_root_on_an_instance_that_follows() {
        use crate::revocation::Revoked;
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let instance = instances_on(&database);
        let a = instance();
        let (b, _runtime) = follower(&test);
        let eventually = |condition: &dyn Fn() -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                if condition() {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            false
        };
        let root = issued("0a", None).facts;
        let renewal = issued("0b", Some("0a")).facts;
        a.record(root.clone(), b"\x01", None).expect("root");
        a.record(renewal.clone(), b"\x01", Some(root.id.clone()))
            .expect("renewal");
        assert!(
            eventually(&|| b.issued_entry(&renewal.id).is_some()),
            "B hears of the renewal A registered"
        );
        assert!(!b.is_certificate_revoked(&renewal.id));
        PgLedgerStore::new(database.clone())
            .add_revocation(
                &Revocation {
                    id: "r".into(),
                    revoked: Revoked::Certificate {
                        authority: "client".into(),
                        issuers: vec![root.id.issuer.clone()],
                        serial: root.id.serial.clone(),
                    },
                    revoked_ms: 1,
                },
                10,
            )
            .expect("revoke the root");
        assert!(
            eventually(&|| b.is_certificate_revoked(&renewal.id)),
            "B refuses the renewal of a revoked root"
        );
    }

    /// An approval's fields survive a first report another instance makes from a stale view: B
    /// never heard of A's approval, makes the Host at its first report, and what is stored is
    /// still the approved Host, its valid until moved forward.
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn an_approval_survives_a_first_report_on_an_instance_that_never_heard_of_it() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let instance = instances_on(&database);
        let b = instance();
        let a = instance();
        let approved = issued("0a", None).facts;
        let host = approved.host.clone().expect("a host");
        a.record(approved.clone(), b"\x01", None).expect("approve");
        b.check_report(&host, &SubjectValues::default(), 50, b"\x02")
            .expect("B's first report");
        let stored = PgLedgerStore::new(database.clone())
            .load()
            .expect("load")
            .hosts
            .remove(&host)
            .expect("the Host");
        assert_eq!(stored.approved_ms, Some(1), "the approval is kept");
        assert_eq!(
            stored.subject, approved.subject_values,
            "its subject is kept"
        );
        assert_eq!(
            stored.valid_until_ms,
            Some(50),
            "its valid until moved forward"
        );
        assert!(stored.instance_uids.contains("02"));
    }

    /// No instance writes a Host's mark back from a stale copy: B takes the Gateway mark away, A
    /// never heard of it and moves the Host's valid until forward at its next report, and the
    /// Host stays unmarked (ADR-0024 clause 16).
    /// Verifies: ADR-0024
    #[test]
    fn a_stale_instance_does_not_write_a_gateways_mark_back() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let instance = instances_on(&database);
        let a = instance();
        let subject = SubjectValues::default();
        a.check_report("g", &subject, 10, b"\x01").expect("report");
        assert!(a.set_gateway("g", true).expect("mark"));
        let b = instance();
        assert!(b.set_gateway("g", false).expect("unmark"));
        a.check_report("g", &subject, 20, b"\x02")
            .expect("A, stale, extends the Host");
        let c = instance();
        assert!(!c.is_gateway("g"), "the mark B took away stays away");
    }

    /// A Host the store holds marked as a Gateway is bound none, however stale the binder's view:
    /// B never heard of A's mark, its report from the Gateway binds nothing, and the
    /// `instance_uid` stays free for the Host that carries it (ADR-0024 clause 16).
    /// Verifies: ADR-0024
    #[test]
    fn a_stale_instance_binds_nothing_to_a_gateways_host() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let instance = instances_on(&database);
        let a = instance();
        let subject = SubjectValues::default();
        a.check_report("g", &subject, 10, b"\x01").expect("report");
        let b = instance();
        assert!(a.set_gateway("g", true).expect("mark"));
        b.check_report("g", &subject, 10, b"\x02")
            .expect("B reports for the Gateway");
        assert!(test
            .rows("SELECT 1 FROM host_bindings WHERE host = 'g'")
            .is_empty());
        instance()
            .check_report("h", &subject, 10, b"\x02")
            .expect("the instance_uid is free");
    }
}
