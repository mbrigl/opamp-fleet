//! The list of Agents on PostgreSQL
//! (ADR-0031
//! clauses 5, 6, 8 and 9): the columns the core derives from each record and writes with it, the
//! rows written without them derived again, and the one query that filters, sorts and cuts a page.
//!
//! The labels an Agent inherits and its Host's values are joined at read time, from the row of
//! its Client Agent and from its Host, so the query places each Agent as the core places it in
//! memory (ADR-0030 clauses 18 and 19), and its search text is the one
//! [`crate::listing::haystack`] builds.

use std::collections::BTreeMap;

use super::Cached as GenericClient;
use opamp::uid::InstanceUid;
use tokio_postgres::types::ToSql;

use super::{client, signed, sql, unsigned, Database};
use crate::agent_store::{AgentPage, Holder, PageQuery, PagedAgent, StoredAgent};
use crate::listing::{AgentFilter, AgentSort, Derived, HolderFilter, Key, Position};

/// The digest of the columns a row's derived ones come from ([`super::schema::stamp`]).
pub(super) const STAMP: &str = super::schema::stamp!();

/// The derived columns, in the order [`derived_params`] gives their values.
pub(super) const DERIVED_COLUMNS: [&str; 10] = [
    "display_name",
    "health_rank",
    "config_status",
    "version_shown",
    "os_shown",
    "service_name",
    "reported",
    "machine",
    "client_agent",
    "search_text",
];

/// The values of [`DERIVED_COLUMNS`], to bind; the pairs as the `json` they are kept as.
pub(super) fn derived_params(derived: &Derived) -> Vec<Box<dyn ToSql + Sync + Send>> {
    vec![
        Box::new(derived.name.clone()),
        Box::new(derived.health),
        Box::new(derived.config.clone()),
        Box::new(derived.version.clone()),
        Box::new(derived.os.clone()),
        Box::new(derived.service_name.clone()),
        Box::new(serde_json::to_value(&derived.reported).expect("pairs serialize")),
        Box::new(derived.machine.clone()),
        Box::new(derived.client),
        Box::new(derived.search.clone()),
    ]
}

/// [`STAMP`] over the values a statement binds rather than over the row's columns, each column it
/// names mapped to the expression `of` gives it: what an `UPDATE` or an `INSERT` writes into
/// `derived_from` beside the columns it is the digest of, in the same statement.
pub(super) fn stamp_of(of: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(STAMP.len());
    let mut word = String::new();
    let mut quoted = false;
    for c in STAMP.chars() {
        if !quoted && (c.is_ascii_alphanumeric() || c == '_') {
            word.push(c);
            continue;
        }
        if !word.is_empty() {
            out.push_str(&of(&word).unwrap_or_else(|| word.clone()));
            word.clear();
        }
        if c == '\'' {
            quoted = !quoted;
        }
        out.push(c);
    }
    out.push_str(&of(&word).unwrap_or(word));
    out
}

/// How many rows one round of derivation reads and writes.
const DERIVED_BATCH: i64 = 500;

/// Writes the derived columns of the row of `key`, and the digest of the columns they come from,
/// inside the caller's transaction. With `only_at`, only while the row's digest is still that one:
/// a row written since keeps what its writer wrote.
pub(super) async fn write_derived(
    client: &impl GenericClient,
    key: &str,
    derived: &Derived,
    only_at: Option<&[u8]>,
) -> Result<bool, String> {
    let reported = serde_json::to_value(&derived.reported).expect("pairs serialize");
    let condition = if only_at.is_some() {
        format!(" AND {STAMP} = $12")
    } else {
        String::new()
    };
    let mut params: Vec<&(dyn ToSql + Sync)> = vec![
        &key,
        &derived.name,
        &derived.health,
        &derived.config,
        &derived.version,
        &derived.os,
        &derived.service_name,
        &reported,
        &derived.machine,
        &derived.client,
        &derived.search,
    ];
    if let Some(stamp) = &only_at {
        params.push(stamp);
    }
    Ok(client
        .execute(
            &format!(
                "UPDATE agents SET display_name = $2, health_rank = $3, config_status = $4, \
                 version_shown = $5, os_shown = $6, service_name = $7, reported = $8, \
                 machine = $9, client_agent = $10, search_text = $11, derived_from = {STAMP} \
                 WHERE instance_uid = $1{condition}"
            ),
            &params,
        )
        .await
        .map_err(sql)?
        > 0)
}

/// Derives again every row whose derived columns were not written with it — written before they
/// existed, or by a release that does not write them — in batches, each one transaction. A row
/// that does not decode fails, naming it.
pub(super) fn derive_missing(database: &Database) -> Result<(), String> {
    loop {
        let (read, written) = database.run(|pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            let rows = transaction
                .query(
                    &format!(
                        "SELECT instance_uid, description, health, remote_config_status, \
                         package_statuses, capabilities, transport, {STAMP} FROM agents \
                         WHERE derived_from IS DISTINCT FROM {STAMP} \
                         ORDER BY instance_uid LIMIT {DERIVED_BATCH}"
                    ),
                    &[],
                )
                .await
                .map_err(sql)?;
            let mut written = 0;
            for row in &rows {
                let key: String = row.get(0);
                let named = |e: String| format!("agent {key} in the database: {e}");
                let uid = InstanceUid::parse(&key)
                    .ok_or_else(|| named("not an Instance UID".to_string()))?;
                let description =
                    super::agents::decode(row.get(1), "description").map_err(named)?;
                let health = super::agents::decode(row.get(2), "health").map_err(named)?;
                let remote_config_status =
                    super::agents::decode(row.get(3), "remote_config_status").map_err(named)?;
                let package_statuses =
                    super::agents::decode(row.get(4), "package_statuses").map_err(named)?;
                let derived = crate::listing::derive(&crate::listing::Source {
                    uid: &uid,
                    description: description.as_ref(),
                    health: health.as_ref(),
                    remote_config_status: remote_config_status.as_ref(),
                    package_statuses: package_statuses.as_ref(),
                    capabilities: unsigned(row.get(5)),
                    transport: crate::fleet::Transport::parse(row.get(6)),
                });
                let stamp: Vec<u8> = row.get(7);
                if write_derived(&transaction, &key, &derived, Some(&stamp)).await? {
                    written += 1;
                }
            }
            transaction.commit().await.map_err(sql)?;
            Ok((rows.len(), written))
        })?;
        // A short batch was the last; one whose rows all changed under it is read again at the
        // next query rather than raced here.
        if read < DERIVED_BATCH as usize || written == 0 {
            return Ok(());
        }
    }
}

/// Every row of `agents` with what is joined at read time: whether a connection carries it, its
/// holder under a live lease, its Host, and the Client Agent of its group with the labels it
/// passes on (ADR-0030 clause 19), none in the `fleet.` or `supervisor.` space, which nothing
/// inherits (ADR-0030 clause 20). A group is a Host the Agent arrived over directly, or a
/// Gateway's Host and the machine the Agent reports.
const PLACED: &str = r#"
WITH g AS (
    SELECT a.instance_uid,
        CASE WHEN a.host IS NULL THEN NULL
            WHEN coalesce(h.gateway, false) THEN
                CASE WHEN a.machine = '' THEN NULL ELSE 'g' || a.host || E'\n' || a.machine END
            ELSE 'h' || a.host END AS grp
    FROM agents AS a LEFT JOIN hosts AS h ON h.host = a.host
),
c AS (
    SELECT g.grp, count(*) AS clients, min(a.instance_uid COLLATE "C") AS client_uid
    FROM g JOIN agents AS a ON a.instance_uid = g.instance_uid
    WHERE a.client_agent AND g.grp IS NOT NULL
    GROUP BY g.grp
),
v AS (
    SELECT a.*, h.gateway AS gateway, h.subject AS subject,
        (a.connected AND l.instance_id IS NOT NULL) AS live,
        l.instance_id::text AS holder_id, l.host AS holder_host,
        coalesce(c.clients, 0) AS clients, c.client_uid,
        CASE WHEN c.clients = 1 AND c.client_uid <> a.instance_uid THEN
            (SELECT coalesce(json_object_agg(e.key, e.value), '{}'::json)
             FROM json_each_text(ca.labels) AS e
             WHERE NOT starts_with(e.key, 'fleet.') AND NOT starts_with(e.key, 'supervisor.'))
        END AS inherited
    FROM agents AS a
    JOIN g ON g.instance_uid = a.instance_uid
    LEFT JOIN hosts AS h ON h.host = a.host
    LEFT JOIN leases AS l ON l.instance_id = a.holder AND l.expires_at > now()
    LEFT JOIN c ON c.grp = g.grp
    LEFT JOIN agents AS ca ON ca.instance_uid = c.client_uid AND c.clients = 1
)
"#;

/// The `fleet.host.*` keys and the field of a Host's subject values each is (ADR-0030 clause 18),
/// in the order of their keys.
const HOST_KEYS: [(&str, &str); 6] = [
    ("fleet.host.country", "country"),
    ("fleet.host.locality", "locality"),
    ("fleet.host.organization", "organization"),
    ("fleet.host.organization_id", "organization_identifier"),
    ("fleet.host.site", "organizational_unit"),
    ("fleet.host.state", "state"),
];

/// A label map's pairs, one per line, lowercased, in the order of their keys.
fn pairs(map: &str) -> String {
    format!(
        "coalesce((SELECT string_agg(lower(e.key || '=' || e.value), E'\\n' \
         ORDER BY e.key COLLATE \"C\") FROM json_each_text({map}) AS e), '')"
    )
}

/// The query's filters, built with their parameters.
struct Conditions {
    clauses: Vec<String>,
    params: Vec<Box<dyn ToSql + Sync + Send>>,
}

impl Conditions {
    /// Names the next parameter.
    fn bind(&mut self, value: impl ToSql + Sync + Send + 'static) -> String {
        self.params.push(Box::new(value));
        format!("${}", self.params.len())
    }

    fn sql(&self) -> String {
        if self.clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", self.clauses.join(" AND "))
        }
    }
}

/// The filters of `query` as the conditions of a query over `v`.
fn conditions(query: &PageQuery<'_>) -> Conditions {
    let mut c = Conditions {
        clauses: Vec::new(),
        params: Vec::new(),
    };
    let filter = query.filter;
    // Bound only where a filter reads it: a parameter no clause names has no type.
    let stale = if filter.stale.is_some() || filter.q.is_some() {
        let before = c.bind(signed(query.seen_before_ms));
        format!(
            "((v.capabilities & {}) <> 0 AND v.last_seen_ms < {before})",
            opamp::proto::AgentCapabilities::ReportsHeartbeat as i64
        )
    } else {
        String::new()
    };
    if let Some(connected) = filter.connected {
        let p = c.bind(connected);
        c.clauses.push(format!("v.live = {p}"));
    }
    if let Some(wanted) = filter.stale {
        let p = c.bind(wanted);
        c.clauses.push(format!("{stale} = {p}"));
    }
    if let Some(name) = &filter.service_name {
        let p = c.bind(name.clone());
        c.clauses.push(format!("v.service_name = {p}"));
    }
    match &filter.holder {
        Some(HolderFilter::Instance(id)) => {
            let p = c.bind(id.clone());
            c.clauses.push(format!("v.holder_id = {p}"));
        }
        Some(HolderFilter::None) => c.clauses.push("v.holder_id IS NULL".to_string()),
        None => {}
    }
    if let Some(host) = &filter.arrived_over {
        let p = c.bind(host.clone());
        c.clauses.push(format!("v.host = {p}"));
    }
    for (key, value) in &filter.selector {
        let v = c.bind(value.clone());
        if key.starts_with(crate::labels::RESERVED_PREFIX) {
            match HOST_KEYS.iter().find(|(name, _)| name == key) {
                Some((_, field)) => c.clauses.push(format!(
                    "(NOT coalesce(v.gateway, false) AND v.subject ->> '{field}' = {v})"
                )),
                None => c.clauses.push("false".to_string()),
            }
            continue;
        }
        let k = c.bind(key.clone());
        // A label in the Supervisor's space matches nothing, so only what is reported can
        // (ADR-0030 clause 20).
        if key.starts_with(opamp::attributes::SUPERVISOR_PREFIX) {
            c.clauses
                .push(format!("coalesce(v.reported ->> {k} = {v}, false)"));
            continue;
        }
        c.clauses.push(format!(
            "coalesce(CASE WHEN v.reported -> {k} IS NOT NULL THEN v.reported ->> {k} = {v} \
             ELSE coalesce(v.labels ->> {k}, v.inherited ->> {k}) = {v} END, false)"
        ));
    }
    if let Some(q) = &filter.q {
        let host = HOST_KEYS
            .iter()
            .map(|(name, field)| {
                format!(
                    "CASE WHEN v.subject ->> '{field}' <> '' \
                     THEN lower('{name}=' || (v.subject ->> '{field}')) END"
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let p = c.bind(q.clone());
        c.clauses.push(format!(
            "strpos(v.search_text || E'\\n' \
             || CASE WHEN v.live THEN 'connected' ELSE 'disconnected' END || E'\\n' \
             || CASE WHEN {stale} THEN 'stale' ELSE '' END || E'\\n' \
             || {} || E'\\n' || {} || E'\\n' \
             || CASE WHEN coalesce(v.gateway, false) THEN '' ELSE concat_ws(E'\\n', {host}) END, \
             {p}) > 0",
            pairs("v.labels"),
            pairs("v.inherited"),
        ));
    }
    c
}

/// The distinct reported pairs and types, as
/// [`AgentStore::reported_values`](crate::agent_store::AgentStore::reported_values) answers them.
pub(super) fn reported_values(
    database: &Database,
    limit: usize,
) -> Result<crate::agent_store::ReportedValues, String> {
    derive_missing(database)?;
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    database.run(move |pool| async move {
        let client = client(&pool).await?;
        let pairs = client
            .query(
                "SELECT DISTINCT (e.key || '=' || e.value) COLLATE \"C\" AS pair \
                 FROM agents AS a, json_each_text(a.reported) AS e \
                 WHERE e.value IS NOT NULL ORDER BY pair LIMIT $1",
                &[&limit],
            )
            .await
            .map_err(sql)?
            .iter()
            .map(|row| row.get(0))
            .collect();
        let service_names = client
            .query(
                "SELECT DISTINCT service_name COLLATE \"C\" AS name FROM agents \
                 WHERE service_name <> '' ORDER BY name LIMIT $1",
                &[&limit],
            )
            .await
            .map_err(sql)?
            .iter()
            .map(|row| row.get(0))
            .collect();
        Ok(crate::agent_store::ReportedValues {
            pairs,
            service_names,
        })
    })
}

/// The column a sort orders by, and whether it is text, compared byte by byte.
fn sort_column(sort: AgentSort) -> (&'static str, bool) {
    match sort {
        AgentSort::Name => ("v.display_name", true),
        AgentSort::Connected => ("v.live::int::bigint", false),
        AgentSort::Health => ("v.health_rank", false),
        AgentSort::Config => ("v.config_status", true),
        AgentSort::Version => ("v.version_shown", true),
        AgentSort::Os => ("v.os_shown", true),
        AgentSort::Seq => ("v.sequence_num", false),
        AgentSort::Seen => ("v.last_seen_ms", false),
    }
}

/// One page, as [`AgentStore::query_page`](crate::agent_store::AgentStore::query_page) answers it:
/// the rows derived again where they need it, then the count and the page in one snapshot, then
/// the page's records.
pub(super) fn query_page(database: &Database, query: &PageQuery<'_>) -> Result<AgentPage, String> {
    if let Some(host) = arrived_over_alone(query) {
        return arrived_over(database, query, host);
    }
    derive_missing(database)?;
    let mut c = conditions(query);
    let count_sql = format!("{PLACED} SELECT count(*) FROM v{}", c.sql());
    let (column, text) = sort_column(query.sort);
    let sorted = if text {
        format!("{column} COLLATE \"C\"")
    } else {
        column.to_string()
    };
    let column = sorted;
    let count_params = c.params.len();
    if let Some(after) = query.after {
        let key = match &after.key {
            Key::Text(text) => c.bind(text.clone()),
            Key::Int(int) => c.bind(*int),
        };
        let tiebreak = c.bind(after.tiebreak.clone());
        let beyond = if query.descending { "<" } else { ">" };
        c.clauses.push(format!(
            "({column} {beyond} {key} OR ({column} = {key} \
             AND v.instance_uid COLLATE \"C\" > {tiebreak}))"
        ));
    }
    let limit = c.bind(i64::try_from(query.limit).unwrap_or(i64::MAX));
    let page_sql = format!(
        "{PLACED} SELECT v.instance_uid, v.live, v.holder_id, v.holder_host, v.clients, \
         v.client_uid, v.inherited::text, {column} FROM v{} ORDER BY {column} {}, \
         v.instance_uid COLLATE \"C\" LIMIT {limit}",
        c.sql(),
        if query.descending { "DESC" } else { "ASC" },
    );
    let params = c.params;
    let (total, rows, records) = database.run(move |pool| async move {
        let mut client = client(&pool).await?;
        let transaction = client
            .build_transaction()
            .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
            .read_only(true)
            .start()
            .await
            .map_err(sql)?;
        let refs: Vec<&(dyn ToSql + Sync)> = params
            .iter()
            .map(|p| p.as_ref() as &(dyn ToSql + Sync))
            .collect();
        let total: i64 = transaction
            .query_one(&count_sql, &refs[..count_params])
            .await
            .map_err(sql)?
            .get(0);
        let rows = transaction.query(&page_sql, &refs).await.map_err(sql)?;
        let keys: Vec<String> = rows.iter().map(|row| row.get(0)).collect();
        let records = super::agents::read(&transaction, Some(&keys)).await?;
        transaction.commit().await.map_err(sql)?;
        Ok((total, rows, records))
    })?;
    let mut records = records;
    let mut agents = Vec::with_capacity(rows.len());
    for row in rows {
        let key: String = row.get(0);
        let named = |e: String| format!("agent {key} in the database: {e}");
        let uid =
            InstanceUid::parse(&key).ok_or_else(|| named("not an Instance UID".to_string()))?;
        let Some(record) = records.remove(&uid) else {
            return Err(named("listed, and not read".to_string()));
        };
        let holder = row.get::<_, Option<String>>(2).map(|instance_id| Holder {
            instance_id,
            host: row.get::<_, Option<String>>(3).unwrap_or_default(),
        });
        let clients: i64 = row.get(4);
        let client_agent = match (clients, row.get::<_, Option<String>>(5)) {
            (1, Some(client)) => Some(
                InstanceUid::parse(&client)
                    .ok_or_else(|| named(format!("client agent {client} is no Instance UID")))?,
            ),
            _ => None,
        };
        let inherited: BTreeMap<String, String> = match row.get::<_, Option<String>>(6) {
            Some(labels) => serde_json::from_str(&labels)
                .map_err(|e| named(format!("inherited labels: {e}")))?,
            None => BTreeMap::new(),
        };
        // The cursor continues after what the database sorted by, not after a value derived again.
        let position = Position {
            key: if text {
                Key::Text(row.get(7))
            } else {
                Key::Int(row.get(7))
            },
            tiebreak: key.clone(),
        };
        agents.push(PagedAgent {
            uid,
            stored: StoredAgent {
                connected: row.get(1),
                holder,
                record,
            },
            client_agent,
            ambiguous: clients > 1,
            inherited,
            position,
        });
    }
    Ok(AgentPage {
        agents,
        total: usize::try_from(total).unwrap_or(usize::MAX),
    })
}

/// The Host a query asks for alone, with no other filter: the read the Server makes of the Agents
/// behind a Gateway (clause 9), answered without the derived columns.
fn arrived_over_alone<'a>(query: &PageQuery<'a>) -> Option<&'a str> {
    let host = query.filter.arrived_over.as_deref()?;
    let alone = AgentFilter {
        arrived_over: Some(host.to_string()),
        ..AgentFilter::default()
    };
    (*query.filter == alone).then_some(host)
}

/// The Agents that last arrived over `host`, read by their `host` alone, in one snapshot with
/// their presence and whether the Host is a Gateway, and placed among themselves: every Agent of a
/// group arrived over the same Host. No derived column is read, so nothing is derived again, and
/// the read touches no row of another Host.
fn arrived_over(
    database: &Database,
    query: &PageQuery<'_>,
    host: &str,
) -> Result<AgentPage, String> {
    let host = host.to_string();
    let (records, presence, gateway) = database.run(move |pool| async move {
        let mut client = client(&pool).await?;
        let transaction = client
            .build_transaction()
            .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
            .read_only(true)
            .start()
            .await
            .map_err(sql)?;
        let presence: Vec<(String, bool, Option<String>, Option<String>)> = transaction
            .query(
                "SELECT a.instance_uid, a.connected AND l.instance_id IS NOT NULL, \
                 l.instance_id::text, l.host FROM agents AS a LEFT JOIN leases AS l \
                 ON l.instance_id = a.holder AND l.expires_at > now() WHERE a.host = $1",
                &[&host],
            )
            .await
            .map_err(sql)?
            .iter()
            .map(|row| (row.get(0), row.get(1), row.get(2), row.get(3)))
            .collect();
        let keys: Vec<String> = presence.iter().map(|(key, ..)| key.clone()).collect();
        let records = super::agents::read(&transaction, Some(&keys)).await?;
        let gateway: bool = transaction
            .query_opt("SELECT gateway FROM hosts WHERE host = $1", &[&host])
            .await
            .map_err(sql)?
            .is_some_and(|row| row.get(0));
        transaction.commit().await.map_err(sql)?;
        Ok((records, presence, gateway))
    })?;
    let mut records = records;
    let mut found = Vec::with_capacity(presence.len());
    for (key, connected, holder, holder_host) in presence {
        let uid = InstanceUid::parse(&key)
            .ok_or_else(|| format!("agent {key} in the database: not an Instance UID"))?;
        // A row gone between the two reads of one snapshot cannot be; one is still not invented.
        let Some(record) = records.remove(&uid) else {
            continue;
        };
        let holder = holder.map(|instance_id| Holder {
            instance_id,
            host: holder_host.unwrap_or_default(),
        });
        found.push((
            uid,
            StoredAgent {
                record,
                connected,
                holder,
            },
        ));
    }
    let placed = crate::listing::place_on_host(
        gateway,
        &found
            .iter()
            .map(|(uid, stored)| {
                (
                    *uid,
                    stored.record.description.as_ref(),
                    &stored.record.labels,
                )
            })
            .collect::<Vec<_>>(),
    );
    let mut placed = placed;
    let entries = found
        .into_iter()
        .map(|(uid, stored)| {
            let derived =
                crate::listing::derive(&crate::agent_store::source_of(&uid, &stored.record));
            let position = Position {
                key: crate::listing::agent_key(
                    query.sort,
                    &derived,
                    stored.connected,
                    stored.record.sequence_num,
                    stored.record.last_seen_ms,
                ),
                tiebreak: uid.to_string(),
            };
            let (client_agent, ambiguous, inherited) = placed.remove(&uid).unwrap_or_default();
            (
                position.clone(),
                PagedAgent {
                    uid,
                    stored,
                    client_agent,
                    ambiguous,
                    inherited,
                    position,
                },
            )
        })
        .collect();
    let page = crate::listing::page(entries, query.descending, query.after, query.limit);
    Ok(AgentPage {
        agents: page.entries,
        total: page.total,
    })
}
