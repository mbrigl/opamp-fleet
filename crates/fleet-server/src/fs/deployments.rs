//! Deployments on the filesystem (ADR-0021): the default adapter behind
//! [`DeploymentBackend`](crate::deployments::DeploymentBackend).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::configs::validate_name;
use crate::deployments::{check_selector, Deployment, DeploymentBackend};
use crate::packages::{PackageId, Platform};

/// One JSON file per Deployment under `<packages_dir>/deployments/`, owner-only.
///
/// It lives beside the Packages rather than beside the Configurations because a Deployment is
/// meaningless without the artifacts it signs — one directory is one backup — and it needs no
/// configuration key of its own: it is armed by `packages_dir`, exactly as the package store is.
pub struct FsDeploymentBackend {
    dir: PathBuf,
}

impl FsDeploymentBackend {
    /// Opens the directory, creating it owner-only.
    pub fn open(dir: PathBuf) -> Result<Self, String> {
        super::create_private_dir(&dir)?;
        Ok(FsDeploymentBackend { dir })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.json"))
    }
}

impl DeploymentBackend for FsDeploymentBackend {
    fn load(&self) -> Result<Vec<Deployment>, String> {
        let mut deployments = Vec::new();
        let listing = std::fs::read_dir(&self.dir)
            .map_err(|e| format!("cannot read {}: {e}", self.dir.display()))?;
        for entry in listing {
            let path = entry
                .map_err(|e| format!("cannot read {}: {e}", self.dir.display()))?
                .path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let meta: DeploymentMeta = serde_json::from_str(&text)
                .map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
            let deployment = meta.into_deployment(&path)?;
            // The file name is derived from the name; a mismatch means a write would land
            // somewhere else, so it is refused rather than silently corrected.
            if path.file_stem().and_then(|n| n.to_str()) != Some(deployment.name.as_str()) {
                return Err(format!(
                    "{} does not match the name {:?} it states — rename the file or fix it",
                    path.display(),
                    deployment.name
                ));
            }
            deployments.push(deployment);
        }
        Ok(deployments)
    }

    fn put(&self, deployment: &Deployment) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(&DeploymentMeta::of(deployment))
            .expect("deployment serializes");
        // Owner-only, like the directory: a Selector says which hosts an operator considers a
        // channel, which is not another local user's business.
        super::replace_owner_only(&self.path(&deployment.name), &bytes)
    }

    fn remove(&self, name: &str) -> Result<(), String> {
        let path = self.path(name);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("cannot delete {}: {e}", path.display())),
        }
    }
}

/// A Deployment as persisted: `<packages_dir>/deployments/<name>.json`.
///
/// The signatures are a **list** rather than a map, because their key is a pair and JSON keys are
/// strings. Folding them into the in-memory map on load keeps the uniqueness where it belongs
/// without inventing a composite key nobody would read.
#[derive(Serialize, Deserialize)]
struct DeploymentMeta {
    name: String,
    selector: BTreeMap<String, String>,
    #[serde(default)]
    packages: Vec<PackageRefMeta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    signatures: Vec<SignatureMeta>,
}

#[derive(Serialize, Deserialize)]
struct PackageRefMeta {
    agent_type: String,
    version: String,
}

#[derive(Serialize, Deserialize)]
struct SignatureMeta {
    agent_type: String,
    version: String,
    os: String,
    arch: String,
    signature_hex: String,
}

impl DeploymentMeta {
    fn of(deployment: &Deployment) -> Self {
        DeploymentMeta {
            name: deployment.name.clone(),
            selector: deployment.selector.clone(),
            packages: deployment
                .packages
                .values()
                .map(|id| PackageRefMeta {
                    agent_type: id.agent_type.clone(),
                    version: id.version.clone(),
                })
                .collect(),
            signatures: deployment
                .signatures
                .iter()
                .map(|((id, platform), signature)| SignatureMeta {
                    agent_type: id.agent_type.clone(),
                    version: id.version.clone(),
                    os: platform.os.clone(),
                    arch: platform.arch.clone(),
                    signature_hex: hex::encode(signature),
                })
                .collect(),
        }
    }

    fn into_deployment(self, path: &std::path::Path) -> Result<Deployment, String> {
        let named = |e: String| format!("invalid deployment in {}: {e}", path.display());
        validate_name(&self.name).map_err(|e| named(format!("name {:?}: {e}", self.name)))?;
        check_selector(&self.selector).map_err(named)?;
        let mut packages = BTreeMap::new();
        for reference in self.packages {
            let id = PackageId::new(&reference.agent_type, &reference.version).map_err(named)?;
            packages.insert(id.agent_type.clone(), id);
        }
        let mut signatures = BTreeMap::new();
        for signature in self.signatures {
            let id = PackageId::new(&signature.agent_type, &signature.version).map_err(named)?;
            let platform = Platform::new(&signature.os, &signature.arch).map_err(named)?;
            let bytes = hex::decode(&signature.signature_hex)
                .map_err(|e| named(format!("signature of {id}: {e}")))?;
            signatures.insert((id, platform), bytes);
        }
        Ok(Deployment {
            name: self.name,
            selector: self.selector,
            packages,
            signatures,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deployments::tests::{channel, id, linux};
    use crate::deployments::DeploymentStore;

    fn open(dir: &std::path::Path) -> Result<DeploymentStore, String> {
        DeploymentStore::open(Box::new(FsDeploymentBackend::open(dir.to_path_buf())?))
    }

    /// The whole store survives a reopen — the signatures included, which is the part that had to
    /// be flattened to be persisted at all.
    #[test]
    fn a_deployment_survives_a_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let telegraf = id("telegraf", "1.30.0");
        {
            let store = open(dir.path()).expect("open");
            channel(&store, "canary", &[("channel", "canary"), ("env", "prod")]);
            store
                .put_package("canary", &telegraf, false)
                .expect("package");
            store
                .put_signature("canary", &telegraf, &linux(), vec![9; 64])
                .expect("signature");
        }

        let reopened = open(dir.path()).expect("reopen");
        let canary = reopened.get("canary").expect("canary");
        assert_eq!(canary.selector["channel"], "canary");
        assert_eq!(canary.selector["env"], "prod");
        assert_eq!(canary.package_for("telegraf"), Some(&telegraf));
        assert_eq!(canary.signature(&telegraf, &linux()), Some(&[9u8; 64][..]));

        assert!(reopened.delete("canary").expect("delete"));
        assert!(!reopened
            .delete("canary")
            .expect("deleting twice is not an error"));
        assert!(open(dir.path()).expect("reopen").is_empty());
    }

    /// A file this Server did not write fails the open, naming it. A channel that silently vanished
    /// would withdraw nothing and offer nothing, and say neither.
    #[test]
    fn an_unreadable_file_fails_the_open_and_names_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("broken.json"), "{").expect("write");
        let error = open(dir.path()).map(|_| ()).expect_err("refused");
        assert!(error.contains("broken.json"), "{error}");

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("stable.json"),
            r#"{"name":"canary","selector":{"channel":"canary"}}"#,
        )
        .expect("write");
        let error = open(dir.path())
            .map(|_| ())
            .expect_err("a file whose name disagrees with its content is refused");
        assert!(error.contains("stable.json"), "{error}");
    }

    /// The store is owner-only, and so is every file in it: a Selector says which hosts a fleet
    /// operator considers a channel, which is not another local user's business.
    #[cfg(unix)]
    #[test]
    fn the_store_and_its_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let store = open(dir.path()).expect("open");
        channel(&store, "stable", &[("channel", "stable")]);
        assert_eq!(
            std::fs::metadata(dir.path())
                .expect("dir")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(dir.path().join("stable.json"))
                .expect("file")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
