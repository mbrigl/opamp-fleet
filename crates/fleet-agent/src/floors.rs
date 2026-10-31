//! The floor of each Agent type on this Client (ADR-0028 clause 17): fetched from the endpoint —
//! the Server, or the Gateway this Client sits behind — verified against `[packages]
//! verification_key`, and kept, the highest version per type, in `<state_dir>/package-floors.json`.
//! Every package offer is held to it, the Client's own included (ADR-0028 clauses 18 and 21).
//!
//! A floor only rises here. A floor that does not verify, or that orders below the one kept, is
//! ignored; a failed fetch keeps what is held; and nothing the Server sends lowers a floor — only
//! the host does, by editing or deleting the file while the Client is stopped (ADR-0028 clause 20).

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::config::ClientConfig;

/// The file under `state_dir` that holds the floors. Like the high-water marks (ADR-0028 clause 8)
/// it sits outside `supervisor_dir`, and its name holds a `.`, which no Supervisor name can: no
/// purge of a Supervisor's directory deletes it.
pub const FILE: &str = "package-floors.json";

/// Where the floors are fetched from, on the Agent plane and downstream of a Gateway alike.
pub const PATH: &str = "/v1/package-floors";

/// How often the floors are fetched while nothing is offered.
pub const REFRESH: Duration = Duration::from_secs(3600);

/// How old the last fetch may be when a package offer is decided.
pub const FRESH: Duration = Duration::from_secs(60);

/// How long one fetch may take. Shorter than a download's: it runs between the reply and the
/// offer it decides, and a Server that does not answer leaves the floors held to decide it.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// The largest body of floors read. One floor is an Agent type of at most 64 characters, a
/// version of at most 64 and 128 hex digits: a megabyte holds thousands of Agent types.
const MAX_BYTES: usize = 1 << 20;

/// One floor as the Server serves it and this Client keeps it: the version, and the operator's
/// signature over its statement in hex (`fleet_core::package::floor_statement`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Floor {
    pub version: String,
    pub signature: String,
}

/// Floors by Agent type: what is served, and what is kept.
pub type Floors = BTreeMap<String, Floor>;

/// The floors a body names.
///
/// # Errors
/// Returns an error for a body that is not a JSON object mapping an Agent type to a version and a
/// signature.
pub fn parse(body: &[u8]) -> Result<Floors, String> {
    serde_json::from_slice(body).map_err(|e| e.to_string())
}

/// Whether `floor` is the operator's for `agent_type`: its version SemVer (ADR-0028 clause 2) and
/// its signature one `key` verifies over the floor statement (ADR-0028 clause 14).
#[must_use]
pub fn verifies(key: &[u8], agent_type: &str, floor: &Floor) -> bool {
    let Ok(signature) = hex::decode(&floor.signature) else {
        return false;
    };
    fleet_core::version::is_semver(&floor.version)
        && ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
            .verify(
                &fleet_core::package::floor_statement(agent_type, &floor.version),
                &signature,
            )
            .is_ok()
}

/// The floors kept on the filesystem, and their fetch.
///
/// Read once, when the Client starts, and not verified again then: a floor was verified when it
/// was kept, so a later change of the key does not undo it. A file that exists but does not parse,
/// or maps a type to a version that is not SemVer, is never read as empty: every floor then
/// answers the reason, which refuses every package offer until the operator repairs or deletes the
/// file (ADR-0028 clause 17).
pub struct FloorFile {
    path: PathBuf,
    /// `[packages] verification_key`: without it nothing verifies, so nothing is fetched or kept —
    /// and no package is taken anyway (ADR-0027).
    key: Option<Vec<u8>>,
    kept: Mutex<Result<Floors, String>>,
    /// When a fetch was last tried, whatever came of it.
    fetched: Mutex<Option<Instant>>,
}

impl FloorFile {
    /// The floors kept under `state_dir`, checked against `key` when new ones are fetched. No file
    /// is no floor; an unreadable one is logged as an error naming it.
    #[must_use]
    pub fn open(state_dir: &Path, key: Option<Vec<u8>>) -> Self {
        let path = state_dir.join(FILE);
        let kept = match crate::nofollow::read(&path) {
            Ok(bytes) => parse_kept(&bytes),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Floors::new()),
            Err(e) => Err(e.to_string()),
        }
        .map_err(|why| {
            let reason = format!(
                "the package floors file {} cannot be read ({why}) — every package offer, the \
                 Client's own included, is refused until it is repaired or deleted",
                path.display()
            );
            error!("{reason}");
            reason
        });
        FloorFile {
            path,
            key,
            kept: Mutex::new(kept),
            fetched: Mutex::new(None),
        }
    }

    /// Keeps, per Agent type, each served floor that verifies and orders above the one kept, and
    /// writes the file when one was kept. One that does not verify, or orders below the one kept,
    /// is ignored and logged. An unreadable file stays as the operator finds it.
    pub fn take(&self, served: Floors) {
        let Some(key) = &self.key else {
            return;
        };
        let mut guard = self.kept.lock().expect("floors lock");
        let Ok(kept) = &mut *guard else {
            return;
        };
        let mut raised = false;
        for (agent_type, floor) in served {
            if !verifies(key, &agent_type, &floor) {
                warn!(
                    agent_type, version = %floor.version,
                    "ignoring a package floor that does not verify against [packages] \
                     verification_key"
                );
                continue;
            }
            match kept.get(&agent_type).map(|held| {
                (
                    fleet_core::version::precedence(&floor.version, &held.version),
                    held.version.clone(),
                )
            }) {
                Some((Some(std::cmp::Ordering::Less), held)) => {
                    warn!(
                        agent_type, version = %floor.version, kept = %held,
                        "ignoring a package floor below the one kept"
                    );
                }
                Some((Some(std::cmp::Ordering::Greater), _)) | None => {
                    info!(agent_type, version = %floor.version, "package floor raised");
                    kept.insert(agent_type, floor);
                    raised = true;
                }
                Some(_) => {}
            }
        }
        if raised {
            if let Err(e) = self.write(kept) {
                warn!(
                    file = %self.path.display(), error = %e,
                    "cannot write the package floors; they are kept in memory, and fetched again \
                     after the next start"
                );
            }
        }
    }

    fn write(&self, floors: &Floors) -> io::Result<()> {
        let mut json = serde_json::to_vec_pretty(floors).expect("floors serialize");
        json.push(b'\n');
        crate::storage::write_private(&self.path, &json)
    }

    /// Fetches the floors from `config`'s endpoint and keeps what verifies (ADR-0028 clause 17). A
    /// failed fetch keeps the floors held, and is logged. Nothing is fetched without a key.
    pub async fn update(&self, config: &ClientConfig) {
        if self.key.is_none() {
            return;
        }
        *self.fetched.lock().expect("fetched lock") = Some(Instant::now());
        match fetch(config).await.and_then(|body| parse(&body)) {
            Ok(served) => self.take(served),
            Err(e) => warn!(
                error = %e,
                "cannot fetch the package floors; deciding offers against the floors held"
            ),
        }
    }

    /// [`update`](Self::update), when the last fetch was tried `age` ago or longer, or never.
    pub async fn update_older_than(&self, config: &ClientConfig, age: Duration) {
        let due = self
            .fetched
            .lock()
            .expect("fetched lock")
            .is_none_or(|fetched| fetched.elapsed() >= age);
        if due {
            self.update(config).await;
        }
    }
}

/// The floors a kept file holds, or why they cannot be read: a JSON object of floors, each version
/// SemVer.
fn parse_kept(bytes: &[u8]) -> Result<Floors, String> {
    let floors = parse(bytes)?;
    if let Some((agent_type, floor)) = floors
        .iter()
        .find(|(_, floor)| !fleet_core::version::is_semver(&floor.version))
    {
        return Err(format!(
            "the floor of {agent_type:?} is {:?}, which is not SemVer",
            floor.version
        ));
    }
    Ok(floors)
}

impl crate::supervisor::ports::PackageFloors for FloorFile {
    fn floor(&self, agent_type: &str) -> Result<Option<String>, String> {
        match &*self.kept.lock().expect("floors lock") {
            Ok(floors) => Ok(floors.get(agent_type).map(|floor| floor.version.clone())),
            Err(reason) => Err(reason.clone()),
        }
    }
}

/// One fetch of the floors from `config`'s endpoint, with this Client's own certificate and no
/// `Authorization` (ADR-0021 clause 3): the body, bounded, or why there is none.
///
/// # Errors
/// Returns an error naming the URL when the endpoint cannot be reached, answers anything but a
/// success, or sends a body larger than a body of floors can be.
pub async fn fetch(config: &ClientConfig) -> Result<Vec<u8>, String> {
    let url = crate::packages::resolve_url(PATH, &config.endpoint)?;
    let client = crate::tls::client_tls(config)?
        .https_client(opamp::client::Timeouts::Total(FETCH_TIMEOUT))?;
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("{url}: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("{url} answered {}", response.status()));
    }
    crate::gateway::revocations::read_within(response, MAX_BYTES)
        .await
        .map_err(|e| format!("{url}: an unreadable body: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::ports::PackageFloors as _;
    use axum::response::IntoResponse as _;
    use ring::signature::{Ed25519KeyPair, KeyPair as _};

    fn keypair() -> Ed25519KeyPair {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).expect("key");
        Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("keypair")
    }

    fn signed(keypair: &Ed25519KeyPair, agent_type: &str, version: &str) -> Floor {
        Floor {
            version: version.to_string(),
            signature: hex::encode(
                keypair.sign(&fleet_core::package::floor_statement(agent_type, version)),
            ),
        }
    }

    fn file(dir: &Path, keypair: &Ed25519KeyPair) -> FloorFile {
        FloorFile::open(dir, Some(keypair.public_key().as_ref().to_vec()))
    }

    /// A served floor that verifies is kept and written owner-only; a higher one raises it; one
    /// that does not verify — signed by another key, over another type, or a package signature —
    /// or that orders below the one kept is ignored, and the file keeps the highest.
    /// Verifies: ADR-0028
    #[test]
    fn a_floor_that_does_not_verify_or_orders_below_the_kept_one_is_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let operator = keypair();
        let floors = file(dir.path(), &operator);
        floors.take(Floors::from([(
            "otelcol".to_string(),
            signed(&operator, "otelcol", "2.0.0"),
        )]));
        assert_eq!(floors.floor("otelcol"), Ok(Some("2.0.0".to_string())));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.path().join(FILE))
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        let package = Floor {
            version: "3.0.0".to_string(),
            signature: hex::encode(operator.sign(&fleet_core::package::statement(
                "otelcol", "3.0.0", &[0; 32],
            ))),
        };
        for (why, floor) in [
            ("another key", signed(&keypair(), "otelcol", "3.0.0")),
            ("another type", signed(&operator, "promtail", "3.0.0")),
            ("a package signature", package),
            ("below the kept one", signed(&operator, "otelcol", "1.9.4")),
        ] {
            floors.take(Floors::from([("otelcol".to_string(), floor)]));
            assert_eq!(
                floors.floor("otelcol"),
                Ok(Some("2.0.0".to_string())),
                "{why} is ignored"
            );
        }
        floors.take(Floors::from([(
            "otelcol".to_string(),
            signed(&operator, "otelcol", "2.1.0"),
        )]));
        assert_eq!(floors.floor("otelcol"), Ok(Some("2.1.0".to_string())));
        assert_eq!(floors.floor("promtail"), Ok(None));
        assert_eq!(
            file(dir.path(), &operator).floor("otelcol"),
            Ok(Some("2.1.0".to_string())),
            "the file holds the highest"
        );
    }

    /// A floor is verified when it is kept, not again when the file is read: a Client whose key
    /// changed keeps it.
    /// Verifies: ADR-0028
    #[test]
    fn a_kept_floor_is_not_verified_again_when_the_file_is_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let operator = keypair();
        file(dir.path(), &operator).take(Floors::from([(
            "otelcol".to_string(),
            signed(&operator, "otelcol", "2.0.0"),
        )]));
        assert_eq!(
            file(dir.path(), &keypair()).floor("otelcol"),
            Ok(Some("2.0.0".to_string()))
        );
    }

    /// A file that does not parse, or maps a type to a version that is not SemVer, is never read
    /// as empty: every floor answers a reason naming the file, and nothing fetched replaces it.
    /// Verifies: ADR-0028
    #[test]
    fn an_unreadable_floors_file_answers_every_floor_with_its_name() {
        let operator = keypair();
        for (what, content) in [
            ("unparsable", "{".to_string()),
            (
                "not SemVer",
                serde_json::json!({"otelcol": {"version": "1.19", "signature": "00"}}).to_string(),
            ),
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            std::fs::write(dir.path().join(FILE), &content).expect("write");
            let floors = file(dir.path(), &operator);
            for agent_type in ["otelcol", "opamp-fleet-supervisor"] {
                let why = floors.floor(agent_type).expect_err(what);
                assert!(why.contains(FILE), "{what}: {why}");
            }
            floors.take(Floors::from([(
                "otelcol".to_string(),
                signed(&operator, "otelcol", "2.0.0"),
            )]));
            assert!(floors.floor("otelcol").is_err(), "{what} stays refused");
            assert_eq!(
                std::fs::read_to_string(dir.path().join(FILE)).expect("read"),
                content,
                "{what}: the file is the operator's to repair"
            );
        }
    }

    /// A fetch that fails keeps the floors held; one that succeeds keeps what verifies; and a
    /// fetch is due again only once the last try is older than asked.
    /// Verifies: ADR-0028
    #[tokio::test]
    async fn a_failed_fetch_keeps_the_kept_floors() {
        opamp::tls::install_ring_provider();
        let operator = keypair();
        let served = std::sync::Arc::new(Mutex::new(Some(Floors::from([(
            "otelcol".to_string(),
            signed(&operator, "otelcol", "2.0.0"),
        )]))));
        let answer = served.clone();
        let app = axum::Router::new().route(
            PATH,
            axum::routing::get(move || {
                let answer = answer.clone();
                async move {
                    match answer.lock().expect("lock").clone() {
                        Some(floors) => axum::Json(floors).into_response(),
                        None => axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response(),
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let server = listener.local_addr().expect("addr");
        tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(crate::config_init::FILE_NAME);
        std::fs::write(&path, format!("endpoint = \"ws://{server}/v1/opamp\"\n")).expect("write");
        let config = ClientConfig::load(&path).expect("config");

        let floors = file(dir.path(), &operator);
        floors.update_older_than(&config, FRESH).await;
        assert_eq!(floors.floor("otelcol"), Ok(Some("2.0.0".to_string())));

        *served.lock().expect("lock") = None;
        floors.update(&config).await;
        assert_eq!(
            floors.floor("otelcol"),
            Ok(Some("2.0.0".to_string())),
            "a failed fetch keeps what is held"
        );

        *served.lock().expect("lock") = Some(Floors::from([(
            "otelcol".to_string(),
            signed(&operator, "otelcol", "3.0.0"),
        )]));
        floors.update_older_than(&config, FRESH).await;
        assert_eq!(
            floors.floor("otelcol"),
            Ok(Some("2.0.0".to_string())),
            "tried within the minute: not fetched again"
        );
        floors.update_older_than(&config, Duration::ZERO).await;
        assert_eq!(floors.floor("otelcol"), Ok(Some("3.0.0".to_string())));
    }
}
