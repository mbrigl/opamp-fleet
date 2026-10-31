//! Deployments in PostgreSQL (ADR-0029 clause 6): the PostgreSQL adapter behind
//! [`DeploymentBackend`](crate::deployments::DeploymentBackend) — one row per Deployment, each
//! call one transaction.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio_postgres::GenericClient;

use super::{client, sql, Database};
use crate::deployments::{Deployment, DeploymentBackend};

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

/// One Package a Deployment holds, as its `packages` column keeps it.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageRef {
    agent_type: String,
    version: String,
}

impl DeploymentBackend for PgDeploymentBackend {
    fn load(&self) -> Result<Vec<Deployment>, String> {
        self.database.run(|pool| async move {
            let client = client(&pool).await?;
            read_all(&**client).await
        })
    }

    fn put(&self, deployment: &Deployment) -> Result<(), String> {
        let deployment = deployment.clone();
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            write(&*transaction, &deployment).await?;
            transaction.commit().await.map_err(sql)
        })
    }

    fn remove(&self, name: &str) -> Result<(), String> {
        let name = name.to_string();
        self.database.run(move |pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            transaction
                .execute("DELETE FROM deployments WHERE name = $1", &[&name])
                .await
                .map_err(sql)?;
            transaction.commit().await.map_err(sql)
        })
    }
}

/// Every Deployment. A row that does not decode — a name or Selector a write would refuse, a
/// Package identity that is no token — fails, naming it.
pub(crate) async fn read_all(client: &impl GenericClient) -> Result<Vec<Deployment>, String> {
    let mut deployments = Vec::new();
    for row in client
        .query(
            "SELECT name, selector, packages, configurations FROM deployments ORDER BY name",
            &[],
        )
        .await
        .map_err(sql)?
    {
        let name: String = row.get(0);
        let named = |e: String| format!("deployment {name:?} in the database: {e}");
        let selector: BTreeMap<String, String> =
            serde_json::from_value(row.get(1)).map_err(|e| named(format!("selector: {e}")))?;
        let references: Vec<PackageRef> =
            serde_json::from_value(row.get(2)).map_err(|e| named(format!("packages: {e}")))?;
        let deployment = Deployment::load(
            name.clone(),
            selector,
            references
                .into_iter()
                .map(|reference| (reference.agent_type, reference.version)),
            row.get::<_, Vec<String>>(3),
        )
        .map_err(named)?;
        deployments.push(deployment);
    }
    Ok(deployments)
}

/// Creates or replaces one Deployment inside the caller's transaction.
pub(crate) async fn write(
    client: &impl GenericClient,
    deployment: &Deployment,
) -> Result<(), String> {
    let selector = serde_json::to_value(&deployment.selector).expect("a Selector serializes");
    let packages = serde_json::to_value(
        deployment
            .packages
            .values()
            .map(|id| PackageRef {
                agent_type: id.agent_type.clone(),
                version: id.version.clone(),
            })
            .collect::<Vec<_>>(),
    )
    .expect("a Package reference serializes");
    let configurations: Vec<&str> = deployment
        .configurations
        .iter()
        .map(String::as_str)
        .collect();
    client
        .execute(
            "INSERT INTO deployments (name, selector, packages, configurations) \
             VALUES ($1, $2, $3, $4) ON CONFLICT (name) DO UPDATE SET \
             selector = EXCLUDED.selector, packages = EXCLUDED.packages, \
             configurations = EXCLUDED.configurations",
            &[&deployment.name, &selector, &packages, &configurations],
        )
        .await
        .map_err(sql)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::FsDeploymentBackend;
    use crate::packages::PackageId;
    use crate::postgres::testing::TestDatabase;
    use std::collections::BTreeSet;

    fn deployment(name: &str, package: Option<&str>) -> Deployment {
        Deployment {
            name: name.to_string(),
            selector: BTreeMap::from([("fleet.ring".to_string(), name.to_string())]),
            packages: package
                .map(|version| {
                    let id = PackageId::new("otelcol", version).expect("id");
                    BTreeMap::from([(id.agent_type.clone(), id)])
                })
                .unwrap_or_default(),
            configurations: BTreeSet::from(["base".to_string(), "extra".to_string()]),
        }
    }

    /// The port's contract on either store: what is written is loaded back, a put replaces, and a
    /// removal tolerates absence. A stored Deployment the core refuses fails the load with the
    /// core's words: `rename` gives the one left the name `Stable`, outside the name grammar.
    fn contract(open: &dyn Fn() -> Box<dyn DeploymentBackend>, rename: &dyn Fn()) {
        let store = open();
        assert!(store.load().expect("empty").is_empty());
        store
            .put(&deployment("canary", Some("1.2.3")))
            .expect("put");
        store.put(&deployment("stable", None)).expect("put");
        assert_eq!(
            open().load().expect("load"),
            [
                deployment("canary", Some("1.2.3")),
                deployment("stable", None)
            ]
        );
        store
            .put(&deployment("canary", Some("1.3.0")))
            .expect("replace");
        assert_eq!(
            open().load().expect("load")[0],
            deployment("canary", Some("1.3.0"))
        );
        store.remove("canary").expect("remove");
        store.remove("canary").expect("removing the absent is fine");
        assert_eq!(open().load().expect("load"), [deployment("stable", None)]);

        rename();
        let stable = deployment("stable", None);
        let core = Deployment::load(
            "Stable".to_string(),
            stable.selector,
            [],
            stable.configurations,
        )
        .map(|_| ())
        .expect_err("the core refuses it");
        let err = open().load().map(|_| ()).expect_err("refused");
        assert!(err.ends_with(&core), "{err}");
        let naming = &err[..err.len() - core.len()];
        assert!(
            naming.to_lowercase().contains("stable"),
            "the record is named: {err}"
        );
    }

    /// Verifies: ADR-0029
    #[test]
    fn the_deployment_port_contract_holds_on_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("deployments/stable.json");
        contract(
            &|| Box::new(FsDeploymentBackend::open(dir.path()).expect("open")),
            &|| {
                let mut meta: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&file).expect("read")).expect("json");
                meta["name"] = "Stable".into();
                std::fs::write(&file, meta.to_string()).expect("write");
            },
        );
    }

    /// Verifies: ADR-0029
    #[test]
    fn the_deployment_port_contract_holds_on_postgres() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        contract(
            &|| Box::new(PgDeploymentBackend::new(database.clone())),
            &|| test.sql("UPDATE deployments SET name = 'Stable'"),
        );
    }

    /// A stored Deployment is held to the rules it was written by, on either store: `plant` breaks
    /// the stored `stable` one way, and the load is refused with `expected` in its words, naming
    /// the record. The Deployment is removed afterwards, so the next case starts clean.
    fn refused_on_load(
        open: &dyn Fn() -> Box<dyn DeploymentBackend>,
        plant: &dyn Fn(),
        expected: &str,
    ) {
        let store = open();
        store
            .put(&deployment("stable", Some("1.2.3")))
            .expect("put");
        plant();
        let err = open().load().map(|_| ()).expect_err("refused");
        assert!(err.contains(expected), "{err}");
        assert!(err.contains("stable"), "the record is named: {err}");
        store.remove("stable").expect("remove");
    }

    /// The two rules a write holds a Deployment's Packages to, on load: a Package reference with
    /// a field no write puts there is refused by both stores alike, and so is a second Package of
    /// an Agent type, in the core's words.
    fn packages_are_held_to_the_write_rules(
        open: &dyn Fn() -> Box<dyn DeploymentBackend>,
        unknown_field: &dyn Fn(),
        second_of_a_type: &dyn Fn(),
    ) {
        refused_on_load(open, unknown_field, "unknown field `extra`");
        let stable = deployment("stable", None);
        let core = Deployment::load(
            "stable".to_string(),
            stable.selector,
            [
                ("otelcol".to_string(), "1.2.3".to_string()),
                ("otelcol".to_string(), "1.3.0".to_string()),
            ],
            stable.configurations,
        )
        .map(|_| ())
        .expect_err("the core refuses it");
        refused_on_load(open, second_of_a_type, &core);
    }

    /// Verifies: ADR-0027, ADR-0029
    #[test]
    fn stored_packages_are_held_to_the_write_rules_on_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("deployments/stable.json");
        let edit = |change: &dyn Fn(&mut serde_json::Value)| {
            let mut meta: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&file).expect("read")).expect("json");
            change(&mut meta);
            std::fs::write(&file, meta.to_string()).expect("write");
        };
        packages_are_held_to_the_write_rules(
            &|| Box::new(FsDeploymentBackend::open(dir.path()).expect("open")),
            &|| edit(&|meta| meta["packages"][0]["extra"] = 1.into()),
            &|| {
                edit(&|meta| {
                    meta["packages"]
                        .as_array_mut()
                        .expect("packages")
                        .push(serde_json::json!({"agent_type": "otelcol", "version": "1.3.0"}));
                });
            },
        );
    }

    /// Verifies: ADR-0027, ADR-0029
    #[test]
    fn stored_packages_are_held_to_the_write_rules_on_postgres() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        packages_are_held_to_the_write_rules(
            &|| Box::new(PgDeploymentBackend::new(database.clone())),
            &|| {
                test.sql(
                    r#"UPDATE deployments SET packages =
                       '[{"agent_type": "otelcol", "version": "1.2.3", "extra": 1}]'"#,
                );
            },
            &|| {
                test.sql(
                    r#"UPDATE deployments SET packages =
                       '[{"agent_type": "otelcol", "version": "1.2.3"},
                         {"agent_type": "otelcol", "version": "1.3.0"}]'"#,
                );
            },
        );
    }

    /// A put that fails leaves the Deployment as it was, on either store. `refuse` makes the
    /// write of `canary` fail; `mend` takes that away.
    fn fails_part_way(store: &dyn DeploymentBackend, refuse: &dyn Fn(), mend: &dyn Fn()) {
        let before = deployment("canary", Some("1.2.3"));
        store.put(&before).expect("put");
        refuse();
        store
            .put(&deployment("canary", Some("1.3.0")))
            .expect_err("refused");
        mend();
        assert_eq!(store.load().expect("load"), [before]);
    }

    /// Verifies: ADR-0029
    #[test]
    fn a_put_that_fails_leaves_the_deployment_as_it_was_on_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("deployments/canary.json");
        fails_part_way(
            &FsDeploymentBackend::open(dir.path()).expect("open"),
            &|| crate::fs::testing::refuse(&file),
            &|| crate::fs::testing::mend(&file),
        );
    }

    /// Verifies: ADR-0029
    #[test]
    fn a_put_that_fails_leaves_the_deployment_as_it_was_on_postgres() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        fails_part_way(
            &PgDeploymentBackend::new(test.open()),
            &|| test.refuse("deployments", "NEW.name = 'canary'"),
            &|| test.mend("deployments"),
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
             ('odd', '{\"a\":\"b\"}', '[{\"agent_type\":\"otelcol\"}]', '{}')",
        );
        let err = store.load().expect_err("an empty Selector");
        assert!(err.contains("\"aimless\""), "{err}");
        test.sql("DELETE FROM deployments WHERE name = 'aimless'");
        let err = store.load().expect_err("a reference without a version");
        assert!(err.contains("\"odd\""), "{err}");
    }
}
