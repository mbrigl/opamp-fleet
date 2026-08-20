//! Entry point: parse the CLI (ADR-0010) and hand off — `run` to the daemon runtime, the
//! `service` verbs to the cross-platform lifecycle. The daemon loads `supervisor.toml`, restores the
//! Agent's identity, and runs the transport the endpoint selects (ADR-0007) until stopped.

use std::path::{Path, PathBuf};

use client::cli::{self, Command, InstallArgs, ServiceAction};
use client::config::ClientConfig;
use client::config_init;
use client::selfupdate;
use client::service::runtime::{self, RunSpec};
use client::service::{layout, manager, run_as, windows_rights, ServiceControl, ServiceLevel};

fn main() {
    // stderr as always, plus an empty slot the OTLP log bridge is dropped into once the Server
    // names a destination (ADR-0036). The slot has to exist from the start: `tracing` takes one
    // subscriber for the process, and it is installed long before any destination is known.
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;
    let (bridge, handle) = tracing_subscriber::reload::Layer::new(None);
    client::telemetry::hold_log_bridge(handle);
    // The bridge goes on first: a reloadable layer is typed for the subscriber it attaches to, and
    // the registry is the only one of these that stays the same shape when the slot is filled.
    tracing_subscriber::registry()
        .with(bridge)
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        // Colour only when stderr is a real terminal: under a service manager stderr is a pipe to
        // journald or the SCM, and ANSI escapes there are noise in the syslog, not colour.
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr())),
        )
        // The log file (ADR-0041), which discards everything until a service run opens it — the
        // state directory is not known until the command line is parsed, a few lines below. No
        // colour: this one is read with a pager, not a terminal.
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(client::logging::LogFile),
        )
        .init();

    // One TLS provider for the whole process (ADR-0007): ring, never a system library.
    client::tls::install_ring_provider();

    let cli::Parsed { cli, config_named } = cli::parse();
    let result = match cli.command {
        // A bare invocation defaults to `run`, preserving the pre-subcommand contract.
        // A bare invocation is a person at a terminal, so it writes no log file (ADR-0041).
        None => runtime::run_foreground(RunSpec {
            config_path: cli.config,
            state_dir: cli.state_dir,
            service: false,
        }),
        Some(Command::Run(args)) => {
            let spec = RunSpec {
                config_path: cli.config,
                state_dir: cli.state_dir,
                service: args.service,
            };
            run_command(spec, args)
        }
        Some(Command::Service { action }) => service_command(&cli.config, config_named, &action),
        // Answer for this executable so a self-update can prove it before pointing at it
        // (ADR-0020). Deliberately does nothing else: it must work on a binary that has no
        // configuration, no state directory, and no Server.
        Some(Command::SelfCheck) => {
            println!(
                "{}{}",
                selfupdate::SELF_CHECK_TOKEN,
                opamp::version::current()
            );
            Ok(())
        }
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

/// `run`: the foreground daemon — except under the Windows SCM, where the hidden `--service`
/// marker set by `service install` routes into the dispatcher shim (ADR-0010). The runtime (or
/// the shim) owns the tokio runtime; `main` stays synchronous.
fn run_command(spec: RunSpec, args: cli::RunArgs) -> Result<(), String> {
    #[cfg(windows)]
    if args.service {
        return client::service::windows::run_as_service(spec);
    }
    let _ = args;
    runtime::run_foreground(spec)
}

/// Dispatch a `service` verb (ADR-0010).
fn service_command(
    config_path: &Path,
    config_named: bool,
    action: &ServiceAction,
) -> Result<(), String> {
    let level = |scope: &cli::ScopeArgs| {
        if scope.user {
            ServiceLevel::User
        } else {
            ServiceLevel::System
        }
    };
    match action {
        ServiceAction::Install(args) => install(config_path, config_named, args),
        ServiceAction::Uninstall(scope) => {
            manager::uninstall(level(scope))?;
            println!(
                "service {} uninstalled (the install layout and state remain)",
                manager::service_name()
            );
            Ok(())
        }
        ServiceAction::Start(scope) => manager::NativeService::new(level(scope)).start(),
        ServiceAction::Stop(scope) => manager::NativeService::new(level(scope)).stop(),
        ServiceAction::Status(scope) => {
            let state = manager::NativeService::new(level(scope)).state()?;
            println!("{}", state.describe());
            Ok(())
        }
    }
}

/// `service install`: write the configuration if asked to (ADR-0027), validate it, lay out the
/// versioned install at the chosen root, and register the service against the `current` pointer
/// (ADR-0010).
fn install(config_path: &Path, config_named: bool, args: &InstallArgs) -> Result<(), String> {
    let level = if args.scope.user {
        ServiceLevel::User
    } else {
        ServiceLevel::System
    };

    // Ask whether this process may register a service at all, before the first thing is written.
    // On Windows the SCM refuses a non-elevated process and nothing here can raise its own rights,
    // while `%ProgramData%` happily takes the layout — so without this the install staged a version
    // and swung `current` at it and only then failed (ADR-0010: fail with a clear message).
    windows_rights::ensure_can_register(level)?;

    // Resolve the `--run-as` account with the same before-anything-is-written rule (ADR-0062): a
    // name that does not exist, or a Windows form that would need a password, fails here.
    let run_as = args
        .run_as
        .as_deref()
        .map(|account| run_as::RunAs::resolve(account, manager::service_name()))
        .transpose()?;

    // Two roots and two flags (ADR-0084 clause 3). The executable layout and the data default to
    // different places on Linux at system scope — and only there — because systemd may not execute
    // from `/var/lib` under SELinux. `--root` alone keeps ADR-0053's meaning and collapses both
    // halves into the one directory the operator named, whose labeling and permissions are then
    // the operator's business; `--data-root` names the other half when they must stay apart.
    let (layout_root, data_root) = match (&args.root, &args.data_root) {
        (Some(root), Some(data)) => (absolute(root)?, absolute(data)?),
        (Some(root), None) => {
            let root = absolute(root)?;
            (root.clone(), root)
        }
        (None, Some(data)) => (manager::default_layout_root(level)?, absolute(data)?),
        (None, None) => (
            manager::default_layout_root(level)?,
            manager::default_root(level)?,
        ),
    };

    // Everything baked into the unit is absolute: a service's working directory is `/` or
    // `System32`, so a relative path would silently point nowhere. An operator who named no path
    // gets one inside the install root rather than one resolved against this shell's working
    // directory, which the service manager will not share (ADR-0027).
    let config_path = if config_named {
        absolute(config_path)?
    } else {
        data_root.join(config_init::FILE_NAME)
    };

    if args.interactive {
        config_init::run(&config_path)?;
    } else if let Some(endpoint) = &args.endpoint {
        // The same file, from an answer given rather than asked for (ADR-0046): this is the branch
        // the MSI's custom action and a `%post` script take. The self-update consent travels with
        // it — standing unless this install was told to withdraw it (ADR-0075).
        let self_update = if args.no_self_update {
            None
        } else {
            Some(
                args.self_update_package
                    .as_deref()
                    .unwrap_or(client::supervisor::agent::CLIENT_AGENT_TYPE),
            )
        };
        config_init::run_with_endpoint(&config_path, endpoint, self_update)?;
    } else if !config_path.exists() {
        // Not an error — automation must not break — but never silent: without this file the
        // service starts, dials the development default, and manages nothing.
        println!(
            "warning: no configuration at {} — the Client will run on defaults until it exists \
             (write it, or re-run with --interactive)",
            config_path.display()
        );
    }

    // Fail on a broken configuration now, not at the service's first start. After the write, so
    // that a file just answered into existence is held to the same rule as any other.
    let config = ClientConfig::load(&config_path)?;

    let layout = layout::Layout::new(&layout_root);
    let program = layout::stage_current_exe(&layout)?;

    let state_dir = if config.state_dir.is_absolute() {
        config.state_dir.clone()
    } else {
        data_root.join(layout::STATE_DIR_NAME)
    };

    manager::install(&manager::InstallSpec {
        level,
        program: program.clone(),
        config_path: config_path.clone(),
        state_dir: state_dir.clone(),
        run_as: run_as.as_ref().map(|r| r.account().to_string()),
    })?;

    // The handover (ADR-0084 clause 12, carrying ADR-0062): both roots belong to the account —
    // config and state because the service reads and rewrites them (ADR-0056), the executable
    // layout because the self-update that stages into it *is* the service (ADR-0020). The state
    // directory is created first: the daemon must not need rights on its parent to begin.
    if let Some(run_as) = &run_as {
        std::fs::create_dir_all(&state_dir)
            .map_err(|e| format!("cannot create the state directory: {e}"))?;
        run_as.hand_over(&[&layout_root, &data_root, &state_dir, &config_path])?;
    }

    println!("installed {}", manager::service_name());
    println!("  program:   {}", program.display());
    println!("  config:    {}", config_path.display());
    println!("  state dir: {}", state_dir.display());
    if let Some(run_as) = &run_as {
        println!("  runs as:   {}", run_as.account());
    }
    // Since service-manager 0.10, launchd installs do not auto-start; say the next step instead
    // of pretending.
    let user = if args.scope.user { " --user" } else { "" };
    println!("start it with: supervisor service start{user}");
    Ok(())
}

/// Absolutize without requiring existence (`canonicalize` would fail for a config file that
/// legitimately does not exist yet — defaults then apply, as in `ClientConfig::load`).
fn absolute(path: &Path) -> Result<PathBuf, String> {
    std::path::absolute(path).map_err(|e| format!("cannot absolutize {}: {e}", path.display()))
}
