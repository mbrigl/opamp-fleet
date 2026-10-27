//! The floors in PostgreSQL (ADR-0028 clause 15): the PostgreSQL adapter behind
//! [`FloorStore`](crate::floors::FloorStore) — one row per Agent type.

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio_postgres::GenericClient;

use super::{client, sql, Database};
use crate::floors::{Floor, FloorStore};

/// The floors in `package_floors`.
pub struct PgFloorStore {
    database: Arc<Database>,
}

impl PgFloorStore {
    /// The adapter on `database`.
    #[must_use]
    pub fn new(database: Arc<Database>) -> Self {
        PgFloorStore { database }
    }
}

impl FloorStore for PgFloorStore {
    fn load(&self) -> Result<BTreeMap<String, Floor>, String> {
        self.database.run(|pool| async move {
            let client = client(&pool).await?;
            read_all(&**client).await
        })
    }

    fn put(&self, agent_type: &str, floor: &Floor) -> Result<(), String> {
        let (agent_type, floor) = (agent_type.to_string(), floor.clone());
        self.database.run(move |pool| async move {
            let client = client(&pool).await?;
            write(&**client, &agent_type, &floor).await
        })
    }

    fn remove(&self, agent_type: &str) -> Result<(), String> {
        let agent_type = agent_type.to_string();
        self.database.run(move |pool| async move {
            let client = client(&pool).await?;
            client
                .execute(
                    "DELETE FROM package_floors WHERE agent_type = $1",
                    &[&agent_type],
                )
                .await
                .map_err(sql)?;
            Ok(())
        })
    }
}

/// Every floor the table holds, by Agent type.
pub(crate) async fn read_all(
    client: &impl GenericClient,
) -> Result<BTreeMap<String, Floor>, String> {
    Ok(client
        .query(
            "SELECT agent_type, version, signature FROM package_floors",
            &[],
        )
        .await
        .map_err(sql)?
        .into_iter()
        .map(|row| {
            (
                row.get::<_, String>(0),
                Floor {
                    version: row.get(1),
                    signature: row.get(2),
                },
            )
        })
        .collect())
}

/// Stores the floor of `agent_type`, replacing the one stored, in one statement.
pub(crate) async fn write(
    client: &impl GenericClient,
    agent_type: &str,
    floor: &Floor,
) -> Result<(), String> {
    client
        .execute(
            "INSERT INTO package_floors (agent_type, version, signature) VALUES ($1, $2, $3) \
             ON CONFLICT (agent_type) DO UPDATE SET version = EXCLUDED.version, \
             signature = EXCLUDED.signature",
            &[&agent_type, &floor.version, &floor.signature],
        )
        .await
        .map_err(sql)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::floors::{FloorError, PackageFloors};
    use crate::fs::FsFloorStore;
    use crate::packages::SignatureState;
    use crate::postgres::testing::TestDatabase;

    /// The floors through the port on either store: none at first; a `PUT` stored and read back
    /// by a Server opened anew; one below the stored floor refused and nothing changed; a
    /// `DELETE` gone after a reopen.
    fn contract(open: &dyn Fn() -> Box<dyn FloorStore>) {
        let floors = PackageFloors::open(open()).expect("open");
        assert!(floors.all().is_empty());
        assert_eq!(
            floors.put("otelcol", "2.0.0", vec![7; 64]),
            Ok(SignatureState::Unchecked)
        );
        floors.put("promtail", "3.1.0", vec![8; 64]).expect("put");
        let reopened = PackageFloors::open(open()).expect("reopen");
        assert_eq!(
            reopened.all().get("otelcol"),
            Some(&Floor {
                version: "2.0.0".to_string(),
                signature: vec![7; 64],
            })
        );
        assert_eq!(
            reopened.put("otelcol", "1.0.0", vec![9; 64]),
            Err(FloorError::Lower("2.0.0".to_string()))
        );
        assert_eq!(
            PackageFloors::open(open())
                .expect("reopen")
                .all()
                .get("otelcol")
                .map(|floor| floor.version.clone()),
            Some("2.0.0".to_string())
        );
        reopened
            .put("otelcol", "2.1.0", vec![9; 64])
            .expect("raise");
        assert_eq!(reopened.remove("promtail"), Ok(true));
        let after = PackageFloors::open(open()).expect("reopen").all();
        assert_eq!(after.keys().collect::<Vec<_>>(), ["otelcol"]);
        assert_eq!(after["otelcol"].version, "2.1.0");
    }

    /// Verifies: ADR-0028
    #[test]
    fn the_floor_port_contract_holds_on_the_filesystem() {
        let dir = tempfile::tempdir().expect("tempdir");
        contract(&|| Box::new(FsFloorStore::open(dir.path())));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.path().join("package-floors.json"))
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::write(dir.path().join("package-floors.json"), "{").expect("corrupt");
        let error = PackageFloors::open(Box::new(FsFloorStore::open(dir.path())))
            .map(|_| ())
            .expect_err("refused");
        assert!(error.contains("package-floors.json"), "{error}");
    }

    /// Verifies: ADR-0028
    #[test]
    fn the_floor_port_contract_holds_on_postgres() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database = test.open();
        contract(&|| Box::new(PgFloorStore::new(database.clone())));
    }
}
