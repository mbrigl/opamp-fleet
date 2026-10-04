//! `supervisor.toml` on disk (ADR-0025): reading the file, the directories it names made absolute
//! against the working directory, and the identity the state directory holds. What the file must
//! say is [`config`](crate::config)'s; this is only where it comes from.

use std::path::{Path, PathBuf};

use crate::config::{redact_secrets, ClientConfig, LEGACY_CONFIG_FILE_NAME};

impl ClientConfig {
    /// Loads the file, or the defaults when it does not exist. A file that exists but does not
    /// parse is an error — never silently ignored.
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            legacy_name_beside(path)?;
            let default = ClientConfig::default();
            return Ok(ClientConfig {
                path: Some(path.to_path_buf()),
                state_dir: absolute(&default.state_dir),
                ..default
            });
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let mut config: ClientConfig =
            toml::from_str(&text).map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
        // Redacted once, here, so no later reader can reach for the unredacted text by mistake:
        // everything downstream — the effective-configuration report above all — sees the mask.
        config.source = Some(redact_secrets(&text));
        config.path = Some(path.to_path_buf());
        // **Every directory this Client derives is made absolute here**, and this is the one place
        // it can be done once. Since ADR-0010 a Managed Process starts in its own directory, so a
        // path the Client hands it — its program, a `--config` a plugin builds, a `${config_dir}`
        // it substitutes — is resolved by that process against a directory the Client has left.
        // `state_dir` defaults to the relative `client-state`, so leaving these relative made the
        // ordinary configuration the broken one: the program was looked for under itself, and a
        // Collector that did start could not find the configuration written for it.
        config.state_dir = absolute(&config.state_dir);
        config.supervisor_dir = config.supervisor_dir.as_deref().map(absolute);
        // Each allowed download source is held to ADR-0018's rules now, not at the first offer.
        if let Some(packages) = &config.packages {
            for entry in &packages.allowed_sources {
                crate::packages::parse_source(entry)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
            }
        }
        config.checked(path)
    }

    /// What this Client must hold before it connects (ADR-0026, ADR-0028): the fleet credential, and
    /// a client certificate — the one the Server issued, or the one `[tls]` names, a bootstrap
    /// certificate included. Without either the Server would refuse it, so the Client refuses to
    /// start, naming what is missing.
    ///
    /// # Errors
    /// Returns a sentence naming the missing setting.
    pub fn check_admission(&self) -> Result<(), String> {
        if self.authorization_value()?.is_none() {
            return Err(
                "[auth] is required — the Server admits no Agent without the fleet credential \
                 (bearer_token, or username and password)"
                    .to_string(),
            );
        }
        if self.client_identity().is_none() {
            return Err(
                "[tls] cert_file and key_file are required — the Server admits no Agent without \
                 a client certificate; a bootstrap certificate enrols this host"
                    .to_string(),
            );
        }
        Ok(())
    }

    /// The client certificate and key this Client presents on both transports (ADR-0026), or
    /// `None` when it has no identity to present.
    ///
    /// A pair the Server issued outranks the configured one, the same precedence persisted
    /// connection settings have over `supervisor.toml` (ADR-0027): the file stays what the operator
    /// wrote, and deleting the stored pair reverts to it. That is also what retires a bootstrap
    /// certificate — it keeps standing in `supervisor.toml`, unused, once a real one has been issued.
    pub fn client_identity(&self) -> Option<(PathBuf, PathBuf)> {
        let cert = self.state_dir.join(crate::tls::ISSUED_CERT_FILE);
        let key = self.state_dir.join(crate::tls::ISSUED_KEY_FILE);
        if cert.exists() && key.exists() {
            return Some((cert, key));
        }
        let tls = self.tls.as_ref()?;
        Some((tls.cert_file.clone()?, tls.key_file.clone()?))
    }
}

/// Refuses to carry on when the configuration is only *missing* because it was renamed
/// (ADR-0029): the file this Client looks for is absent and a `supervisor.toml` — what it was called
/// until ADR-0029 — sits where it would be.
///
/// Everywhere else a missing configuration is not an error: a Client comes up on defaults, says so,
/// and manages nothing until one exists (ADR-0028). That is exactly the wrong answer here, and the
/// dangerous one: an upgraded host would go on running, connect to the development endpoint, report
/// none of the Agents it used to, and nothing about it would look like a failure. So this one case
/// fails closed, naming both paths and the single command that fixes it.
fn legacy_name_beside(path: &Path) -> Result<(), String> {
    let legacy = path.with_file_name(LEGACY_CONFIG_FILE_NAME);
    if path
        .file_name()
        .is_some_and(|name| name == LEGACY_CONFIG_FILE_NAME)
        || !legacy.exists()
    {
        return Ok(());
    }
    Err(format!(
        "no configuration at {}, but {} is beside it: the file was renamed in this release \
         (ADR-0029). Rename it — `mv {} {}` — and start the service again. Nothing else about it \
         changed.",
        path.display(),
        legacy.display(),
        legacy.display(),
        path.display()
    ))
}

/// `path` against the current working directory when it is relative, unchanged when it is not.
///
/// Lexical rather than `canonicalize`: that needs the file to exist, and these directories are
/// named before they are created. It also follows symbolic links, which would be wrong here — the
/// versioned install layout (ADR-0028) points at its current version *with* a link, and resolving
/// it would freeze a path that stops being true at the next update.
pub(crate) fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(path),
        // Nothing to be relative to: hand the path over as written and let the failure name the
        // real reason rather than inventing a directory.
        Err(_) => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Client without the fleet credential, or without a certificate, does not start: the Server
    /// would refuse it at admission or in the handshake anyway (ADR-0026 clause 3).
    /// Verifies: ADR-0026, Q-1
    #[test]
    fn a_client_without_its_credential_or_a_certificate_does_not_start() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cert = dir.path().join("c.pem");
        let key = dir.path().join("k.pem");
        std::fs::write(&cert, "cert").expect("write");
        std::fs::write(&key, "key").expect("write");
        let state = dir.path().join("state").display().to_string();

        let bare: ClientConfig =
            toml::from_str(&format!("state_dir = {state:?}\n")).expect("parse");
        let err = bare.check_admission().expect_err("no credential");
        assert!(err.contains("[auth] is required"), "{err}");

        let with_auth: ClientConfig = toml::from_str(&format!(
            "state_dir = {state:?}\n[auth]\nbearer_token = \"t\"\n"
        ))
        .expect("parse");
        let err = with_auth.check_admission().expect_err("no certificate");
        assert!(err.contains("cert_file and key_file are required"), "{err}");

        let complete: ClientConfig = toml::from_str(&format!(
            "state_dir = {state:?}\n[auth]\nbearer_token = \"t\"\n[tls]\ncert_file = {:?}\nkey_file = {:?}\n",
            cert.display().to_string(),
            key.display().to_string()
        ))
        .expect("parse");
        complete
            .check_admission()
            .expect("the credential and a certificate");
    }
}
