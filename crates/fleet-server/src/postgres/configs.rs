//! Configurations in PostgreSQL (ADR-0029 clause 5): the PostgreSQL adapter behind
//! [`ConfigBackend`](crate::configs::ConfigBackend) — one row per Configuration, its retained
//! revisions in rows of their own, each call one transaction on the rows it names
//! (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
//! clause 19).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tokio_postgres::GenericClient;

use super::{client, sql, Database};
use crate::configs::{check_type, validate_name, ConfigBackend, Configuration, Pinned, Revision};

/// The Configurations in `configurations` and `configuration_revisions`.
pub struct PgConfigBackend {
    database: Arc<Database>,
}

impl PgConfigBackend {
    /// The adapter on `database`.
    #[must_use]
    pub fn new(database: Arc<Database>) -> Self {
        PgConfigBackend { database }
    }
}

impl ConfigBackend for PgConfigBackend {
    fn load(&self) -> Result<Vec<Configuration>, String> {
        self.database.run(|pool| async move {
            let client = client(&pool).await?;
            read_all(&**client).await
        })
    }

    /// The Configuration's row alone: its retained revisions are rows of their own, which this
    /// call names none of.
    fn save(&self, name: &str, saved: &Revision) -> Result<(), String> {
        let (name, saved) = (name.to_string(), saved.clone());
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            save(&*transaction, &name, &saved).await?;
            transaction.commit().await.map_err(sql)
        })
    }

    /// One transaction: the Configuration's row is held `FOR UPDATE`, which waits for every
    /// rollout that holds it to pin one of its revisions and keeps every other out until this one
    /// commits; then the revisions neither `referenced` nor a stored assignment names are deleted,
    /// read from what the database holds once the hold is had, and the ones left are answered.
    fn collect(
        &self,
        name: &str,
        referenced: &BTreeSet<String>,
    ) -> Result<BTreeMap<String, Revision>, String> {
        let name = name.to_string();
        let referenced: Vec<String> = referenced.iter().cloned().collect();
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            let held = transaction
                .query_opt(
                    "SELECT 1 FROM configurations WHERE name = $1 FOR UPDATE",
                    &[&name],
                )
                .await
                .map_err(sql)?;
            if held.is_none() {
                transaction.commit().await.map_err(sql)?;
                return Ok(BTreeMap::new());
            }
            transaction
                .execute(
                    "DELETE FROM configuration_revisions r WHERE r.configuration = $1 \
                     AND NOT r.hash = ANY($2) AND NOT EXISTS (SELECT 1 FROM \
                     agent_assignment_configurations a WHERE a.configuration = r.configuration \
                     AND a.revision = r.hash)",
                    &[&name, &referenced],
                )
                .await
                .map_err(sql)?;
            let mut kept = BTreeMap::new();
            for row in transaction
                .query(
                    "SELECT hash, body, role, service_name FROM configuration_revisions \
                     WHERE configuration = $1",
                    &[&name],
                )
                .await
                .map_err(sql)?
            {
                kept.insert(
                    row.get(0),
                    Revision {
                        body: row.get(1),
                        role: row.get(2),
                        service_name: row.get(3),
                    },
                );
            }
            transaction.commit().await.map_err(sql)?;
            Ok(kept)
        })
    }

    /// The Configuration and its revisions go in one statement, the revisions by cascade.
    fn remove(&self, name: &str) -> Result<(), String> {
        let name = name.to_string();
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            transaction
                .execute("DELETE FROM configurations WHERE name = $1", &[&name])
                .await
                .map_err(sql)?;
            transaction.commit().await.map_err(sql)
        })
    }
}

/// Every Configuration with its retained revisions. A row that does not decode — a name outside
/// the grammar, a saved revision without its Agent type — fails, naming it.
pub(crate) async fn read_all(client: &impl GenericClient) -> Result<Vec<Configuration>, String> {
    let mut configs: BTreeMap<String, Configuration> = BTreeMap::new();
    for row in client
        .query(
            "SELECT name, body, role, service_name FROM configurations ORDER BY name",
            &[],
        )
        .await
        .map_err(sql)?
    {
        let name: String = row.get(0);
        let named = |e: String| format!("configuration {name:?} in the database: {e}");
        validate_name(&name).map_err(named)?;
        let saved = Revision {
            body: row.get(1),
            role: row.get(2),
            service_name: row.get(3),
        };
        check_type(&saved).map_err(|e| {
            named(format!(
                "{e} — save it again with the Agent type it is written for"
            ))
        })?;
        configs.insert(
            name.clone(),
            Configuration {
                name,
                saved,
                retained: BTreeMap::new(),
            },
        );
    }
    for row in client
        .query(
            "SELECT configuration, hash, body, role, service_name FROM configuration_revisions",
            &[],
        )
        .await
        .map_err(sql)?
    {
        let name: String = row.get(0);
        let config = configs.get_mut(&name).ok_or_else(|| {
            format!("a revision in the database names configuration {name:?}, which is not there")
        })?;
        config.retained.insert(
            row.get(1),
            Revision {
                body: row.get(2),
                role: row.get(3),
                service_name: row.get(4),
            },
        );
    }
    Ok(configs.into_values().collect())
}

/// Creates or replaces one Configuration's row, its saved revision, inside the caller's
/// transaction.
async fn save(client: &impl GenericClient, name: &str, saved: &Revision) -> Result<(), String> {
    client
        .execute(
            "INSERT INTO configurations (name, body, role, service_name) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (name) DO UPDATE SET body = EXCLUDED.body, role = EXCLUDED.role, \
             service_name = EXCLUDED.service_name",
            &[&name, &saved.body, &saved.role, &saved.service_name],
        )
        .await
        .map_err(sql)?;
    Ok(())
}

/// Adds each pinned revision to its Configuration by its hash, inside a rollout's transaction
/// (ADR-0029 clause 21), and deletes none; a revision already retained is left as it is, as its
/// hash names its content.
/// Each Configuration's row is held `FOR KEY SHARE`, in name order, until the transaction ends:
/// a collection's `FOR UPDATE` waits for it, so no instance collects a revision this transaction
/// pins before its assignments are committed, while rollouts and saves of the Configuration go on
/// beside it. A Configuration that is not stored is an error.
pub(crate) async fn retain(client: &impl GenericClient, pinned: &[Pinned]) -> Result<(), String> {
    let mut pinned: Vec<&Pinned> = pinned.iter().collect();
    pinned.sort_by(|a, b| a.configuration.cmp(&b.configuration));
    for pinned in pinned {
        if client
            .query_opt(
                "SELECT 1 FROM configurations WHERE name = $1 FOR KEY SHARE",
                &[&pinned.configuration],
            )
            .await
            .map_err(sql)?
            .is_none()
        {
            return Err(format!("no configuration {:?}", pinned.configuration));
        }
        client
            .execute(
                "INSERT INTO configuration_revisions (configuration, hash, body, role, \
                 service_name) VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
                &[
                    &pinned.configuration,
                    &pinned.hash,
                    &pinned.revision.body,
                    &pinned.revision.role,
                    &pinned.revision.service_name,
                ],
            )
            .await
            .map_err(sql)?;
    }
    Ok(())
}

/// Creates or replaces one Configuration and makes its retained revisions exactly `config`'s,
/// inside the caller's transaction — the write of an import into a database no instance serves.
pub(crate) async fn write(
    client: &impl GenericClient,
    config: &Configuration,
) -> Result<(), String> {
    save(client, &config.name, &config.saved).await?;
    let kept: Vec<&str> = config.retained.keys().map(String::as_str).collect();
    client
        .execute(
            "DELETE FROM configuration_revisions WHERE configuration = $1 AND NOT hash = ANY($2)",
            &[&config.name, &kept],
        )
        .await
        .map_err(sql)?;
    for (hash, revision) in &config.retained {
        client
            .execute(
                "INSERT INTO configuration_revisions (configuration, hash, body, role, \
                 service_name) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (configuration, hash) DO \
                 UPDATE SET body = EXCLUDED.body, role = EXCLUDED.role, \
                 service_name = EXCLUDED.service_name",
                &[
                    &config.name,
                    hash,
                    &revision.body,
                    &revision.role,
                    &revision.service_name,
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
    use crate::agent_store::AgentStore;
    use crate::configs::revision_hash;
    use crate::fs::FsAgentStore;
    use crate::fs::FsConfigBackend;
    use crate::postgres::agents::PgAgentStore;
    use crate::postgres::testing::TestDatabase;

    /// The Configurations and the Agent store whose rollouts add their revisions, on one store.
    type Open<'a> = &'a dyn Fn() -> (Box<dyn ConfigBackend>, Box<dyn AgentStore>);

    /// Adds `pinned` as a rollout that writes no record does.
    fn release(agents: &dyn AgentStore, pinned: &Pinned) -> Result<(), String> {
        agents.release(std::slice::from_ref(pinned), &[])
    }

    fn revision(body: &str) -> Revision {
        Revision {
            body: body.to_string(),
            role: String::new(),
            service_name: "otelcol".to_string(),
        }
    }

    fn configuration(name: &str, saved: &str, retained: &[&str]) -> Configuration {
        Configuration {
            name: name.to_string(),
            saved: revision(saved),
            retained: retained
                .iter()
                .map(|body| (revision_hash(&revision(body)), revision(body)))
                .collect(),
        }
    }

    fn pinned(name: &str, body: &str) -> Pinned {
        Pinned {
            configuration: name.to_string(),
            hash: revision_hash(&revision(body)),
            revision: revision(body),
        }
    }

    /// The port's contract, whatever keeps the records: what is written is loaded back by another
    /// adapter on the same store; a save writes the saved revision and touches no retained one;
    /// revisions are added by hash and collected down to what is referenced, each call naming
    /// the parts it writes; and a removal takes the revisions with it and tolerates absence.
    fn contract(open: Open<'_>) {
        let (store, agents) = open();
        assert!(store.load().expect("empty").is_empty());
        let mut base = configuration("base", "v2\n", &["v1\n", "v2\n"]);
        base.saved.role = crate::configs::ROLE_SUPPLEMENTARY.to_string();
        store.save("base", &base.saved).expect("save");
        release(&*agents, &pinned("base", "v1\n")).expect("release");
        release(&*agents, &pinned("base", "v2\n")).expect("release");
        release(&*agents, &pinned("base", "v2\n")).expect("releasing twice is fine");
        store.save("other", &revision("x\n")).expect("save");
        assert_eq!(
            open().0.load().expect("load"),
            [base.clone(), configuration("other", "x\n", &[])]
        );
        release(&*agents, &pinned("missing", "v1\n"))
            .expect_err("a revision of a Configuration the store does not hold");

        store.save("base", &revision("v3\n")).expect("save again");
        let saved = configuration("base", "v3\n", &["v1\n", "v2\n"]);
        assert_eq!(
            open().0.load().expect("load")[0],
            saved,
            "a save keeps every retained revision"
        );

        let v2 = revision_hash(&revision("v2\n"));
        assert_eq!(
            store
                .collect("base", &BTreeSet::from([v2.clone()]))
                .expect("collect")
                .into_keys()
                .collect::<Vec<_>>(),
            std::slice::from_ref(&v2),
            "what is kept is answered"
        );
        assert_eq!(
            open().0.load().expect("load")[0],
            configuration("base", "v3\n", &["v2\n"]),
            "the unreferenced revision is gone, the others stay"
        );
        assert!(store
            .collect("missing", &BTreeSet::new())
            .expect("collecting the absent is fine")
            .is_empty());

        store.remove("base").expect("remove");
        store.remove("base").expect("removing the absent is fine");
        assert_eq!(
            open().0.load().expect("load"),
            [configuration("other", "x\n", &[])]
        );
    }

    /// Verifies: ADR-0029
    #[test]
    fn the_configuration_port_contract_holds_on_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        contract(&|| {
            (
                Box::new(FsConfigBackend::open(dir.path()).expect("open")),
                Box::new(FsAgentStore::open(dir.path()).expect("open")),
            )
        });
    }

    /// Verifies: ADR-0029, ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn the_configuration_port_contract_holds_on_postgres() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        contract(&|| {
            (
                Box::new(PgConfigBackend::new(database.clone())),
                Box::new(PgAgentStore::new(database.clone())),
            )
        });
    }

    /// A save or a retention that fails leaves the Configuration as it was, on either store: one
    /// transaction per call, one file replaced in one step. `refuse` makes every write of the
    /// Configuration fail; `mend` takes that away.
    fn fails_part_way(
        store: &dyn ConfigBackend,
        agents: &dyn AgentStore,
        refuse: &dyn Fn(),
        mend: &dyn Fn(),
    ) {
        let before = configuration("base", "v1\n", &["v1\n"]);
        store.save("base", &before.saved).expect("save");
        release(agents, &pinned("base", "v1\n")).expect("release");
        refuse();
        store
            .save("base", &revision("v2\n"))
            .expect_err("the save is refused");
        release(agents, &pinned("base", "v2\n")).expect_err("the rollout's revision is refused");
        mend();
        assert_eq!(store.load().expect("load"), [before]);
    }

    /// Verifies: ADR-0029
    #[test]
    fn a_write_that_fails_leaves_nothing_of_itself_on_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("configurations/base.json");
        fails_part_way(
            &FsConfigBackend::open(dir.path()).expect("open"),
            &FsAgentStore::open(dir.path()).expect("open"),
            &|| crate::fs::testing::refuse(&file),
            &|| crate::fs::testing::mend(&file),
        );
    }

    /// Run from inside a runtime's own thread, as the Server's handlers call the port: the bridge
    /// never needs the thread that waits for it.
    /// Verifies: ADR-0029
    #[tokio::test(flavor = "current_thread")]
    async fn a_write_that_fails_leaves_nothing_of_itself_on_postgres() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let second = revision_hash(&revision("v2\n"));
        let database = test.open();
        fails_part_way(
            &PgConfigBackend::new(database.clone()),
            &PgAgentStore::new(database),
            &|| {
                test.refuse("configurations", "NEW.body = 'v2\n'");
                test.refuse("configuration_revisions", &format!("NEW.hash = '{second}'"));
            },
            &|| {
                test.mend("configurations");
                test.mend("configuration_revisions");
            },
        );
    }

    /// A collection on PostgreSQL keeps every revision a stored assignment pins, whatever the
    /// caller names: the assignments are reckoned in its own transaction, so a revision another
    /// instance pinned is not collected by one that never heard of it.
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn a_collection_on_postgres_keeps_what_a_stored_assignment_pins() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let store = PgConfigBackend::new(database.clone());
        let agents = PgAgentStore::new(database);
        store.save("base", &revision("v2\n")).expect("save");
        release(&agents, &pinned("base", "v1\n")).expect("release");
        release(&agents, &pinned("base", "v2\n")).expect("release");
        let v1 = revision_hash(&revision("v1\n"));
        test.sql(&format!(
            "INSERT INTO agents (instance_uid, sequence_num, capabilities, transport, \
             last_seen_ms, restart_pending, labels) VALUES ('one', 0, 0, 'websocket', 0, false, \
             '{{}}'); INSERT INTO agent_assignment_configurations VALUES ('one', 'base', '{v1}')"
        ));
        assert_eq!(
            store
                .collect("base", &BTreeSet::new())
                .expect("collect")
                .into_keys()
                .collect::<Vec<_>>(),
            [v1],
            "the pinned revision is kept, the other collected"
        );
    }

    /// A rollout that pins a revision already stored, while another instance collects the
    /// Configuration, keeps it: the collection's `FOR UPDATE` on the Configuration's row waits for
    /// the rollout's `FOR KEY SHARE`, and its delete then reads the assignment the rollout
    /// committed. The rollout is held at a gate in the database between its revision and its
    /// record, so the collection runs exactly then; the gate opens once the collection waits, or
    /// once it has finished, which only a collection that took no hold does.
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn a_collection_keeps_a_stored_revision_a_rollout_pins_meanwhile() {
        use opamp::uid::InstanceUid;
        use std::sync::atomic::{AtomicBool, Ordering};
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let configs = PgConfigBackend::new(database.clone());
        configs.save("base", &revision("v1\n")).expect("save");
        let held = pinned("base", "v1\n");
        release(&PgAgentStore::new(database.clone()), &held)
            .expect("a revision stored and pinned by nothing");
        test.sql(
            "CREATE TABLE fleet_test_gate (open boolean NOT NULL); \
             INSERT INTO fleet_test_gate VALUES (false); \
             CREATE FUNCTION fleet_test_wait() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
             WHILE NOT (SELECT open FROM fleet_test_gate) LOOP PERFORM pg_sleep(0.01); \
             END LOOP; RETURN NEW; END $$; \
             CREATE TRIGGER fleet_test_wait BEFORE INSERT OR UPDATE ON agents \
             FOR EACH ROW EXECUTE FUNCTION fleet_test_wait();",
        );
        let waiting = |condition: &str| {
            !test
                .rows(&format!(
                    "SELECT 1 FROM pg_stat_activity WHERE pid <> pg_backend_pid() AND {condition}"
                ))
                .is_empty()
        };
        let until = |what: &str, done: &dyn Fn() -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while !done() {
                assert!(std::time::Instant::now() < deadline, "{what}");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };

        let mut record = crate::agent_store::tests::record();
        if let Some(assignment) = record.assignment.as_mut() {
            assignment.configurations = BTreeMap::from([("base".to_string(), held.hash.clone())]);
        }
        let rollout = {
            let (database, held) = (database.clone(), held.clone());
            std::thread::spawn(move || {
                PgAgentStore::new(database)
                    .release(&[held], &[(InstanceUid::default(), record)])
                    .expect("the rollout")
            })
        };
        until("the rollout reaches the gate", &|| {
            waiting("wait_event = 'PgSleep' AND query LIKE 'INSERT INTO agents%'")
        });
        let collected = Arc::new(AtomicBool::new(false));
        let collection = {
            let (database, collected) = (database.clone(), collected.clone());
            std::thread::spawn(move || {
                let kept = PgConfigBackend::new(database)
                    .collect("base", &BTreeSet::new())
                    .expect("the collection");
                collected.store(true, Ordering::SeqCst);
                kept
            })
        };
        until("the collection waits or ends", &|| {
            collected.load(Ordering::SeqCst)
                || waiting(
                    "wait_event_type = 'Lock' AND (query LIKE \
                     'SELECT 1 FROM configurations WHERE name = $1 FOR UPDATE' \
                     OR query LIKE 'DELETE FROM configuration_revisions r %')",
                )
        });
        test.sql("UPDATE fleet_test_gate SET open = true");
        rollout.join().expect("the rollout's thread");
        let kept = collection.join().expect("the collection's thread");
        assert_eq!(
            kept.into_keys().collect::<Vec<_>>(),
            std::slice::from_ref(&held.hash),
            "the collection kept what the rollout pins"
        );
        assert_eq!(
            test.rows("SELECT hash FROM configuration_revisions"),
            [[Some(held.hash)]],
            "and so does the database"
        );
    }

    /// A row that does not decode fails the load naming it, rather than being skipped.
    /// Verifies: ADR-0029
    #[test]
    fn a_configuration_row_that_does_not_decode_fails_the_load_naming_it() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let store = PgConfigBackend::new(test.open());
        test.sql("INSERT INTO configurations VALUES ('untyped', 'x', '', '')");
        let err = store.load().expect_err("no Agent type");
        assert!(err.contains("\"untyped\""), "{err}");
    }
}
