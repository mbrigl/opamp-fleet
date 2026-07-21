//! The Windows Service Control Manager (SCM) runtime shim (ADR-0006), compiled only on Windows.
//!
//! Unlike systemd and launchd — which supervise an ordinary foreground process — the Windows SCM
//! launches the service and then expects it to register a control handler and report `Running`
//! within a bounded time, or it kills the process with error 1053. This shim does exactly that: it
//! reports `StartPending` → `Running`, runs the same daemon body as the foreground path, and reports
//! `Stopped` when the SCM asks it to stop. It is entered via the `run --service` marker argument set
//! by `service install`; a bare `run` never reaches here.

use std::ffi::OsString;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::error;
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

use super::runtime::{self, RuntimeConfig};

/// The own-process service name. For an `OWN_PROCESS` service the SCM does not match on this string,
/// so it only needs to be stable; it mirrors the install label's application component.
const SERVICE_NAME: &str = "supervisor-host";
const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

/// Configuration handed from [`run_as_service`] to the SCM-invoked `service_main`, which the
/// `define_windows_service!` entry point cannot receive as a parameter.
static CONFIG: OnceLock<RuntimeConfig> = OnceLock::new();

define_windows_service!(ffi_service_main, service_main);

/// Enter the SCM dispatcher (called from `main` when started with `run --service`). Blocks until the
/// service stops. `StartServiceCtrlDispatcher` fails with error 1063 if the process was not started
/// by the SCM, which is why this path is guarded by the marker argument rather than auto-detected.
///
/// # Errors
/// Returns an error if the dispatcher cannot be started.
pub fn run_as_service(config: RuntimeConfig) -> Result<()> {
    // Stash the config for `service_main`, which the SCM invokes without our arguments.
    let _ = CONFIG.set(config);
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
        .context("starting the Windows service control dispatcher")?;
    Ok(())
}

/// The SCM entry point. Errors cannot propagate out of here, so they are logged.
fn service_main(_arguments: Vec<OsString>) {
    if let Err(err) = run_service() {
        error!(
            error = format!("{err:#}"),
            "Windows service exited with an error"
        );
    }
}

fn run_service() -> Result<()> {
    // The control handler runs on an SCM thread; a watch channel carries a stop request into the
    // async runtime. The handler is `Send + 'static` and `watch::Sender::send` needs no runtime.
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                let _ = shutdown_tx.send(true);
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    let status_handle = service_control_handler::register(SERVICE_NAME, event_handler)
        .context("registering the service control handler")?;

    // Report that startup is in progress (async init below may take a moment).
    status_handle
        .set_service_status(status(
            ServiceState::StartPending,
            ServiceControlAccept::empty(),
        ))
        .context("reporting StartPending")?;

    let config = CONFIG
        .get()
        .cloned()
        .context("service configuration was not initialised")?;
    let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building the tokio runtime")?;

    // Now serving: accept Stop and Shutdown.
    status_handle
        .set_service_status(status(
            ServiceState::Running,
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        ))
        .context("reporting Running")?;

    let result = tokio_runtime.block_on(async move {
        let mut shutdown_rx = shutdown_rx;
        let shutdown = async move {
            // Resolves when the control handler flips the value to `true`.
            let _ = shutdown_rx.changed().await;
        };
        runtime::run_until_shutdown(config, shutdown).await
    });

    status_handle
        .set_service_status(status(ServiceState::Stopped, ServiceControlAccept::empty()))
        .context("reporting Stopped")?;

    result
}

/// Build a `ServiceStatus` for the given state and accepted controls.
fn status(current_state: ServiceState, controls_accepted: ServiceControlAccept) -> ServiceStatus {
    ServiceStatus {
        service_type: SERVICE_TYPE,
        current_state,
        controls_accepted,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::from_secs(10),
        process_id: None,
    }
}
