//! OpAMP Fleet Supervisor Host — the client process that runs Supervisors.
//!
//! The binary is a thin `clap` dispatcher (ADR-0006): it parses the command line and hands off to
//! the `service` module, which owns running standalone, running under an OS service, and the service
//! lifecycle. A bare invocation defaults to `run`, preserving the original "just run it" contract.
//! The self-update `update` subcommand (ADR-0007) is added in a later stage.

mod cli;
mod service;

use anyhow::Result;
use clap::Parser;
use tracing::info;

use cli::{Cli, Command, ConfigArgs, RunArgs, ServiceAction};
use service::runtime::RuntimeConfig;
use service::{manager, NativeService, ServiceControl, ServiceLevel};

fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();
    match cli.command.unwrap_or_else(default_command) {
        Command::Run(args) => run(args),
        Command::Service { action } => run_service_action(action),
    }
}

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
}

/// A bare invocation (no subcommand) defaults to `run`, taking configuration from the environment.
fn default_command() -> Command {
    Command::Run(RunArgs {
        config: ConfigArgs::from_env(),
        service: false,
    })
}

fn run(args: RunArgs) -> Result<()> {
    let config = RuntimeConfig::resolve(args.config);
    #[cfg(windows)]
    {
        if args.service {
            return service::windows::run_as_service(config);
        }
    }
    service::runtime::run_foreground(config)
}

fn run_service_action(action: ServiceAction) -> Result<()> {
    match action {
        ServiceAction::Install(args) => {
            let level = level_of(args.scope.user);
            let config = RuntimeConfig::resolve(args.config);
            manager::install(level, &config)?;
            info!("service installed");
            Ok(())
        }
        ServiceAction::Uninstall(scope) => {
            manager::uninstall(level_of(scope.user))?;
            info!("service uninstalled");
            Ok(())
        }
        ServiceAction::Start(scope) => {
            NativeService::new(level_of(scope.user)).start()?;
            info!("service started");
            Ok(())
        }
        ServiceAction::Stop(scope) => {
            NativeService::new(level_of(scope.user)).stop()?;
            info!("service stopped");
            Ok(())
        }
        ServiceAction::Status(scope) => {
            let state = NativeService::new(level_of(scope.user)).state()?;
            println!("{}", state.describe());
            Ok(())
        }
    }
}

fn level_of(user: bool) -> ServiceLevel {
    if user {
        ServiceLevel::User
    } else {
        ServiceLevel::System
    }
}
