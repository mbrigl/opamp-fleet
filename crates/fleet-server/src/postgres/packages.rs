//! Package records in PostgreSQL (ADR-0044 clause 6): the PostgreSQL adapter behind
//! [`PackageBackend`](crate::packages::PackageBackend) — one row per Package and one per entry,
//! each entry naming its artifact by hash, and each call one transaction on the rows it names
//! (ADR-0041
//! clause 19). The bytes are the blob store's, with either.

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio_postgres::GenericClient;

use super::notify::{notify, PACKAGE};
use super::{blob_lock_refused, BLOB_LOCK};
use super::{client, signed, sql, unsigned, Database};
use crate::packages::{
    assigned, pinned, Entry, Package, PackageBackend, PackageId, Platform, Signed, Stored,
    ED25519_SIGNATURE_LEN, REMOVED_WHILE_UPLOADED,
};

/// The Packages in `packages` and `package_entries`.
pub struct PgPackageBackend {
    database: Arc<Database>,
}

impl PgPackageBackend {
    /// The adapter on `database`.
    #[must_use]
    pub fn new(database: Arc<Database>) -> Self {
        PgPackageBackend { database }
    }
}

impl PackageBackend for PgPackageBackend {
    fn load(&self) -> Result<Vec<Package>, String> {
        self.database.run(|pool| async move {
            let client = client(&pool).await?;
            read_all(&**client).await
        })
    }

    fn create(&self, id: &PackageId) -> Result<(), String> {
        let id = id.clone();
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            let created = transaction
                .execute(
                    "INSERT INTO packages (agent_type, version) VALUES ($1, $2) \
                     ON CONFLICT DO NOTHING",
                    &[&id.agent_type, &id.version],
                )
                .await
                .map_err(sql)?;
            if created > 0 {
                notify(&*transaction, PACKAGE, &id.to_string()).await?;
            }
            transaction.commit().await.map_err(sql)
        })
    }

    /// The entry's row alone, inserted or replaced, while no stored assignment pins the Package
    /// ([`hold_unassigned`]). The transaction takes the blob store's lock shared first, and asks
    /// `stored` under it before it locks the Package's row: a sweep holds the lock alone, so none
    /// runs between the answer and the commit, and none deletes the blob the entry names
    /// (ADR-0044 clause 14). A blob a sweep marked is refused before the store is asked: that
    /// sweep may still delete it once its lock is gone. The lock goes with the
    /// transaction, its session's end included, which writes no entry.
    fn put_entry(&self, id: &PackageId, entry: &Entry, stored: Stored) -> Result<(), String> {
        let (id, entry) = (id.clone(), entry.clone());
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            transaction
                .execute(
                    &format!("SELECT pg_advisory_xact_lock_shared({BLOB_LOCK})"),
                    &[],
                )
                .await
                .map_err(|e| format!("{} — upload the artifact again", blob_lock_refused(e)))?;
            if transaction
                .query_opt(
                    "SELECT 1 FROM blob_deletions WHERE sha256 = $1",
                    &[&hex::encode(&entry.content_hash)],
                )
                .await
                .map_err(sql)?
                .is_some()
            {
                return Err(REMOVED_WHILE_UPLOADED.to_string());
            }
            // The blob store is asked off the database's runtime, as its calls block, and before
            // the Package's row is locked, so no act on the row waits for the bucket: the shared
            // lock already keeps every sweep out until the commit.
            if !tokio::task::spawn_blocking(move || stored())
                .await
                .map_err(|e| format!("the check for the artifact ended: {e}"))??
            {
                return Err(REMOVED_WHILE_UPLOADED.to_string());
            }
            if !hold_unassigned(&*transaction, &id, |id, _| assigned(id)).await? {
                return Err(format!("no package set {id}"));
            }
            transaction
                .execute(
                    "INSERT INTO package_entries (agent_type, version, os, arch, sha256, size, \
                     signature) VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (agent_type, \
                     version, os, arch) DO UPDATE SET sha256 = EXCLUDED.sha256, \
                     size = EXCLUDED.size, signature = EXCLUDED.signature",
                    &[
                        &id.agent_type,
                        &id.version,
                        &entry.platform.os,
                        &entry.platform.arch,
                        &hex::encode(&entry.content_hash),
                        &signed(entry.size),
                        &entry.signature,
                    ],
                )
                .await
                .map_err(sql)?;
            notify(&*transaction, PACKAGE, &id.to_string()).await?;
            transaction.commit().await.map_err(sql)
        })
    }

    /// One statement on the entry's row, conditioned on the bytes it names, while no stored
    /// assignment pins the Package ([`hold_unassigned`]): the row is locked by the update, so it is
    /// decided from what is stored. When it writes nothing, the entry as stored is read in the same
    /// transaction.
    fn sign(
        &self,
        id: &PackageId,
        platform: &Platform,
        content_hash: &[u8],
        signature: Option<&[u8]>,
    ) -> Result<Signed, String> {
        let (id, platform) = (id.clone(), platform.clone());
        let (sha256, signature) = (hex::encode(content_hash), signature.map(<[u8]>::to_vec));
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            hold_unassigned(&*transaction, &id, |id, _| assigned(id)).await?;
            let updated = transaction
                .execute(
                    "UPDATE package_entries SET signature = $6 WHERE agent_type = $1 \
                     AND version = $2 AND os = $3 AND arch = $4 AND sha256 = $5",
                    &[
                        &id.agent_type,
                        &id.version,
                        &platform.os,
                        &platform.arch,
                        &sha256,
                        &signature,
                    ],
                )
                .await
                .map_err(sql)?;
            let signed = if updated == 1 {
                notify(&*transaction, PACKAGE, &id.to_string()).await?;
                Signed::Written
            } else {
                let stored = transaction
                    .query_opt(
                        &format!(
                            "SELECT {ENTRY} FROM package_entries WHERE agent_type = $1 \
                             AND version = $2 AND os = $3 AND arch = $4"
                        ),
                        &[&id.agent_type, &id.version, &platform.os, &platform.arch],
                    )
                    .await
                    .map_err(sql)?;
                Signed::Changed(stored.as_ref().map(entry).transpose()?.map(|(_, e)| e))
            };
            transaction.commit().await.map_err(sql)?;
            Ok(signed)
        })
    }

    /// The entry's row alone, while no stored assignment pins the Package ([`hold_unassigned`]).
    fn remove_entry(&self, id: &PackageId, platform: &Platform) -> Result<(), String> {
        let (id, platform) = (id.clone(), platform.clone());
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            hold_unassigned(&*transaction, &id, |id, _| assigned(id)).await?;
            let removed = transaction
                .execute(
                    "DELETE FROM package_entries WHERE agent_type = $1 AND version = $2 \
                     AND os = $3 AND arch = $4",
                    &[&id.agent_type, &id.version, &platform.os, &platform.arch],
                )
                .await
                .map_err(sql)?;
            if removed > 0 {
                notify(&*transaction, PACKAGE, &id.to_string()).await?;
            }
            transaction.commit().await.map_err(sql)
        })
    }

    /// The Package and its entries go in one statement, the entries by cascade, while no stored
    /// assignment pins it ([`hold_unassigned`]), and it leaves every Deployment that holds it in
    /// the same transaction. A Package that is not stored is left as a Configuration that is not
    /// is: nothing is written and nothing announced.
    fn remove(&self, id: &PackageId) -> Result<(), String> {
        let id = id.clone();
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            if !hold_unassigned(&*transaction, &id, pinned).await? {
                return transaction.commit().await.map_err(sql);
            }
            transaction
                .execute(
                    "DELETE FROM packages WHERE agent_type = $1 AND version = $2",
                    &[&id.agent_type, &id.version],
                )
                .await
                .map_err(sql)?;
            super::deployments::drop_member_rows(
                &*transaction,
                &crate::deployments::Member::Package(id.clone()),
            )
            .await?;
            notify(&*transaction, PACKAGE, &id.to_string()).await?;
            transaction.commit().await.map_err(sql)
        })
    }

    /// On a connection of its own (ADR-0044 clause 14).
    fn hold_blobs(&self) -> Result<crate::blobs::BlobHold, String> {
        self.hold()
    }

    /// On the session that holds the lock (ADR-0044 clause 14): the marks of every lease that is
    /// not live, or of none, are taken away first once they were last renewed more than
    /// [`MARKS_OUTLIVE`](super::MARKS_OUTLIVE) before, which a sweep that stopped short left; then
    /// each hash no stored entry names is marked in one statement, under the lease this instance
    /// sweeps under and the hold's sweep. A hash another sweep marked is left to it. With the
    /// session ended, the statement fails, and nothing is to be deleted.
    fn mark(
        &self,
        hold: &crate::blobs::BlobHold,
        hashes: &[String],
    ) -> Result<Vec<String>, String> {
        let (session, sweep) = session_of(hold)?;
        let (hashes, lease) = (hashes.to_vec(), self.database.sweeps_under());
        let outlive = super::MARKS_OUTLIVE.as_secs_f64();
        self.database.run(move |_pool| async move {
            session
                .execute(
                    "DELETE FROM blob_deletions WHERE (lease IS NULL OR lease NOT IN \
                     (SELECT instance_id::text FROM leases WHERE expires_at > now())) \
                     AND marked_at < now() - make_interval(secs => $1)",
                    &[&outlive],
                )
                .await
                .map_err(sql)?;
            Ok(session
                .query(
                    "INSERT INTO blob_deletions (sha256, lease, sweep) \
                     SELECT h, $2, $3 FROM unnest($1::text[]) AS h \
                     WHERE NOT EXISTS (SELECT 1 FROM package_entries WHERE sha256 = h) \
                     ON CONFLICT (sha256) DO NOTHING RETURNING sha256",
                    &[&hashes, &lease, &sweep],
                )
                .await
                .map_err(sql)?
                .iter()
                .map(|row| row.get(0))
                .collect())
        })
    }

    /// One statement on the hold's session: the sweep's mark of `sha256` renewed, if it is still
    /// there. The marks of the blobs after it are left as they are: another sweep may take one
    /// away meanwhile, and that blob's own confirmation then finds it gone.
    fn confirm(&self, hold: &crate::blobs::BlobHold, sha256: &str) -> Result<bool, String> {
        let (session, sweep) = session_of(hold)?;
        let sha256 = sha256.to_string();
        self.database.run(move |_pool| async move {
            Ok(session
                .query_opt(
                    "UPDATE blob_deletions SET marked_at = now() WHERE sweep = $1 \
                     AND sha256 = $2 RETURNING 1",
                    &[&sweep, &sha256],
                )
                .await
                .map_err(sql)?
                .is_some())
        })
    }

    /// Through the pool, as the hold's session may be gone.
    fn unmark(&self, sweep: &str) -> Result<(), String> {
        let sweep = sweep.to_string();
        self.database.run(move |pool| async move {
            client(&pool)
                .await?
                .execute("DELETE FROM blob_deletions WHERE sweep = $1", &[&sweep])
                .await
                .map(drop)
                .map_err(sql)
        })
    }
}

/// The session of a sweep's hold on the blob store's lock, and what its marks are known by.
fn session_of(
    hold: &crate::blobs::BlobHold,
) -> Result<(Arc<tokio_postgres::Client>, String), String> {
    match (hold.held::<super::LockHold>(), hold.sweep()) {
        (Some(held), Some(sweep)) => Ok((held.client.clone(), sweep.to_string())),
        _ => Err("a sweep on PostgreSQL holds the blob store's lock".to_string()),
    }
}

impl PgPackageBackend {
    /// [`PackageBackend::hold_blobs`] on the database.
    fn hold(&self) -> Result<crate::blobs::BlobHold, String> {
        self.database.hold_blobs()
    }
}

/// Holds the Package's row `FOR UPDATE` until the caller's transaction ends, and then counts the
/// stored assignments that pin it, failing with `refusal` when any does (ADR-0026 clauses 12 and
/// 13). A rollout holds the row of each Package it pins `FOR KEY SHARE` until its assignments are
/// committed, so the count waits for it and sees them, and no rollout pins the Package until this
/// transaction ends
/// (ADR-0041
/// clause 19). Answers whether the Package is stored.
async fn hold_unassigned(
    client: &impl GenericClient,
    id: &PackageId,
    refusal: fn(&PackageId, usize) -> String,
) -> Result<bool, String> {
    if client
        .query_opt(
            "SELECT 1 FROM packages WHERE agent_type = $1 AND version = $2 FOR UPDATE",
            &[&id.agent_type, &id.version],
        )
        .await
        .map_err(sql)?
        .is_none()
    {
        return Ok(false);
    }
    let pinning: i64 = client
        .query_one(
            "SELECT count(*) FROM agents WHERE package_agent_type = $1 AND package_version = $2",
            &[&id.agent_type, &id.version],
        )
        .await
        .map_err(sql)?
        .get(0);
    if pinning > 0 {
        return Err(refusal(id, usize::try_from(pinning).unwrap_or(usize::MAX)));
    }
    Ok(true)
}

/// Every Package with its entries. A row that does not decode — an identity or platform a write
/// would refuse, a hash that is no blob's name, a signature of the wrong length — fails, naming it.
pub(crate) async fn read_all(client: &impl GenericClient) -> Result<Vec<Package>, String> {
    let mut packages: BTreeMap<PackageId, Package> = BTreeMap::new();
    for row in client
        .query("SELECT agent_type, version FROM packages", &[])
        .await
        .map_err(sql)?
    {
        let (agent_type, version): (String, String) = (row.get(0), row.get(1));
        let id = PackageId::load(&agent_type, &version)
            .map_err(|e| format!("package {agent_type}@{version} in the database: {e}"))?;
        packages.insert(
            id.clone(),
            Package {
                id,
                entries: BTreeMap::new(),
            },
        );
    }
    for row in client
        .query(&format!("SELECT {ENTRY} FROM package_entries"), &[])
        .await
        .map_err(sql)?
    {
        let (id, entry) = entry(&row)?;
        packages
            .get_mut(&id)
            .ok_or_else(|| {
                format!("package {id} in the database: an entry of a Package that is not there")
            })?
            .entries
            .insert(entry.platform.clone(), entry);
    }
    Ok(packages.into_values().collect())
}

/// The columns of an entry, in the order [`entry`] reads them.
const ENTRY: &str = "agent_type, version, os, arch, sha256, size, signature";

/// One row of `package_entries`, selected as [`ENTRY`] names its columns. A row that does not
/// decode — an identity or platform a write would refuse, a hash that is no blob's name, a
/// signature of the wrong length — fails, naming its Package.
fn entry(row: &tokio_postgres::Row) -> Result<(PackageId, Entry), String> {
    let (agent_type, version): (String, String) = (row.get(0), row.get(1));
    let named = |e: String| format!("package {agent_type}@{version} in the database: {e}");
    let id = PackageId::load(&agent_type, &version).map_err(named)?;
    let platform = Platform::new(row.get(2), row.get(3))
        .map_err(|e| named(format!("invalid platform: {e}")))?;
    let sha256: String = row.get(4);
    let content_hash = hex::decode(&sha256)
        .ok()
        .filter(|_| crate::blobs::is_hash(&sha256))
        .ok_or_else(|| named(format!("invalid sha256 {sha256:?}")))?;
    let signature: Option<Vec<u8>> = row.get(6);
    if let Some(signature) = &signature {
        if signature.len() != ED25519_SIGNATURE_LEN {
            return Err(named(format!(
                "invalid signature: {} bytes, not {ED25519_SIGNATURE_LEN}",
                signature.len()
            )));
        }
    }
    Ok((
        id,
        Entry {
            platform,
            content_hash,
            size: unsigned(row.get(5)),
            signature,
        },
    ))
}

/// Creates or replaces one Package, its entries exactly `package`'s, inside the caller's
/// transaction — the write of an import into a database no instance serves.
pub(crate) async fn write(client: &impl GenericClient, package: &Package) -> Result<(), String> {
    let id = &package.id;
    client
        .execute(
            "INSERT INTO packages (agent_type, version) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            &[&id.agent_type, &id.version],
        )
        .await
        .map_err(sql)?;
    client
        .execute(
            "DELETE FROM package_entries WHERE agent_type = $1 AND version = $2",
            &[&id.agent_type, &id.version],
        )
        .await
        .map_err(sql)?;
    for entry in package.entries.values() {
        client
            .execute(
                "INSERT INTO package_entries (agent_type, version, os, arch, sha256, size, \
                 signature) VALUES ($1, $2, $3, $4, $5, $6, $7)",
                &[
                    &id.agent_type,
                    &id.version,
                    &entry.platform.os,
                    &entry.platform.arch,
                    &hex::encode(&entry.content_hash),
                    &signed(entry.size),
                    &entry.signature,
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
    use crate::fs::FsPackageBackend;
    use crate::postgres::testing::TestDatabase;

    /// The blob of every entry found stored: the port's tests write entries whose bytes are not
    /// the point.
    fn present() -> Stored {
        Arc::new(|| Ok(true))
    }

    fn package(version: &str, platforms: &[(&str, &str, u8)]) -> Package {
        let id = PackageId::new("otelcol", version).expect("id");
        Package {
            id,
            entries: platforms
                .iter()
                .map(|(os, arch, byte)| {
                    let platform = Platform::new(os, arch).expect("platform");
                    (
                        platform.clone(),
                        Entry {
                            platform,
                            content_hash: vec![*byte; 32],
                            size: u64::from(*byte) << 40,
                            signature: (*byte % 2 == 0).then(|| vec![*byte; ED25519_SIGNATURE_LEN]),
                        },
                    )
                })
                .collect(),
        }
    }

    /// One entry as plain values: os, arch, hash, size, signature.
    type Shown = (String, String, Vec<u8>, u64, Option<Vec<u8>>);

    /// One entry as plain values, to compare.
    fn shown_entry(entry: &Entry) -> Shown {
        (
            entry.platform.os.clone(),
            entry.platform.arch.clone(),
            entry.content_hash.clone(),
            entry.size,
            entry.signature.clone(),
        )
    }

    /// A Package as plain values, to compare.
    fn shown(package: &Package) -> (String, Vec<Shown>) {
        (
            package.id.to_string(),
            package
                .entries
                .values()
                .map(|entry| {
                    (
                        entry.platform.os.clone(),
                        entry.platform.arch.clone(),
                        entry.content_hash.clone(),
                        entry.size,
                        entry.signature.clone(),
                    )
                })
                .collect(),
        )
    }

    fn loaded(store: &dyn PackageBackend) -> Vec<(String, Vec<Shown>)> {
        let mut packages: Vec<_> = store.load().expect("load").iter().map(shown).collect();
        packages.sort();
        packages
    }

    /// Writes `package` through the port, one entry at a time.
    fn put(store: &dyn PackageBackend, package: &Package) {
        store.create(&package.id).expect("create");
        for entry in package.entries.values() {
            store
                .put_entry(&package.id, entry, present())
                .expect("put entry");
        }
    }

    /// The port's contract on either store: what is written is loaded back; an entry is written,
    /// signed and removed by its platform and no other entry is touched; a signature is written
    /// only onto the bytes it names; and a removal tolerates absence.
    fn contract(open: &dyn Fn() -> Box<dyn PackageBackend>) {
        let store = open();
        assert!(store.load().expect("empty").is_empty());
        let one = package("1.2.3", &[("linux", "amd64", 1), ("windows", "amd64", 2)]);
        let two = package("2.0.0", &[("darwin", "arm64", 4)]);
        put(&*store, &one);
        put(&*store, &two);
        store
            .create(&two.id)
            .expect("creating what is stored keeps it");
        assert_eq!(loaded(&*open()), [shown(&one), shown(&two)]);
        store
            .put_entry(
                &PackageId::new("otelcol", "9.9.9").expect("id"),
                &one.entries.values().next().expect("an entry").clone(),
                present(),
            )
            .expect_err("an entry of a Package the store does not hold");

        let replaced = package("1.2.3", &[("linux", "amd64", 3), ("windows", "amd64", 2)]);
        let linux = Platform::new("linux", "amd64").expect("platform");
        store
            .put_entry(&one.id, &replaced.entries[&linux], present())
            .expect("replace one entry");
        assert_eq!(loaded(&*open()), [shown(&replaced), shown(&two)]);

        let windows = Platform::new("windows", "amd64").expect("platform");
        match store
            .sign(
                &one.id,
                &windows,
                &[9; 32],
                Some(&[7; ED25519_SIGNATURE_LEN]),
            )
            .expect("sign")
        {
            Signed::Changed(Some(stored)) => assert_eq!(
                shown_entry(&stored),
                shown_entry(&replaced.entries[&windows]),
                "the entry as stored is answered"
            ),
            _ => panic!("a signature is not written onto other bytes than it names"),
        }
        assert!(matches!(
            store
                .sign(
                    &one.id,
                    &windows,
                    &[2; 32],
                    Some(&[7; ED25519_SIGNATURE_LEN])
                )
                .expect("sign"),
            Signed::Written
        ));
        let mut signed = replaced.clone();
        signed.entries.get_mut(&windows).expect("windows").signature =
            Some(vec![7; ED25519_SIGNATURE_LEN]);
        assert_eq!(loaded(&*open()), [shown(&signed), shown(&two)]);
        assert!(matches!(
            store
                .sign(&one.id, &windows, &[2; 32], None)
                .expect("clear"),
            Signed::Written
        ));
        signed.entries.get_mut(&windows).expect("windows").signature = None;
        assert_eq!(loaded(&*open()), [shown(&signed), shown(&two)]);
        assert!(matches!(
            store
                .sign(&two.id, &windows, &[2; 32], None)
                .expect("no such entry"),
            Signed::Changed(None)
        ));

        store.remove_entry(&one.id, &linux).expect("remove entry");
        store
            .remove_entry(&one.id, &linux)
            .expect("removing the absent is fine");
        signed.entries.remove(&linux);
        assert_eq!(loaded(&*open()), [shown(&signed), shown(&two)]);

        store.remove(&two.id).expect("remove");
        store.remove(&two.id).expect("removing the absent is fine");
        assert_eq!(loaded(&*open()), [shown(&signed)]);
    }

    /// Verifies: G-19, ADR-0044
    #[test]
    fn the_package_port_contract_holds_on_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        contract(&|| Box::new(FsPackageBackend::open(dir.path()).expect("open")));
    }

    /// Verifies: G-19, ADR-0044
    #[test]
    fn the_package_port_contract_holds_on_postgres() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        contract(&|| Box::new(PgPackageBackend::new(database.clone())));
    }

    /// An entry write that fails leaves the Package as it was, on either store: one transaction
    /// per call, one file replaced in one step. `refuse` makes the write of every entry of the
    /// Windows platform fail; `mend` takes that away.
    fn fails_part_way(store: &dyn PackageBackend, refuse: &dyn Fn(), mend: &dyn Fn()) {
        let before = package("1.2.3", &[("linux", "amd64", 1), ("windows", "amd64", 2)]);
        put(store, &before);
        refuse();
        let windows = Platform::new("windows", "amd64").expect("platform");
        let broken = package("1.2.3", &[("windows", "amd64", 3)]);
        store
            .put_entry(&before.id, &broken.entries[&windows], present())
            .expect_err("the entry is refused");
        store
            .sign(
                &before.id,
                &windows,
                &[2; 32],
                Some(&[5; ED25519_SIGNATURE_LEN]),
            )
            .expect_err("the signature is refused");
        mend();
        assert_eq!(loaded(store), [shown(&before)]);
    }

    /// Verifies: ADR-0044
    #[test]
    fn a_package_put_is_whole_or_nothing_on_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("packages/otelcol@1.2.3.json");
        fails_part_way(
            &FsPackageBackend::open(dir.path()).expect("open"),
            &|| crate::fs::testing::refuse(&file),
            &|| crate::fs::testing::mend(&file),
        );
    }

    /// One transaction per call, and a row that does not decode fails the load naming the
    /// Package.
    /// Verifies: ADR-0044
    #[test]
    fn a_package_put_is_whole_or_nothing_and_a_bad_row_is_named_on_postgres() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let store = PgPackageBackend::new(test.open());
        fails_part_way(
            &store,
            &|| test.refuse("package_entries", "NEW.os = 'windows'"),
            &|| test.mend("package_entries"),
        );

        test.sql("UPDATE package_entries SET sha256 = 'not-a-hash'");
        let err = store.load().map(|_| ()).expect_err("not a hash");
        assert!(err.contains("otelcol@1.2.3"), "{err}");
    }

    /// A deletion of a Package through B while a rollout through A that pins it has not yet
    /// committed waits for the rollout, and then refuses it from the assignment the rollout
    /// stored: the rollout holds the Package's row until it commits.
    /// Verifies: ADR-0041, ADR-0026
    #[test]
    fn a_deletion_waits_for_a_rollout_that_pins_the_package_and_is_refused() {
        use crate::agent_store::{tests::record, AgentStore as _};
        use crate::postgres::agents::PgAgentStore;
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (agents, b) = (
            PgAgentStore::new(database.clone()),
            PgPackageBackend::new(database),
        );
        let id = PackageId::new("otelcol", "1.2.3").expect("id");
        b.create(&id).expect("create");
        let uid = opamp::uid::InstanceUid::default();
        let mut reported = record();
        reported.assignment = None;
        agents.put(&uid, &reported).expect("report");
        test.store_deployments(&["stable"]);
        let key = 0x19_0001;
        let gate = test.hold_assignment_writes(key);
        let mut released = record();
        if let Some(assignment) = released.assignment.as_mut() {
            assignment.configurations.clear();
        }
        let rollout = std::thread::spawn(move || agents.release(&[], &[(uid, released)]));
        // A has written the assignment, and waits at the gate with its transaction open.
        let a_pid = test.once(&format!(
            "SELECT pid::text FROM pg_locks WHERE locktype = 'advisory' AND NOT granted \
             AND objid::text = '{key}'"
        ));
        let deletion = std::thread::spawn(move || b.remove(&id));
        test.once(&format!(
            "SELECT pid::text FROM pg_stat_activity WHERE {a_pid} = ANY(pg_blocking_pids(pid))"
        ));
        gate.open();
        assert_eq!(rollout.join().expect("A").map(|wrote| wrote.len()), Ok(1));
        let refused = deletion.join().expect("B").expect_err("refused");
        assert!(
            refused.ends_with(&pinned(&PackageId::new("otelcol", "1.2.3").expect("id"), 1)),
            "{refused}"
        );
        assert_eq!(
            test.rows("SELECT version FROM packages"),
            [[Some("1.2.3".to_string())]]
        );
    }

    /// A blob store on a directory that holds the first call of `at` (`"put"` once the bytes are
    /// stored, `"exists"` before it answers) until the test lets it go: where a test holds an
    /// upload between its steps. The directory stands in for the bucket every instance shares.
    struct Pausing {
        inner: crate::fs::FsBlobStore,
        at: &'static str,
        reached: std::sync::Mutex<Option<std::sync::mpsc::Sender<()>>>,
        go: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        /// How many times the staging objects were cleaned up.
        cleaned: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Pausing {
        /// The store, the receiver told once `at` is reached, and the sender that lets it go.
        fn at(
            dir: &std::path::Path,
            at: &'static str,
        ) -> (
            Self,
            std::sync::mpsc::Receiver<()>,
            std::sync::mpsc::Sender<()>,
        ) {
            let (reached, reaches) = std::sync::mpsc::channel();
            let (go, goes) = std::sync::mpsc::channel();
            (
                Pausing {
                    inner: crate::fs::FsBlobStore::open(dir).expect("blobs"),
                    at,
                    reached: std::sync::Mutex::new(Some(reached)),
                    go: std::sync::Mutex::new(goes),
                    cleaned: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                },
                reaches,
                go,
            )
        }

        fn pause(&self, operation: &str) {
            if operation != self.at {
                return;
            }
            if let Some(reached) = self.reached.lock().expect("reached").take() {
                reached.send(()).expect("reached");
                self.go.lock().expect("go").recv().expect("go");
            }
        }
    }

    impl crate::blobs::BlobStore for Pausing {
        fn put(&self, bytes: &mut dyn std::io::Read) -> Result<(String, u64), String> {
            let put = self.inner.put(bytes);
            self.pause("put");
            put
        }
        fn get(&self, sha256: &str) -> Result<Box<dyn std::io::Read + Send>, String> {
            self.inner.get(sha256)
        }
        fn exists(&self, sha256: &str) -> Result<bool, String> {
            self.pause("exists");
            self.inner.exists(sha256)
        }
        fn delete(&self, sha256: &str) -> Result<(), String> {
            self.pause("delete");
            self.inner.delete(sha256)
        }
        fn list(&self) -> Result<Vec<(String, u64)>, String> {
            self.inner.list()
        }
        fn clean_staging(&self) -> Result<usize, String> {
            self.cleaned
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.clean_staging()
        }
    }

    /// An instance's database on the test's schema, shared with the others, holding a live lease
    /// under which its sweeps mark: what each instance of these tests is.
    fn shared(test: &TestDatabase) -> Arc<Database> {
        let database = Database::open_in(
            &test.config(),
            crate::postgres::Exclusive::Shared,
            Some(&test.schema),
        )
        .expect("open");
        let id = crate::postgres::lease::new_instance_id().expect("an instance id");
        let lapse = std::time::Duration::from_secs(600);
        database
            .take_lease_within(id, "a host", lapse, lapse)
            .expect("a lease");
        database
    }

    /// Ends the session that holds the blob store's lock alone, and waits until `database`, whose
    /// hold it is, has marked the hold lost.
    fn end_the_sweeps_session(test: &TestDatabase, database: &Database) {
        let key = format!("(SELECT {} AS key)", crate::postgres::BLOB_LOCK);
        test.rows(&format!(
            "SELECT pg_terminate_backend(pid) FROM pg_locks, {key} k \
             WHERE locktype = 'advisory' AND granted AND mode = 'ExclusiveLock' \
             AND classid = ((k.key >> 32) & 4294967295)::oid \
             AND objid = (k.key & 4294967295)::oid"
        ));
        until("the sweep's hold is marked lost", &|| {
            database.holds_lost().0 >= 1
        });
    }

    /// A's sweep marked the blob and is about to delete it when its session ends, its lock with
    /// it. An upload of the same bytes through B is refused, to be uploaded again, while the mark
    /// stands; once A's sweep is done and its mark gone, the upload again commits, and the blob
    /// its entry names is stored.
    /// Verifies: ADR-0044
    #[test]
    fn an_upload_beside_a_sweep_whose_session_ended_keeps_its_artifact() {
        let _beside = LOSING_SWEEPS
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let (database_a, database_b) = (shared(&test), shared(&test));
        let dir = tempfile::tempdir().expect("tempdir");
        let linux = Platform::new("linux", "amd64").expect("platform");
        let (first, second) = (
            PackageId::new("otelcol", "1.0.0").expect("id"),
            PackageId::new("otelcol", "2.0.0").expect("id"),
        );
        let (pausing, reaches, go) = Pausing::at(dir.path(), "delete");
        let a = Arc::new(
            crate::packages::PackageStore::with_backend(
                Box::new(PgPackageBackend::new(database_a.clone())),
                Box::new(pausing),
                crate::clock::Elapsed::monotonic(),
            )
            .expect("open A"),
        );
        let b = instance(&database_b, dir.path());
        a.create(&first).expect("create");
        a.put_entry(&first, &linux, b"the same bytes".to_vec())
            .expect("an entry");
        let (sha256, _) = a.artifact(&first, &linux).expect("A holds it");
        let blob = dir.path().join("blobs").join(hex::encode(&sha256));
        b.create(&second).expect("create");
        let deleting = std::thread::spawn({
            let (a, first, linux) = (a.clone(), first.clone(), linux.clone());
            move || a.delete_entry(&first, &linux)
        });
        reaches
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("A's sweep deletes the blob it marked");
        end_the_sweeps_session(&test, &database_a);
        let upload = b.put_entry(&second, &linux, b"the same bytes".to_vec());
        go.send(()).expect("go on");
        assert_eq!(deleting.join().expect("A"), Ok(true));
        match upload {
            Ok(()) => {}
            Err(refused) => {
                assert!(refused.contains("upload it again"), "{refused}");
                b.put_entry(&second, &linux, b"the same bytes".to_vec())
                    .expect("uploaded again once the sweep is done");
            }
        }
        assert_eq!(
            test.rows("SELECT sha256 FROM package_entries"),
            [[Some(hex::encode(&sha256))]],
            "B's entry is stored"
        );
        assert!(blob.exists(), "and so is the blob it names");
        assert!(
            test.rows("SELECT 1 FROM blob_deletions").is_empty(),
            "no mark is left"
        );
    }

    /// A sweep whose hold is lost by the time it reaches a deletion deletes nothing more and says
    /// so in the log: the one blob it was deleting goes, the other stays, and no mark is left.
    /// Verifies: ADR-0044
    #[test]
    fn a_sweep_whose_hold_is_lost_deletes_nothing_more_and_says_so() {
        let _alone = LOSING_SWEEPS
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = shared(&test);
        let dir = tempfile::tempdir().expect("tempdir");
        let id = PackageId::new("otelcol", "1.0.0").expect("id");
        let (linux, windows) = (
            Platform::new("linux", "amd64").expect("platform"),
            Platform::new("windows", "amd64").expect("platform"),
        );
        let (pausing, reaches, go) = Pausing::at(dir.path(), "delete");
        let a = Arc::new(
            crate::packages::PackageStore::with_backend(
                Box::new(PgPackageBackend::new(database.clone())),
                Box::new(pausing),
                crate::clock::Elapsed::monotonic(),
            )
            .expect("open"),
        );
        a.create(&id).expect("create");
        a.put_entry(&id, &linux, b"one artifact".to_vec())
            .expect("an entry");
        a.put_entry(&id, &windows, b"another artifact".to_vec())
            .expect("an entry");
        let blobs: Vec<std::path::PathBuf> = [&linux, &windows]
            .iter()
            .map(|platform| {
                let (sha256, _) = a.artifact(&id, platform).expect("held");
                dir.path().join("blobs").join(hex::encode(sha256))
            })
            .collect();
        let collected = Collected::default();
        let deleting = std::thread::spawn({
            let (a, id, writer) = (a.clone(), id.clone(), collected.clone());
            move || {
                let subscriber = tracing_subscriber::fmt()
                    .with_ansi(false)
                    .without_time()
                    .with_writer(move || writer.clone())
                    .finish();
                tracing::subscriber::with_default(subscriber, || a.delete_set(&id))
            }
        });
        reaches
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the sweep deletes its first blob");
        end_the_sweeps_session(&test, &database);
        go.send(()).expect("go on");
        assert_eq!(deleting.join().expect("the sweep"), Ok(true));
        assert_eq!(
            blobs.iter().filter(|blob| blob.exists()).count(),
            1,
            "the blob it was deleting goes, the other stays"
        );
        let log = String::from_utf8(collected.0.lock().expect("log").clone()).expect("utf-8");
        assert!(log.contains("deletes nothing more"), "{log}");
        assert!(
            test.rows("SELECT 1 FROM blob_deletions").is_empty(),
            "no mark is left"
        );
    }

    /// A sweep whose session has ended marks nothing, and so deletes nothing: its marks are
    /// written on the session that holds the lock, never through the pool, where they would land
    /// with the lock gone and an upload between its check and its commit.
    /// Verifies: ADR-0044
    #[test]
    fn a_sweep_marks_nothing_once_its_session_has_ended() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = shared(&test);
        let backend = PgPackageBackend::new(database.clone());
        let hold = backend.hold_blobs().expect("held");
        end_the_sweeps_session(&test, &database);
        let unnamed = "ab".repeat(32);
        assert!(
            backend.mark(&hold, &[unnamed]).is_err(),
            "nothing is marked on a session that has ended"
        );
        assert!(
            test.rows("SELECT 1 FROM blob_deletions").is_empty(),
            "no mark is written"
        );
    }

    /// Every sweep takes away the marks of a lease that is not live, or of none, once they were
    /// last renewed longer ago than a sweep that lost its lock may still be deleting; it keeps
    /// younger ones, which such a sweep may still be deleting by, and those of a live lease. It
    /// marks under its own lease, and takes its own marks away once it is done.
    /// Verifies: ADR-0044
    #[test]
    fn a_sweep_takes_away_old_marks_of_a_lease_that_is_not_live_and_keeps_the_others() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let (other, database) = (shared(&test), shared(&test));
        let live = other.sweeps_under().expect("a lease");
        let gone_lease = "00000000-0000-4000-8000-000000000000";
        let old = "now() - interval '131 seconds'";
        let young = "now() - interval '129 seconds'";
        let [kept, gone, of_none, young_gone, young_none] =
            ["aa", "bb", "cc", "ee", "ff"].map(|byte| byte.repeat(32));
        test.sql(&format!(
            "INSERT INTO blob_deletions (sha256, lease, sweep, marked_at) VALUES \
             ('{kept}', '{live}', 'another sweep', {old}), \
             ('{gone}', '{gone_lease}', 'a sweep that stopped short', {old}), \
             ('{of_none}', NULL, 'a sweep of a command', {old}), \
             ('{young_gone}', '{gone_lease}', 'a sweep that may still delete', {young}), \
             ('{young_none}', NULL, 'a startup sweep that may still delete', {young})"
        ));
        let backend = PgPackageBackend::new(database.clone());
        let hold = backend.hold_blobs().expect("held");
        let marked = backend
            .mark(&hold, &[kept.clone(), "dd".repeat(32)])
            .expect("marked");
        assert_eq!(
            marked,
            ["dd".repeat(32)],
            "a hash another sweep marked is its"
        );
        assert_eq!(
            test.rows("SELECT sha256, lease FROM blob_deletions ORDER BY sha256"),
            [
                [Some(kept.clone()), Some(live)],
                [Some("dd".repeat(32)), database.sweeps_under()],
                [Some(young_gone.clone()), Some(gone_lease.to_string())],
                [Some(young_none.clone()), None],
            ],
            "the old marks of a lease that is not live, or of none, are gone"
        );
        backend
            .unmark(hold.sweep().expect("a sweep"))
            .expect("unmarked");
        assert_eq!(
            test.rows("SELECT sha256 FROM blob_deletions ORDER BY sha256"),
            [[Some(kept)], [Some(young_gone)], [Some(young_none)]],
            "its own marks go once it is done, another sweep's stay"
        );
    }

    /// Before each deletion a sweep renews, on its own session, the mark of the blob it is about
    /// to delete, and finds whether it is still there; the marks of the blobs after it are left
    /// as they are, so a sweep of many blobs writes each mark once.
    /// Verifies: ADR-0044
    #[test]
    fn a_sweep_renews_a_marks_time_and_finds_a_blob_another_sweep_unmarked() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = shared(&test);
        let backend = PgPackageBackend::new(database.clone());
        let hold = backend.hold_blobs().expect("held");
        let [first, second, third] = ["aa", "bb", "cc"].map(|byte| byte.repeat(32));
        backend
            .mark(&hold, &[first.clone(), second.clone(), third.clone()])
            .expect("marked");
        test.sql("UPDATE blob_deletions SET marked_at = now() - interval '1 hour'");
        test.sql(&format!(
            "DELETE FROM blob_deletions WHERE sha256 = '{second}'"
        ));
        assert_eq!(backend.confirm(&hold, &first), Ok(true));
        assert_eq!(
            backend.confirm(&hold, &second),
            Ok(false),
            "its mark is gone"
        );
        assert_eq!(
            test.rows(
                "SELECT sha256 FROM blob_deletions WHERE marked_at > now() - interval '1 minute'"
            ),
            [[Some(first.clone())]],
            "only the mark of the blob about to be deleted is renewed"
        );
        assert_eq!(
            test.rows("SELECT sha256 FROM blob_deletions ORDER BY sha256"),
            [[Some(first)], [Some(third)]],
            "the marks after it are left as they are"
        );
    }

    /// A mark of a lease that is not live outlives the renewal that answered, which the database
    /// may have stamped up to one call's bound before, and the one call to the blob store that
    /// follows it, with a margin.
    /// Verifies: ADR-0044
    #[test]
    fn the_marks_outlive_a_renewal_its_call_and_the_longest_blob_call() {
        assert!(
            crate::postgres::MARKS_OUTLIVE
                >= crate::postgres::CALL_TIMEOUT + crate::s3::LONGEST_CALL,
            "a mark outlives what a sweep that renewed it may still be doing"
        );
        assert_eq!(
            crate::postgres::MARKS_OUTLIVE,
            std::time::Duration::from_secs(130),
            "the bound the ADR names"
        );
        assert_eq!(
            crate::packages::DELETED_WITHIN,
            crate::postgres::CALL_TIMEOUT,
            "a sweep calls the blob store no later than the bound of the renewal before it"
        );
    }

    /// A's startup sweep, on `a`, marks a blob no entry names, is held inside its deletion, and
    /// its session ends; B, live, sweeps, and then uploads the same bytes. B's sweep keeps A's
    /// young mark, so B's upload is refused while A may still delete, and once A is done and its
    /// mark gone, the upload again commits and the blob its entry names is stored.
    fn a_mark_outlives_its_lost_session(test: &TestDatabase, a: &Arc<Database>) {
        let b = shared(test);
        let dir = tempfile::tempdir().expect("tempdir");
        let linux = Platform::new("linux", "amd64").expect("platform");
        let b_store = instance(&b, dir.path());
        let blob = {
            let blobs = crate::fs::FsBlobStore::open(dir.path()).expect("blobs");
            let (sha256, _) = crate::blobs::BlobStore::put(&blobs, &mut &b"the same bytes"[..])
                .expect("an unnamed blob");
            dir.path().join("blobs").join(sha256)
        };
        let (pausing, reaches, go) = Pausing::at(dir.path(), "delete");
        let starting = std::thread::spawn({
            let a = a.clone();
            move || {
                crate::packages::PackageStore::with_backend(
                    Box::new(PgPackageBackend::new(a)),
                    Box::new(pausing),
                    crate::clock::Elapsed::monotonic(),
                )
                .map(drop)
            }
        });
        reaches
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("A's startup sweep deletes the blob it marked");
        end_the_sweeps_session(test, a);
        let other = PackageId::new("other", "1.0.0").expect("id");
        b_store.create(&other).expect("create");
        assert_eq!(b_store.delete_set(&other), Ok(true), "B sweeps");
        let id = PackageId::new("otelcol", "1.0.0").expect("id");
        b_store.create(&id).expect("create");
        let upload = b_store.put_entry(&id, &linux, b"the same bytes".to_vec());
        go.send(()).expect("go on");
        starting.join().expect("A").expect("A starts");
        if let Err(refused) = upload {
            assert!(refused.contains("upload it again"), "{refused}");
            b_store
                .put_entry(&id, &linux, b"the same bytes".to_vec())
                .expect("uploaded again once A's sweep is done");
        }
        assert!(blob.exists(), "the blob B's entry names is stored");
    }

    /// A startup sweep marks under the lease its instance is about to take, which is not live yet:
    /// another instance's sweep keeps that mark while the sweep may still be deleting.
    /// Verifies: ADR-0044
    #[test]
    fn a_startup_sweeps_marks_outlive_its_lost_session_and_keep_an_uploads_artifact() {
        let _beside = LOSING_SWEEPS
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let a = Database::open_in(
            &test.config(),
            crate::postgres::Exclusive::Shared,
            Some(&test.schema),
        )
        .expect("open");
        a.sweep_under(&crate::postgres::lease::new_instance_id().expect("an instance id"));
        a_mark_outlives_its_lost_session(&test, &a);
    }

    /// A sweep whose lease lapsed meanwhile: another instance's sweep keeps its young mark.
    /// Verifies: ADR-0044
    #[test]
    fn a_lapsed_leases_marks_outlive_its_lost_session_and_keep_an_uploads_artifact() {
        let _beside = LOSING_SWEEPS
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let a = shared(&test);
        let lease = a.sweeps_under().expect("a lease");
        test.sql(&format!(
            "UPDATE leases SET expires_at = now() - interval '1 second' \
             WHERE instance_id::text = '{lease}'"
        ));
        a_mark_outlives_its_lost_session(&test, &a);
    }

    /// A sweep whose hold is lost by the time its deletions are done cleans up no staging object:
    /// the cleanup runs under the lock too.
    /// Verifies: ADR-0044
    #[test]
    fn a_sweep_whose_hold_is_lost_cleans_up_no_staging_object() {
        let _beside = LOSING_SWEEPS
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = shared(&test);
        let dir = tempfile::tempdir().expect("tempdir");
        let id = PackageId::new("otelcol", "1.0.0").expect("id");
        let linux = Platform::new("linux", "amd64").expect("platform");
        let (pausing, reaches, go) = Pausing::at(dir.path(), "delete");
        let cleaned = pausing.cleaned.clone();
        let a = Arc::new(
            crate::packages::PackageStore::with_backend(
                Box::new(PgPackageBackend::new(database.clone())),
                Box::new(pausing),
                crate::clock::Elapsed::monotonic(),
            )
            .expect("open"),
        );
        let opened = cleaned.load(std::sync::atomic::Ordering::SeqCst);
        a.create(&id).expect("create");
        a.put_entry(&id, &linux, b"an artifact".to_vec())
            .expect("an entry");
        let deleting = std::thread::spawn({
            let (a, id) = (a.clone(), id.clone());
            move || a.delete_entry(&id, &linux)
        });
        reaches
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the sweep deletes its one blob");
        end_the_sweeps_session(&test, &database);
        go.send(()).expect("go on");
        assert_eq!(deleting.join().expect("the sweep"), Ok(true));
        assert_eq!(
            cleaned.load(std::sync::atomic::Ordering::SeqCst),
            opened,
            "no staging object is cleaned up once the lock is lost"
        );
    }

    /// Held for writing by a test that reads what a sweep that lost its lock logs, and for reading
    /// by every other test whose sweep logs it: a warning first met on a thread with no
    /// subscriber of its own is cached as of no interest, and the reading test would miss it.
    static LOSING_SWEEPS: std::sync::RwLock<()> = std::sync::RwLock::new(());

    /// What a log writes, collected.
    #[derive(Clone, Default)]
    struct Collected(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Collected {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("log").extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// The package store of one instance on `database`, its blobs in `dir`.
    fn instance(database: &Arc<Database>, dir: &std::path::Path) -> crate::packages::PackageStore {
        crate::packages::PackageStore::with_backend(
            Box::new(PgPackageBackend::new(database.clone())),
            Box::new(crate::fs::FsBlobStore::open(dir).expect("blobs")),
            crate::clock::Elapsed::monotonic(),
        )
        .expect("open")
    }

    /// Waits until `done` holds; a test fails after 30 seconds without.
    fn until(what: &str, done: &dyn Fn() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "never: {what}");
            std::thread::yield_now();
        }
    }

    /// B deletes the one entry it holds of a blob; an entry written through A, which B has not
    /// read, names the same blob, and B's sweep keeps it: it reads the stored entries under the
    /// blob store's lock, not what B holds.
    /// Verifies: ADR-0044, ADR-0041
    #[test]
    fn a_sweep_keeps_a_blob_an_entry_stored_through_another_writer_names() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let dir = tempfile::tempdir().expect("tempdir");
        let linux = Platform::new("linux", "amd64").expect("platform");
        let (first, second) = (
            PackageId::new("otelcol", "1.0.0").expect("id"),
            PackageId::new("otelcol", "2.0.0").expect("id"),
        );
        let a = instance(&database, dir.path());
        a.create(&first).expect("create");
        a.put_entry(&first, &linux, b"the same bytes".to_vec())
            .expect("an entry");
        let b = instance(&database, dir.path());
        a.create(&second).expect("create");
        a.put_entry(&second, &linux, b"the same bytes".to_vec())
            .expect("an entry of the same bytes, which B has not read");
        let (sha256, _) = b.artifact(&first, &linux).expect("B holds the first");
        assert!(
            b.artifact(&second, &linux).is_none(),
            "B has not read the second"
        );
        assert_eq!(b.delete_entry(&first, &linux), Ok(true));
        assert!(
            dir.path().join("blobs").join(hex::encode(&sha256)).exists(),
            "the blob the second entry names is kept"
        );
        a.reload().expect("A reads what B deleted");
        assert_eq!(a.delete_entry(&second, &linux), Ok(true));
        assert!(
            !dir.path().join("blobs").join(hex::encode(&sha256)).exists(),
            "and goes with the last entry that names it"
        );
    }

    /// An upload holds the blob store's lock shared from its check that its blob is stored until
    /// its entry is committed: a sweep that starts meanwhile, at another instance's start, waits
    /// for it, then reads the entry and keeps the blob.
    /// Verifies: ADR-0044
    #[test]
    fn an_upload_holds_off_a_sweep_until_its_entry_is_committed() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let dir = tempfile::tempdir().expect("tempdir");
        let id = PackageId::new("otelcol", "1.0.0").expect("id");
        let linux = Platform::new("linux", "amd64").expect("platform");
        let (pausing, reaches, go) = Pausing::at(dir.path(), "exists");
        let a = Arc::new(
            crate::packages::PackageStore::with_backend(
                Box::new(PgPackageBackend::new(database.clone())),
                Box::new(pausing),
                crate::clock::Elapsed::monotonic(),
            )
            .expect("open A"),
        );
        a.create(&id).expect("create");
        let uploading = std::thread::spawn({
            let (a, id, linux) = (a.clone(), id.clone(), linux.clone());
            move || a.put_entry(&id, &linux, b"an artifact".to_vec())
        });
        reaches
            .recv()
            .expect("A checks its blob, holding the lock shared");
        let starting = std::thread::spawn({
            let (database, dir) = (database.clone(), dir.path().to_path_buf());
            move || instance(&database, &dir).artifact(&id, &linux)
        });
        until("B's sweep waits for the lock, or ends", &|| {
            test.waits_for_the_blob_lock() || starting.is_finished()
        });
        go.send(()).expect("go on");
        assert_eq!(uploading.join().expect("A"), Ok(()));
        let found = starting.join().expect("B").expect("B opens with the entry");
        assert!(
            dir.path()
                .join("blobs")
                .join(hex::encode(&found.0))
                .exists(),
            "the blob A's entry names is kept"
        );
    }

    /// An upload whose blob a sweep deleted before the upload checked for it is refused, to be
    /// uploaded again, and writes no entry.
    /// Verifies: ADR-0044
    #[test]
    fn an_upload_whose_blob_a_sweep_took_first_is_refused() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let dir = tempfile::tempdir().expect("tempdir");
        let id = PackageId::new("otelcol", "1.0.0").expect("id");
        let linux = Platform::new("linux", "amd64").expect("platform");
        let (pausing, reaches, go) = Pausing::at(dir.path(), "put");
        let a = Arc::new(
            crate::packages::PackageStore::with_backend(
                Box::new(PgPackageBackend::new(database.clone())),
                Box::new(pausing),
                crate::clock::Elapsed::monotonic(),
            )
            .expect("open A"),
        );
        a.create(&id).expect("create");
        let uploading = std::thread::spawn({
            let (a, id, linux) = (a.clone(), id.clone(), linux.clone());
            move || a.put_entry(&id, &linux, b"an artifact".to_vec())
        });
        reaches.recv().expect("A's bytes are stored");
        // Another instance starts, and its sweep deletes the blob no entry names yet.
        drop(instance(&database, dir.path()));
        go.send(()).expect("go on");
        let refused = uploading.join().expect("A").expect_err("refused");
        assert!(refused.contains("upload it again"), "{refused}");
        assert!(
            test.rows("SELECT 1 FROM package_entries").is_empty(),
            "no entry is written"
        );
    }

    /// A sweep waits the whole of its wait for the blob store's lock, 60 seconds, which the
    /// pool's `statement_timeout` of 10 seconds would otherwise cut short; an upload waits the
    /// pool's `lock_timeout` of 5 seconds.
    /// Verifies: ADR-0044
    #[test]
    fn a_sweep_waits_its_whole_wait_for_the_lock_and_an_upload_five_seconds() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let dir = tempfile::tempdir().expect("tempdir");
        let store = instance(&database, dir.path());
        let id = PackageId::new("otelcol", "1.0.0").expect("id");
        store.create(&id).expect("create");
        let hold = database.hold_blobs().expect("a sweep holds the lock");
        assert_eq!(
            hold.settings,
            Some(("65s".to_string(), "1min".to_string())),
            "a sweep's session waits 60 s: its statement_timeout and lock_timeout"
        );
        // While it holds, an upload's transaction waits its session's lock_timeout and is refused.
        let refused = store
            .put_entry(
                &id,
                &Platform::new("linux", "amd64").expect("platform"),
                b"an artifact".to_vec(),
            )
            .expect_err("refused while a sweep holds the lock");
        assert!(refused.contains("blob store's lock"), "{refused}");
        assert!(refused.contains("lock timeout"), "{refused}");
        drop(hold);
    }

    /// A hold on the blob store's lock whose session the database ends is marked lost, and
    /// logged: the lock is gone with the session.
    /// Verifies: ADR-0044
    #[test]
    fn a_hold_whose_session_ends_is_marked_lost() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let hold = database.hold_blobs().expect("held");
        assert!(!hold.lost(), "held");
        let key = format!("(SELECT {} AS key)", crate::postgres::BLOB_LOCK);
        test.rows(&format!(
            "SELECT pg_terminate_backend(pid) FROM pg_locks, {key} k \
             WHERE locktype = 'advisory' AND granted \
             AND classid = ((k.key >> 32) & 4294967295)::oid \
             AND objid = (k.key & 4294967295)::oid"
        ));
        until("the hold is marked lost", &|| hold.lost());
        drop(hold);
        drop(database.hold_blobs().expect("the lock is free again"));
    }

    /// An upload whose transaction loses its session after the check that its blob is stored, and
    /// with it the blob store's lock, writes no entry: a sweep may run once the lock is gone, and
    /// the entry would name what it deleted.
    /// Verifies: ADR-0044
    #[test]
    fn an_upload_whose_session_ends_before_its_commit_writes_no_entry() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let dir = tempfile::tempdir().expect("tempdir");
        let id = PackageId::new("otelcol", "1.0.0").expect("id");
        let linux = Platform::new("linux", "amd64").expect("platform");
        let (pausing, reaches, go) = Pausing::at(dir.path(), "exists");
        let a = Arc::new(
            crate::packages::PackageStore::with_backend(
                Box::new(PgPackageBackend::new(database.clone())),
                Box::new(pausing),
                crate::clock::Elapsed::monotonic(),
            )
            .expect("open A"),
        );
        a.create(&id).expect("create");
        let uploading = std::thread::spawn({
            let (a, id, linux) = (a.clone(), id.clone(), linux.clone());
            move || a.put_entry(&id, &linux, b"an artifact".to_vec())
        });
        reaches
            .recv()
            .expect("A checks its blob, holding the lock shared");
        let key = format!("(SELECT {} AS key)", crate::postgres::BLOB_LOCK);
        // The session that holds the lock shared in a transaction: the upload's.
        let pid = test.once(&format!(
            "SELECT l.pid::text FROM pg_locks l, {key} k \
             WHERE l.locktype = 'advisory' AND l.granted AND l.mode = 'ShareLock' \
             AND l.classid = ((k.key >> 32) & 4294967295)::oid \
             AND l.objid = (k.key & 4294967295)::oid \
             AND l.pid IN (SELECT pid FROM pg_stat_activity WHERE xact_start IS NOT NULL)"
        ));
        test.rows(&format!("SELECT pg_terminate_backend({pid})"));
        until("the upload's session is gone", &|| {
            test.rows(&format!("SELECT 1 FROM pg_stat_activity WHERE pid = {pid}"))
                .is_empty()
        });
        go.send(()).expect("go on");
        let failed = uploading
            .join()
            .expect("A")
            .expect_err("its transaction does not commit");
        assert!(
            !failed.contains("no answer within"),
            "it failed with its session, not its bound: {failed}"
        );
        assert!(
            test.rows("SELECT 1 FROM package_entries").is_empty(),
            "no entry is written"
        );
    }

    /// A sweep at startup that cannot have the blob store's lock fails the start, naming the lock.
    /// Verifies: ADR-0044
    #[test]
    fn a_startup_sweep_that_cannot_have_the_lock_fails_naming_it() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        // The hold is a session of its own, as another instance's would be.
        let database = test.open();
        let hold = database.hold_blobs().expect("another instance sweeps");
        database.wait_for_sweeps(std::time::Duration::from_millis(200));
        let dir = tempfile::tempdir().expect("tempdir");
        let err = crate::packages::PackageStore::with_backend(
            Box::new(PgPackageBackend::new(database.clone())),
            Box::new(crate::fs::FsBlobStore::open(dir.path()).expect("blobs")),
            crate::clock::Elapsed::monotonic(),
        )
        .map(drop)
        .expect_err("the lock is held");
        assert!(err.contains("blob store's lock"), "{err}");
        // The refused session ends, and is no hold lost: it never had the lock.
        until("the refused session ends", &|| database.holds_lost().1 >= 1);
        assert_eq!(database.holds_lost().0, 0, "a refused hold loses nothing");
        drop(hold);
        drop(instance(&database, dir.path()));
    }
}
