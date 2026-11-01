//! A Server on PostgreSQL and S3 keeps nothing on its own disk
//! (ADR-draft-the-servers-state-and-its-stores clause 1): started from a directory it cannot write,
//! with no data directory, it serves, and writes no file. In the bucket it keeps the staging
//! objects of every live lease and deletes those of a lease that is no longer live, at startup and
//! whenever it takes over a lapsed lease (clause 16), and an upload it takes is staged under its
//! own lease and lands as one blob.
//!
//! The Server works in the schema that is its role's default, so the test makes a role of its own
//! with a schema of its name, which it drops afterwards. It runs against the database the
//! `FLEET_TEST_POSTGRES_*` variables name and the bucket the `FLEET_TEST_S3_*` ones name, and skips,
//! saying so, where either is not named. `scripts/check-store.sh postgres` runs it, and fails on a
//! skip.
#![cfg(unix)]

mod ports;

use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use futures_util::TryStreamExt as _;
use object_store::aws::{AmazonS3, AmazonS3Builder};
use object_store::path::Path as Key;
use object_store::{ObjectStore as _, ObjectStoreExt as _, PutPayload};

/// Kills the server on drop, so a failing assertion never leaks the process.
struct ServerUnderTest(Child);

impl Drop for ServerUnderTest {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn variable(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn random_hex() -> String {
    let mut random = [0u8; 8];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut random).expect("random");
    hex::encode(random)
}

/// The database the variables name, as its administrator, and a role of this test's own whose
/// default schema is the schema of its name; both are dropped with it.
struct Role {
    host: String,
    port: u16,
    dbname: String,
    user: String,
    password: String,
    ca_file: Option<PathBuf>,
    name: String,
}

impl Role {
    fn named() -> Option<Self> {
        let (Some(host), Some(dbname), Some(user), Some(password_file)) = (
            variable("FLEET_TEST_POSTGRES_HOST"),
            variable("FLEET_TEST_POSTGRES_DBNAME"),
            variable("FLEET_TEST_POSTGRES_USER"),
            variable("FLEET_TEST_POSTGRES_PASSWORD_FILE"),
        ) else {
            eprintln!(
                "skipped: no PostgreSQL is named — set FLEET_TEST_POSTGRES_HOST, \
                 FLEET_TEST_POSTGRES_PORT, FLEET_TEST_POSTGRES_DBNAME, FLEET_TEST_POSTGRES_USER and \
                 FLEET_TEST_POSTGRES_PASSWORD_FILE to run the test of a Server without a disk"
            );
            return None;
        };
        let role = Role {
            host,
            port: variable("FLEET_TEST_POSTGRES_PORT")
                .map_or(5432, |port| port.parse().expect("FLEET_TEST_POSTGRES_PORT")),
            dbname,
            user,
            password: std::fs::read_to_string(password_file)
                .expect("the password")
                .trim()
                .to_string(),
            ca_file: variable("FLEET_TEST_POSTGRES_CA_FILE").map(Into::into),
            name: format!("fleet_test_{}", random_hex()),
        };
        role.sql(&format!(
            "CREATE ROLE {name} LOGIN PASSWORD '{name}'; \
             CREATE SCHEMA {name} AUTHORIZATION {name};",
            name = role.name
        ));
        Some(role)
    }

    /// Runs `statements` as the administrator, in the role's schema.
    fn sql(&self, statements: &str) {
        self.query(statements, None);
    }

    /// How many rows `table` of the role's schema holds.
    fn count(&self, table: &str) -> i64 {
        self.query("", Some(format!("SELECT count(*) FROM {table}")))
            .expect("a count")
    }

    /// Runs `statements` as the administrator, in the role's schema, and then `count`, a query of
    /// one number, answering it.
    fn query(&self, statements: &str, count: Option<String>) -> Option<i64> {
        let mut pg = tokio_postgres::Config::new();
        pg.host(&self.host)
            .port(self.port)
            .dbname(&self.dbname)
            .user(&self.user)
            .password(&self.password);
        let statements = format!("SET search_path = {};{statements}", self.name);
        let roots = self.ca_file.as_ref().map(|path| {
            opamp::tls::root_store(&std::fs::read(path).expect("FLEET_TEST_POSTGRES_CA_FILE"))
                .expect("roots")
        });
        if roots.is_some() {
            pg.ssl_mode(tokio_postgres::config::SslMode::Require);
        }
        std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime")
                .block_on(async move {
                    let client = match roots {
                        Some(roots) => {
                            opamp::tls::install_ring_provider();
                            let tls = tokio_postgres_rustls::MakeRustlsConnect::new(
                                opamp::tls::client_builder()
                                    .with_root_certificates(roots)
                                    .with_no_client_auth(),
                            );
                            let (client, connection) = pg.connect(tls).await.expect("db");
                            tokio::spawn(connection);
                            client
                        }
                        None => {
                            let (client, connection) =
                                pg.connect(tokio_postgres::NoTls).await.expect("db");
                            tokio::spawn(connection);
                            client
                        }
                    };
                    client.batch_execute(&statements).await.expect("sql");
                    match count {
                        Some(query) => Some(
                            client
                                .query_one(query.as_str(), &[])
                                .await
                                .expect("count")
                                .get::<_, i64>(0),
                        ),
                        None => None,
                    }
                })
        })
        .join()
        .expect("the statements' thread")
    }

    /// `[database.postgres]` for the role, its password in a file under `dir`.
    fn section(&self, dir: &Path) -> String {
        let password_file = dir.join("database-password");
        std::fs::write(&password_file, &self.name).expect("password");
        std::fs::set_permissions(&password_file, std::fs::Permissions::from_mode(0o600))
            .expect("chmod");
        let tls = match &self.ca_file {
            Some(ca) => format!("ca_file = {:?}\n", ca.display().to_string()),
            None => "tls = false\n".to_string(),
        };
        format!(
            "[database.postgres]\nhost = {:?}\nport = {}\ndbname = {:?}\nuser = {:?}\n\
             password_file = {:?}\n{tls}",
            self.host,
            self.port,
            self.dbname,
            self.name,
            password_file.display().to_string(),
        )
    }
}

impl Drop for Role {
    fn drop(&mut self) {
        self.sql(&format!(
            "SET search_path = public; DROP SCHEMA IF EXISTS {name} CASCADE; \
             DROP ROLE IF EXISTS {name};",
            name = self.name
        ));
    }
}

/// The bucket the variables name, under a prefix of this test's own, reached past the Server.
struct Bucket {
    endpoint: String,
    bucket: String,
    region: String,
    prefix: String,
    access_key_id: Option<String>,
    secret_access_key_file: Option<String>,
    store: AmazonS3,
    runtime: tokio::runtime::Runtime,
}

impl Bucket {
    fn named() -> Option<Self> {
        let (Some(endpoint), Some(bucket)) = (
            variable("FLEET_TEST_S3_ENDPOINT"),
            variable("FLEET_TEST_S3_BUCKET"),
        ) else {
            eprintln!(
                "skipped: no S3 store is named — set FLEET_TEST_S3_ENDPOINT, FLEET_TEST_S3_BUCKET, \
                 FLEET_TEST_S3_ACCESS_KEY_ID and FLEET_TEST_S3_SECRET_ACCESS_KEY_FILE to run the \
                 test of a Server without a disk"
            );
            return None;
        };
        let region = variable("FLEET_TEST_S3_REGION").unwrap_or_else(|| "us-east-1".to_string());
        let access_key_id = variable("FLEET_TEST_S3_ACCESS_KEY_ID");
        let secret_access_key_file = variable("FLEET_TEST_S3_SECRET_ACCESS_KEY_FILE");
        let mut builder = AmazonS3Builder::new()
            .with_endpoint(&endpoint)
            .with_bucket_name(&bucket)
            .with_region(&region)
            .with_allow_http(endpoint.starts_with("http://"));
        if let (Some(id), Some(file)) = (&access_key_id, &secret_access_key_file) {
            builder = builder
                .with_access_key_id(id)
                .with_secret_access_key(std::fs::read_to_string(file).expect("the secret").trim());
        }
        Some(Bucket {
            endpoint,
            bucket,
            region,
            prefix: format!("test-{}/", random_hex()),
            access_key_id,
            secret_access_key_file,
            store: builder.build().expect("an S3 client"),
            runtime: tokio::runtime::Runtime::new().expect("runtime"),
        })
    }

    /// `[blobs]` for the bucket and the prefix.
    fn section(&self) -> String {
        let mut section = format!(
            "[blobs]\nstore = \"s3\"\n[blobs.s3]\nendpoint = {:?}\nbucket = {:?}\nregion = {:?}\n\
             prefix = {:?}\n",
            self.endpoint, self.bucket, self.region, self.prefix
        );
        if let (Some(id), Some(file)) = (&self.access_key_id, &self.secret_access_key_file) {
            section += &format!("access_key_id = {id:?}\nsecret_access_key_file = {file:?}\n");
        }
        section
    }

    /// Writes an object at `key` under the prefix: what an upload in progress leaves.
    fn plant(&self, key: &str) {
        let key = Key::parse(format!("{}{key}", self.prefix)).expect("key");
        self.runtime
            .block_on(self.store.put(&key, PutPayload::from_static(b"x")))
            .expect("plant");
    }

    /// Every key under the prefix.
    fn keys(&self) -> Vec<String> {
        let root = Key::from(self.prefix.trim_end_matches('/'));
        let mut keys: Vec<String> = self
            .runtime
            .block_on(self.store.list(Some(&root)).try_collect::<Vec<_>>())
            .expect("list")
            .into_iter()
            .map(|object| object.location.to_string())
            .collect();
        keys.sort();
        keys
    }
}

impl Drop for Bucket {
    fn drop(&mut self) {
        for key in self.keys() {
            let _ = self.runtime.block_on(self.store.delete(&Key::from(key)));
        }
    }
}

/// Every entry under `dir`, as paths relative to it.
fn tree(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).expect("read") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                pending.push(path.clone());
            }
            found.push(
                path.strip_prefix(dir)
                    .expect("under dir")
                    .display()
                    .to_string(),
            );
        }
    }
    found.sort();
    found
}

/// A random version 4 UUID, the shape of an instance id.
fn instance_id() -> String {
    let hex = format!("{}{}", random_hex(), random_hex());
    format!(
        "{}-{}-4{}-8{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[13..16],
        &hex[17..20],
        &hex[20..32]
    )
}

// Verifies: ADR-draft-the-servers-state-and-its-stores, ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
#[test]
fn a_server_on_postgresql_and_s3_serves_from_a_directory_it_cannot_write() {
    opamp::tls::install_ring_provider();
    let Some(role) = Role::named() else { return };
    let Some(bucket) = Bucket::named() else {
        return;
    };
    // An upload cut off under a lease that is gone, from before this Server starts.
    let gone = instance_id();
    bucket.plant(&format!("staging/{gone}/0123456789abcdef"));

    // Everything the Server reads, in a directory of its own; it runs from another that nobody may
    // write, as the image runs it from `/` on a read-only root filesystem.
    let files = tempfile::tempdir().expect("tempdir");
    let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()])
        .expect("a self-signed certificate");
    std::fs::write(files.path().join("cert.pem"), cert.cert.pem()).expect("write cert");
    std::fs::write(
        files.path().join("key.pem"),
        cert.signing_key.serialize_pem(),
    )
    .expect("write key");
    let (agent, operator) = (ports::spare_port(), ports::spare_port());
    let config = files.path().join("server.toml");
    std::fs::write(
        &config,
        format!(
            "listen = \"127.0.0.1:{agent}\"\n\
             [tls]\ncert_file = {cert:?}\nkey_file = {key:?}\nclient_ca_file = {cert:?}\n\
             [rest]\nlisten = \"127.0.0.1:{operator}\"\n\
             [database]\nstore = \"postgres\"\nlease_renew_secs = 1\nlease_secs = 3\n\
             {database}{blobs}",
            cert = files.path().join("cert.pem").display().to_string(),
            key = files.path().join("key.pem").display().to_string(),
            database = role.section(files.path()),
            blobs = bucket.section(),
        ),
    )
    .expect("write server.toml");
    let read = tree(files.path());
    let root = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o555)).expect("chmod");

    let mut server = ServerUnderTest(
        Command::new(env!("CARGO_BIN_EXE_server"))
            .arg("--config")
            .arg(&config)
            .current_dir(root.path())
            .env("RUST_LOG", "info")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start the server"),
    );
    let (lines, logged) = mpsc::channel();
    let stderr = server.0.stderr.take().expect("stderr");
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let mut seen = Vec::new();
    let came_up = loop {
        match logged.recv_timeout(Duration::from_secs(60)) {
            Ok(line) if line.contains("serving the REST API") => break true,
            Ok(line) => seen.push(line),
            Err(_) => break false,
        }
    };
    assert!(came_up, "the server never came up:\n{}", seen.join("\n"));
    assert!(
        std::net::TcpStream::connect_timeout(
            &([127, 0, 0, 1], agent).into(),
            Duration::from_secs(5)
        )
        .is_ok(),
        "the Agent plane takes a connection, as a probe sees it"
    );
    assert!(
        !bucket.keys().iter().any(|key| key.contains(&gone)),
        "the staging object of a lease that is gone is deleted at startup: {:?}",
        bucket.keys()
    );

    // A live lease of another instance and one that lapses: once this Server takes the lapsed one
    // over, its uploads go and the live one's stay.
    let (live, lapsing) = (instance_id(), instance_id());
    role.sql(&format!(
        "INSERT INTO leases (instance_id, host, must_be_alone, taken_at, renewed_at, expires_at) \
         VALUES ('{live}', 'elsewhere', false, now(), now(), now() + interval '1 hour'), \
         ('{lapsing}', 'gone', false, now(), now(), now());"
    ));
    bucket.plant(&format!("staging/{live}/0123456789abcdef"));
    bucket.plant(&format!("staging/{lapsing}/0123456789abcdef"));
    let deadline = Instant::now() + Duration::from_secs(30);
    while bucket.keys().iter().any(|key| key.contains(&lapsing)) {
        assert!(
            Instant::now() < deadline,
            "the staging object of the lease taken over is still there: {:?}",
            bucket.keys()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(
        bucket.keys(),
        [format!("{}staging/{live}/0123456789abcdef", bucket.prefix)],
        "the live lease's staging object stays"
    );

    // An upload through the operator plane is staged under this instance's lease, which it names
    // only after it opened the bucket, and lands as one blob.
    let package = format!("https://127.0.0.1:{operator}/api/v1/packages/otelcol/1.0.0");
    let artifact = b"an artifact the operator uploads".to_vec();
    let (created, uploaded) = bucket.runtime.block_on(async {
        let client = reqwest::Client::builder()
            .tls_certs_only([
                reqwest::Certificate::from_pem(cert.cert.pem().as_bytes()).expect("the root")
            ])
            .build()
            .expect("a client");
        let created = client
            .put(&package)
            .json(&serde_json::json!({}))
            .send()
            .await
            .expect("create the package");
        let uploaded = client
            .put(format!("{package}/entries/linux/amd64"))
            .body(artifact.clone())
            .send()
            .await
            .expect("upload the entry");
        (created.status(), uploaded.status())
    });
    assert_eq!(created, 200);
    assert_eq!(uploaded, 200, "the upload is taken under the lease");
    let sha256 = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&artifact));
    assert_eq!(
        bucket.keys(),
        [
            format!("{}blobs/{sha256}", bucket.prefix),
            format!("{}staging/{live}/0123456789abcdef", bucket.prefix),
        ],
        "the blob is stored, and the upload left no staging object"
    );

    drop(server);
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).expect("chmod");
    assert_eq!(
        tree(root.path()),
        Vec::<String>::new(),
        "nothing is written where it runs"
    );
    assert_eq!(
        tree(files.path()),
        read,
        "nothing is written beside what it reads"
    );
}

/// A bucket that fails at startup, unreachable or holding a key outside the store's layout, stops
/// the Server before it takes its lease: no lease row and no audit chain is left for another
/// instance to count as live or to close as lapsed
/// (ADR-draft-the-servers-state-and-its-stores clause 16,
/// ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
/// clause 2).
// Verifies: ADR-draft-the-servers-state-and-its-stores, ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
#[test]
fn a_bucket_that_fails_startup_leaves_no_lease_behind() {
    opamp::tls::install_ring_provider();
    let Some(role) = Role::named() else { return };
    let Some(bucket) = Bucket::named() else {
        return;
    };
    bucket.plant("otelcol.bin");
    let unreachable = format!(
        "[blobs]\nstore = \"s3\"\n[blobs.s3]\nendpoint = \"http://127.0.0.1:{}\"\n\
         bucket = \"fleet\"\nregion = \"us-east-1\"\n",
        ports::spare_port()
    );
    for (case, blobs, named) in [
        (
            "a stray key",
            bucket.section(),
            format!("{}otelcol.bin", bucket.prefix),
        ),
        ("an unreachable bucket", unreachable, "fleet".to_string()),
    ] {
        let files = tempfile::tempdir().expect("tempdir");
        let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()])
            .expect("a self-signed certificate");
        std::fs::write(files.path().join("cert.pem"), cert.cert.pem()).expect("write cert");
        std::fs::write(
            files.path().join("key.pem"),
            cert.signing_key.serialize_pem(),
        )
        .expect("write key");
        let config = files.path().join("server.toml");
        std::fs::write(
            &config,
            format!(
                "listen = \"127.0.0.1:{agent}\"\n\
                 [tls]\ncert_file = {cert:?}\nkey_file = {key:?}\nclient_ca_file = {cert:?}\n\
                 [rest]\nlisten = \"127.0.0.1:{operator}\"\n\
                 [database]\nstore = \"postgres\"\n{database}{blobs}",
                agent = ports::spare_port(),
                operator = ports::spare_port(),
                cert = files.path().join("cert.pem").display().to_string(),
                key = files.path().join("key.pem").display().to_string(),
                database = role.section(files.path()),
            ),
        )
        .expect("write server.toml");
        let output = Command::new(env!("CARGO_BIN_EXE_server"))
            .arg("--config")
            .arg(&config)
            .current_dir(files.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .expect("run the server");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "{case}: the start fails:\n{stderr}"
        );
        assert!(stderr.contains(&named), "{case}: names {named}:\n{stderr}");
        assert_eq!(role.count("leases"), 0, "{case}: no lease is left");
        assert_eq!(
            role.count("audit_chains"),
            0,
            "{case}: no audit chain is left"
        );
    }
}
