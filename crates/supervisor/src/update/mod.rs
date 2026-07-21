//! In-place self-update of the Supervisor Host (ADR-0007).
//!
//! The mechanism installs each version side by side and switches an atomic `current` pointer — it
//! never overwrites the running binary. This module drives the service only through the
//! [`ServiceControl`](crate::service::ServiceControl) seam, so the swap/rollback orchestration is
//! decoupled from the service backends and unit-testable with a fake control.
//!
//! Sequence ([`apply_update`]): write the marker → reset the health signal → stop → repoint `current`
//! → start → two-tier health gate → commit (clear marker) or roll back (repoint to the previous
//! version). An update interrupted by a crash is recovered on the next startup by
//! [`resume_if_pending`].
//!
//! Scope: the update is applied by a separate process from the running daemon (the operator's
//! `update` invocation), which satisfies the specification's process boundary. The daemon
//! *self-triggering* its own update via a detached, supervision-escaping updater is only needed once
//! the Server delivers packages over the wire, and is deferred to ADR-0008 together with that work.

pub mod health;
pub mod layout;
pub mod marker;
pub mod verify;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use tracing::{info, warn};

use crate::service::runtime::RuntimeConfig;
use crate::service::{NativeService, ServiceControl, ServiceLevel};
use health::{FileHealthGate, HealthGate};
use layout::Layout;
use marker::UpdateMarker;

/// A version staged and verified on disk, ready to switch to.
#[derive(Debug, Clone)]
pub struct StagedVersion {
    /// SHA-256 of the staged binary.
    pub hash: String,
    /// The version directory it was staged into.
    pub dir: PathBuf,
}

/// What to switch to, and where to fall back to.
#[derive(Debug, Clone)]
pub struct UpdatePlan {
    /// SHA-256 of the target version.
    pub target_hash: String,
    /// The target version directory.
    pub target_dir: PathBuf,
    /// The version to roll back to on failure, if any.
    pub previous_dir: Option<PathBuf>,
    /// How long to wait for the new version to prove healthy.
    pub settle: Duration,
}

/// The result of applying an update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateOutcome {
    /// The new version passed the health gate and is now current.
    Committed,
    /// The new version failed the health gate; the previous version was restored.
    RolledBack,
}

/// The result of a startup recovery check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeOutcome {
    /// No update was in progress.
    Nothing,
    /// An interrupted update had already switched `current` to the target; the marker is cleared.
    Committed,
    /// An interrupted update had not switched over; `current` is left as-is and the marker cleared.
    Aborted,
}

/// Lay out the versioned install for a fresh `service install`: stage the current executable into a
/// version directory and point `current` at it. Returns the program path the service should run
/// (`current/supervisor-host`).
///
/// # Errors
/// Returns an error if the current executable cannot be resolved or staged, or the pointer cannot be
/// created.
pub fn install_layout(layout: &Layout) -> Result<PathBuf> {
    let exe = std::env::current_exe().context("resolving the current executable path")?;
    let staged = stage_and_verify(layout, &exe, None)?;
    layout.set_current(&staged.dir)?;
    Ok(layout.current_binary())
}

/// Stage `new_binary` into its content-addressed version directory, verifying its SHA-256 (against
/// `expected` when provided). The staged binary is made executable on Unix.
///
/// # Errors
/// Returns an error if the binary cannot be read, its hash does not match `expected`, or it cannot
/// be written into the layout.
pub fn stage_and_verify(
    layout: &Layout,
    new_binary: &Path,
    expected: Option<&str>,
) -> Result<StagedVersion> {
    let bytes = std::fs::read(new_binary)
        .with_context(|| format!("reading the new binary {}", new_binary.display()))?;
    let hash = verify::sha256_hex(&bytes);
    if let Some(expected) = expected {
        ensure!(
            hash.eq_ignore_ascii_case(expected),
            "content hash mismatch: expected {expected}, got {hash}"
        );
    }

    let dir = layout.version_dir(&hash);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let dest = Layout::binary_in(&dir);
    std::fs::write(&dest, &bytes).with_context(|| format!("staging into {}", dest.display()))?;
    set_executable(&dest)?;

    Ok(StagedVersion { hash, dir })
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .with_context(|| format!("reading permissions of {}", path.display()))?
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)
        .with_context(|| format!("marking {} executable", path.display()))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

/// Apply an update across the process boundary: stop, repoint `current`, restart, health-gate, and
/// commit or roll back. Generic over the service control and health gate so it is testable with
/// fakes.
///
/// # Errors
/// Returns an error if any file/pointer/service step fails.
pub fn apply_update<S: ServiceControl, G: HealthGate>(
    service: &S,
    gate: &G,
    layout: &Layout,
    plan: &UpdatePlan,
) -> Result<UpdateOutcome> {
    // Record the in-flight update before touching anything, so a crash is recoverable.
    marker::write_atomic(
        &layout.marker(),
        &UpdateMarker {
            target_hash: plan.target_hash.clone(),
            target_dir: plan.target_dir.clone(),
            previous_dir: plan.previous_dir.clone(),
        },
    )?;

    gate.reset()?;
    service
        .stop()
        .context("stopping the service to switch versions")?;
    layout.set_current(&plan.target_dir)?;
    service.start().context("starting the new version")?;

    if gate.await_healthy(plan.settle)? {
        marker::clear(&layout.marker())?;
        return Ok(UpdateOutcome::Committed);
    }

    // The new version did not prove healthy — roll back to the previous one.
    gate.reset()?;
    service
        .stop()
        .context("stopping the unhealthy new version")?;
    if let Some(previous) = &plan.previous_dir {
        layout.set_current(previous)?;
    }
    service.start().context("restarting the previous version")?;
    marker::clear(&layout.marker())?;
    Ok(UpdateOutcome::RolledBack)
}

/// Recover from an update interrupted by a crash or power loss, using the leftover marker: if
/// `current` already resolves to the marker's target the switch had completed (commit); otherwise it
/// had not (aborted). Either way the marker is cleared.
///
/// # Errors
/// Returns an error if the marker or pointer cannot be read.
pub fn resume_if_pending(layout: &Layout) -> Result<ResumeOutcome> {
    let Some(marker) = marker::read(&layout.marker())? else {
        return Ok(ResumeOutcome::Nothing);
    };
    let current = layout.current_target()?;
    let outcome = if current.as_deref() == Some(marker.target_dir.as_path()) {
        ResumeOutcome::Committed
    } else {
        ResumeOutcome::Aborted
    };
    marker::clear(&layout.marker())?;
    Ok(outcome)
}

/// The `update` subcommand: stage and verify a new binary, then apply it, retaining the current and
/// previous versions and pruning older ones.
///
/// # Errors
/// Returns an error if staging, verification, or applying the update fails.
pub fn run_update(
    config: &RuntimeConfig,
    level: ServiceLevel,
    new_binary: &Path,
    expected_hash: Option<&str>,
    settle: Duration,
) -> Result<()> {
    let layout = Layout::new(&config.state_dir);
    let staged = stage_and_verify(&layout, new_binary, expected_hash)?;
    let previous_dir = layout.current_target()?;

    let service = NativeService::new(level);
    let gate = FileHealthGate::new(layout.health_file(), config.endpoint.clone());
    let plan = UpdatePlan {
        target_hash: staged.hash.clone(),
        target_dir: staged.dir.clone(),
        previous_dir: previous_dir.clone(),
        settle,
    };

    let outcome = apply_update(&service, &gate, &layout, &plan)?;

    // Keep the (now current) target and the previous version; prune the rest.
    let mut keep = vec![staged.dir.clone()];
    if let Some(previous) = previous_dir {
        keep.push(previous);
    }
    layout.prune(&keep)?;

    match outcome {
        UpdateOutcome::Committed => info!(hash = %staged.hash, "self-update applied"),
        UpdateOutcome::RolledBack => {
            warn!(hash = %staged.hash, "self-update failed the health gate; rolled back")
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::cell::RefCell;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("opamp-update-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    /// Records the service calls made during an update, so ordering can be asserted.
    ///
    /// Only the pointer-switch tests below drive a service, and those are Unix-only (see
    /// [`Layout::current_target`]), so this fake is scoped to match.
    #[cfg(unix)]
    #[derive(Default)]
    struct FakeService {
        calls: RefCell<Vec<&'static str>>,
    }
    #[cfg(unix)]
    impl ServiceControl for FakeService {
        fn start(&self) -> Result<()> {
            self.calls.borrow_mut().push("start");
            Ok(())
        }
        fn stop(&self) -> Result<()> {
            self.calls.borrow_mut().push("stop");
            Ok(())
        }
        fn state(&self) -> Result<crate::service::ServiceState> {
            Ok(crate::service::ServiceState::Running)
        }
    }

    #[cfg(unix)]
    struct FakeGate {
        healthy: bool,
    }
    #[cfg(unix)]
    impl HealthGate for FakeGate {
        fn reset(&self) -> Result<()> {
            Ok(())
        }
        fn await_healthy(&self, _settle: Duration) -> Result<bool> {
            Ok(self.healthy)
        }
    }

    #[cfg(unix)]
    fn version(layout: &Layout, hash: &str) -> PathBuf {
        let dir = layout.version_dir(hash);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    #[test]
    fn healthy_update_commits_and_points_at_target() {
        let tmp = TempDir::new();
        let layout = Layout::new(&tmp.0);
        let previous = version(&layout, "old");
        layout.set_current(&previous).unwrap();
        let target = version(&layout, "new");

        let service = FakeService::default();
        let outcome = apply_update(
            &service,
            &FakeGate { healthy: true },
            &layout,
            &UpdatePlan {
                target_hash: "new".to_string(),
                target_dir: target.clone(),
                previous_dir: Some(previous),
                settle: Duration::from_millis(1),
            },
        )
        .unwrap();

        assert_eq!(outcome, UpdateOutcome::Committed);
        assert_eq!(
            layout.current_target().unwrap().as_deref(),
            Some(target.as_path())
        );
        assert_eq!(*service.calls.borrow(), vec!["stop", "start"]);
        assert_eq!(marker::read(&layout.marker()).unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn unhealthy_update_rolls_back_to_previous() {
        let tmp = TempDir::new();
        let layout = Layout::new(&tmp.0);
        let previous = version(&layout, "old");
        layout.set_current(&previous).unwrap();
        let target = version(&layout, "new");

        let service = FakeService::default();
        let outcome = apply_update(
            &service,
            &FakeGate { healthy: false },
            &layout,
            &UpdatePlan {
                target_hash: "new".to_string(),
                target_dir: target,
                previous_dir: Some(previous.clone()),
                settle: Duration::from_millis(1),
            },
        )
        .unwrap();

        assert_eq!(outcome, UpdateOutcome::RolledBack);
        assert_eq!(
            layout.current_target().unwrap().as_deref(),
            Some(previous.as_path())
        );
        // stop → switch → start → (unhealthy) → stop → rollback → start
        assert_eq!(
            *service.calls.borrow(),
            vec!["stop", "start", "stop", "start"]
        );
        assert_eq!(marker::read(&layout.marker()).unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn resume_detects_completed_switch() {
        let tmp = TempDir::new();
        let layout = Layout::new(&tmp.0);
        let target = version(&layout, "new");
        layout.set_current(&target).unwrap();
        marker::write_atomic(
            &layout.marker(),
            &UpdateMarker {
                target_hash: "new".to_string(),
                target_dir: target,
                previous_dir: None,
            },
        )
        .unwrap();

        assert_eq!(
            resume_if_pending(&layout).unwrap(),
            ResumeOutcome::Committed
        );
        assert_eq!(marker::read(&layout.marker()).unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn resume_detects_aborted_switch() {
        let tmp = TempDir::new();
        let layout = Layout::new(&tmp.0);
        let previous = version(&layout, "old");
        let target = version(&layout, "new");
        layout.set_current(&previous).unwrap(); // never switched to target
        marker::write_atomic(
            &layout.marker(),
            &UpdateMarker {
                target_hash: "new".to_string(),
                target_dir: target,
                previous_dir: Some(previous),
            },
        )
        .unwrap();

        assert_eq!(resume_if_pending(&layout).unwrap(), ResumeOutcome::Aborted);
    }

    #[test]
    fn resume_is_a_noop_without_a_marker() {
        let tmp = TempDir::new();
        let layout = Layout::new(&tmp.0);
        assert_eq!(resume_if_pending(&layout).unwrap(), ResumeOutcome::Nothing);
    }

    #[test]
    fn stage_and_verify_writes_content_addressed_binary() {
        let tmp = TempDir::new();
        let layout = Layout::new(&tmp.0);
        let source = tmp.0.join("incoming");
        std::fs::write(&source, b"#!/bin/true\n").unwrap();
        let expected = verify::sha256_hex(b"#!/bin/true\n");

        let staged = stage_and_verify(&layout, &source, Some(&expected)).unwrap();

        assert_eq!(staged.hash, expected);
        assert_eq!(staged.dir, layout.version_dir(&expected));
        assert!(Layout::binary_in(&staged.dir).exists());
        // A wrong expected hash is rejected.
        assert!(stage_and_verify(&layout, &source, Some("deadbeef")).is_err());
    }
}
