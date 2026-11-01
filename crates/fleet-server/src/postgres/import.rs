//! `server migrate-to-postgres` (ADR-0029 clause 27): an existing `data_dir` copied into
//! PostgreSQL while the Server is stopped.
//!
//! The database is checked first: it must hold none of the Server's tables, and nothing is created
//! before it has been found so. Every record of the old `data_dir` is then read through the
//! filesystem adapters — from a copy, since they repair what they open — so a record that would
//! fail a Server's startup fails here before anything is written. With the filesystem blob store,
//! the blobs are copied byte for byte into the new `data_dir`; with S3 there is no new `data_dir`,
//! and the blobs the Package entries reference are uploaded into the bucket, the records read
//! from a copy in a directory of the run's own. The audit record is not copied: the old `audit/`
//! stays as the closed archive, and the `seq` and the hash of its last entry are recorded for the
//! first chain the database opens to name as its predecessor. Then the
//! schema and every record are written in one transaction, so a copy that fails leaves the
//! database without the Server's tables, and the new `data_dir` as empty as it was. The old
//! `data_dir` is only read — no directory created, no mode changed, no file removed: it stays as
//! the backup. Moving records back from PostgreSQL to files is not provided.

use std::collections::HashMap;
use std::path::Path;

use opamp::uid::InstanceUid;

use super::{client, schema, sql, Database, IMPORT_TIMEOUT};
use crate::agent_store::{AgentStore as _, PersistedAgent};
use crate::blobs::BlobStore as _;
use crate::bootstrap::{BootstrapStore as _, Entry as BootstrapEntry};
use crate::config::{PostgresConfig, S3BlobsConfig};
use crate::configs::{ConfigBackend as _, Configuration};
use crate::deployments::{Deployment, DeploymentBackend as _};
use crate::enrolment::{EnrolmentStore as _, Persisted};
use crate::floors::{Floor, FloorStore as _};
use crate::fs;
use crate::packages::{Package, PackageBackend as _};
use crate::revocation::{Ledger, LedgerStore as _};

/// What the copy carried, one line per kind, for the operator to read.
#[derive(Debug, Default)]
pub struct Report {
    pub lines: Vec<String>,
}

/// Every record of a `data_dir`, as its filesystem adapters read it.
struct Records {
    configurations: Vec<Configuration>,
    deployments: Vec<Deployment>,
    packages: Vec<Package>,
    agents: HashMap<InstanceUid, PersistedAgent>,
    ledger: Ledger,
    bootstrap: Vec<(BootstrapEntry, Option<Vec<u8>>)>,
    enrolment: Option<Persisted>,
    floors: std::collections::BTreeMap<String, Floor>,
}

impl Records {
    /// Reads every record under `root` through the filesystem adapters, as a Server's startup does.
    fn read(root: &Path) -> Result<Self, String> {
        let bootstrap_store = fs::FsBootstrapStore::open(root)?;
        let mut bootstrap = Vec::new();
        for entry in bootstrap_store.load()? {
            let file = bootstrap_store.p12(&entry.id)?;
            bootstrap.push((entry, file));
        }
        Ok(Records {
            configurations: fs::FsConfigBackend::open(root)?.load()?,
            deployments: fs::FsDeploymentBackend::open(root)?.load()?,
            packages: fs::FsPackageBackend::open(root)?.load()?,
            agents: fs::FsAgentStore::open(root)?.load()?,
            ledger: fs::FsLedgerStore::open(root)?.load()?,
            bootstrap,
            enrolment: fs::FsEnrolmentStore::open(root).load()?,
            floors: fs::FsFloorStore::open(root).load()?,
        })
    }

    fn lines(&self) -> Vec<String> {
        let revisions: usize = self
            .configurations
            .iter()
            .map(|config| config.retained.len())
            .sum();
        let p12 = self
            .bootstrap
            .iter()
            .filter(|(_, file)| file.is_some())
            .count();
        vec![
            format!(
                "{} configurations with {revisions} retained revisions",
                self.configurations.len()
            ),
            format!("{} deployments", self.deployments.len()),
            format!("{} packages", self.packages.len()),
            format!("{} agents", self.agents.len()),
            format!("{} hosts", self.ledger.hosts.len()),
            format!("{} certificates in the register", self.ledger.issued.len()),
            format!("{} revocations", self.ledger.revocations.len()),
            format!(
                "{} bootstrap certificates, {p12} with a PKCS#12 file",
                self.bootstrap.len()
            ),
            format!(
                "the enrolment window and {} pending requests",
                self.enrolment.as_ref().map_or(0, |persisted| {
                    serde_json::to_value(persisted)
                        .ok()
                        .and_then(|value| value.get("requests")?.as_array().map(Vec::len))
                        .unwrap_or(0)
                })
            ),
            format!("{} package floors", self.floors.len()),
        ]
    }

    /// Writes every record inside the caller's transaction.
    async fn write(self, client: &impl tokio_postgres::GenericClient) -> Result<(), String> {
        for configuration in &self.configurations {
            super::configs::write(client, configuration).await?;
        }
        for deployment in &self.deployments {
            super::deployments::write(client, deployment).await?;
        }
        for package in &self.packages {
            super::packages::write(client, package).await?;
        }
        for (uid, record) in &self.agents {
            super::agents::write(client, uid, record).await?;
        }
        for issued in &self.ledger.issued {
            super::ledger::write_issued(client, issued).await?;
        }
        let revocations: Vec<(i32, crate::revocation::Revocation)> = self
            .ledger
            .revocations
            .into_iter()
            .enumerate()
            .map(|(position, entry)| (i32::try_from(position).expect("the list is bounded"), entry))
            .collect();
        super::ledger::write_revocations(client, &revocations).await?;
        for (host, entry) in &self.ledger.hosts {
            super::ledger::write_host(client, host, entry).await?;
            super::ledger::write_bindings(client, host, entry).await?;
        }
        for (entry, file) in &self.bootstrap {
            super::bootstrap::write(client, entry, file.as_deref()).await?;
        }
        if let Some(enrolment) = &self.enrolment {
            super::enrolment::write(client, enrolment).await?;
        }
        for (agent_type, floor) in &self.floors {
            super::floors::write(client, agent_type, floor).await?;
        }
        Ok(())
    }
}

/// Where the copy puts the blobs (ADR-draft-the-servers-state-and-its-stores clause 27).
#[derive(Clone, Copy)]
pub enum Blobs<'a> {
    /// The filesystem blob store: `blobs/` is copied into this new, empty `data_dir`.
    Files(&'a Path),
    /// The bucket: every blob a Package entry references and the bucket lacks is uploaded from the
    /// old `data_dir/blobs/`. There is no new `data_dir`.
    Bucket(&'a S3BlobsConfig),
}

/// Copies the `data_dir` `from` into the database `config` names, and its blobs where `blobs`
/// says (ADR-draft-the-servers-state-and-its-stores clause 27).
///
/// # Errors
/// Returns an error naming what could not be read or copied, when the new `data_dir` is not
/// empty, when the database holds any of the Server's tables, when a blob does not match its
/// name, and when a Package entry's blob is neither in the old `data_dir` nor in the bucket; the
/// database then holds none of the Server's tables.
pub fn run(from: &Path, blobs: Blobs<'_>, config: &PostgresConfig) -> Result<Report, String> {
    run_in(from, blobs, config, None)
}

/// [`run`] in the schema `schema` rather than the role's default.
pub(crate) fn run_in(
    from: &Path,
    blobs: Blobs<'_>,
    config: &PostgresConfig,
    schema: Option<&str>,
) -> Result<Report, String> {
    // Nothing is created or changed before the database has been found without the Server's
    // tables, and the old data_dir is only ever read (ADR-0029 clause 27).
    if !from.is_dir() {
        return Err(format!("{} is not a data directory", from.display()));
    }
    let to = match blobs {
        Blobs::Files(to) => to,
        Blobs::Bucket(bucket) => {
            let database = checked(config, schema)?;
            let store = crate::s3::S3BlobStore::open(bucket)?;
            // The records are read from a copy, made in a directory of this run's own that
            // nobody else may enter, and removed whatever the outcome.
            let work = work_dir()?;
            let copied = copy(from, &work, &database, Some((&store, bucket)));
            let _ = std::fs::remove_dir_all(&work);
            return copied;
        }
    };
    let existed = to.exists();
    if existed {
        let empty = std::fs::read_dir(to)
            .map_err(|e| format!("cannot read {}: {e}", to.display()))?
            .next()
            .is_none();
        if !empty {
            return Err(format!(
                "{} is not empty — name a new, empty directory as --data-dir",
                to.display()
            ));
        }
    }
    let database = checked(config, schema)?;
    let copied = copy(from, to, &database, None);
    if copied.is_err() {
        // `to` was empty or absent, so whatever is in it now is this copy's.
        let _ = if existed {
            std::fs::read_dir(to).map(|entries| {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let _ = if path.is_dir() {
                        std::fs::remove_dir_all(&path)
                    } else {
                        std::fs::remove_file(&path)
                    };
                }
            })
        } else {
            std::fs::remove_dir_all(to)
        };
    }
    copied
}

/// The database `config` names, checked to hold none of the Server's tables.
fn checked(config: &PostgresConfig, schema: Option<&str>) -> Result<Database, String> {
    let database = Database::connect(config, schema)?;
    let tables = database.server_tables()?;
    if !tables.is_empty() {
        return Err(format!(
            "the {} already holds the Server's tables ({}) — copy into a database without them",
            database.name(),
            tables.join(", ")
        ));
    }
    Ok(database)
}

/// A new, owner-only directory of this run's own under the system's temporary directory, created
/// where nothing of that name exists: where the records are read with S3, which has no new
/// `data_dir`.
fn work_dir() -> Result<std::path::PathBuf, String> {
    let mut random = [0u8; 16];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut random)
        .map_err(|_| "no randomness for a working directory".to_string())?;
    let dir = std::env::temp_dir().join(format!("opamp-fleet-migrate-{}", hex::encode(random)));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder
        .create(&dir)
        .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    Ok(dir)
}

/// The copy itself, once the database has been checked. The records are read from a copy of the
/// old `data_dir` made inside `to`: the filesystem adapters create what is missing, remove a
/// leftover temporary file and rewrite what they drop, as a Server's startup does, and the old
/// `data_dir` must not change in any way. The copy is removed once read; an error names the old
/// path, not the copy's.
/// With `bucket`, `to` is only where the records are read, and the blobs go into the bucket.
fn copy(
    from: &Path,
    to: &Path,
    database: &Database,
    bucket: Option<(&crate::s3::S3BlobStore, &S3BlobsConfig)>,
) -> Result<Report, String> {
    fs::create_private_dir(to)?;
    let staging = to.join(STAGING);
    let named = |e: String| e.replace(&staging.display().to_string(), &from.display().to_string());
    copy_records(from, &staging)?;
    let records = fs::check_layout(&staging)
        .and_then(|()| Records::read(&staging))
        .map_err(named);
    std::fs::remove_dir_all(&staging)
        .map_err(|e| format!("cannot remove {}: {e}", staging.display()))?;
    let records = records?;

    let mut report = Report::default();
    // No audit entry is copied: the files stay where they are as the closed archive, and its last
    // entry is recorded for the first chain to name as its predecessor
    // (ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance clause 6).
    let audit = from.join(fs::AUDIT);
    let archive = super::audit::archive_checked(&audit)?;
    let archive_end = archive.map(|archive| {
        report.lines.extend(archive.unlinked);
        (super::signed(archive.end.0), archive.end.1)
    });
    report.lines.push(match &archive_end {
        Some((seq, _)) => format!(
            "the audit record stays in {} as the archive; its last entry, {seq}, is the \
             predecessor of the first chain",
            audit.display()
        ),
        None => "no audit record to link".to_string(),
    });
    if let Some((store, bucket)) = bucket {
        report.lines.push(upload(
            &from.join(fs::BLOBS),
            &records.packages,
            store,
            bucket,
        )?);
    } else {
        let blobs = from.join(fs::BLOBS);
        let target = to.join(fs::BLOBS);
        fs::create_private_dir(&target)?;
        if blobs.is_dir() {
            for (name, path) in read_dir(&blobs)? {
                // What a crash left of an upload is no blob, and stays where it is.
                if name.ends_with(".tmp") && path.is_file() {
                    continue;
                }
                if !path.is_file() {
                    return Err(fs::unknown(&path));
                }
                copy_file(&path, &target.join(&name))?;
            }
        }
        // Each copy is hashed against its name, as a Server's startup hashes every blob.
        let listed = fs::FsBlobStore::open(to)
            .and_then(|store| store.list())
            .map_err(|e| e.replace(&target.display().to_string(), &blobs.display().to_string()))?;
        report.lines.push(format!("{} blobs", listed.len()));
    }

    let lines = records.lines();
    let ours = schema::server_version();
    database
        .run_within(IMPORT_TIMEOUT, move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            schema::apply(&*transaction, schema::MIGRATIONS, ours).await?;
            records.write(&*transaction).await?;
            if let Some((seq, hash)) = archive_end {
                transaction
                    .execute(
                        "INSERT INTO audit_archive (seq, hash) VALUES ($1, $2)",
                        &[&seq, &hash],
                    )
                    .await
                    .map_err(sql)?;
            }
            transaction.commit().await.map_err(sql)
        })
        .map_err(|e| match bucket {
            Some((_, bucket)) => format!(
                "{e} — nothing was written into the database, and what was uploaded stays in \
                 bucket {}; run the copy again",
                bucket.bucket
            ),
            None => format!(
                "{e} — nothing was written into the database or into {}; run the copy again",
                to.display()
            ),
        })?;
    report.lines.extend(lines);
    Ok(report)
}

/// Uploads into the bucket every blob of `packages` it lacks from the old `blobs/` directory,
/// each hashed against its name, as a Server's switch to S3 does (clause 19), and checks that the
/// bucket then holds every one. Answers the report's line.
fn upload(
    blobs: &Path,
    packages: &[Package],
    store: &crate::s3::S3BlobStore,
    bucket: &S3BlobsConfig,
) -> Result<String, String> {
    let referenced: std::collections::BTreeSet<String> = packages
        .iter()
        .flat_map(|package| package.entries.values())
        .map(|entry| hex::encode(&entry.content_hash))
        .collect();
    let uploaded =
        crate::s3::copy_from_filesystem(blobs, referenced.iter().map(String::as_str), store)?;
    let held: std::collections::HashSet<String> = store
        .list()?
        .into_iter()
        .map(|(sha256, _)| sha256)
        .collect();
    for package in packages {
        for (platform, entry) in &package.entries {
            let sha256 = hex::encode(&entry.content_hash);
            if !held.contains(&sha256) {
                return Err(format!(
                    "package {} for {}: its artifact {sha256} is neither in {} nor in bucket {} — \
                     restore it from a backup, or delete the entry, and copy again",
                    package.id,
                    platform.tag(),
                    blobs.display(),
                    bucket.bucket
                ));
            }
        }
    }
    Ok(format!(
        "{} blobs in bucket {}, {uploaded} of them uploaded from {}",
        referenced.len(),
        bucket.bucket,
        blobs.display()
    ))
}

/// Where inside the new `data_dir` the records of the old one are copied to be read.
const STAGING: &str = ".records-being-copied";

/// Copies every entry of the data directory `from` into `to`, but for the files of `audit/` and
/// `blobs/`, which are copied on their own: the layout as it stands, read and nothing else.
fn copy_records(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_private_dir(to)?;
    for (name, path) in read_dir(from)? {
        let target = to.join(&name);
        if path.is_dir() {
            fs::create_private_dir(&target)?;
            if name != fs::AUDIT && name != fs::BLOBS {
                copy_tree(&path, &target)?;
            }
        } else {
            copy_file(&path, &target)?;
        }
    }
    Ok(())
}

/// Copies the directory `from` into the existing directory `to`, every level of it.
fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    for (name, path) in read_dir(from)? {
        let target = to.join(&name);
        if path.is_dir() {
            fs::create_private_dir(&target)?;
            copy_tree(&path, &target)?;
        } else {
            copy_file(&path, &target)?;
        }
    }
    Ok(())
}

/// The entries of `dir` by name, read and nothing else. A name that is not UTF-8 is refused, as
/// the layout refuses it.
fn read_dir(dir: &Path) -> Result<Vec<(String, std::path::PathBuf)>, String> {
    let mut found = Vec::new();
    for entry in
        std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?
    {
        let path = entry
            .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
            .path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            return Err(fs::unknown(&path));
        };
        found.push((name.to_string(), path.clone()));
    }
    found.sort();
    Ok(found)
}

/// Copies one file byte for byte into a new owner-only file.
fn copy_file(from: &Path, to: &Path) -> Result<(), String> {
    let mut source =
        std::fs::File::open(from).map_err(|e| format!("cannot read {}: {e}", from.display()))?;
    let mut target = fs::create_owner_only(to)?;
    std::io::copy(&mut source, &mut target)
        .and_then(|_| target.sync_all())
        .map_err(|e| format!("cannot copy {} to {}: {e}", from.display(), to.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::*;
    use crate::audit::{Audit as _, Entry};
    use crate::postgres::testing::TestDatabase;
    use crate::revocation::{CertId, Facts, Host, Issued, Revocation, Revoked, SubjectValues};

    /// Every path under `root` with its mode and, for a file, its bytes: what "unchanged" is
    /// checked against — a directory created or removed, a mode changed, a file removed or
    /// rewritten all show.
    fn snapshot(root: &Path) -> BTreeMap<PathBuf, (u32, Option<Vec<u8>>)> {
        let mode = |path: &Path| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::metadata(path).expect("meta").permissions().mode()
            }
            #[cfg(not(unix))]
            {
                u32::from(
                    std::fs::metadata(path)
                        .expect("meta")
                        .permissions()
                        .readonly(),
                )
            }
        };
        let mut files = BTreeMap::from([(root.to_path_buf(), (mode(root), None))]);
        let mut dirs = vec![root.to_path_buf()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).expect("read") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    files.insert(path.clone(), (mode(&path), None));
                    dirs.push(path);
                } else {
                    let bytes = std::fs::read(&path).expect("read");
                    files.insert(path.clone(), (mode(&path), Some(bytes)));
                }
            }
        }
        files
    }

    /// What a Server's startup would repair in `root`, and the copy must leave as it is: a
    /// directory of the layout missing, a temporary file a crash left beside a record and at the
    /// root, and a directory whose mode is not the Server's.
    fn unrepaired(root: &Path) {
        std::fs::remove_dir_all(root.join(fs::HOSTS)).expect("no hosts/");
        std::fs::write(root.join("revocations.json.tmp"), "[").expect("leftover");
        let agent = fs::json_files(&root.join(fs::AGENTS)).expect("agents")[0]
            .1
            .clone();
        let mut leftover = agent.into_os_string();
        leftover.push(".tmp");
        std::fs::write(leftover, "{").expect("leftover");
        std::fs::write(root.join(fs::BLOBS).join("upload-1-0.tmp"), "half").expect("leftover");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(
                root.join(fs::CONFIGURATIONS),
                std::fs::Permissions::from_mode(0o750),
            )
            .expect("chmod");
        }
    }

    /// A `data_dir` holding at least one record of every kind, written through the filesystem
    /// adapters as a Server writes it, with an audit record of three entries.
    fn fixture(root: &Path) {
        fs::open_data_dir(root, true).expect("open");
        let pinned = crate::configs::Revision {
            body: "receivers: {}\n".to_string(),
            role: String::new(),
            service_name: "otelcol".to_string(),
        };
        let hash = crate::configs::revision_hash(&pinned);
        fs::FsConfigBackend::open(root)
            .expect("open")
            .put(&Configuration {
                name: "base".to_string(),
                saved: pinned.clone(),
                retained: BTreeMap::from([(hash.clone(), pinned)]),
            })
            .expect("configuration");
        let id = crate::packages::PackageId::new("otelcol", "1.2.3").expect("id");
        fs::FsDeploymentBackend::open(root)
            .expect("open")
            .put(&Deployment {
                name: "stable".to_string(),
                selector: BTreeMap::from([("fleet.ring".to_string(), "stable".to_string())]),
                packages: BTreeMap::from([(id.agent_type.clone(), id.clone())]),
                configurations: BTreeSet::from(["base".to_string()]),
            })
            .expect("deployment");
        let (sha256, size) = fs::FsBlobStore::open(root)
            .expect("open")
            .put(&mut &b"the artifact"[..])
            .expect("blob");
        let platform = crate::packages::Platform::new("linux", "amd64").expect("platform");
        fs::FsPackageBackend::open(root)
            .expect("open")
            .put(&Package {
                id: id.clone(),
                entries: BTreeMap::from([(
                    platform.clone(),
                    crate::packages::Entry {
                        platform,
                        content_hash: hex::decode(&sha256).expect("hex"),
                        size,
                        signature: Some(vec![7; 64]),
                    },
                )]),
            })
            .expect("package");
        let mut agent = crate::agent_store::tests::record();
        if let Some(assignment) = agent.assignment.as_mut() {
            assignment.configurations = BTreeMap::from([("base".to_string(), hash)]);
            assignment.package = Some(id);
        }
        fs::FsAgentStore::open(root)
            .expect("open")
            .put(&InstanceUid::default(), &agent)
            .expect("agent");
        let ledger = fs::FsLedgerStore::open(root).expect("open");
        let issued = Issued {
            facts: Facts {
                id: CertId::new(b"ca", "0a"),
                issuer_name: "CN=ca".into(),
                subject: "CN=edge-01".into(),
                key_fingerprint: "k".into(),
                not_after_ms: 9,
                host: Some("0192f0c4-host".into()),
                subject_values: SubjectValues::default(),
            },
            instance_uid: "00".into(),
            predecessor: None,
            issued_ms: 1,
        };
        ledger.put_issued(&issued).expect("issued");
        ledger
            .add_revocation(
                &Revocation {
                    id: "r1".into(),
                    revoked: Revoked::Certificate {
                        authority: "client".into(),
                        issuers: vec![issued.facts.id.issuer.clone()],
                        serial: "0b".into(),
                    },
                    revoked_ms: 2,
                },
                1,
            )
            .expect("revocations");
        ledger
            .save_hosts(&BTreeMap::from([(
                "0192f0c4-host".to_string(),
                Host {
                    instance_uids: BTreeSet::from(["00".to_string()]),
                    ..Host::default()
                },
            )]))
            .expect("hosts");
        fs::FsBootstrapStore::open(root)
            .expect("open")
            .put_with_p12(
                &BootstrapEntry {
                    id: CertId::new(b"bootstrap", "b1"),
                    serial: "b1".into(),
                    subject: "CN=fleet bootstrap".into(),
                    key_fingerprint: "00".into(),
                    issued_ms: 1,
                    not_after_ms: 2,
                    ca_key: "ab".into(),
                    p12: true,
                },
                b"the PKCS#12 file",
            )
            .expect("bootstrap");
        fs::FsEnrolmentStore::open(root)
            .save(
                &serde_json::from_value(serde_json::json!({"window_until_ms": 99, "requests": []}))
                    .expect("window"),
            )
            .expect("enrolment");
        fs::FsFloorStore::open(root)
            .put(
                "otelcol",
                &Floor {
                    version: "2.0.0".to_string(),
                    signature: vec![7; 64],
                },
            )
            .expect("floor");
        let audit = crate::audit_log::AuditLog::start(
            Box::new(fs::FsAuditStore::open(root).expect("open")),
            crate::audit_log::Limits::default(),
            Arc::new(crate::clock::SystemClock),
        )
        .expect("audit");
        for n in 0..3 {
            audit
                .record(Entry::new("test.event", "ok").with("n", n.to_string()))
                .expect("record");
        }
        assert!(audit.drain(std::time::Duration::from_secs(5)));
    }

    /// How many entries the chain in `dir` holds, verified.
    fn verified(dir: &Path) -> Result<u64, String> {
        fs::verify_audit(dir)?.1.map(|held| held.entries)
    }

    /// What a ledger holds, without the version only a database keeps.
    fn ledger_content(ledger: crate::revocation::Ledger) -> crate::revocation::Ledger {
        crate::revocation::Ledger {
            version: None,
            ..ledger
        }
    }

    /// The copy over a fixture `data_dir` carries every record into the database and the blobs
    /// into the new `data_dir`, leaves the audit record where it is as the archive and records its
    /// last entry, leaves the old `data_dir` unchanged, and refuses a database that holds the Server's tables and a
    /// `--data-dir` that is not empty (ADR-0029 clause 27).
    /// Verifies: ADR-0029
    #[test]
    fn migrate_to_postgres_carries_every_record_and_leaves_the_old_data_dir() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let (old, new) = (dir.path().join("old"), dir.path().join("new"));
        fixture(&old);
        std::fs::remove_dir_all(old.join(fs::HOSTS)).expect("no hosts/");
        // Read before the old data_dir is made unrepaired: the adapters would repair it.
        let expected = Records::read(&old).expect("read");
        unrepaired(&old);
        let before = snapshot(&old);

        std::fs::create_dir(&new).expect("mkdir");
        std::fs::write(new.join("stray"), "x").expect("write");
        let err = run_in(&old, Blobs::Files(&new), &test.config(), Some(&test.schema))
            .expect_err("not empty");
        assert!(err.contains("not empty"), "{err}");
        std::fs::remove_file(new.join("stray")).expect("rm");
        assert_eq!(snapshot(&old), before, "a refused copy changes nothing");

        let report =
            run_in(&old, Blobs::Files(&new), &test.config(), Some(&test.schema)).expect("copy");
        assert!(
            report.lines.iter().any(|line| line == "1 agents"),
            "{report:?}"
        );
        assert!(
            report
                .lines
                .iter()
                .any(|line| line.contains("as the archive; its last entry, 3,")),
            "{report:?}"
        );
        assert_eq!(snapshot(&old), before, "the old data_dir is unchanged");

        let database = test.open();
        let records = crate::Records::Postgres(database);
        assert_eq!(
            records.configs().expect("c").load().expect("load"),
            expected.configurations
        );
        assert_eq!(
            records.deployments().expect("d").load().expect("load"),
            expected.deployments
        );
        assert!(records.agents().expect("a").load().expect("load") == expected.agents);
        // The list's version is the database's own, which the files keep none of: the move starts
        // it at the migration's 0.
        let ledger = records.ledger().expect("l").load().expect("load");
        assert_eq!(ledger.version, Some(0));
        assert_eq!(ledger_content(ledger), ledger_content(expected.ledger));
        let bootstrap = records.bootstrap().expect("b");
        let entries = bootstrap.load().expect("load");
        assert_eq!(
            entries
                .iter()
                .map(|entry| (entry.clone(), bootstrap.p12(&entry.id).expect("p12")))
                .collect::<Vec<_>>(),
            expected.bootstrap
        );
        assert_eq!(
            records.enrolment().load().expect("load"),
            expected.enrolment
        );
        assert_eq!(records.floors().load().expect("load"), expected.floors);
        assert_eq!(expected.floors.len(), 1, "the fixture holds a floor");
        let packages = records.packages().expect("p").load().expect("load");
        // Every Package whole: its identity and each entry's platform, hash, size and signature.
        let shown = |packages: &[Package]| -> Vec<_> {
            packages
                .iter()
                .map(|package| {
                    let entries: Vec<_> = package
                        .entries
                        .iter()
                        .map(|(platform, entry)| {
                            (
                                platform.tag(),
                                entry.platform.tag(),
                                hex::encode(&entry.content_hash),
                                entry.size,
                                entry.signature.clone(),
                            )
                        })
                        .collect();
                    (package.id.to_string(), entries)
                })
                .collect()
        };
        assert_eq!(shown(&packages), shown(&expected.packages));
        assert!(
            shown(&packages)[0].1[0].4.is_some(),
            "the fixture's entry carries a signature"
        );
        let (state_packages, _) = crate::packages::PackageStore::open_on(
            &records,
            Some(&new),
            &crate::config::BlobsConfig::default(),
        )
        .expect("every entry finds its blob in the new data_dir");
        assert!(!state_packages.is_empty());

        // No audit entry is copied: the old record stays as the archive, and its end is recorded.
        assert!(!new.join(fs::AUDIT).exists(), "the audit record was copied");
        assert_eq!(verified(&old.join(fs::AUDIT)), Ok(3));
        let tail = fs::audit_tail(&old.join(fs::AUDIT))
            .expect("tail")
            .expect("entries");
        assert_eq!(
            test.rows("SELECT seq::text, hash FROM audit_archive"),
            vec![vec![
                Some(tail.last_seq.to_string()),
                Some(crate::audit_log::line_hash(&tail.last_line))
            ]]
        );
        drop((records, state_packages, bootstrap));

        // The lock goes with its session, which the server notices once the connection closes.
        let again = dir.path().join("again");
        let mut err = String::new();
        for _ in 0..50 {
            err = run_in(
                &old,
                Blobs::Files(&again),
                &test.config(),
                Some(&test.schema),
            )
            .expect_err("the tables are there");
            if !err.contains("one Server per database") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(err.contains("already holds the Server's tables"), "{err}");
        assert!(err.contains("agents"), "{err}");
        assert!(
            !again.exists(),
            "nothing is created before the database is checked"
        );
        assert_eq!(snapshot(&old), before, "the old data_dir is unchanged");
    }

    /// With S3 the copy has no new `data_dir`: it uploads into the bucket every blob a Package
    /// entry references and the bucket lacks, and nothing else, leaves no staging object and the old
    /// `data_dir` unchanged, and a Server on PostgreSQL and S3 then opens every entry's blob. A
    /// Package entry whose blob neither the old `data_dir` nor the bucket holds fails the copy,
    /// naming the entry, and leaves the database without the Server's tables
    /// (ADR-draft-the-servers-state-and-its-stores clause 27).
    /// Verifies: ADR-draft-the-servers-state-and-its-stores
    #[test]
    fn migrate_to_postgres_with_s3_uploads_the_referenced_blobs_and_takes_no_data_dir() {
        use crate::s3::tests::{again, clear, keys, named_store};
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let Some(bucket) = named_store() else { return };
        let dir = tempfile::tempdir().expect("tempdir");
        let old = dir.path().join("old");
        fixture(&old);
        let artifact = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(b"the artifact"));
        let unreferenced =
            hex::encode(<sha2::Sha256 as sha2::Digest>::digest(b"no entry names it"));
        std::fs::write(
            old.join(fs::BLOBS).join(&unreferenced),
            b"no entry names it",
        )
        .expect("unreferenced");
        let before = snapshot(&old);
        // The copies of the records this test's runs make: no other test makes one.
        let copies = || {
            std::fs::read_dir(std::env::temp_dir())
                .expect("temp")
                .flatten()
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("opamp-fleet-migrate-")
                })
                .count()
        };
        let entries = copies();

        let report = run_in(
            &old,
            Blobs::Bucket(&bucket),
            &test.config(),
            Some(&test.schema),
        )
        .expect("copy");
        assert!(
            report.lines.iter().any(|line| line
                == &format!(
                    "1 blobs in bucket {}, 1 of them uploaded from {}",
                    bucket.bucket,
                    old.join(fs::BLOBS).display()
                )),
            "{report:?}"
        );
        let store = crate::s3::S3BlobStore::open(&bucket).expect("open");
        assert_eq!(
            keys(&store),
            vec![format!("{}blobs/{artifact}", bucket.prefix)],
            "the referenced blob alone, and no staging object"
        );
        assert_eq!(snapshot(&old), before, "the old data_dir is unchanged");
        assert_eq!(copies(), entries, "the copy of the records is removed");
        let records = crate::Records::Postgres(test.open());
        let (packages, _) = crate::packages::PackageStore::open_on(
            &records,
            None,
            &crate::config::BlobsConfig {
                store: "s3".to_string(),
                s3: Some(again(&bucket)),
            },
        )
        .expect("every entry finds its blob in the bucket");
        assert!(!packages.is_empty());

        let Some(second) = TestDatabase::named() else {
            return;
        };
        let mut elsewhere = again(&bucket);
        elsewhere.prefix = format!("{}elsewhere/", bucket.prefix);
        std::fs::remove_file(old.join(fs::BLOBS).join(&artifact)).expect("lose the blob");
        let err = run_in(
            &old,
            Blobs::Bucket(&elsewhere),
            &second.config(),
            Some(&second.schema),
        )
        .expect_err("a blob neither holds");
        assert!(err.contains("otelcol@1.2.3"), "{err}");
        assert!(err.contains(&artifact), "{err}");
        assert!(
            crate::postgres::Database::connect(&second.config(), Some(&second.schema))
                .expect("connect")
                .server_tables()
                .expect("tables")
                .is_empty(),
            "the database holds none of the Server's tables"
        );
        clear(&store);
    }

    /// The database is checked before anything is touched: one that cannot be reached leaves the
    /// old `data_dir` as it was — no directory created, no mode changed, no leftover removed — and
    /// creates no new one (ADR-0029 clause 27). Needs no database.
    /// Verifies: ADR-0029
    #[test]
    fn a_copy_checks_the_database_before_it_touches_anything() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (old, new) = (dir.path().join("old"), dir.path().join("new"));
        fixture(&old);
        unrepaired(&old);
        let before = snapshot(&old);
        let password = dir.path().join("password");
        std::fs::write(&password, "secret").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&password, std::fs::Permissions::from_mode(0o600))
                .expect("chmod");
        }
        // A port nothing listens on.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .expect("a free port")
            .port();
        let config = PostgresConfig {
            host: "127.0.0.1".to_string(),
            port,
            dbname: "fleet".to_string(),
            user: "fleet".to_string(),
            password_file: password,
            ca_file: None,
            tls: false,
        };
        let err = run(&old, Blobs::Files(&new), &config).expect_err("nothing listens");
        assert!(err.contains(&format!("127.0.0.1:{port}")), "{err}");
        assert_eq!(snapshot(&old), before, "the old data_dir is unchanged");
        assert!(!new.exists(), "no new data_dir");
    }

    /// A copy that fails part-way through its transaction leaves the database without the
    /// Server's tables and the new `data_dir` empty. The failure is an object in the way of the
    /// schema's last table — no table, so the copy starts, and the first tables are created before
    /// the statement fails.
    /// Verifies: ADR-0029
    #[test]
    fn a_copy_that_fails_leaves_the_database_without_the_servers_tables() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let old = dir.path().join("old");
        fixture(&old);
        test.sql("CREATE TYPE enrolment_requests AS (id text)");
        let new = dir.path().join("new");
        let err = run_in(&old, Blobs::Files(&new), &test.config(), Some(&test.schema))
            .expect_err("the last table cannot be created");
        assert!(
            err.contains("nothing was written into the database"),
            "{err}"
        );
        let database = crate::postgres::Database::connect(&test.config(), Some(&test.schema))
            .expect("connect");
        assert!(database.server_tables().expect("tables").is_empty());
        assert!(!new.exists(), "the new data_dir is left as it was: absent");
    }

    /// After a move from files, the first chain the database opens names the archive's last entry
    /// as its predecessor; `audit-verify --archive` checks the link over the untouched archive and
    /// fails, naming the predecessor, once the archive's last entry was changed or removed;
    /// without `--archive` the predecessor is reported as not checked.
    /// Verifies: ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance
    #[test]
    fn the_first_chain_names_the_archive_and_audit_verify_checks_the_link() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let (old, new) = (dir.path().join("old"), dir.path().join("new"));
        fixture(&old);
        run_in(&old, Blobs::Files(&new), &test.config(), Some(&test.schema)).expect("copy");
        let archive = old.join(fs::AUDIT);
        let tail = fs::audit_tail(&archive).expect("tail").expect("entries");

        // The lock of the copy goes with its session; the first lease opens the first chain.
        let mut database = None;
        for _ in 0..50 {
            if let Ok(opened) = Database::open_in(
                &test.config(),
                super::super::Exclusive::Shared,
                Some(&test.schema),
            ) {
                database = Some(opened);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let database = database.expect("the copy's lock was not released");
        let first = database
            .take_lease("host-a", std::time::Duration::from_secs(30))
            .expect("lease");
        let second = database
            .take_lease("host-b", std::time::Duration::from_secs(30))
            .expect("lease");
        let opened = test.rows(&format!(
            "SELECT line FROM audit_entries WHERE chain = '{}' AND seq = 1",
            first.instance_id
        ));
        let opened = opened[0][0].clone().expect("chain_opened");
        assert!(
            opened.contains(&format!("\"predecessor_seq\":{}", tail.last_seq)),
            "{opened}"
        );
        assert!(
            opened.contains(&crate::audit_log::line_hash(&tail.last_line)),
            "{opened}"
        );
        let later = test.rows(&format!(
            "SELECT line FROM audit_entries WHERE chain = '{}' AND seq = 1",
            second.instance_id
        ));
        assert!(
            !later[0][0].clone().expect("line").contains("predecessor"),
            "only the first chain names the archive"
        );

        // Through the connection `audit-verify --config` makes.
        let checked =
            super::super::audit::verify_in(&test.config(), Some(&test.schema), Some(&archive))
                .expect("verify");
        assert!(!checked.failed, "{:?}", checked.lines);
        assert!(
            checked
                .lines
                .iter()
                .any(|line| line.contains("holds, and ends on entry 3")),
            "{:?}",
            checked.lines
        );
        let unchecked = super::super::audit::verify(&database, None).expect("verify");
        assert!(!unchecked.failed, "{:?}", unchecked.lines);
        assert!(
            unchecked
                .lines
                .iter()
                .any(|line| line.contains("was not checked")),
            "{:?}",
            unchecked.lines
        );

        // The archive's last entry removed: its chain still holds, but ends elsewhere.
        let newest = fs::audit_files(&archive)
            .expect("files")
            .pop()
            .expect("a file");
        let text = std::fs::read_to_string(&newest).expect("read");
        let mut lines: Vec<&str> = text.lines().collect();
        lines.pop();
        std::fs::write(&newest, lines.join("\n") + "\n").expect("write");
        let removed = super::super::audit::verify(&database, Some(&archive)).expect("verify");
        assert!(removed.failed, "{:?}", removed.lines);
        assert!(
            removed
                .lines
                .iter()
                .any(|line| line.contains("the predecessor") && line.contains("entry 3")),
            "{:?}",
            removed.lines
        );

        // Changed instead: the chain the predecessor names does not end there.
        let mut changed: Vec<String> = text.lines().map(str::to_string).collect();
        let last = changed
            .pop()
            .expect("a line")
            .replace("\"n\":\"2\"", "\"n\":\"9\"");
        changed.push(last);
        std::fs::write(&newest, changed.join("\n") + "\n").expect("write");
        let altered = super::super::audit::verify(&database, Some(&archive)).expect("verify");
        assert!(altered.failed, "{:?}", altered.lines);
        assert!(
            altered
                .lines
                .iter()
                .any(|line| line.contains("the predecessor")),
            "{:?}",
            altered.lines
        );
    }

    /// A file record that ends in a torn line stops the copy, naming the file, before anything is
    /// written: its end could never be verified as the first chain's predecessor.
    /// Verifies: ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance
    #[test]
    fn a_copy_refuses_an_audit_record_that_ends_in_a_torn_line() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let (old, new) = (dir.path().join("old"), dir.path().join("new"));
        fixture(&old);
        let file = fs::audit_files(&old.join(fs::AUDIT))
            .expect("files")
            .pop()
            .expect("a file");
        let mut text = std::fs::read_to_string(&file).expect("read");
        text.push_str("{\"seq\":4,\"pr");
        std::fs::write(&file, text).expect("write");
        let err = run_in(&old, Blobs::Files(&new), &test.config(), Some(&test.schema))
            .expect_err("a torn end");
        assert!(
            err.contains("torn") && err.contains(&file.display().to_string()),
            "{err}"
        );
        let database = crate::postgres::Database::connect(&test.config(), Some(&test.schema))
            .expect("connect");
        assert!(database.server_tables().expect("tables").is_empty());
    }
}
