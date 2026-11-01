//! Deployments in PostgreSQL (ADR-0029 clause 6): the PostgreSQL adapter behind
//! [`DeploymentBackend`](crate::deployments::DeploymentBackend) — one row per Deployment and one
//! per member, each call one transaction on the rows it names
//! (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
//! clause 19).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tokio_postgres::GenericClient;

use super::{client, sql, Database};
use crate::configs::validate_name;
use crate::deployments::{
    check_selector, not_held, Change, Deployment, DeploymentBackend, DeploymentError, Member,
};
use crate::packages::PackageId;

/// The Deployments in `deployments`.
pub struct PgDeploymentBackend {
    database: Arc<Database>,
}

impl PgDeploymentBackend {
    /// The adapter on `database`.
    #[must_use]
    pub fn new(database: Arc<Database>) -> Self {
        PgDeploymentBackend { database }
    }
}

impl DeploymentBackend for PgDeploymentBackend {
    fn load(&self) -> Result<Vec<Deployment>, String> {
        self.database.run(|pool| async move {
            let client = client(&pool).await?;
            read(&**client, None).await
        })
    }

    /// The Deployment's row alone: a new one holds no member, a stored one keeps its members.
    fn save(&self, name: &str, selector: &BTreeMap<String, String>) -> Result<Deployment, String> {
        let name = name.to_string();
        let selector = serde_json::to_value(selector).expect("a Selector serializes");
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            transaction
                .execute(
                    "INSERT INTO deployments (name, selector, packages, configurations) \
                     VALUES ($1, $2, '[]', '{}') ON CONFLICT (name) DO UPDATE SET \
                     selector = EXCLUDED.selector",
                    &[&name, &selector],
                )
                .await
                .map_err(sql)?;
            let deployment = one(&*transaction, &name).await?;
            super::notify::notify(&*transaction, super::notify::DEPLOYMENT, &name).await?;
            transaction.commit().await.map_err(sql)?;
            Ok(deployment)
        })
    }

    /// One transaction: the Deployment's row is held `FOR UPDATE`, the change decided from the
    /// member rows it reads then, the rows it names written, and the columns the release before
    /// reads written from the rows.
    fn amend(&self, name: &str, change: &Change) -> Result<Deployment, DeploymentError> {
        let (name, change) = (name.to_string(), change.clone());
        let decided = self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            let decided = change_rows(&*transaction, &name, &change).await?;
            if decided.is_ok() {
                mirror(&*transaction, &name).await?;
            }
            let decided = match decided {
                Ok(()) => {
                    super::notify::notify(&*transaction, super::notify::DEPLOYMENT, &name).await?;
                    Ok(one(&*transaction, &name).await?)
                }
                Err(refused) => Err(refused),
            };
            transaction.commit().await.map_err(sql)?;
            Ok(decided)
        });
        decided.map_err(DeploymentError::Storage)?
    }

    /// One transaction: every Deployment that holds the member is held `FOR UPDATE`, in name
    /// order, and loses it.
    fn drop_member(&self, member: &Member) -> Result<Vec<Deployment>, String> {
        let member = member.clone();
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            let holding: Vec<String> = match &member {
                Member::Configuration(configuration) => {
                    transaction
                        .query(
                            "SELECT name FROM deployments WHERE name IN (SELECT deployment \
                             FROM deployment_configurations WHERE configuration = $1) \
                             ORDER BY name FOR UPDATE",
                            &[configuration],
                        )
                        .await
                }
                Member::Package(id) => {
                    transaction
                        .query(
                            "SELECT name FROM deployments WHERE name IN (SELECT deployment \
                             FROM deployment_packages WHERE agent_type = $1 AND version = $2) \
                             ORDER BY name FOR UPDATE",
                            &[&id.agent_type, &id.version],
                        )
                        .await
                }
            }
            .map_err(sql)?
            .iter()
            .map(|row| row.get(0))
            .collect();
            let mut changed = Vec::new();
            for name in &holding {
                let removed = match &member {
                    Member::Configuration(configuration) => {
                        Change::RemoveConfiguration(configuration.clone())
                    }
                    Member::Package(id) => Change::RemovePackage(id.clone()),
                };
                if change_rows(&*transaction, name, &removed).await?.is_ok() {
                    mirror(&*transaction, name).await?;
                    super::notify::notify(&*transaction, super::notify::DEPLOYMENT, name).await?;
                    changed.push(one(&*transaction, name).await?);
                }
            }
            transaction.commit().await.map_err(sql)?;
            Ok(changed)
        })
    }

    /// The Deployment and its members go in one statement, the members by cascade.
    fn remove(&self, name: &str) -> Result<(), String> {
        let name = name.to_string();
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            transaction
                .execute("DELETE FROM deployments WHERE name = $1", &[&name])
                .await
                .map_err(sql)?;
            super::notify::notify(&*transaction, super::notify::DEPLOYMENT, &name).await?;
            transaction.commit().await.map_err(sql)
        })
    }
}

/// Makes `change` to the member rows of the Deployment `name` inside the caller's transaction,
/// holding its row `FOR UPDATE` first and deciding from the rows it then reads. A refusal is the
/// inner error, and nothing is written.
async fn change_rows(
    client: &impl GenericClient,
    name: &str,
    change: &Change,
) -> Result<Result<(), DeploymentError>, String> {
    if client
        .query_opt(
            "SELECT 1 FROM deployments WHERE name = $1 FOR UPDATE",
            &[&name],
        )
        .await
        .map_err(sql)?
        .is_none()
    {
        return Ok(Err(DeploymentError::NotFound));
    }
    match change {
        Change::PutPackage { id, replace } => {
            let held = client
                .query_opt(
                    "SELECT version FROM deployment_packages WHERE deployment = $1 \
                     AND agent_type = $2",
                    &[&name, &id.agent_type],
                )
                .await
                .map_err(sql)?
                .map(|row| row.get::<_, String>(0));
            if let Some(held) = held.filter(|held| !replace && *held != id.version) {
                let held = PackageId::load(&id.agent_type, &held)
                    .map_err(|e| format!("deployment {name:?} in the database: {e}"))?;
                return Ok(Err(DeploymentError::TypeTaken {
                    agent_type: id.agent_type.clone(),
                    held,
                }));
            }
            client
                .execute(
                    "INSERT INTO deployment_packages (deployment, agent_type, version) \
                     VALUES ($1, $2, $3) ON CONFLICT (deployment, agent_type) DO UPDATE SET \
                     version = EXCLUDED.version",
                    &[&name, &id.agent_type, &id.version],
                )
                .await
                .map_err(sql)?;
        }
        Change::RemovePackage(id) => {
            let removed = client
                .execute(
                    "DELETE FROM deployment_packages WHERE deployment = $1 AND agent_type = $2 \
                     AND version = $3",
                    &[&name, &id.agent_type, &id.version],
                )
                .await
                .map_err(sql)?;
            if removed == 0 {
                return Ok(Err(DeploymentError::NotFound));
            }
        }
        Change::PutConfiguration(configuration) => {
            client
                .execute(
                    "INSERT INTO deployment_configurations (deployment, configuration) \
                     VALUES ($1, $2) ON CONFLICT DO NOTHING",
                    &[&name, configuration],
                )
                .await
                .map_err(sql)?;
        }
        Change::RemoveConfiguration(configuration) => {
            let removed = client
                .execute(
                    "DELETE FROM deployment_configurations WHERE deployment = $1 \
                     AND configuration = $2",
                    &[&name, configuration],
                )
                .await
                .map_err(sql)?;
            if removed == 0 {
                return Ok(Err(not_held(name, configuration)));
            }
        }
    }
    Ok(Ok(()))
}

/// Writes the `packages` and `configurations` columns of the Deployment `name` from its member
/// rows, inside the caller's transaction, which holds its row: the release before migration 5
/// reads its members there.
async fn mirror(client: &impl GenericClient, name: &str) -> Result<(), String> {
    client
        .execute(
            "UPDATE deployments SET \
             packages = (SELECT coalesce(json_agg(json_build_object('agent_type', agent_type, \
             'version', version) ORDER BY agent_type), '[]'::json) FROM deployment_packages \
             WHERE deployment = $1), \
             configurations = ARRAY(SELECT configuration FROM deployment_configurations \
             WHERE deployment = $1 ORDER BY configuration) \
             WHERE name = $1",
            &[&name],
        )
        .await
        .map_err(sql)?;
    Ok(())
}

/// The Deployment `name` as stored, inside the caller's transaction.
async fn one(client: &impl GenericClient, name: &str) -> Result<Deployment, String> {
    read(client, Some(name))
        .await?
        .pop()
        .ok_or_else(|| format!("deployment {name:?} is not in the database"))
}

/// Every Deployment, or the one `name` names, its members read from their rows. A row that does
/// not decode — a name or Selector a write would refuse, a Package identity that is no token —
/// fails, naming it.
pub(crate) async fn read(
    client: &impl GenericClient,
    name: Option<&str>,
) -> Result<Vec<Deployment>, String> {
    let mut deployments: BTreeMap<String, Deployment> = BTreeMap::new();
    for row in client
        .query(
            "SELECT name, selector FROM deployments WHERE $1::text IS NULL OR name = $1",
            &[&name],
        )
        .await
        .map_err(sql)?
    {
        let name: String = row.get(0);
        let named = |e: String| format!("deployment {name:?} in the database: {e}");
        validate_name(&name).map_err(named)?;
        let selector: BTreeMap<String, String> =
            serde_json::from_value(row.get(1)).map_err(|e| named(format!("selector: {e}")))?;
        check_selector(&selector).map_err(named)?;
        deployments.insert(
            name.clone(),
            Deployment {
                name,
                selector,
                packages: BTreeMap::new(),
                configurations: BTreeSet::new(),
            },
        );
    }
    for row in client
        .query(
            "SELECT deployment, agent_type, version FROM deployment_packages \
             WHERE $1::text IS NULL OR deployment = $1",
            &[&name],
        )
        .await
        .map_err(sql)?
    {
        let deployment: String = row.get(0);
        let named = |e: String| format!("deployment {deployment:?} in the database: {e}");
        let id = PackageId::load(row.get(1), row.get(2)).map_err(named)?;
        deployments
            .get_mut(&deployment)
            .ok_or_else(|| named("a Package of a Deployment that is not there".to_string()))?
            .packages
            .insert(id.agent_type.clone(), id);
    }
    for row in client
        .query(
            "SELECT deployment, configuration FROM deployment_configurations \
             WHERE $1::text IS NULL OR deployment = $1",
            &[&name],
        )
        .await
        .map_err(sql)?
    {
        let deployment: String = row.get(0);
        let named = |e: String| format!("deployment {deployment:?} in the database: {e}");
        let configuration: String = row.get(1);
        validate_name(&configuration)
            .map_err(|e| named(format!("configuration {configuration:?}: {e}")))?;
        deployments
            .get_mut(&deployment)
            .ok_or_else(|| named("a Configuration of a Deployment that is not there".to_string()))?
            .configurations
            .insert(configuration);
    }
    Ok(deployments.into_values().collect())
}

/// Creates or replaces one Deployment whole, its members exactly `deployment`'s, inside the
/// caller's transaction — the write of an import into a database no instance serves.
pub(crate) async fn write(
    client: &impl GenericClient,
    deployment: &Deployment,
) -> Result<(), String> {
    let selector = serde_json::to_value(&deployment.selector).expect("a Selector serializes");
    client
        .execute(
            "INSERT INTO deployments (name, selector, packages, configurations) \
             VALUES ($1, $2, '[]', '{}') ON CONFLICT (name) DO UPDATE SET \
             selector = EXCLUDED.selector",
            &[&deployment.name, &selector],
        )
        .await
        .map_err(sql)?;
    for table in ["deployment_packages", "deployment_configurations"] {
        client
            .execute(
                &format!("DELETE FROM {table} WHERE deployment = $1"),
                &[&deployment.name],
            )
            .await
            .map_err(sql)?;
    }
    for id in deployment.packages.values() {
        client
            .execute(
                "INSERT INTO deployment_packages (deployment, agent_type, version) \
                 VALUES ($1, $2, $3)",
                &[&deployment.name, &id.agent_type, &id.version],
            )
            .await
            .map_err(sql)?;
    }
    for configuration in &deployment.configurations {
        client
            .execute(
                "INSERT INTO deployment_configurations (deployment, configuration) \
                 VALUES ($1, $2)",
                &[&deployment.name, configuration],
            )
            .await
            .map_err(sql)?;
    }
    mirror(client, &deployment.name).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::FsDeploymentBackend;
    use crate::postgres::testing::TestDatabase;

    fn id(version: &str) -> PackageId {
        PackageId::new("otelcol", version).expect("id")
    }

    fn selector(name: &str) -> BTreeMap<String, String> {
        BTreeMap::from([("fleet.ring".to_string(), name.to_string())])
    }

    fn deployment(name: &str, package: Option<&str>, configurations: &[&str]) -> Deployment {
        Deployment {
            name: name.to_string(),
            selector: selector(name),
            packages: package
                .map(|version| BTreeMap::from([("otelcol".to_string(), id(version))]))
                .unwrap_or_default(),
            configurations: configurations.iter().map(ToString::to_string).collect(),
        }
    }

    /// Writes `deployment` through the port, one member at a time.
    fn put(store: &dyn DeploymentBackend, deployment: &Deployment) {
        store
            .save(&deployment.name, &deployment.selector)
            .expect("save");
        for id in deployment.packages.values() {
            store
                .amend(
                    &deployment.name,
                    &Change::PutPackage {
                        id: id.clone(),
                        replace: false,
                    },
                )
                .expect("package");
        }
        for configuration in &deployment.configurations {
            store
                .amend(
                    &deployment.name,
                    &Change::PutConfiguration(configuration.clone()),
                )
                .expect("configuration");
        }
    }

    /// The port's contract on either store: what is written is loaded back; a save replaces the
    /// Selector and keeps the members; each change adds or removes the member it names, decided
    /// from what is stored, and answers the Deployment as stored; a dropped member leaves every
    /// Deployment that held it; and a removal tolerates absence.
    fn contract(open: &dyn Fn() -> Box<dyn DeploymentBackend>) {
        let store = open();
        assert!(store.load().expect("empty").is_empty());
        let canary = deployment("canary", Some("1.2.3"), &["base", "extra"]);
        put(&*store, &canary);
        put(&*store, &deployment("stable", None, &["base"]));
        assert_eq!(
            open().load().expect("load"),
            [canary.clone(), deployment("stable", None, &["base"])]
        );

        let moved = BTreeMap::from([("fleet.ring".to_string(), "early".to_string())]);
        let saved = store.save("canary", &moved).expect("save again");
        assert_eq!(saved.selector, moved);
        assert_eq!(saved.packages, canary.packages, "a save keeps the members");
        assert_eq!(saved.configurations, canary.configurations);
        store.save("canary", &canary.selector).expect("save back");

        let taken = store.amend(
            "canary",
            &Change::PutPackage {
                id: id("1.3.0"),
                replace: false,
            },
        );
        assert_eq!(
            taken,
            Err(DeploymentError::TypeTaken {
                agent_type: "otelcol".to_string(),
                held: id("1.2.3"),
            })
        );
        let replaced = store
            .amend(
                "canary",
                &Change::PutPackage {
                    id: id("1.3.0"),
                    replace: true,
                },
            )
            .expect("replace");
        assert_eq!(
            replaced,
            deployment("canary", Some("1.3.0"), &["base", "extra"])
        );
        assert_eq!(
            store.amend("canary", &Change::RemovePackage(id("1.2.3"))),
            Err(DeploymentError::NotFound),
            "a Package it does not hold"
        );
        assert_eq!(
            store.amend("gone", &Change::PutConfiguration("base".to_string())),
            Err(DeploymentError::NotFound),
            "a Deployment the store does not hold"
        );
        assert!(matches!(
            store.amend("stable", &Change::RemoveConfiguration("extra".to_string())),
            Err(DeploymentError::Missing(_))
        ));
        store
            .amend("canary", &Change::RemoveConfiguration("extra".to_string()))
            .expect("take one out");
        assert_eq!(
            open().load().expect("load")[0],
            deployment("canary", Some("1.3.0"), &["base"])
        );

        let mut dropped: Vec<String> = store
            .drop_member(&Member::Configuration("base".to_string()))
            .expect("drop")
            .into_iter()
            .map(|deployment| deployment.name)
            .collect();
        dropped.sort();
        assert_eq!(dropped, ["canary", "stable"]);
        assert_eq!(
            store
                .drop_member(&Member::Package(id("1.3.0")))
                .expect("drop"),
            [deployment("canary", None, &[])]
        );
        assert!(store
            .drop_member(&Member::Package(id("1.3.0")))
            .expect("dropping what nothing holds")
            .is_empty());

        store.remove("canary").expect("remove");
        store.remove("canary").expect("removing the absent is fine");
        assert_eq!(
            open().load().expect("load"),
            [deployment("stable", None, &[])]
        );
    }

    /// Verifies: ADR-0029
    #[test]
    fn the_deployment_port_contract_holds_on_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        contract(&|| Box::new(FsDeploymentBackend::open(dir.path()).expect("open")));
    }

    /// Verifies: ADR-0029, ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn the_deployment_port_contract_holds_on_postgres() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        contract(&|| Box::new(PgDeploymentBackend::new(database.clone())));
    }

    /// A save or a change that fails leaves the Deployment as it was, on either store. `refuse`
    /// makes every write of `canary` fail; `mend` takes that away.
    fn fails_part_way(store: &dyn DeploymentBackend, refuse: &dyn Fn(), mend: &dyn Fn()) {
        let before = deployment("canary", Some("1.2.3"), &["base"]);
        put(store, &before);
        refuse();
        store
            .save("canary", &selector("early"))
            .expect_err("the save is refused");
        store
            .amend(
                "canary",
                &Change::PutPackage {
                    id: id("1.3.0"),
                    replace: true,
                },
            )
            .expect_err("the change is refused");
        store
            .amend("canary", &Change::PutConfiguration("extra".to_string()))
            .expect_err("the change is refused");
        mend();
        assert_eq!(store.load().expect("load"), [before]);
    }

    /// Verifies: ADR-0029
    #[test]
    fn a_write_that_fails_leaves_the_deployment_as_it_was_on_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("deployments/canary.json");
        fails_part_way(
            &FsDeploymentBackend::open(dir.path()).expect("open"),
            &|| crate::fs::testing::refuse(&file),
            &|| crate::fs::testing::mend(&file),
        );
    }

    /// The columns the release before reads are written last, from the rows: refusing that write
    /// refuses the whole change.
    /// Verifies: ADR-0029
    #[test]
    fn a_write_that_fails_leaves_the_deployment_as_it_was_on_postgres() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        fails_part_way(
            &PgDeploymentBackend::new(test.open()),
            &|| test.refuse("deployments", "NEW.name = 'canary'"),
            &|| test.mend("deployments"),
        );
    }

    /// The release before migration 5 reads a Deployment's members from its own row, so every
    /// change writes those columns from the member rows in the same transaction.
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn the_columns_the_release_before_reads_follow_the_member_rows() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let store = PgDeploymentBackend::new(test.open());
        put(
            &store,
            &deployment("canary", Some("1.2.3"), &["extra", "base"]),
        );
        assert_eq!(
            test.rows("SELECT packages::text, configurations::text FROM deployments"),
            [[
                Some(r#"[{"agent_type" : "otelcol", "version" : "1.2.3"}]"#.to_string()),
                Some("{base,extra}".to_string())
            ]]
        );
        store
            .drop_member(&Member::Configuration("base".to_string()))
            .expect("drop");
        store
            .amend("canary", &Change::RemovePackage(id("1.2.3")))
            .expect("remove");
        assert_eq!(
            test.rows("SELECT packages::text, configurations::text FROM deployments"),
            [[Some("[]".to_string()), Some("{extra}".to_string())]]
        );
    }

    /// Migration 5 moves the members out of the Deployments' columns into rows of their own, and
    /// the Deployments load as the release before left them.
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn the_members_move_out_of_the_deployments_columns() {
        use crate::postgres::schema::{MIGRATIONS, SCHEMA_TABLE};
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        // The database as the release before the member rows left it.
        test.sql(&format!(
            "DROP TABLE deployment_packages; DROP TABLE deployment_configurations; \
             DELETE FROM {SCHEMA_TABLE} WHERE version = 5; \
             INSERT INTO deployments VALUES \
             ('canary', '{{\"fleet.ring\":\"canary\"}}', \
              '[{{\"agent_type\":\"otelcol\",\"version\":\"1.2.3\"}}]', '{{base,extra}}'), \
             ('stable', '{{\"fleet.ring\":\"stable\"}}', '[]', '{{}}')"
        ));
        assert_eq!(
            database.apply_schema(MIGRATIONS).expect("migrate").versions,
            [5]
        );
        assert_eq!(
            PgDeploymentBackend::new(database.clone())
                .load()
                .expect("load"),
            [
                deployment("canary", Some("1.2.3"), &["base", "extra"]),
                deployment("stable", None, &[])
            ]
        );
    }

    /// Two instances that each add a Package of one Agent type to one Deployment at once, neither
    /// replacing, add one: the other is refused with the type taken, naming the Package that won.
    /// The first is held at a gate in the database between its read of the held type and its
    /// write, so the second runs exactly then; the gate opens once the second waits for the
    /// Deployment's row, or has reached the gate itself, which only an amendment that took no hold
    /// does.
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it, ADR-0027
    #[test]
    fn two_packages_of_one_type_added_at_once_add_one() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        PgDeploymentBackend::new(database.clone())
            .save("stable", &selector("stable"))
            .expect("save");
        test.sql(
            "CREATE TABLE fleet_test_gate (open boolean NOT NULL); \
             INSERT INTO fleet_test_gate VALUES (false); \
             CREATE FUNCTION fleet_test_wait() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
             WHILE NOT (SELECT open FROM fleet_test_gate) LOOP PERFORM pg_sleep(0.01); \
             END LOOP; RETURN NEW; END $$; \
             CREATE TRIGGER fleet_test_wait BEFORE INSERT ON deployment_packages \
             FOR EACH ROW EXECUTE FUNCTION fleet_test_wait();",
        );
        let sessions = |condition: &str| {
            test.rows(&format!(
                "SELECT 1 FROM pg_stat_activity WHERE pid <> pg_backend_pid() AND {condition}"
            ))
            .len()
        };
        let at_gate =
            || sessions("wait_event = 'PgSleep' AND query LIKE 'INSERT INTO deployment_packages%'");
        let until = |what: &str, done: &dyn Fn() -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while !done() {
                assert!(std::time::Instant::now() < deadline, "{what}");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        let add = |version: &str| {
            let (database, id) = (database.clone(), id(version));
            std::thread::spawn(move || {
                PgDeploymentBackend::new(database)
                    .amend("stable", &Change::PutPackage { id, replace: false })
            })
        };

        let first = add("1.0.0");
        until("the first reaches the gate", &|| at_gate() == 1);
        let second = add("2.0.0");
        until("the second waits for the row, or reaches the gate", &|| {
            at_gate() == 2
                || sessions(
                    "wait_event_type = 'Lock' AND query LIKE \
                     'SELECT 1 FROM deployments WHERE name = $1 FOR UPDATE'",
                ) == 1
        });
        test.sql("UPDATE fleet_test_gate SET open = true");
        let first = first.join().expect("the first's thread");
        let second = second.join().expect("the second's thread");
        assert_eq!(
            first.expect("the first adds its Package").packages["otelcol"],
            id("1.0.0")
        );
        assert_eq!(
            second,
            Err(DeploymentError::TypeTaken {
                agent_type: "otelcol".to_string(),
                held: id("1.0.0"),
            }),
            "the second is refused, naming the first"
        );
        assert_eq!(
            test.rows("SELECT version FROM deployment_packages"),
            [[Some("1.0.0".to_string())]]
        );
    }

    /// A row that does not decode fails the load naming it (ADR-0029 clause 6).
    /// Verifies: ADR-0029
    #[test]
    fn a_deployment_row_that_does_not_decode_fails_the_load_naming_it() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let store = PgDeploymentBackend::new(test.open());
        test.sql(
            "INSERT INTO deployments VALUES ('aimless', '{}', '[]', '{}'), \
             ('odd', '{\"a\":\"b\"}', '[]', '{}'); \
             INSERT INTO deployment_packages VALUES ('odd', 'otelcol', '')",
        );
        let err = store.load().expect_err("an empty Selector");
        assert!(err.contains("\"aimless\""), "{err}");
        test.sql("DELETE FROM deployments WHERE name = 'aimless'");
        let err = store.load().expect_err("a reference without a version");
        assert!(err.contains("\"odd\""), "{err}");
    }
}
