//! Deployments in PostgreSQL (ADR-0044 clause 6): the PostgreSQL adapter behind
//! [`DeploymentBackend`](crate::deployments::DeploymentBackend) — one row per Deployment and one
//! per member, each call one transaction on the rows it names
//! (ADR-0041
//! clause 19).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::Cached as GenericClient;

use super::{client, sql, Database};
use crate::configs::validate_name;
use crate::deployments::{
    check_selector, no_configuration, no_package, not_held, Change, Deployment, DeploymentBackend,
    DeploymentError, Member,
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
            read(&client, None).await
        })
    }

    /// The Deployment's row alone: a new one holds no member, a stored one keeps its members.
    fn save(&self, name: &str, selector: &BTreeMap<String, String>) -> Result<Deployment, String> {
        let name = name.to_string();
        let selector = serde_json::to_value(selector).expect("a Selector serializes");
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            // `packages` and `configurations` mirrored the member rows; no release reads them,
            // so a new row's are empty and nothing writes them after
            // (ADR-draft-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision
            // clause 22).
            transaction
                .execute(
                    "INSERT INTO deployments (name, selector, packages, configurations) \
                     VALUES ($1, $2, '[]', '{}') ON CONFLICT (name) DO UPDATE SET \
                     selector = EXCLUDED.selector",
                    &[&name, &selector],
                )
                .await
                .map_err(sql)?;
            let deployment = one(&transaction, &name).await?;
            super::notify::notify(&transaction, super::notify::DEPLOYMENT, &name).await?;
            transaction.commit().await.map_err(sql)?;
            Ok(deployment)
        })
    }

    /// One transaction: the Deployment's row is held `FOR UPDATE`, the change decided from the
    /// member rows it reads then, and the rows it names written.
    fn amend(&self, name: &str, change: &Change) -> Result<Deployment, DeploymentError> {
        let (name, change) = (name.to_string(), change.clone());
        let decided = self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            let decided = change_rows(&transaction, &name, &change).await?;
            let decided = match decided {
                Ok(()) => {
                    super::notify::notify(&transaction, super::notify::DEPLOYMENT, &name).await?;
                    Ok(one(&transaction, &name).await?)
                }
                Err(refused) => Err(refused),
            };
            transaction.commit().await.map_err(sql)?;
            Ok(decided)
        });
        decided.map_err(DeploymentError::Storage)?
    }

    /// One transaction: every Deployment that holds the member is held `FOR UPDATE`, in name
    /// order, and loses it. The deletion of the member took it out of every Deployment already,
    /// in its own transaction ([`drop_member_rows`]), so this finds one only where a Server of
    /// 0.8.0 beside this one added it after.
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
                if change_rows(&transaction, name, &removed).await?.is_ok() {
                    super::notify::notify(&transaction, super::notify::DEPLOYMENT, name).await?;
                    changed.push(one(&transaction, name).await?);
                }
            }
            transaction.commit().await.map_err(sql)?;
            Ok(changed)
        })
    }

    /// One transaction: the Deployment's row is held `FOR UPDATE`, which waits for every rollout
    /// that releases through it and keeps every other out until this one commits; then the stored
    /// assignments that name it are counted, and only when none does are the Deployment and its
    /// members deleted, the members by cascade.
    fn remove(&self, name: &str) -> Result<(), DeploymentError> {
        let name = name.to_string();
        let decided = self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            let held = transaction
                .query_opt(
                    "SELECT 1 FROM deployments WHERE name = $1 FOR UPDATE",
                    &[&name],
                )
                .await
                .map_err(sql)?;
            if held.is_none() {
                transaction.commit().await.map_err(sql)?;
                return Ok(Ok(()));
            }
            let pinning: i64 = transaction
                .query_one(
                    "SELECT count(*) FROM agents WHERE deployment = $1",
                    &[&name],
                )
                .await
                .map_err(sql)?
                .get(0);
            if pinning > 0 {
                transaction.commit().await.map_err(sql)?;
                return Ok(Err(DeploymentError::Conflict(crate::deployments::pinned(
                    &name,
                    usize::try_from(pinning).unwrap_or(usize::MAX),
                ))));
            }
            transaction
                .execute("DELETE FROM deployments WHERE name = $1", &[&name])
                .await
                .map_err(sql)?;
            super::notify::notify(&transaction, super::notify::DEPLOYMENT, &name).await?;
            transaction.commit().await.map_err(sql)?;
            Ok(Ok(()))
        });
        decided.map_err(DeploymentError::Storage)?
    }
}

/// Takes `member` out of every Deployment that holds it inside the transaction that deletes it,
/// which holds the member's row `FOR UPDATE`: an add that holds that row `FOR KEY SHARE` has
/// committed before this reads the member rows, and every later one finds the member gone
/// (ADR-draft-several-server-instances-share-one-postgresql-database-and-two-versions-run-side-by-side
/// clause 19). The Deployments' rows are not held, so this takes no lock in the order of an add,
/// which holds the Deployment's row before the member's.
pub(super) async fn drop_member_rows(
    client: &impl GenericClient,
    member: &Member,
) -> Result<(), String> {
    let rows = match member {
        Member::Configuration(configuration) => {
            client
                .query(
                    "DELETE FROM deployment_configurations WHERE configuration = $1 \
                     RETURNING deployment",
                    &[configuration],
                )
                .await
        }
        Member::Package(id) => {
            client
                .query(
                    "DELETE FROM deployment_packages WHERE agent_type = $1 AND version = $2 \
                     RETURNING deployment",
                    &[&id.agent_type, &id.version],
                )
                .await
        }
    }
    .map_err(sql)?;
    let left: BTreeSet<String> = rows.iter().map(|row| row.get(0)).collect();
    for name in &left {
        super::notify::notify(client, super::notify::DEPLOYMENT, name).await?;
    }
    Ok(())
}

/// Makes `change` to the member rows of the Deployment `name` inside the caller's transaction,
/// holding its row `FOR UPDATE` first and deciding from the rows it then reads. A member it adds
/// is held `FOR KEY SHARE` until the commit, and one that is not stored is refused: its deletion
/// waits for the add, and then takes it out again ([`drop_member_rows`]). A refusal is the inner
/// error, and nothing is written.
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
            if client
                .query_opt(
                    "SELECT 1 FROM packages WHERE agent_type = $1 AND version = $2 FOR KEY SHARE",
                    &[&id.agent_type, &id.version],
                )
                .await
                .map_err(sql)?
                .is_none()
            {
                return Ok(Err(no_package(id)));
            }
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
            if client
                .query_opt(
                    "SELECT 1 FROM configurations WHERE name = $1 FOR KEY SHARE",
                    &[configuration],
                )
                .await
                .map_err(sql)?
                .is_none()
            {
                return Ok(Err(no_configuration(configuration)));
            }
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
    Ok(())
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

    /// Stores every Configuration and Package these tests put into a Deployment: an add refuses a
    /// member that is not stored.
    fn store_members(test: &TestDatabase) {
        test.sql(
            "INSERT INTO configurations (name, body, role, service_name) VALUES \
             ('base', '', '', 'otelcol'), ('extra', '', '', 'otelcol'), \
             ('other', '', '', 'otelcol'); \
             INSERT INTO packages (agent_type, version) VALUES ('otelcol', '1.0.0'), \
             ('otelcol', '1.2.3'), ('otelcol', '1.2.4'), ('otelcol', '1.3.0'), \
             ('otelcol', '2.0.0')",
        );
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

    /// Verifies: ADR-0044
    #[test]
    fn the_deployment_port_contract_holds_on_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        contract(&|| Box::new(FsDeploymentBackend::open(dir.path()).expect("open")));
    }

    /// Verifies: ADR-0044, ADR-0041
    #[test]
    fn the_deployment_port_contract_holds_on_postgres() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        store_members(&test);
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

    /// Verifies: ADR-0044
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

    /// Each write of `canary`'s row and of its member rows is refused: a replacing Package
    /// removes the old row before its insert is refused, and the whole change is refused with it.
    /// Verifies: ADR-0044
    #[test]
    fn a_write_that_fails_leaves_the_deployment_as_it_was_on_postgres() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let tables = [
            ("deployments", "NEW.name = 'canary'"),
            ("deployment_packages", "NEW.deployment = 'canary'"),
            ("deployment_configurations", "NEW.deployment = 'canary'"),
        ];
        let database = test.open();
        store_members(&test);
        fails_part_way(
            &PgDeploymentBackend::new(database),
            &|| {
                for (table, condition) in tables {
                    test.refuse(table, condition);
                }
            },
            &|| {
                for (table, _) in tables {
                    test.mend(table);
                }
            },
        );
    }

    /// No release reads the Deployments' `packages` and `configurations` columns, which mirror
    /// the member rows, so this release writes them no more: a new Deployment's are empty, and
    /// what a Server of 0.8.0 wrote into them stays as it was through every change of the
    /// members, until the release after this one removes them
    /// (ADR-draft-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision
    /// clause 22).
    /// Verifies: ADR-draft-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision
    #[test]
    fn the_mirror_columns_are_written_no_more() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let store = PgDeploymentBackend::new(test.open());
        store_members(&test);
        let mirrors = || test.rows("SELECT packages::text, configurations::text FROM deployments");
        put(
            &store,
            &deployment("canary", Some("1.2.3"), &["extra", "base"]),
        );
        assert_eq!(
            mirrors(),
            [[Some("[]".to_string()), Some("{}".to_string())]],
            "a new Deployment's are empty"
        );
        // As a Server of 0.8.0 beside this one leaves them.
        test.sql(
            "UPDATE deployments SET packages = \
             '[{\"agent_type\":\"otelcol\",\"version\":\"1.2.3\"}]', \
             configurations = '{base,extra}'",
        );
        let written = mirrors();
        store
            .drop_member(&Member::Configuration("base".to_string()))
            .expect("drop");
        store
            .amend("canary", &Change::RemovePackage(id("1.2.3")))
            .expect("remove");
        put(&store, &deployment("canary", Some("1.2.4"), &["other"]));
        assert_eq!(mirrors(), written, "no change of the members writes them");
        assert_eq!(
            store.load().expect("load"),
            [deployment("canary", Some("1.2.4"), &["extra", "other"])],
            "the members are the rows'"
        );
    }

    /// Migration 5 moves the members out of the Deployments' columns into rows of their own, and
    /// the Deployments load as the release before left them.
    /// Verifies: ADR-0041
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
    /// Verifies: ADR-0041, ADR-0027
    #[test]
    fn two_packages_of_one_type_added_at_once_add_one() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        store_members(&test);
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
        let sessions = |condition: &str| test.sessions(condition);
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

    /// The deletion of a Configuration or a Package takes it out of every Deployment that holds it
    /// in the transaction that deletes it: nothing the fleet does after, which a crash would cut
    /// off, is needed for no Deployment to name it.
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-two-versions-run-side-by-side, ADR-0026
    #[test]
    fn a_deleted_member_leaves_every_deployment_in_the_transaction_that_deletes_it() {
        use crate::configs::{ConfigBackend as _, Removal};
        use crate::packages::PackageBackend as _;
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        store_members(&test);
        let store = PgDeploymentBackend::new(database.clone());
        put(
            &store,
            &deployment("canary", Some("1.2.3"), &["base", "extra"]),
        );
        put(&store, &deployment("stable", Some("1.2.3"), &["base"]));

        assert_eq!(
            crate::postgres::configs::PgConfigBackend::new(database.clone()).remove("base"),
            Ok(Removal::Removed)
        );
        crate::postgres::packages::PgPackageBackend::new(database)
            .remove(&id("1.2.3"))
            .expect("the Package is deleted");
        assert_eq!(
            store.load().expect("load"),
            [
                deployment("canary", None, &["extra"]),
                deployment("stable", None, &[])
            ]
        );
    }

    /// A row that does not decode fails the load naming it (ADR-0044 clause 6).
    /// Verifies: ADR-0044
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

    /// A deletion of a Deployment through B while a rollout through A that releases through it
    /// has not yet committed waits for the rollout, and then refuses it from the assignment the
    /// rollout stored: the rollout holds the Deployment's row until it commits.
    /// Verifies: ADR-0041, ADR-0026
    #[test]
    fn a_deletion_waits_for_a_rollout_through_the_deployment_and_is_refused() {
        use crate::agent_store::{tests::record, AgentStore as _};
        use crate::postgres::agents::PgAgentStore;
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        let (agents, b) = (
            PgAgentStore::new(database.clone()),
            PgDeploymentBackend::new(database),
        );
        put(&b, &deployment("stable", None, &[]));
        let uid = opamp::uid::InstanceUid::default();
        let mut reported = record();
        reported.assignment = None;
        agents.put(&uid, &reported).expect("report");
        let key = 0x19_0003;
        let gate = test.hold_assignment_writes(key);
        let mut released = record();
        if let Some(assignment) = released.assignment.as_mut() {
            assignment.package = None;
            assignment.configurations.clear();
        }
        let rollout = std::thread::spawn(move || agents.release(&[], &[(uid, released)]));
        // A has written the assignment, and waits at the gate with its transaction open.
        let a_pid = test.once(&format!(
            "SELECT pid::text FROM pg_locks WHERE locktype = 'advisory' AND NOT granted \
             AND objid::text = '{key}'"
        ));
        let deletion = std::thread::spawn(move || b.remove("stable"));
        test.once(&format!(
            "SELECT pid::text FROM pg_stat_activity WHERE {a_pid} = ANY(pg_blocking_pids(pid))"
        ));
        gate.open();
        assert_eq!(rollout.join().expect("A").map(|wrote| wrote.len()), Ok(1));
        assert_eq!(
            deletion.join().expect("B"),
            Err(DeploymentError::Conflict(crate::deployments::pinned(
                "stable", 1
            )))
        );
        assert_eq!(
            test.rows("SELECT name FROM deployments"),
            [[Some("stable".to_string())]]
        );
    }
}
