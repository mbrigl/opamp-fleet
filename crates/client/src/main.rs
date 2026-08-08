//! Entry point: parse the CLI (ADR-0010) and hand off — `run` to the daemon runtime, the
//! `service` verbs to the cross-platform lifecycle. The daemon loads `client.toml`, restores the
//! Agent's identity, and runs the transport the endpoint selects (ADR-0007) until stopped.

use std::path::{Path, PathBuf};

use client::cli::{self, Command, InstallArgs, InstanceName, ServiceAction};
use client::config::ClientConfig;
use client::config_init;
use client::service::runtime::{self, RunSpec};
use client::service::{layout, manager, ServiceControl, ServiceLevel};
use client::{selfupdate, version};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // One TLS provider for the whole process (ADR-0007): ring, never a system library.
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("install the rustls ring provider");

    let cli::Parsed { cli, config_named } = cli::parse();
    let result = match cli.command {
        // A bare invocation defaults to `run`, preserving the pre-subcommand contract.
        None => runtime::run_foreground(RunSpec {
            config_path: cli.config,
            state_dir: cli.state_dir,
        }),
        Some(Command::Run(args)) => run_command(
            RunSpec {
                config_path: cli.config,
                state_dir: cli.state_dir,
            },
            args,
        ),
        Some(Command::Service { action }) => {
            service_command(&cli.config, config_named, cli.instance, &action)
        }
        // Answer for this executable so a self-update can prove it before pointing at it
        // (ADR-0020). Deliberately does nothing else: it must work on a binary that has no
        // configuration, no state directory, and no Server.
        Some(Command::SelfCheck) => {
            println!("{}{}", selfupdate::SELF_CHECK_TOKEN, version::version());
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
    instance: InstanceName,
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
        ServiceAction::Install(args) => install(config_path, config_named, instance, args),
        ServiceAction::Uninstall(scope) => {
            manager::uninstall(level(scope), &instance)?;
            println!("service io.opamp-fleet.client.{instance} uninstalled (the install layout and state remain)");
            Ok(())
        }
        ServiceAction::Start(scope) => manager::NativeService::new(level(scope), instance).start(),
        ServiceAction::Stop(scope) => manager::NativeService::new(level(scope), instance).stop(),
        ServiceAction::Status(scope) => {
            let state = manager::NativeService::new(level(scope), instance).state()?;
            println!("{}", state.describe());
            Ok(())
        }
    }
}

/// `service install`: write the configuration if asked to (ADR-0027), validate it, lay out the
/// versioned install at the chosen root, and register the service against the `current` pointer
/// (ADR-0010).
fn install(
    config_path: &Path,
    config_named: bool,
    instance: InstanceName,
    args: &InstallArgs,
) -> Result<(), String> {
    let level = if args.scope.user {
        ServiceLevel::User
    } else {
        ServiceLevel::System
    };
    let root = match &args.root {
        Some(root) => absolute(root)?,
        None => manager::default_root(level, &instance)?,
    };

    // Everything baked into the unit is absolute: a service's working directory is `/` or
    // `System32`, so a relative path would silently point nowhere. An operator who named no path
    // gets one inside the install root rather than one resolved against this shell's working
    // directory, which the service manager will not share (ADR-0027).
    let config_path = if config_named {
        absolute(config_path)?
    } else {
        root.join(config_init::FILE_NAME)
    };

    if args.interactive {
        config_init::run(&config_path)?;
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

    let layout = layout::Layout::new(&root);
    let program = layout::stage_current_exe(&layout)?;

    let state_dir = if config.state_dir.is_absolute() {
        config.state_dir.clone()
    } else {
        layout.state_dir()
    };

    manager::install(&manager::InstallSpec {
        level,
        instance: instance.clone(),
        program: program.clone(),
        config_path: config_path.clone(),
        state_dir: state_dir.clone(),
    })?;

    println!("installed io.opamp-fleet.client.{instance}");
    println!("  program:   {}", program.display());
    println!("  config:    {}", config_path.display());
    println!("  state dir: {}", state_dir.display());
    // Since service-manager 0.10, launchd installs do not auto-start; say the next step instead
    // of pretending.
    let user = if args.scope.user { " --user" } else { "" };
    println!("start it with: opamp-fleet-client service start{user} --instance {instance}");
    Ok(())
}

/// Absolutize without requiring existence (`canonicalize` would fail for a config file that
/// legitimately does not exist yet — defaults then apply, as in `ClientConfig::load`).
fn absolute(path: &Path) -> Result<PathBuf, String> {
    std::path::absolute(path).map_err(|e| format!("cannot absolutize {}: {e}", path.display()))
}
