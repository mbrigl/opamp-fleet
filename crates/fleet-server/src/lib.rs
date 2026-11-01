//! The OpAMP Fleet Server (ADR-0008, ADR-0012): the control plane that tells Agents which
//! configuration they should run and records what they report back.
//!
//! A library crate so integration tests can assemble the exact router the binary serves.

#![forbid(unsafe_code)]

pub mod agent_rate;
pub mod agent_store;
pub mod api;
pub mod audit;
pub mod audit_log;
pub mod blobs;
pub mod bootstrap;
pub mod bridge;
pub mod ca;
pub mod clock;
pub mod config;
pub mod configs;
pub mod credentials;
pub mod deployments;
pub mod enrolment;
pub mod fleet;
pub mod floors;
pub mod fs;
pub mod instance;
pub mod labels;
pub mod listen;
pub mod migrate;
pub mod packages;
pub mod pki;
pub mod postgres;
pub mod revocation;
pub mod s3;
pub mod throttle;
pub mod tls;
pub mod transport;

use std::sync::Arc;

use axum::Router;

use deployments::DeploymentStore;
use fleet::{AppState, PackageOffering};
use fs::{FsAgentStore, FsBlobStore, FsConfigBackend, FsDeploymentBackend, FsPackageBackend};

/// Whether an instance on PostgreSQL must be alone on its database, and the key that makes it so:
/// blobs kept as files, or a CA key in a file
/// (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
/// clause 2).
#[must_use]
pub fn exclusive(config: &config::ServerConfig) -> postgres::Exclusive {
    if config.blobs.s3().is_none() {
        postgres::Exclusive::Alone("[blobs] store = \"fs\"".to_string())
    } else if config.client_ca.is_some() {
        postgres::Exclusive::Alone("[client_ca] key_file".to_string())
    } else if config
        .enrolment
        .as_ref()
        .is_some_and(|enrolment| enrolment.bootstrap_ca_key_file.is_some())
    {
        postgres::Exclusive::Alone("[enrolment] bootstrap_ca_key_file".to_string())
    } else {
        postgres::Exclusive::Shared
    }
}

/// An error of the data directory, naming the key that chose it: a Server given none falls back
/// to `fleet-data` in its working directory, which the operator may not have meant.
fn named(error: String) -> String {
    format!("data_dir: {error}")
}

/// Where the Server keeps its records (ADR-0029 clause 1): files under `data_dir`, or one
/// PostgreSQL database. The composition root asks it for each port's adapter, so the rest of the
/// Server names the ports alone.
pub enum Records {
    /// One file per record under this `data_dir` (ADR-0029).
    Files(std::path::PathBuf),
    /// One PostgreSQL database, shared by the instances on it (ADR-0029).
    Postgres(Arc<postgres::Database>),
}

impl Records {
    /// Opens the store `[database]` chooses: the data directory, its layout created owner-only
    /// and checked, and with PostgreSQL the database, its lock taken and its schema applied.
    ///
    /// # Errors
    /// Returns an error naming the path or the key when the data directory or the database cannot
    /// be opened.
    pub fn open(config: &config::ServerConfig) -> Result<Self, String> {
        match (config.database.postgres(), config.data_dir()) {
            (None, Some(data_dir)) => {
                fs::open_data_dir(data_dir, config.blobs.s3().is_none()).map_err(named)?;
                Ok(Records::Files(data_dir.to_path_buf()))
            }
            (None, None) => Err("the filesystem record store needs data_dir".to_string()),
            (Some(database), data_dir) => {
                // With S3 too the Server keeps nothing on its disk and has no data directory
                // (ADR-draft-the-servers-state-and-its-stores clause 1).
                if let Some(data_dir) = data_dir {
                    fs::open_data_dir_beside_database(data_dir).map_err(named)?;
                }
                Ok(Records::Postgres(postgres::Database::open(
                    database,
                    exclusive(config),
                )?))
            }
        }
    }

    /// The Agent records.
    ///
    /// # Errors
    /// Returns an error when the filesystem adapter's directory cannot be created.
    pub fn agents(&self) -> Result<Box<dyn agent_store::AgentStore>, String> {
        Ok(match self {
            Records::Files(dir) => Box::new(FsAgentStore::open(dir)?),
            Records::Postgres(db) => Box::new(postgres::agents::PgAgentStore::new(db.clone())),
        })
    }

    /// The Configurations.
    ///
    /// # Errors
    /// As [`agents`](Self::agents).
    pub fn configs(&self) -> Result<Box<dyn configs::ConfigBackend>, String> {
        Ok(match self {
            Records::Files(dir) => Box::new(FsConfigBackend::open(dir)?),
            Records::Postgres(db) => Box::new(postgres::configs::PgConfigBackend::new(db.clone())),
        })
    }

    /// The Deployments.
    ///
    /// # Errors
    /// As [`agents`](Self::agents).
    pub fn deployments(&self) -> Result<Box<dyn deployments::DeploymentBackend>, String> {
        Ok(match self {
            Records::Files(dir) => Box::new(FsDeploymentBackend::open(dir)?),
            Records::Postgres(db) => {
                Box::new(postgres::deployments::PgDeploymentBackend::new(db.clone()))
            }
        })
    }

    /// The Package records.
    ///
    /// # Errors
    /// As [`agents`](Self::agents).
    pub fn packages(&self) -> Result<Box<dyn packages::PackageBackend>, String> {
        Ok(match self {
            Records::Files(dir) => Box::new(FsPackageBackend::open(dir)?),
            Records::Postgres(db) => {
                Box::new(postgres::packages::PgPackageBackend::new(db.clone()))
            }
        })
    }

    /// The bootstrap certificates the Server issued.
    ///
    /// # Errors
    /// As [`agents`](Self::agents).
    pub fn bootstrap(&self) -> Result<Box<dyn bootstrap::BootstrapStore>, String> {
        Ok(match self {
            Records::Files(dir) => Box::new(fs::FsBootstrapStore::open(dir)?),
            Records::Postgres(db) => {
                Box::new(postgres::bootstrap::PgBootstrapStore::new(db.clone()))
            }
        })
    }

    /// The enrolment window and its queue.
    #[must_use]
    pub fn enrolment(&self) -> Box<dyn enrolment::EnrolmentStore> {
        match self {
            Records::Files(dir) => Box::new(fs::FsEnrolmentStore::open(dir)),
            Records::Postgres(db) => {
                Box::new(postgres::enrolment::PgEnrolmentStore::new(db.clone()))
            }
        }
    }

    /// The floor of each Agent type (ADR-0028 clause 15).
    #[must_use]
    pub fn floors(&self) -> Box<dyn floors::FloorStore> {
        match self {
            Records::Files(dir) => Box::new(fs::FsFloorStore::open(dir)),
            Records::Postgres(db) => Box::new(postgres::floors::PgFloorStore::new(db.clone())),
        }
    }

    /// The register, the revocation list and the Hosts.
    ///
    /// # Errors
    /// As [`agents`](Self::agents).
    pub fn ledger(&self) -> Result<Box<dyn revocation::LedgerStore>, String> {
        Ok(match self {
            Records::Files(dir) => Box::new(fs::FsLedgerStore::open(dir)?),
            Records::Postgres(db) => Box::new(postgres::ledger::PgLedgerStore::new(db.clone())),
        })
    }
}

/// The default wiring of the fleet's state to its storage adapters (ADR-0009): it lives here, in
/// the composition root, so the fleet logic names the port — [`AgentStore`](agent_store::AgentStore)
/// — and never the filesystem behind it.
impl AppState {
    /// Builds the state on the default storage adapters in the data directory `data_dir`
    /// (ADR-0029) — one JSON file per Agent, its labels inside, under `agents/`, and one per
    /// Configuration under `configurations/` — restoring every persisted Configuration and Agent
    /// record. A store that cannot be opened (or holds an unparsable file) fails startup loudly.
    ///
    /// # Errors
    /// Returns an error when a store cannot be opened or holds an unparsable record.
    pub fn new(data_dir: std::path::PathBuf) -> Result<Self, String> {
        let agents = FsAgentStore::open(&data_dir)?;
        let configs = FsConfigBackend::open(&data_dir)?;
        Self::with_stores(
            Box::new(agents),
            Box::new(configs),
            Box::new(clock::SystemClock),
        )
    }

    /// Builds the state on the adapters of `records` (ADR-0029), restoring every persisted
    /// Configuration and Agent record.
    ///
    /// # Errors
    /// Returns an error when a store cannot be opened or holds a record that does not decode.
    pub fn on(records: &Records) -> Result<Self, String> {
        Self::with_stores(
            records.agents()?,
            records.configs()?,
            Box::new(clock::SystemClock),
        )
    }
}

/// The default wiring of the package store to its filesystem adapter (ADR-0009).
impl packages::PackageStore {
    /// Opens the store in the data directory `data_dir`, loading every persisted Package
    /// (ADR-0027, ADR-0029).
    ///
    /// # Errors
    /// Returns an error when the store cannot be opened, or a Package in it cannot be read, does
    /// not match its recorded hash, or is in a layout this Server does not write.
    pub fn open(data_dir: std::path::PathBuf) -> Result<Self, String> {
        Self::with_backend(
            Box::new(FsPackageBackend::open(&data_dir)?),
            Box::new(FsBlobStore::open(&data_dir)?),
        )
    }

    /// Opens the store in the data directory `data_dir` with the artifacts in the store `[blobs]`
    /// chooses (ADR-0029 clause 14): the filesystem as [`open`](Self::open) does, or the bucket of
    /// `[blobs.s3]`. On the bucket, a referenced artifact it lacks and `data_dir/blobs/` holds is
    /// copied into it first (ADR-0029 clause 19).
    ///
    /// # Errors
    /// Returns an error when the store cannot be opened, as [`open`](Self::open) does, when the
    /// bucket cannot be reached or listed, or when an artifact to copy does not match its name.
    pub fn open_with(
        data_dir: std::path::PathBuf,
        blobs: &config::BlobsConfig,
    ) -> Result<Self, String> {
        Self::open_on(&Records::Files(data_dir.clone()), Some(&data_dir), blobs)
            .map(|(store, _)| store)
    }

    /// [`open_with`](Self::open_with), the Package records in `records` (ADR-0029) and the bytes
    /// in the store `[blobs]` chooses, the filesystem's under `data_dir`, which is `None` only
    /// with PostgreSQL and S3 (ADR-draft-the-servers-state-and-its-stores clause 1).
    ///
    /// On the bucket the staging objects of every lease that is not live are deleted (clause 16),
    /// with PostgreSQL under the blob store's lock (clause 14) and by every sweep, and the cleanup
    /// comes back for the instance to run again whenever it takes over a lapsed lease. With the
    /// filesystem record store an upload is staged under `fs`, and everything staged is deleted
    /// at startup alone. On PostgreSQL the store is opened before the instance takes its lease, so
    /// that a bucket that fails leaves no lease behind, and it takes no upload until
    /// [`StagingCleanup::stage_under`](s3::StagingCleanup::stage_under) names that lease.
    ///
    /// # Errors
    /// As [`open_with`](Self::open_with), and when the filesystem blob store has no `data_dir`.
    pub fn open_on(
        records: &Records,
        data_dir: Option<&std::path::Path>,
        blobs: &config::BlobsConfig,
    ) -> Result<(Self, Option<s3::StagingCleanup>), String> {
        let backend = records.packages()?;
        let Some(bucket) = blobs.s3() else {
            let data_dir =
                data_dir.ok_or_else(|| "the filesystem blob store needs data_dir".to_string())?;
            return Self::with_backend(backend, Box::new(FsBlobStore::open(data_dir)?))
                .map(|store| (store, None));
        };
        let (store, live, hold): (_, s3::LiveLeases, s3::HoldBlobs) = match records {
            // One Server and no lease: every staging object is a leftover, and nobody else
            // sweeps.
            Records::Files(_) => {
                let store = s3::S3BlobStore::open(bucket)?;
                store.clean_staging_with(&|| Ok(std::collections::HashSet::new()))?;
                (
                    store,
                    Arc::new(|| Ok(std::collections::HashSet::new())),
                    Arc::new(|| Ok(blobs::BlobHold::none())),
                )
            }
            // The startup cleanup runs in the sweep of `with_backend`, under the lock.
            Records::Postgres(database) => {
                let (live, hold) = (database.clone(), database.clone());
                let live: s3::LiveLeases = Arc::new(move || live.live_leases());
                (
                    s3::S3BlobStore::open(bucket)?
                        .awaiting_a_lease()
                        .with_live(live.clone()),
                    live,
                    Arc::new(move || hold.hold_blobs()),
                )
            }
        };
        let cleanup = s3::StagingCleanup::new(store.clone(), live, hold);
        if let Some(data_dir) = data_dir {
            let referenced: Vec<String> = backend
                .load()?
                .iter()
                .flat_map(|package| package.entries.values())
                .map(|entry| hex::encode(&entry.content_hash))
                .collect();
            s3::copy_from_filesystem(
                &data_dir.join(fs::BLOBS),
                referenced.iter().map(String::as_str),
                &store,
            )?;
        }
        Self::with_backend(backend, Box::new(store)).map(|store| (store, Some(cleanup)))
    }
}

/// The default wiring of the package offering to the Deployments' filesystem adapter (ADR-0009).
impl PackageOffering {
    /// Arms the **Deployments** beside the package store, from `deployments/` in the data
    /// directory `data_dir` (ADR-0027, ADR-0029).
    ///
    /// # Errors
    /// Returns an error when the Deployments cannot be opened or one of them cannot be read.
    pub fn new(data_dir: &std::path::Path, store: packages::PackageStore) -> Result<Self, String> {
        let backend = FsDeploymentBackend::open(data_dir)?;
        let deployments = DeploymentStore::open(Box::new(backend))?;
        Ok(Self::with_deployments(store, deployments))
    }

    /// Arms the Deployments of `records` beside the package store (ADR-0029).
    ///
    /// # Errors
    /// Returns an error when the Deployments cannot be opened or one of them does not decode.
    pub fn on(records: &Records, store: packages::PackageStore) -> Result<Self, String> {
        let deployments = DeploymentStore::open(records.deployments()?)?;
        Ok(Self::with_deployments(store, deployments))
    }
}

/// The default wiring of the bootstrap record to its filesystem adapter (ADR-0009, ADR-0029).
impl bootstrap::BootstrapCertificates {
    /// Opens the record in the data directory `data_dir`, one file per certificate under
    /// `bootstrap/` (ADR-0029), forgetting what has ended.
    ///
    /// # Errors
    /// Returns an error when the directory cannot be opened or a file in it cannot be read.
    pub fn on_disk(
        signer: Option<Box<dyn bootstrap::BootstrapSigner>>,
        data_dir: &std::path::Path,
        clock: Arc<dyn fleet::Clock>,
    ) -> Result<Self, String> {
        Self::open(
            signer,
            Box::new(fs::FsBootstrapStore::open(data_dir)?),
            clock,
        )
    }

    /// Opens the record in `records` (ADR-0029), forgetting what has ended.
    ///
    /// # Errors
    /// Returns an error when the store cannot be opened or read.
    pub fn on(
        signer: Option<Box<dyn bootstrap::BootstrapSigner>>,
        records: &Records,
        clock: Arc<dyn fleet::Clock>,
    ) -> Result<Self, String> {
        Self::open(signer, records.bootstrap()?, clock)
    }
}

/// The **Agent plane** (ADR-0012): the OpAMP endpoint, guarded by Admission (ADR-0021), and the
/// package download route beside it — behind the same handshake, and reached only with a
/// certificate of the fleet (ADR-0021 clause 18).
///
/// The download lives here rather than with the rest of `/api/v1` because the split between the
/// two planes is by *audience*, not by path: this route is the one an Agent calls, and its
/// `download_url` is resolved against the Agent's own endpoint.
pub fn agent_app(state: Arc<AppState>, admission: transport::Admission) -> Router {
    let guard = admission
        .download_guard()
        .with_agent_rate(state.agent_rate().cloned());
    transport::router(state.clone(), admission).merge(transport::guard_download(
        api::download_router(state),
        guard,
    ))
}

/// The **Operator plane** (ADR-0012): the REST API, its OpenAPI document and docs page, and the
/// bundled UI — on their own listener, guarded as a whole by `[rest.auth]` when one is configured
/// (ADR-0021). Without it the plane is open, which is what its loopback default is for.
pub fn operator_app(state: Arc<AppState>, auth: Option<api::OperatorAuth>) -> Router {
    api::router(state, auth)
}
