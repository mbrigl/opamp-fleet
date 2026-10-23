//! Entry point: parse the CLI (ADR-0019) and hand off — `run` to the daemon runtime, the
//! `service` verbs to the cross-platform lifecycle. The daemon loads `supervisor.toml`, restores the
//! Agent's identity, and runs the transport the endpoint selects (ADR-0013) until stopped.

use std::path::{Path, PathBuf};

use fleet_agent::cli::{self, Command, ConfigArgs, InstallArgs, ServiceAction};
use fleet_agent::config::ClientConfig;
use fleet_agent::config_init;
use fleet_agent::service::runtime::{self, RunSpec};
use fleet_agent::service::{
    enrol, layout, manager, run_as, windows_rights, ServiceControl, ServiceLevel,
};
use fleet_agent::update;

fn main() {
    // stderr as always, plus two empty slots the OTLP exporters are dropped into once the Server
    // names a destination: the log bridge for events (ADR-0016) and the span layer for traces
    // (ADR-0016). Both have to exist from the start: `tracing` takes one subscriber for the
    // process, and it is installed long before any destination is known.
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;
    let (spans, span_handle) = tracing_subscriber::reload::Layer::new(None);
    fleet_agent::telemetry::hold_span_layer(span_handle);
    let (bridge, bridge_handle) = tracing_subscriber::reload::Layer::new(None);
    fleet_agent::telemetry::hold_log_bridge(bridge_handle);
    // The slots go on first, spans before logs: a reloadable layer is typed for the subscriber it
    // attaches to, and the registry is the only one of these that stays the same shape when a slot
    // is filled — so the layer that carries that subscriber in its own type goes on the bare one.
    tracing_subscriber::registry()
        .with(spans)
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
        // The log file (ADR-0019), which discards everything until a service run opens it — the
        // state directory is not known until the command line is parsed, a few lines below. No
        // colour: this one is read with a pager, not a terminal.
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(fleet_agent::logging::LogFile),
        )
        .init();

    // One TLS provider for the whole process (ADR-0013): ring, never a system library.
    opamp::tls::install_ring_provider();

    let cli::Parsed { cli, config_named } = cli::parse();
    let result = match cli.command {
        // A bare invocation defaults to `run`, preserving the pre-subcommand contract.
        // A bare invocation is a person at a terminal, so it writes no log file (ADR-0019).
        None => runtime::run_foreground(RunSpec {
            config_path: cli.config,
            state_dir: cli.state_dir,
            service: false,
            agents: None,
        }),
        Some(Command::Run(args)) => {
            let spec = RunSpec {
                config_path: cli.config,
                state_dir: cli.state_dir,
                service: args.service,
                agents: args
                    .agents_channel()
                    .map(|(socket, process)| runtime::AgentsChannel { socket, process }),
            };
            run_command(spec, args)
        }
        Some(Command::Service { action }) => {
            service_command(&cli.config, config_named, cli.state_dir.as_deref(), &action)
        }
        // Answer for this executable so a self-update can prove it before pointing at it
        // (ADR-0018). Deliberately does nothing else: it must work on a binary that has no
        // configuration, no state directory, and no Server.
        Some(Command::SelfCheck) => {
            println!(
                "{}{}",
                update::SELF_CHECK_TOKEN,
                fleet_core::version::current()
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
/// marker set by `service install` routes into the dispatcher shim (ADR-0019). The runtime (or
/// the shim) owns the tokio runtime; `main` stays synchronous.
fn run_command(spec: RunSpec, args: cli::RunArgs) -> Result<(), String> {
    // The second service (ADR-0059): it reads neither the configuration nor the state directory.
    if let Some((socket, client)) = args.agents_service() {
        return agents_service(&socket, client);
    }
    #[cfg(windows)]
    if args.service {
        return fleet_agent::service::windows::run_as_service(spec);
    }
    let _ = args;
    runtime::run_foreground(spec)
}

/// `run --agents`: the second service, which starts the Managed Processes under the process
/// account (ADR-0059). Its channel is a Unix socket.
#[cfg(unix)]
fn agents_service(socket: &Path, client_uid: u32) -> Result<(), String> {
    use fleet_agent::supervisor::channel::{run_service, Served};
    if run_service(socket, client_uid)? == Served::Superseded {
        // Non-zero, as the Client's own exit for an update is: the service manager restarts only
        // a service that failed (ADR-0019 clause 11).
        std::process::exit(update::EXIT_RESTART_FOR_UPDATE);
    }
    Ok(())
}

/// `run --agents` on Windows: the second service under the SCM, on a named pipe.
#[cfg(windows)]
fn agents_service(pipe: &Path, client: fleet_agent::win32::Sid) -> Result<(), String> {
    fleet_agent::service::windows::run_agents_as_service(pipe, client)
}

/// Dispatch a `service` verb (ADR-0019).
fn service_command(
    config_path: &Path,
    config_named: bool,
    state_dir: Option<&Path>,
    action: &ServiceAction,
) -> Result<(), String> {
    match action {
        ServiceAction::Install(args) => install(config_path, config_named, state_dir, args),
        ServiceAction::Config(args) => configure(config_path, config_named, state_dir, args),
        ServiceAction::Uninstall(scope) => {
            uninstall(ServiceLevel::from(*scope))?;
            println!(
                "service {} uninstalled (the install layout and state remain)",
                manager::service_name()
            );
            Ok(())
        }
        ServiceAction::Start(scope) => {
            manager::NativeService::new(ServiceLevel::from(*scope)).start()
        }
        ServiceAction::Stop(scope) => {
            manager::NativeService::new(ServiceLevel::from(*scope)).stop()
        }
        ServiceAction::Status(scope) => {
            let state = manager::NativeService::new(ServiceLevel::from(*scope)).state()?;
            println!("{}", state.describe());
            Ok(())
        }
    }
}

/// Completes the withdrawal `pending` recorded in `store` but for the accounts `kept`, naming in a
/// warning what each walk passed over (ADR-0059 clauses 1 and 6).
#[cfg(windows)]
fn complete_withdrawal(
    store: &dyn fleet_agent::service::withdrawal::Store,
    pending: &fleet_agent::service::withdrawal::Pending,
    kept: &[(fleet_agent::service::withdrawal::Part, &str)],
) -> Result<(), String> {
    fleet_agent::service::withdrawal::complete(store, pending, kept, |sid, roots| {
        let former = fleet_agent::win32::Sid::parse(sid)?;
        for skipped in run_as::withdraw(&former, roots)? {
            println!("warning: {}", skipped.warning(sid));
        }
        Ok(())
    })
}

/// `service uninstall` on Windows: a withdrawal an install left undone is completed first, and its
/// key removed — the services go, and nothing would name the former accounts again. One that cannot
/// be completed fails the uninstall before anything is deregistered.
#[cfg(windows)]
fn uninstall(level: ServiceLevel) -> Result<(), String> {
    use fleet_agent::service::withdrawal::{self, Registry};
    let store = Registry::of_the_install();
    let pending = withdrawal::read(&store)?;
    if !pending.is_empty() {
        complete_withdrawal(&store, &pending, &[]).map_err(|e| {
            format!(
                "the service is not uninstalled: a withdrawal an install left undone cannot be \
                 completed ({e}). Nothing was deregistered"
            )
        })?;
    }
    store.remove()?;
    manager::uninstall(level)
}

#[cfg(not(windows))]
fn uninstall(level: ServiceLevel) -> Result<(), String> {
    manager::uninstall(level)
}

/// `service install`: write the configuration if asked to (ADR-0019), validate it, lay out the
/// versioned install at the chosen root, and register the service against the `current` pointer
/// (ADR-0019).
fn install(
    config_path: &Path,
    config_named: bool,
    state_dir_named: Option<&Path>,
    args: &InstallArgs,
) -> Result<(), String> {
    let level = ServiceLevel::from(args.scope);

    // Ask whether this process may register a service at all, before the first thing is written.
    // On Windows the SCM refuses a non-elevated process and nothing here can raise its own rights,
    // while `%ProgramData%` happily takes the layout — so without this the install staged a version
    // and swung `current` at it and only then failed (ADR-0019: fail with a clear message).
    windows_rights::ensure_can_register(level)?;

    // Two roots and two flags (ADR-0019 clause 8). The executable layout and the data default to
    // different places on Linux at system scope — and only there — because systemd may not execute
    // from `/var/lib` under SELinux. `--root` alone keeps ADR-0019's meaning and collapses both
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
    // Where the state directory comes from (ADR-0053 clause 3): `--state-dir`, else the existing
    // registration — so an upgrade keeps it (clause 2) — and only then the configuration, which the
    // service's account may rewrite, and from that only beneath these roots.
    let roots = layout::Roots {
        named: state_dir_named.map(absolute).transpose()?,
        registered: manager::registered_state_dir(level)?,
        data: data_root.clone(),
        layout: layout_root.clone(),
    };

    // The account, decided and resolved with the same before-anything-is-written rule (ADR-0042
    // clause 2): `--run-as`, else the account the existing unit or plist names, else the owner of
    // a data root that unit lost its account on, else root. A name that does not exist, or a
    // Windows form that would need a password, fails here.
    let registered = manager::registration(level)?;
    let registered_agents = manager::agents_registration(level)?;
    let account = run_as::decide(
        level,
        args.run_as.as_deref(),
        registered.as_ref(),
        &data_root,
        manager::service_name(),
    )?;
    let run_as = account.run_as;
    // The process account, decided and checked by the same rule (ADR-0059 clause 1):
    // `--process-account`, else the account the second service's unit names, else none — and then
    // no second service, and nothing changes.
    let process = run_as::decide_process_account(
        args.process_account.as_deref(),
        args.no_process_account,
        registered_agents.as_ref(),
        run_as.as_ref(),
        manager::agents_supported(level),
    )?;
    // What a former account was granted is withdrawn from it on Windows (ADR-0059 clauses 1 and 6):
    // a process account dropped or replaced, and a Client's account replaced. Each is resolved
    // here, before anything changes — one that cannot be found fails the install now, not after
    // the SCM has forgotten it — and a withdrawal an earlier install left undone is read with
    // them, to be completed by this one.
    #[cfg(windows)]
    let withdrawal = fleet_agent::service::withdrawal::Registry::of_the_install();
    #[cfg(windows)]
    let mut pending = fleet_agent::service::withdrawal::read(&withdrawal)?;
    #[cfg(windows)]
    let formers = {
        use fleet_agent::service::withdrawal::Part;
        let mut formers = Vec::new();
        for (part, former, current) in [
            (
                Part::Process,
                registered_agents
                    .as_ref()
                    .and_then(|r| r.account.as_deref()),
                process.as_ref().map(|p| (p.account(), p.sid())),
            ),
            (
                Part::Client,
                registered.as_ref().and_then(|r| r.account.as_deref()),
                run_as.as_ref().map(|r| (r.account(), r.sid())),
            ),
        ] {
            if let Some(sid) = run_as::former_sid(former, current.as_slice())? {
                formers.push((part, sid.as_str().to_string()));
            }
        }
        formers
    };
    // Nothing may run under an account whose rights this install changes or takes away: where a
    // withdrawal is due, or a process account is or was registered — whose Managed Processes could
    // hold a directory the restriction, the take-over or the withdrawal must change — both services
    // are stopped, and waited for until their processes have ended, the second service's programs
    // with them, before anything is written (ADR-0059 clauses 1, 4 and 6). One that does not end
    // in time fails the install, changing nothing; once it has succeeded, what ran is started
    // again.
    #[cfg(windows)]
    let halted = if !formers.is_empty()
        || !pending.is_empty()
        || registered_agents.is_some()
        || process.is_some()
    {
        Some(manager::halt_for_change()?)
    } else {
        None
    };
    // The second service's socket lives in a directory of the process account's, which no root
    // may be (ADR-0059 clause 9).
    #[cfg(unix)]
    if process.is_some() {
        manager::check_roots_apart(&[&layout_root, &data_root], &manager::agents_socket())?;
    }

    // Everything baked into the unit is absolute: a service's working directory is `/` or
    // `System32`, so a relative path would silently point nowhere. An operator who named no path
    // gets one inside the install root rather than one resolved against this shell's working
    // directory, which the service manager will not share (ADR-0019).
    let config_path = if config_named {
        absolute(config_path)?
    } else {
        data_root.join(config_init::FILE_NAME)
    };

    // The fleet's files the operator put beside this program, before it is staged anywhere else,
    // or in the directory `--fleet-files` names (ADR-0029 clause 6).
    let beside = match &args.fleet_files {
        Some(dir) => config_init::Beside::in_named_dir(&absolute(dir)?)?,
        None => config_init::Beside::the_program(),
    };
    let password = args
        .bootstrap_password_file
        .as_deref()
        .map(|path| config_init::read_password_file(path, "--bootstrap-password-file"))
        .transpose()?;
    // Not asked, the bootstrap identity found enrols a Client that holds no issued certificate,
    // over TLS (ADR-0029 clause 10) — decided, and a `bootstrap.p12` opened, before anything is
    // written, against the file this install is about to write or keep.
    let unasked = if args.interactive {
        None
    } else {
        // An absent file loads as the defaults; `--endpoint` then writes its endpoint, and an
        // existing file is kept.
        let mut about_to_run = ClientConfig::load(&config_path)?;
        if let (false, Some(endpoint)) = (config_path.exists(), &args.endpoint) {
            about_to_run.endpoint.clone_from(endpoint);
        }
        about_to_run.state_dir = about_to_run.service_state_dir(&roots)?;
        beside.unasked_enrolment(&about_to_run, password.as_deref().map(String::as_str))?
    };
    // Held from the first write to the last, so the account cannot swap the data root for a link
    // while this privileged run writes into it (ADR-0054).
    let mut held_root = hold_data_root(&data_root)?;
    let mut written = config_init::Written::default();
    if args.interactive {
        written = ask(
            &config_path,
            &roots,
            &beside,
            password.as_deref().map(String::as_str),
        )?;
    } else if let Some(endpoint) = &args.endpoint {
        // The same file, from an answer given rather than asked for (ADR-0019): this is the branch
        // the MSI's custom action and a `%post` script take. The self-update consent travels with
        // it — standing unless this install was told to withdraw it (ADR-0018) — and so do the CA
        // and the verification key beside the program (ADR-0020 clause 3).
        let self_update = !args.no_self_update;
        written.copies =
            config_init::run_with_endpoint(&config_path, endpoint, self_update, &beside)?;
    } else if !config_path.exists() {
        // Not an error — automation must not break — but never silent: without this file the
        // service refuses to start.
        println!(
            "warning: no configuration at {} — the Client refuses to start until it exists with \
             a client identity (write it, or re-run with --interactive)",
            config_path.display()
        );
    }

    // Fail on a broken configuration now, not at the service's first start. After the write, so
    // that a file just answered into existence is held to the same rule as any other.
    let mut config = ClientConfig::load(&config_path)?;
    // The state directory the service will run with — where an issued certificate counts. Asked
    // again of the file as it is now, which the service's account may have rewritten meanwhile.
    let state_dir = config.service_state_dir(&roots)?;
    let state_dir_source = config.service_state_dir_source(&roots);
    config.state_dir.clone_from(&state_dir);
    // A registration an earlier install wrote is kept wherever it points (ADR-0053 clause 3), and
    // that install may have taken it from a file the service's account had rewritten. Said, so an
    // operator who did not choose it sees it on every upgrade.
    if roots.named.is_none()
        && roots.registered.is_some()
        && layout::beneath(&state_dir, &[&data_root, &layout_root]).is_err()
    {
        println!(
            "warning: the service's registration names the state directory {}, outside the data \
             root and the layout root; it is kept and handed over to the service's account. \
             If you did not choose it, name the right one with --state-dir.",
            state_dir.display()
        );
    }
    // A Supervisors' root the file places outside every root this install acts on is not the
    // install's to create, restrict or hand over.
    let supervisors_root = config.supervisors_root();
    let supervisors_root_ours =
        layout::beneath(&supervisors_root, &[&data_root, &layout_root, &state_dir]).is_ok();
    // On Windows every local user inherits read access under %ProgramData%; the data root, a state
    // directory placed elsewhere, and a Supervisors' root this install creates outside both are cut
    // off from it before anyone is granted anything (H8) — and before an enrolment writes its
    // private key there. Decided before anything is created. Unix needs nothing here: the Client writes its
    // secrets owner-only itself.
    let restrictions = windows_rights::plan(
        &data_root,
        args.root.is_some() || args.data_root.is_some(),
        &state_dir,
        &supervisors_root,
        supervisors_root_ours,
        Path::exists,
    );
    std::fs::create_dir_all(&data_root)
        .map_err(|e| format!("cannot create {}: {e}", data_root.display()))?;
    if held_root.is_none() {
        held_root = hold_data_root(&data_root)?;
    }
    if !state_dir.starts_with(&data_root) {
        std::fs::create_dir_all(&state_dir)
            .map_err(|e| format!("cannot create the state directory: {e}"))?;
    }
    // What the process account owns there is its own, and left as it is (ADR-0059 clause 10).
    #[cfg(windows)]
    let process_sid = process.as_ref().map(|p| p.sid().to_string());
    #[cfg(not(windows))]
    let process_sid: Option<String> = None;
    let created_outside = windows_rights::restrict(level, &restrictions, process_sid.as_deref())?;
    if let Some(bootstrap) = unasked {
        println!("enrolling with {}", bootstrap.describe());
        written.bootstrap = Some(bootstrap);
    }
    // Before anything is staged or registered (ADR-0020 clause 5): an enrolment that is stopped
    // leaves the answered file and nothing else.
    if let Some(bootstrap) = &written.bootstrap {
        enrol::run(&config, bootstrap)?;
    }
    // And held to what the Client needs at startup. A packaged install registers the service even
    // so — it has an endpoint and no client identity to write — but never starts it, and the
    // service refuses to run until the file names one (ADR-0019). Said loudly here.
    if let Err(e) = config.check_admission() {
        println!(
            "warning: {}: {e}. The service is registered, and refuses to start until this is set \
             (or re-run with --interactive).",
            config_path.display()
        );
    }

    let layout = layout::Layout::new(&layout_root);
    let program = layout::stage_current_exe(&layout)?;

    // Written down before the SCM is switched, which forgets the former accounts: every root the
    // hand-over grants the Client anything in, a configuration and its copies outside the data
    // root among them.
    #[cfg(windows)]
    {
        let supervisors_root = config.supervisors_root();
        let mut roots = vec![
            layout_root.as_path(),
            data_root.as_path(),
            state_dir.as_path(),
            supervisors_root.as_path(),
            config_path.as_path(),
        ];
        roots.extend(created_outside.iter().map(PathBuf::as_path));
        roots.extend(written.copies.iter().map(PathBuf::as_path));
        pending.add(formers, &roots);
        if !pending.is_empty() {
            fleet_agent::service::withdrawal::record(&withdrawal, &pending)?;
        }
    }

    manager::install(&manager::InstallSpec {
        level,
        program: program.clone(),
        config_path: config_path.clone(),
        state_dir: state_dir.clone(),
        run_as: run_as.as_ref().map(|r| r.account().to_string()),
        agents: agents_spec(process.as_ref(), run_as.as_ref()),
    })?;
    // Dropped (ADR-0059 clause 1): the Client's unit no longer names the second service, which is
    // stopped and deregistered after it.
    let dropped = args.no_process_account && manager::remove_agents(level)?;

    // What a former account was granted on Windows is withdrawn from it now that the SCM names it
    // no more, and what it owns given to the Administrators, as a change of owner withdraws it on
    // Unix (ADR-0059 clauses 1 and 6); the record goes once all of it is done. Before the
    // hand-over and the share of the layout, which grant what the accounts named now are to hold.
    #[cfg(windows)]
    if !pending.is_empty() {
        use fleet_agent::service::withdrawal::Part;
        let kept: Vec<(Part, &str)> = process
            .iter()
            .map(|p| (Part::Process, p.sid().as_str()))
            .chain(run_as.iter().map(|r| (Part::Client, r.sid().as_str())))
            .collect();
        complete_withdrawal(&withdrawal, &pending, &kept)?;
    }

    // The handover (ADR-0042 clause 2), on every registration that runs under an account — a kept
    // one and root included: both roots belong to it — config and state because the service reads
    // and rewrites them (ADR-0017), the executable layout because the self-update that stages into
    // it *is* the service (ADR-0018). The state directory is created first: the daemon must not
    // need rights on its parent to begin. Where a process account is registered, what it keeps
    // there stays its own (ADR-0059 clause 10): every package upgrade re-registers.
    if let Some(run_as) = &run_as {
        std::fs::create_dir_all(&state_dir)
            .map_err(|e| format!("cannot create the state directory: {e}"))?;
        let kept = process.as_ref();
        let mut skipped =
            run_as.hand_over(&[&layout_root, &data_root, &state_dir, &config_path], kept)?;
        // A Supervisors' root this install created and cut on Windows is the service's to write
        // too (ADR-0017 clause 21); none is created on Unix.
        skipped.extend(
            run_as.hand_over(
                &created_outside
                    .iter()
                    .filter(|dir| **dir != state_dir)
                    .map(PathBuf::as_path)
                    .collect::<Vec<_>>(),
                kept,
            )?,
        );
        // The copy beside a configuration named outside the data root is not under it.
        skipped.extend(
            run_as.hand_over(
                &written
                    .copies
                    .iter()
                    .map(PathBuf::as_path)
                    .collect::<Vec<_>>(),
                kept,
            )?,
        );
        for skipped in skipped {
            println!("warning: {}", skipped.warning(run_as.account()));
        }
    }
    // The second service runs from the layout, which a root taken over or cut off on Windows
    // grants the process account nothing of (ADR-0059 clause 2).
    #[cfg(windows)]
    if let Some(process) = &process {
        process.share_layout(&layout.versions_dir())?;
    }
    // What ran before the stop runs again, now under the registration just made.
    #[cfg(windows)]
    if let Some(halted) = halted {
        manager::resume(level, halted)?;
    }

    drop(held_root);
    println!("installed {}", manager::service_name());
    println!("  program:   {}", program.display());
    println!("  config:    {}", config_path.display());
    println!("  state dir: {} ({state_dir_source})", state_dir.display());
    if let Some(run_as) = &run_as {
        println!(
            "  runs as:   {} ({})",
            run_as.account(),
            account.source.describe()
        );
    }
    if let Some(process) = &process {
        println!(
            "  processes: {} ({}), started by {}",
            process.account(),
            process.describe(),
            manager::agents_service_name()
        );
    }
    if dropped {
        println!(
            "  processes: started by the Client itself once it restarts; {} was stopped and \
             removed",
            manager::agents_service_name()
        );
    }
    // Since service-manager 0.10, launchd installs do not auto-start; say the next step instead
    // of pretending.
    let user = if args.scope.user { " --user" } else { "" };
    println!("start it with: supervisor service start{user}");
    Ok(())
}

/// The second service a registration names, where a process account was decided (ADR-0059): it
/// serves the Client's account, and the Client accepts the process account as its peer.
#[cfg(unix)]
fn agents_spec(
    process: Option<&run_as::ProcessAccount>,
    client: Option<&run_as::RunAs>,
) -> Option<manager::Agents> {
    let (process, client) = (process?, client?);
    Some(manager::Agents {
        account: process.account().to_string(),
        uid: process.uid(),
        gid: process.gid(),
        client_uid: client.uid(),
        socket: manager::agents_socket(),
    })
}

/// The second service a registration names on Windows (ADR-0059): it serves the Client's SID on
/// its named pipe, and the Client accepts the process account's SID as the pipe's owner.
#[cfg(windows)]
fn agents_spec(
    process: Option<&run_as::ProcessAccount>,
    _client: Option<&run_as::RunAs>,
) -> Option<manager::Agents> {
    let process = process?;
    Some(manager::Agents {
        account: process.account().to_string(),
        sid: process.sid().clone(),
        client_sid: process.client_sid().clone(),
        socket: manager::agents_socket(),
    })
}

/// `service config`: the questionnaire of `install --interactive` against an installed Client, and
/// its enrolment when it holds no certificate yet — registering nothing (ADR-0019 clause 15).
fn configure(
    config_path: &Path,
    config_named: bool,
    state_dir_named: Option<&Path>,
    args: &ConfigArgs,
) -> Result<(), String> {
    let level = ServiceLevel::from(args.scope);
    // The file and the state directory, found as `install` laid them out.
    let data_root = match args.data_root() {
        Some(data) => absolute(data)?,
        None => manager::default_root(level)?,
    };
    let roots = layout::Roots {
        named: state_dir_named.map(absolute).transpose()?,
        registered: manager::registered_state_dir(level)?,
        layout: match &args.root {
            Some(root) => absolute(root)?,
            None => manager::default_layout_root(level)?,
        },
        data: data_root.clone(),
    };
    let config_path = if config_named {
        absolute(config_path)?
    } else {
        data_root.join(config_init::FILE_NAME)
    };
    // Held while this privileged run writes into it, as `install` holds it (ADR-0054).
    let _held_root = hold_data_root(&data_root)?;
    // The installed copy runs from the layout, where no fleet files lie (ADR-0020, out of scope).
    let written = ask(&config_path, &roots, &config_init::Beside::default(), None)?;

    let mut config = ClientConfig::load(&config_path)?;
    let state_dir = config.service_state_dir(&roots)?;
    config.state_dir.clone_from(&state_dir);
    if let Some(bootstrap) = &written.bootstrap {
        enrol::run(&config, bootstrap)?;
    }
    // What this run created belongs to whoever owns the configuration — the service's account when
    // it runs as one (ADR-0019 clause 13) — not to the administrator who ran it.
    let mut created = enrol::written(&state_dir);
    created.extend(written.copies.iter().cloned());
    let skipped = run_as::match_owner(
        &config_path,
        &data_root,
        &created.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
    )?;
    for file in skipped {
        println!(
            "warning: {} has a second hard link and was not given the owner of {}",
            file.display(),
            config_path.display()
        );
    }
    if let Err(e) = config.check_admission() {
        println!("warning: {}: {e}", config_path.display());
    }

    let user = if args.scope.user { " --user" } else { "" };
    println!(
        "restart the service to apply: supervisor service stop{user} && \
         supervisor service start{user}"
    );
    Ok(())
}

/// The questionnaire against the file at `config_path`, offering what it holds and asking for the
/// bootstrap identity only while the service's state directory holds no issued certificate
/// (ADR-0019 clauses 16, 17). That directory is decided, and refused, before anything is asked.
fn ask(
    config_path: &Path,
    roots: &layout::Roots,
    beside: &config_init::Beside,
    password: Option<&str>,
) -> Result<config_init::Written, String> {
    let current = ClientConfig::load(config_path)?;
    let mut in_service = current.clone();
    in_service.state_dir = current.service_state_dir(roots)?;
    config_init::run(
        config_path,
        &current,
        in_service.is_enrolled(),
        beside,
        password,
    )
}

/// The data root, held where it exists already; `None` where it does not yet.
///
/// On Windows the service's account holds Modify, delete included, on the data root itself, so it
/// could rename the root away and put a junction at its name: a root that is a reparse point is
/// refused, and the held handle keeps it in place (ADR-0054). On Unix the root's name lies
/// in a directory only root writes, and holding it costs nothing.
fn hold_data_root(data_root: &Path) -> Result<Option<fleet_agent::nofollow::HeldDir>, String> {
    match fleet_agent::nofollow::HeldDir::open_root(data_root) {
        Ok(held) => Ok(Some(held)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot open {}: {e}", data_root.display())),
    }
}

/// Absolutize without requiring existence (`canonicalize` would fail for a config file that
/// legitimately does not exist yet — defaults then apply, as in `ClientConfig::load`).
fn absolute(path: &Path) -> Result<PathBuf, String> {
    std::path::absolute(path).map_err(|e| format!("cannot absolutize {}: {e}", path.display()))
}
