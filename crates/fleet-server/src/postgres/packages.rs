//! Package records in PostgreSQL (ADR-0029 clause 6): the PostgreSQL adapter behind
//! [`PackageBackend`](crate::packages::PackageBackend) — one row per Package and one per entry,
//! each entry naming its artifact by hash, and each call one transaction on the rows it names
//! (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
//! clause 19). The bytes are the blob store's, with either.

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio_postgres::GenericClient;

use super::notify::{notify, PACKAGE};
use super::{client, signed, sql, unsigned, Database};
use crate::packages::{
    Entry, Package, PackageBackend, PackageId, Platform, Signed, ED25519_SIGNATURE_LEN,
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

    /// The entry's row alone, inserted or replaced; the Package's row is held `FOR KEY SHARE`
    /// by the reference, so a deletion of the Package waits for it or is waited for.
    fn put_entry(&self, id: &PackageId, entry: &Entry) -> Result<(), String> {
        let (id, entry) = (id.clone(), entry.clone());
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            if transaction
                .query_opt(
                    "SELECT 1 FROM packages WHERE agent_type = $1 AND version = $2 \
                     FOR KEY SHARE",
                    &[&id.agent_type, &id.version],
                )
                .await
                .map_err(sql)?
                .is_none()
            {
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

    /// One statement on the entry's row, conditioned on the bytes it names: the row is locked
    /// by the update, so it is decided from what is stored. When it writes nothing, the entry as
    /// stored is read in the same transaction.
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

    fn remove_entry(&self, id: &PackageId, platform: &Platform) -> Result<(), String> {
        let (id, platform) = (id.clone(), platform.clone());
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
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

    /// The Package and its entries go in one statement, the entries by cascade.
    fn remove(&self, id: &PackageId) -> Result<(), String> {
        let id = id.clone();
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            transaction
                .execute(
                    "DELETE FROM packages WHERE agent_type = $1 AND version = $2",
                    &[&id.agent_type, &id.version],
                )
                .await
                .map_err(sql)?;
            notify(&*transaction, PACKAGE, &id.to_string()).await?;
            transaction.commit().await.map_err(sql)
        })
    }
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
            store.put_entry(&package.id, entry).expect("put entry");
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
            )
            .expect_err("an entry of a Package the store does not hold");

        let replaced = package("1.2.3", &[("linux", "amd64", 3), ("windows", "amd64", 2)]);
        let linux = Platform::new("linux", "amd64").expect("platform");
        store
            .put_entry(&one.id, &replaced.entries[&linux])
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

    /// Verifies: G-19, ADR-0029
    #[test]
    fn the_package_port_contract_holds_on_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        contract(&|| Box::new(FsPackageBackend::open(dir.path()).expect("open")));
    }

    /// Verifies: G-19, ADR-0029
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
            .put_entry(&before.id, &broken.entries[&windows])
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

    /// Verifies: ADR-0029
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
    /// Verifies: ADR-0029
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
}
