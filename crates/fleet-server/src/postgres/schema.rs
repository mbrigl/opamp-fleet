//! The Server's schema (ADR-0023 clause 22): embedded as numbered migrations, applied by the Server
//! itself at startup, each recorded in `fleet_schema` with the oldest Server version that
//! understands the schema it leaves.
//!
//! A migration only adds — tables, columns with defaults, indexes. A column or table is removed
//! only by a release after the one that stopped using it. Each migration names the oldest Server
//! that understands the schema it leaves, and a Server older than any of them refuses the schema.
//! The Server creates tables, indexes and `fleet_schema`, and nothing else: the database, the role,
//! its rights and its extensions are the operator's.

use std::cmp::Ordering;

use tokio_postgres::GenericClient;

/// One step of the schema.
#[derive(Clone, Copy, Debug)]
pub struct Migration {
    /// Its number, from 1, one more than the step before it.
    pub version: i32,
    /// The oldest Server version, `MAJOR.MINOR.PATCH`, that understands the schema once this
    /// step is applied.
    pub oldest_server: &'static str,
    /// What it creates.
    pub sql: &'static str,
}

/// The table that records every migration applied.
pub const SCHEMA_TABLE: &str = "fleet_schema";

/// Every table the migrations create, so that a database holding any of them is recognised as one
/// the Server already wrote ([`super::Database::server_tables`]).
pub const TABLES: [&str; 26] = [
    "configurations",
    "configuration_revisions",
    "deployments",
    "packages",
    "package_entries",
    "agents",
    "agent_assignment_configurations",
    "hosts",
    "certificates",
    "revocations",
    "bootstrap_certificates",
    "enrolment_window",
    "enrolment_requests",
    "package_floors",
    "leases",
    "host_bindings",
    "deployment_packages",
    "deployment_configurations",
    "revocation_list_version",
    "audit_chains",
    "audit_entries",
    "audit_archive",
    "blob_deletions",
    "agent_count",
    "enrolment_rejected",
    // Not tables of the records, but the Server's all the same.
    SCHEMA_TABLE,
];

/// The schema, in order.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        oldest_server: "0.6.1",
        sql: RECORDS,
    },
    // A table only added: the release before it runs on the schema after it, never reading it.
    Migration {
        version: 2,
        oldest_server: "0.6.1",
        sql: PACKAGE_FLOORS,
    },
    // Migrations 3 to 10 are the schema of 0.8.0, the release in which several instances share
    // a database, and name it. A Server of 0.7.1 holds no lease, keeps its audit record in files
    // and leaves the revocation list's version where it was, so it refuses the schema they leave
    // rather than serve beside the instances of 0.8.0
    // (ADR-0023 clause 22).
    //
    // A table only added.
    Migration {
        version: 3,
        oldest_server: "0.8.0",
        sql: LEASES,
    },
    // A table and an index only added, the table filled from the Hosts, whose arrays this release
    // still writes too.
    Migration {
        version: 4,
        oldest_server: "0.8.0",
        sql: HOST_BINDINGS,
    },
    // Two tables only added, filled from the Deployments' columns, which this release still
    // writes too, from the rows.
    Migration {
        version: 5,
        oldest_server: "0.8.0",
        sql: DEPLOYMENT_MEMBERS,
    },
    // A table only added.
    Migration {
        version: 6,
        oldest_server: "0.8.0",
        sql: REVOCATION_LIST_VERSION,
    },
    // Tables only added.
    Migration {
        version: 7,
        oldest_server: "0.8.0",
        sql: AUDIT_CHAINS,
    },
    // Two columns only added, with defaults.
    Migration {
        version: 8,
        oldest_server: "0.8.0",
        sql: AGENT_PRESENCE,
    },
    // Two columns only added, with defaults.
    Migration {
        version: 9,
        oldest_server: "0.8.0",
        sql: TAKES,
    },
    // A column only added, with a default, and an index.
    Migration {
        version: 10,
        oldest_server: "0.8.0",
        sql: AUDIT_RECORDED_AT,
    },
    // Columns only added, with defaults, and indexes. A Server of 0.8.0 writes none of them, and
    // the rows it writes are derived again before the list of Agents reads them, so it serves on
    // the schema they leave
    // (ADR-0031
    // clause 8).
    Migration {
        version: 11,
        oldest_server: "0.8.0",
        sql: AGENTS_DERIVED,
    },
    // A column only added, with a default. A Server of 0.8.0 writes no release into it, and every
    // lease taken before it is one of 0.8.0, which the default names
    // (ADR-0025
    // clause 16).
    Migration {
        version: 12,
        oldest_server: "0.8.0",
        sql: LEASE_RELEASE,
    },
    // A table only added. A Server of 0.8.0 reads and writes none of it, so its sweeps and uploads
    // are as before, beside the instances that mark
    // (ADR-0023 clause 14).
    Migration {
        version: 13,
        oldest_server: "0.8.0",
        sql: BLOB_DELETIONS,
    },
    // A table, its row and the triggers that keep it only added. A Server of 0.8.0 reads none of
    // it, and what it adds and removes is counted all the same
    // (ADR-0023 clause 22).
    Migration {
        version: 14,
        oldest_server: "0.8.0",
        sql: AGENT_COUNT,
    },
    // A table only added. A Server of 0.8.0 reads and writes none of it: it neither refuses nor
    // drops a rejected key, so beside the instances of this release a rejected key may wait again
    // through it and be approved
    // (ADR-0027
    // clause 16).
    Migration {
        version: 15,
        oldest_server: "0.8.0",
        sql: ENROLMENT_REJECTED,
    },
];

/// The rejected keys, each the id of the request it refuses, with when it was rejected
/// (ADR-0027
/// clause 16): written by a rejection under the queue's lock, read by every take under it and by
/// every approval, and deleted by no close or end of the window.
const ENROLMENT_REJECTED: &str = "
CREATE TABLE enrolment_rejected (
    key text PRIMARY KEY,
    rejected_ms bigint NOT NULL
);
";

/// The count of the Agents' records (ADR-0023 clause 22): one row, raised and lowered by the
/// triggers on `agents` in the statement that adds or removes rows, so the Agent-record ceiling is
/// decided from it and not from a count of every row. The triggers exist before the row is
/// filled, and creating them locks `agents` against every addition and removal until the
/// migration commits, so none is missed between the count and the triggers. Their functions are
/// pinned to the schema the migration runs in, by name rather than by the search path it runs
/// under, which may start with `"$user"`: a session of any role, whatever it names first, counts
/// in this schema.
const AGENT_COUNT: &str = "
CREATE TABLE agent_count (
    one boolean PRIMARY KEY DEFAULT true CHECK (one),
    agents bigint NOT NULL
);
CREATE FUNCTION agent_count_added() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE n bigint;
BEGIN
    SELECT count(*) INTO n FROM added;
    IF n > 0 THEN
        UPDATE agent_count SET agents = agents + n;
    END IF;
    RETURN NULL;
END
$$;
CREATE FUNCTION agent_count_removed() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE n bigint;
BEGIN
    SELECT count(*) INTO n FROM removed;
    IF n > 0 THEN
        UPDATE agent_count SET agents = agents - n;
    END IF;
    RETURN NULL;
END
$$;
DO $$
BEGIN
    EXECUTE format('ALTER FUNCTION agent_count_added() SET search_path = %I', current_schema());
    EXECUTE format('ALTER FUNCTION agent_count_removed() SET search_path = %I', current_schema());
END
$$;
CREATE TRIGGER agents_added AFTER INSERT ON agents
    REFERENCING NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION agent_count_added();
CREATE TRIGGER agents_removed AFTER DELETE ON agents
    REFERENCING OLD TABLE AS removed FOR EACH STATEMENT EXECUTE FUNCTION agent_count_removed();
INSERT INTO agent_count (agents) SELECT count(*) FROM agents;
";

/// The blobs a sweep marked before it deletes them (ADR-0023 clause 14): written on the session
/// that holds the blob store's lock, read by every upload in the transaction that checks its blob,
/// and taken away by the sweep that wrote them, `sweep`, or by any sweep once their `lease` is
/// not live.
const BLOB_DELETIONS: &str = "
CREATE TABLE blob_deletions (
    sha256 text PRIMARY KEY,
    lease text,
    sweep text NOT NULL,
    marked_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX blob_deletions_sweep ON blob_deletions (sweep);
";

/// The release of the instance that holds each lease
/// (ADR-0025
/// clause 16): what an instance compares with the oldest Server its migrations leave before it
/// migrates.
const LEASE_RELEASE: &str = "
ALTER TABLE leases ADD COLUMN release text NOT NULL DEFAULT '0.8.0';
";

/// What a lease of 0.8.0 records, which writes no release: the column's default.
pub const RELEASE_BEFORE_THE_RECORD: &str = "0.8.0";

/// The first schema: one table per kind of record, the parts of a record that are many in tables of
/// their own (ADR-0023 clauses 4 to 6, 9 to 11 and 21). Unsigned integers are kept as `bigint` with
/// their bits unchanged, so every value round-trips.
///
/// PostgreSQL's `text` and `jsonb` refuse a NUL character, which a file keeps. Text an Agent or a
/// CSR chose — an Agent's effective configuration, a certificate's subject — is kept as `bytea`,
/// its UTF-8 bytes unchanged, and every JSON document as `json`, which keeps a `\u0000` escape
/// where `jsonb` refuses it (the Server never queries inside one). Either way the same bytes
/// round-trip as with the filesystem store, and one Agent's report can never fail the writes of
/// others or a rollout. The other `text` columns hold names and identities the Server or an
/// operator gave, each checked before it is stored.
const RECORDS: &str = r"
CREATE TABLE configurations (
    name text PRIMARY KEY,
    body text NOT NULL,
    role text NOT NULL,
    service_name text NOT NULL
);
CREATE TABLE configuration_revisions (
    configuration text NOT NULL REFERENCES configurations (name) ON DELETE CASCADE,
    hash text NOT NULL,
    body text NOT NULL,
    role text NOT NULL,
    service_name text NOT NULL,
    PRIMARY KEY (configuration, hash)
);
CREATE TABLE deployments (
    name text PRIMARY KEY,
    selector json NOT NULL,
    packages json NOT NULL,
    configurations text[] NOT NULL
);
CREATE TABLE packages (
    agent_type text NOT NULL,
    version text NOT NULL,
    PRIMARY KEY (agent_type, version)
);
CREATE TABLE package_entries (
    agent_type text NOT NULL,
    version text NOT NULL,
    os text NOT NULL,
    arch text NOT NULL,
    sha256 text NOT NULL,
    size bigint NOT NULL,
    signature bytea,
    PRIMARY KEY (agent_type, version, os, arch),
    FOREIGN KEY (agent_type, version) REFERENCES packages ON DELETE CASCADE
);
CREATE TABLE agents (
    instance_uid text PRIMARY KEY,
    sequence_num bigint NOT NULL,
    capabilities bigint NOT NULL,
    transport text NOT NULL,
    last_seen_ms bigint NOT NULL,
    restart_pending boolean NOT NULL,
    effective_config bytea,
    description bytea,
    health bytea,
    remote_config_status bytea,
    connection_settings_status bytea,
    package_statuses bytea,
    available_components bytea,
    labels json NOT NULL,
    host text,
    deployment text,
    package_agent_type text,
    package_version text
);
CREATE TABLE agent_assignment_configurations (
    instance_uid text NOT NULL REFERENCES agents ON DELETE CASCADE,
    configuration text NOT NULL,
    revision text NOT NULL,
    PRIMARY KEY (instance_uid, configuration)
);
CREATE TABLE hosts (
    host text PRIMARY KEY,
    gateway boolean NOT NULL,
    instance_uids text[] NOT NULL,
    subject json NOT NULL,
    approved_ms bigint,
    valid_until_ms bigint
);
CREATE TABLE certificates (
    issuer text NOT NULL,
    serial text NOT NULL,
    issuer_name text NOT NULL,
    subject bytea NOT NULL,
    key_fingerprint text NOT NULL,
    not_after_ms bigint NOT NULL,
    host text,
    subject_values json NOT NULL,
    instance_uid text NOT NULL,
    predecessor_issuer text,
    predecessor_serial text,
    issued_ms bigint NOT NULL,
    PRIMARY KEY (issuer, serial)
);
CREATE TABLE revocations (
    id text PRIMARY KEY,
    position integer NOT NULL,
    revoked json NOT NULL,
    revoked_ms bigint NOT NULL
);
CREATE TABLE bootstrap_certificates (
    issuer text NOT NULL,
    id_serial text NOT NULL,
    serial text NOT NULL,
    subject text NOT NULL,
    key_fingerprint text NOT NULL,
    issued_ms bigint NOT NULL,
    not_after_ms bigint NOT NULL,
    ca_key text NOT NULL,
    p12 boolean NOT NULL,
    p12_file bytea,
    PRIMARY KEY (issuer, id_serial)
);
CREATE TABLE enrolment_window (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    until_ms bigint
);
CREATE TABLE enrolment_requests (
    id text PRIMARY KEY,
    position integer NOT NULL,
    request json NOT NULL
);
";

/// The floor of each Agent type (ADR-0035 clause 15): one row per type, the signature as its
/// bytes.
const PACKAGE_FLOORS: &str = r"
CREATE TABLE package_floors (
    agent_type text PRIMARY KEY,
    version text NOT NULL,
    signature bytea NOT NULL
);
";

/// The lease of every instance on this database
/// (ADR-0025
/// clauses 1 to 3). The times are the database's own, so every instance reads the same clock. A
/// lease whose `expires_at` has passed is lapsed, whatever its row still says.
const LEASES: &str = r"
CREATE TABLE leases (
    instance_id uuid PRIMARY KEY,
    host text NOT NULL,
    must_be_alone boolean NOT NULL,
    taken_at timestamptz NOT NULL,
    renewed_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL
);
";

/// Which Host each `instance_uid` is bound to: one row per binding, the `instance_uid` its key, so
/// no two Hosts hold one
/// (ADR-0025
/// clause 19). It is filled from the arrays of the Hosts not marked as Gateways, as a Gateway's
/// Host is bound none (ADR-0030 clause 16); an `instance_uid` two such Hosts' arrays hold fails the
/// migration, naming it and the Hosts, and changes nothing, for the operator to decide. A renewal's
/// walk down its chain finds an entry's renewals by the index on the predecessor.
const HOST_BINDINGS: &str = r"
DO $$
DECLARE taken record;
BEGIN
    SELECT uid, string_agg(host, ', ' ORDER BY host) AS holders INTO taken
        FROM hosts, unnest(instance_uids) AS uid
        WHERE NOT gateway
        GROUP BY uid HAVING count(*) > 1 ORDER BY uid LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'the instance_uid % is bound to the Hosts %; release it from all but one '
            'before this release starts', taken.uid, taken.holders;
    END IF;
END $$;
CREATE TABLE host_bindings (
    instance_uid text PRIMARY KEY,
    host text NOT NULL
);
INSERT INTO host_bindings (instance_uid, host)
    SELECT uid, host FROM hosts, unnest(instance_uids) AS uid WHERE NOT gateway;
CREATE INDEX certificates_predecessor ON certificates (predecessor_issuer, predecessor_serial);
";

/// The members of each Deployment, one row each
/// (ADR-0025
/// clause 19): its Packages keyed by Agent type, so it holds at most one of each, and its
/// Configurations by name. They are filled from the `packages` and `configurations` columns of
/// `deployments`, which stay and are written from these rows.
const DEPLOYMENT_MEMBERS: &str = r"
CREATE TABLE deployment_packages (
    deployment text NOT NULL REFERENCES deployments (name) ON DELETE CASCADE,
    agent_type text NOT NULL,
    version text NOT NULL,
    PRIMARY KEY (deployment, agent_type)
);
CREATE TABLE deployment_configurations (
    deployment text NOT NULL REFERENCES deployments (name) ON DELETE CASCADE,
    configuration text NOT NULL,
    PRIMARY KEY (deployment, configuration)
);
INSERT INTO deployment_packages (deployment, agent_type, version)
    SELECT name, package->>'agent_type', package->>'version'
        FROM deployments, json_array_elements(packages) AS package;
INSERT INTO deployment_configurations (deployment, configuration)
    SELECT DISTINCT name, configuration FROM deployments, unnest(configurations) AS configuration;
";

/// The revocation list's one version
/// (ADR-0023 clause 10): a counter in one row that the
/// transaction of every revocation added or widened and every lift increments. It is the `ETag`
/// of the Gateways' list on every instance, and what each lease renewal compares.
const REVOCATION_LIST_VERSION: &str = r"
CREATE TABLE revocation_list_version (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    version bigint NOT NULL
);
INSERT INTO revocation_list_version (singleton, version) VALUES (true, 0);
";

/// The audit record, one hash chain per lease
/// (ADR-0029): a record per
/// chain, named by the instance id of the lease that opened it, with the hash of its closing entry
/// once it is closed; each entry as the line it was hashed as; and the end of the file record a
/// move from files left as the archive, which the first chain names as its predecessor.
const AUDIT_CHAINS: &str = r"
CREATE TABLE audit_chains (
    id uuid PRIMARY KEY,
    host text NOT NULL,
    opened_at timestamptz NOT NULL,
    closed_at timestamptz,
    closed_hash text
);
CREATE TABLE audit_entries (
    chain uuid NOT NULL REFERENCES audit_chains (id),
    seq bigint NOT NULL,
    line text NOT NULL,
    PRIMARY KEY (chain, seq)
);
CREATE TABLE audit_archive (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    seq bigint NOT NULL,
    hash text NOT NULL
);
";

/// Where each Agent is connected (ADR-0030 clause 3): whether
/// a connection carries it, and the instance that holds that connection, by the id of its lease.
/// Each instance writes them for the Agents it carries in batches
/// (ADR-0030 clause 4); a holder without a live lease reads as
/// disconnected, whatever `connected` says (clause 6).
const AGENT_PRESENCE: &str = r"
ALTER TABLE agents ADD COLUMN connected boolean NOT NULL DEFAULT false;
ALTER TABLE agents ADD COLUMN holder uuid;
";

/// How each Agent was taken
/// (ADR-0025
/// clause 8): `takes` counts every take and every batch that names a holder anew, so an instance
/// writes a disconnect only where the row still holds the count it was answered, with no clock
/// compared; `holder_changes` holds when the holder changed, the latest eleven times, by the
/// database's clock, which tells a likely duplicate whose connections instances take in turn.
const TAKES: &str = r"
ALTER TABLE agents ADD COLUMN takes bigint NOT NULL DEFAULT 0;
ALTER TABLE agents ADD COLUMN holder_changes timestamptz[] NOT NULL DEFAULT '{}';
";

/// When each audit entry was stored, by the database's clock: what its age is reckoned from when the
/// record is pruned (ADR-0023 clause 12). Entries stored before
/// take the time their line carries, read as text so that a line that is no entry keeps the time of
/// the migration rather than stopping it.
const AUDIT_RECORDED_AT: &str = r#"
ALTER TABLE audit_entries ADD COLUMN recorded_at timestamptz NOT NULL DEFAULT now();
UPDATE audit_entries
    SET recorded_at = substring(line FROM '"time":"([0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:.]+Z)"')::timestamptz
    WHERE line ~ '"time":"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:.]+Z"';
CREATE INDEX audit_entries_recorded_at ON audit_entries (chain, recorded_at);
"#;

/// The digest of the columns of a row of `agents` its derived ones come from
/// (ADR-0031
/// clause 8): what a row whose derived columns were written with it holds in `derived_from`. Each
/// text column enters as its MD5, each `bytea` column length-prefixed, so two rows differ in the
/// digest wherever they differ in a column, and every function in it is immutable, so the partial
/// index of migration 11 can name it. The query that finds the rows to derive again names it
/// exactly as the index does, or the index is not used.
macro_rules! stamp {
    () => {
        "sha256('\\x01'::bytea \
    || decode(md5(instance_uid), 'hex') || decode(md5(transport), 'hex') \
    || int8send(capabilities) \
    || CASE WHEN description IS NULL THEN '\\x00'::bytea \
        ELSE '\\x01'::bytea || int4send(length(description)) || description END \
    || CASE WHEN health IS NULL THEN '\\x00'::bytea \
        ELSE '\\x01'::bytea || int4send(length(health)) || health END \
    || CASE WHEN remote_config_status IS NULL THEN '\\x00'::bytea \
        ELSE '\\x01'::bytea || int4send(length(remote_config_status)) || remote_config_status END \
    || CASE WHEN package_statuses IS NULL THEN '\\x00'::bytea \
        ELSE '\\x01'::bytea || int4send(length(package_statuses)) || package_statuses END)"
    };
}
pub(crate) use stamp;

/// What the list of Agents reads of each record
/// (ADR-0031
/// clause 8): the display name and the other sort keys, the reported values a Selector matches,
/// the `service_name`, the machine that groups an Agent behind a Gateway, whether it is its
/// Client's own Agent, one search text, and the digest of the columns they come from. A row whose
/// digest does not match them, as every row the migration finds, is derived again, and the partial
/// index finds those rows; the other index finds a group's Client Agents.
const AGENTS_DERIVED: &str = concat!(
    "
ALTER TABLE agents ADD COLUMN display_name text NOT NULL DEFAULT '';
ALTER TABLE agents ADD COLUMN health_rank bigint NOT NULL DEFAULT 1;
ALTER TABLE agents ADD COLUMN config_status text NOT NULL DEFAULT '';
ALTER TABLE agents ADD COLUMN version_shown text NOT NULL DEFAULT '';
ALTER TABLE agents ADD COLUMN os_shown text NOT NULL DEFAULT '';
ALTER TABLE agents ADD COLUMN service_name text NOT NULL DEFAULT '';
ALTER TABLE agents ADD COLUMN reported json NOT NULL DEFAULT '{}';
ALTER TABLE agents ADD COLUMN machine text NOT NULL DEFAULT '';
ALTER TABLE agents ADD COLUMN client_agent boolean NOT NULL DEFAULT false;
ALTER TABLE agents ADD COLUMN search_text text NOT NULL DEFAULT '';
ALTER TABLE agents ADD COLUMN derived_from bytea;
CREATE INDEX agents_host ON agents (host);
CREATE INDEX agents_client_agents ON agents (host, machine) WHERE client_agent;
CREATE INDEX agents_derived_missing ON agents (instance_uid) WHERE derived_from IS DISTINCT FROM ",
    stamp!(),
    ";
"
);

/// This Server's version, `MAJOR.MINOR.PATCH`: the base of the version it reports, so a
/// development build understands the schema of the release it is heading for.
#[must_use]
pub fn server_version() -> &'static str {
    let current = fleet_core::version::current();
    fleet_core::version::parse(current).map_or(current, |version| version.base)
}

/// The order of two `MAJOR.MINOR.PATCH` versions; one that does not parse is above every other,
/// so a schema naming it is never taken for one this Server understands.
fn compare(a: &str, b: &str) -> Ordering {
    fleet_core::version::precedence(a, b).unwrap_or_else(|| {
        match (
            fleet_core::version::is_semver(a),
            fleet_core::version::is_semver(b),
        ) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => a.cmp(b),
        }
    })
}

/// What [`apply`] found and did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// The migrations it applied, by number; empty when the schema was current.
    pub versions: Vec<i32>,
}

/// The newest of the oldest Server versions `fleet_schema` records, `None` when it records none
/// or does not exist.
async fn oldest_recorded(
    client: &impl GenericClient,
) -> Result<(std::collections::BTreeSet<i32>, Option<String>), String> {
    let exists: bool = client
        .query_one("SELECT to_regclass($1) IS NOT NULL", &[&SCHEMA_TABLE])
        .await
        .map_err(super::sql)?
        .get(0);
    let mut seen = std::collections::BTreeSet::new();
    let mut needs: Option<String> = None;
    if exists {
        let rows = client
            .query(
                &format!("SELECT version, oldest_server FROM {SCHEMA_TABLE}"),
                &[],
            )
            .await
            .map_err(super::sql)?;
        for row in rows {
            seen.insert(row.get::<_, i32>(0));
            needs = Some(newer(needs, row.get(1)));
        }
    }
    Ok((seen, needs))
}

/// The newer of `known`, if any, and `other`.
fn newer(known: Option<String>, other: String) -> String {
    match known {
        Some(known) if compare(&other, &known) != Ordering::Greater => known,
        _ => other,
    }
}

/// Why a Server of `ours` refuses a schema whose oldest Server is `needs`, naming both versions.
fn needs_newer(needs: &str, ours: &str) -> String {
    format!(
        "its schema needs Server {needs} or later, and this Server is {ours} — run a Server of \
         {needs} or later; this one changed nothing in the database"
    )
}

/// Refuses the schema when it names an oldest Server newer than `ours`, naming both versions: what
/// an instance checks under the migrations' lock before it migrates and in the transaction that
/// takes each lease
/// (ADR-0025
/// clause 16).
///
/// # Errors
/// Returns the refusal, or an error when a statement fails.
pub(crate) async fn check(client: &impl GenericClient, ours: &str) -> Result<(), String> {
    match oldest_recorded(client).await?.1 {
        Some(needs) if compare(&needs, ours) == Ordering::Greater => Err(needs_newer(&needs, ours)),
        _ => Ok(()),
    }
}

/// Whether `leases` records the release of each lease, as it does once migration 12 is applied.
///
/// # Errors
/// Returns an error when the statement fails.
async fn leases_record_releases(client: &impl GenericClient) -> Result<bool, String> {
    Ok(client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
             WHERE table_schema = current_schema() AND table_name = 'leases' \
             AND column_name = 'release')",
            &[],
        )
        .await
        .map_err(super::sql)?
        .get(0))
}

/// The live lease whose release is the oldest below `oldest`, as its instance id and release,
/// `None` when there is none. A lease of an instance that had to be alone counts as none: the
/// caller holds the exclusive lock or found it free, so that lease's process is gone, and the
/// next lease taken takes it over
/// (ADR-0025
/// clauses 2 and 16).
async fn lease_older_than(
    client: &impl GenericClient,
    oldest: &str,
) -> Result<Option<(String, String)>, String> {
    let exists: bool = client
        .query_one("SELECT to_regclass('leases') IS NOT NULL", &[])
        .await
        .map_err(super::sql)?
        .get(0);
    if !exists {
        return Ok(None);
    }
    // Before migration 12 no lease records its release, and every one is of 0.8.0.
    let rows = if leases_record_releases(client).await? {
        client
            .query(
                "SELECT instance_id::text, release FROM leases \
                 WHERE expires_at > now() AND NOT must_be_alone",
                &[],
            )
            .await
    } else {
        client
            .query(
                "SELECT instance_id::text, $1::text FROM leases \
                 WHERE expires_at > now() AND NOT must_be_alone",
                &[&RELEASE_BEFORE_THE_RECORD],
            )
            .await
    }
    .map_err(super::sql)?;
    let mut found: Option<(String, String)> = None;
    for row in rows {
        let (id, release): (String, String) = (row.get(0), row.get(1));
        if compare(&release, oldest) != Ordering::Less {
            continue;
        }
        if found
            .as_ref()
            .is_none_or(|(_, known)| compare(&release, known) == Ordering::Less)
        {
            found = Some((id, release));
        }
    }
    Ok(found)
}

/// Applies every migration of `migrations` the database has not yet seen, in order, inside the
/// transaction `client` holds, and records each. A schema whose oldest Server is above `ours` is
/// refused before anything is written, naming both versions, and so is every schema while a live
/// lease names a release older than the oldest Server the schema names once `migrations` are
/// applied, naming both releases, whether they are applied already or not
/// (ADR-0025
/// clause 16).
///
/// # Errors
/// Returns an error when the schema needs a newer Server, a live lease is of an older one, or a
/// statement fails.
pub async fn apply(
    client: &impl GenericClient,
    migrations: &[Migration],
    ours: &str,
) -> Result<Applied, String> {
    let (seen, needs) = oldest_recorded(client).await?;
    if let Some(needs) = needs
        .as_deref()
        .filter(|needs| compare(needs, ours) == Ordering::Greater)
    {
        return Err(needs_newer(needs, ours));
    }
    let leaves = migrations.iter().fold(needs, |known, migration| {
        Some(newer(known, migration.oldest_server.to_string()))
    });
    if let Some(leaves) = &leaves {
        if let Some((instance_id, release)) = lease_older_than(client, leaves).await? {
            return Err(format!(
                "the live lease {instance_id} is of Server {release}, and the schema needs Server \
                 {leaves} or later once this Server, {ours}, has applied its migrations — stop \
                 the instances of {release} first; this one changed nothing in the database"
            ));
        }
    }
    let exists: bool = client
        .query_one("SELECT to_regclass($1) IS NOT NULL", &[&SCHEMA_TABLE])
        .await
        .map_err(super::sql)?
        .get(0);
    if !exists {
        client
            .batch_execute(&format!(
                "CREATE TABLE {SCHEMA_TABLE} (
                     version integer PRIMARY KEY,
                     oldest_server text NOT NULL,
                     applied_at timestamptz NOT NULL DEFAULT now()
                 )"
            ))
            .await
            .map_err(super::sql)?;
    }
    let mut applied = Applied::default();
    for migration in migrations {
        if seen.contains(&migration.version) {
            continue;
        }
        client
            .batch_execute(migration.sql)
            .await
            .map_err(|e| format!("migration {}: {}", migration.version, super::sql(e)))?;
        client
            .execute(
                &format!("INSERT INTO {SCHEMA_TABLE} (version, oldest_server) VALUES ($1, $2)"),
                &[&migration.version, &migration.oldest_server],
            )
            .await
            .map_err(super::sql)?;
        applied.versions.push(migration.version);
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The migrations are numbered from 1 without a gap, and none names a Server newer than this
    /// one — a build that refused its own schema would never start. This release's own name 0.8.0.
    /// Verifies: ADR-0023
    /// Verifies: ADR-0025
    #[test]
    fn the_migrations_are_numbered_and_this_server_understands_them() {
        for (index, migration) in MIGRATIONS.iter().enumerate() {
            assert_eq!(
                migration.version,
                i32::try_from(index + 1).expect("few"),
                "numbered from 1 without a gap"
            );
            assert!(
                fleet_core::version::is_semver(migration.oldest_server),
                "{}",
                migration.oldest_server
            );
            assert_ne!(
                compare(migration.oldest_server, server_version()),
                Ordering::Greater,
                "migration {} needs a newer Server than this one",
                migration.version
            );
        }
        // The first release to run beside another serves beside 0.8.0: its migrations name it, so
        // the instances of 0.8.0 that still run serve on the schema they leave
        // (ADR-0025
        // clause 16).
        for migration in &MIGRATIONS[10..] {
            assert_eq!(
                migration.oldest_server, "0.8.0",
                "migration {} names 0.8.0",
                migration.version
            );
        }
        assert_eq!(
            MIGRATIONS.len(),
            15,
            "migrations 11 to 15 are this release's"
        );
        assert!(
            LEASE_RELEASE.contains(&format!("DEFAULT '{RELEASE_BEFORE_THE_RECORD}'")),
            "a lease of 0.8.0 records 0.8.0"
        );
        assert_eq!(compare("0.10.0", "0.9.0"), Ordering::Greater);
        assert_eq!(compare("not-a-version", "99.0.0"), Ordering::Greater);
    }

    /// A migration only adds (ADR-0023 clause 22): nothing in the embedded schema drops, renames or
    /// alters a column away, and every table it creates is one [`TABLES`] knows.
    /// Verifies: ADR-0023
    #[test]
    fn a_migration_only_adds() {
        for migration in MIGRATIONS {
            let sql = migration.sql.to_ascii_uppercase();
            for removing in [
                "DROP ",
                "RENAME ",
                "TRUNCATE ",
                "DELETE FROM",
                "ALTER COLUMN",
            ] {
                assert!(
                    !sql.contains(removing),
                    "migration {} removes: {removing}",
                    migration.version
                );
            }
            for created in sql.split("CREATE TABLE ").skip(1) {
                let name = created
                    .split_whitespace()
                    .next()
                    .expect("a name")
                    .to_ascii_lowercase();
                assert!(TABLES.contains(&name.as_str()), "{name} is not in TABLES");
            }
        }
    }
}
