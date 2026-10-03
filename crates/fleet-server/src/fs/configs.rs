//! Configurations on the filesystem (ADR-0016, ADR-0027): the default adapter behind
//! [`ConfigBackend`](crate::configs::ConfigBackend) — one JSON file per Configuration, written in
//! one step.

use std::path::PathBuf;

use crate::configs::{ConfigBackend, Configuration};

/// One JSON file per Configuration under the configuration directory, named after it.
pub struct FsConfigBackend {
    dir: PathBuf,
}

impl FsConfigBackend {
    /// Opens the directory, creating it when it is missing.
    pub fn open(dir: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        Ok(FsConfigBackend { dir })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.json"))
    }
}

impl ConfigBackend for FsConfigBackend {
    /// A file that does not parse is a startup error that names it, and that includes a file in a
    /// shape this Server no longer writes: there is no legacy reader, so it is named rather than
    /// guessed at.
    fn load(&self) -> Result<Vec<Configuration>, String> {
        let mut configs = Vec::new();
        let entries = std::fs::read_dir(&self.dir)
            .map_err(|e| format!("cannot read {}: {e}", self.dir.display()))?;
        for entry in entries {
            let path = entry
                .map_err(|e| format!("cannot read {}: {e}", self.dir.display()))?
                .path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let config: Configuration = serde_json::from_str(&text)
                .map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
            configs.push(config);
        }
        Ok(configs)
    }

    fn put(&self, config: &Configuration) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(config).expect("a Configuration serializes");
        super::replace(&self.path(&config.name), &json)
    }

    fn remove(&self, name: &str) -> Result<(), String> {
        let path = self.path(name);
        std::fs::remove_file(&path).map_err(|e| format!("cannot delete {}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::configs::tests::{put_assigned, revision, with_role};
    use crate::configs::{ConfigStore, ROLE_SUPPLEMENTARY};

    fn open(dir: &std::path::Path) -> Result<ConfigStore, String> {
        ConfigStore::open(Box::new(FsConfigBackend::open(dir.to_path_buf())?))
    }

    /// What one store wrote, a store opened on the same directory reads — assignments' pinned
    /// revisions included — and a deletion removes the file.
    #[test]
    fn the_store_round_trips_through_the_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = open(dir.path()).expect("open");
        let assignments = put_assigned(&store, "base", revision(&[], "receivers: {}\n"));
        store
            .put_saved("other", revision(&[], "exporters: {}\n"))
            .expect("put");

        let reopened = open(dir.path()).expect("reopen");
        assert_eq!(reopened.list().len(), 2);
        assert_eq!(
            reopened.compose(&assignments).expect("pinned").entries[0].body,
            "receivers: {}\n"
        );
        assert!(reopened.delete("base").expect("delete"));
        assert!(!dir.path().join("base.json").exists());
        assert_eq!(open(dir.path()).expect("open").list().len(), 1);
    }

    /// No legacy reader: a file in a shape this Server no longer writes is a startup error that
    /// names the path, not a file quietly read as something else. Both retired shapes are covered
    /// — the flat pre-ADR-0027 record and the two-revision ADR-0027 one.
    #[test]
    fn a_file_in_a_retired_shape_refuses_to_open_and_names_it() {
        for (file, body) in [
            (
                "flat.json",
                r#"{"name":"flat","selector":{"os.type":"linux"},"body":"receivers: {}\n"}"#,
            ),
            (
                "staged.json",
                r#"{"name":"staged","draft":{"body":"v2\n"},"published":{"body":"v1\n"}}"#,
            ),
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            std::fs::write(dir.path().join(file), body).expect("write the retired shape");

            let error = open(dir.path())
                .map(|_| ())
                .expect_err("a retired shape is refused, never guessed at");
            assert!(
                error.contains(file),
                "the error must name the file an operator has to deal with, got: {error}"
            );
        }
    }

    #[test]
    fn a_role_survives_a_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = open(dir.path()).expect("open");
        put_assigned(
            &store,
            "certs",
            with_role(revision(&[], "PEM\n"), ROLE_SUPPLEMENTARY),
        );
        let reopened = open(dir.path()).expect("reopen");
        assert_eq!(
            reopened.get("certs").expect("certs").saved.role,
            ROLE_SUPPLEMENTARY
        );
    }
}
