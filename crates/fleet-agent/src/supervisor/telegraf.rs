//! The `telegraf` plugin (ADR-draft-telegraf): Telegraf, delivered as a package and run out of this
//! Supervisor's own directory.
//!
//! There is nothing to decide about how Telegraf is invoked. It is a single-file program, it takes
//! its configuration from `--config`, it prints a Semantic Versioning version for `--version`, and
//! it re-reads its configuration on `SIGHUP`. All four are Telegraf's own properties, and as long
//! as they were block keys every host repeated them — one of them, the reload signal, in a form no
//! mixed fleet could write at all: the key is refused on Windows at parse time, so a Supervisor set
//! carrying it was rejected by every Windows host in the fleet. Here the reload is the kind's, and
//! platform-correct by construction: the signal where there are signals, and the Runner's restart
//! where there are none.

use tokio::sync::mpsc;

use crate::supervisor::ports::{
    parse_settings, KindDefaults, KindTiming, Plugin, ProcessCommand, SupervisorContext,
};
use crate::supervisor::process::{sighup, Preflight, ProcessSpec, Runner, Spawner, VersionProbe};

/// The program's file name in this Supervisor's `program/` directory (ADR-0017).
#[cfg(windows)]
const PROGRAM: &str = "telegraf.exe";
#[cfg(not(windows))]
const PROGRAM: &str = "telegraf";

/// Where InfluxData's archive holds the program, below the version-named directory that wraps it
/// (ADR-draft-telegraf clause 3). The Unix tarballs carry further entries named `telegraf` — the
/// logrotate rule `etc/logrotate.d/telegraf`, and the directories `etc/telegraf/`,
/// `usr/lib/telegraf/` and `var/log/telegraf/` — so the file name alone does not say which member
/// is the program; the path does, and the installer refuses an archive in which it matches more
/// than one (ADR-draft-packages-signatures-and-delivery clause 55).
#[cfg(windows)]
const MEMBER: &str = "telegraf.exe";
#[cfg(not(windows))]
const MEMBER: &str = "usr/bin/telegraf";

/// The Agent type every Telegraf Configuration is aimed at (ADR-0016), and the name
/// `opamp-fleetctl package fetch` uploads its default Configuration under. The second is the first
/// plus `-conf`, and both are properties of the packing side rather than of a host.
const SERVICE_NAME: &str = "telegraf";
const CONFIG_ENTRY: &str = "telegraf-conf";

/// How Telegraf is asked for its version — and, run against a *staged* program before the running
/// one is stopped, this kind's preflight (ADR-0017 clause 54). The same arguments serve both,
/// because what makes them a version probe is what makes them a safe check: cheap, and touching no
/// state.
const VERSION_ARGS: &[&str] = &["--version"];

/// The keys a `command` recipe for Telegraf carries and this kind supplies itself
/// (ADR-draft-telegraf), each with what answers it — refused by name rather than met with serde's
/// "unknown field", so an operator rewriting a `command` block is told where each value went.
const RETIRED: &[(&str, &str)] = &[
    ("args", "the kind points Telegraf at its delivered configuration"),
    (
        "version_args",
        "Telegraf prints a Semantic Versioning version for `--version`, which the kind asks for",
    ),
    (
        "reload_signal",
        "Telegraf re-reads its configuration on SIGHUP, which the kind applies where signals exist \
         and replaces with a restart where they do not",
    ),
    (
        "env",
        "a single-file program published as-is needs no environment of ours",
    ),
    (
        "working_dir",
        "a Managed Process starts in the directory its program lives in",
    ),
];

/// The arguments the daemon is started with — derived whole, so the test below is the statement of
/// what a host runs.
fn daemon_args(ctx: &SupervisorContext) -> Vec<String> {
    vec![
        "--config".to_string(),
        ctx.config_dir
            .join(CONFIG_ENTRY)
            .to_string_lossy()
            .into_owned(),
    ]
}

/// The `telegraf` kind. Its [`Spawner`] is the one the registry hands every plugin (ADR-0037).
#[derive(Default)]
pub struct TelegrafPlugin {
    pub spawner: Spawner,
}

impl Plugin for TelegrafPlugin {
    fn kind(&self) -> &'static str {
        "telegraf"
    }

    fn program_key(&self) -> &'static str {
        "command"
    }

    /// A single file, so there is no tree and no `program_path`: the installer takes the member at
    /// [`MEMBER`] out of the archive as InfluxData published it (ADR-draft-telegraf).
    fn defaults(&self) -> KindDefaults {
        KindDefaults {
            program: Some(PROGRAM),
            program_path: None,
            member: Some(MEMBER),
            // A single file, so no tree and no link it keeps.
            tree_links: &[],
            service_name: Some(SERVICE_NAME),
            // Wrapped, with nothing to correct: the fleet's `[supervisors]`/`[updates]` policy
            // stands, and the block says nothing about it (ADR-0017).
            timing: Some(KindTiming::default()),
            // Telegraf speaks no OpAMP to us; its Endpoint is bound and nothing connects to it.
            endpoint_port: false,
        }
    }

    fn start(&self, mut ctx: SupervisorContext) -> Result<mpsc::Sender<ProcessCommand>, String> {
        // Strictly empty: a kind that knows its agent has no escape hatch (ADR-0017), so anything
        // left here is a key nobody supplies.
        let _: TelegrafSettings = parse_settings(
            &ctx.name,
            self.kind(),
            RETIRED,
            std::mem::take(&mut ctx.settings),
        )?;
        let args = daemon_args(&ctx);
        let program = ctx.program;
        let (commands, command_rx) = mpsc::channel(16);
        let version_args: Vec<String> = VERSION_ARGS.iter().map(|a| (*a).to_string()).collect();
        let runner = Runner {
            endpoint_token: ctx.endpoint_token.clone(),
            name: ctx.name,
            stop_timeout: ctx.stop_timeout,
            apply_grace: ctx.apply_grace,
            retain_previous: ctx.retain_previous,
            install: Some(ctx.install),
            archive_key: ctx.archive_key,
            version_probe: Some(VersionProbe {
                program: program.clone(),
                args: version_args.clone(),
                working_dir: None,
                // Telegraf's banner carries a plain Semantic Versioning version, so the strict
                // default read finds it and no parser of its own is needed.
                parse: None,
                env: Vec::new(),
            }),
            preflight: Some(Preflight {
                args: version_args,
                env: Vec::new(),
                working_dir: None,
            }),
            // `SIGHUP` is Telegraf's documented way of re-reading its configuration — the same
            // signal `systemctl reload` sends it. Windows has no signal at all, and the Runner
            // falls back to the restart there, which is the whole reason this is a property of the
            // kind rather than a key: the one block then serves both platforms.
            reload_signal: sighup(),
            // The Client itself, or the second service where a process account is registered
            // (ADR-0037 clause 18): the registry says which.
            spawner: self.spawner.clone(),
            process_account_group: ctx.process_account_group,
            supervisor_dir: ctx.supervisor_dir.clone(),
            events: ctx.events,
            commands: command_rx,
            build: Box::new(move || {
                // Telegraf starts whether or not the fleet has delivered anything: with no
                // configuration it exits and the Runner reports that, which is the honest state.
                Some(ProcessSpec {
                    program: program.clone(),
                    args: args.clone(),
                    env: Vec::new(),
                    // Its own program's directory (ADR-0017).
                    working_dir: None,
                    // One process, no worker of its own.
                    own_process_group: false,
                    // Telegraf writes to its outputs, not to a directory of its own.
                    ensure_dirs: Vec::new(),
                })
            }),
        };
        tokio::spawn(runner.run(ctx.shutdown));
        Ok(commands)
    }

    fn check(&self, name: &str, settings: toml::Table) -> Result<(), String> {
        parse_settings::<TelegrafSettings>(name, self.kind(), RETIRED, settings).map(|_| ())
    }
}

/// This kind has no settings at all — the strict parse accepts an empty table and refuses every key
/// (ADR-draft-telegraf). Written as a type rather than a length check so the refusal reads the same
/// as every other plugin's.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TelegrafSettings {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole block is `type` and `name`, so anything else is a key nobody supplies.
    /// Verifies: ADR-draft-telegraf
    #[test]
    fn the_block_has_no_settings() {
        let empty: toml::Table = toml::Table::new();
        TelegrafPlugin::default()
            .check("telegraf", empty)
            .expect("empty");

        let extra: toml::Table = toml::from_str("interval = \"10s\"").expect("table");
        let err = TelegrafPlugin::default()
            .check("telegraf", extra)
            .expect_err("refused");
        assert!(err.contains("interval"), "{err}");
    }

    /// Each key a `command` recipe carries is refused by name with what supplies it — including
    /// through `check`, so a Supervisor set the Server offers is refused before any running process
    /// is touched (ADR-0017).
    /// Verifies: ADR-draft-telegraf
    #[test]
    fn the_recipes_keys_are_refused_by_name() {
        for (key, line) in [
            ("args", "args = [\"--config\", \"/x\"]"),
            ("version_args", "version_args = [\"--version\"]"),
            ("reload_signal", "reload_signal = \"HUP\""),
            ("env", "env = { HOME = \"/x\" }"),
            ("working_dir", "working_dir = \"/x\""),
        ] {
            let table: toml::Table = toml::from_str(line).expect("table");
            let err = TelegrafPlugin::default()
                .check("telegraf", table)
                .expect_err(key);
            assert!(err.contains(key), "{err}");
            assert!(err.contains("no longer a supervisor key"), "{err}");
        }
    }

    /// The whole invocation: Telegraf is pointed at the one Configuration the fleet delivers for
    /// it, in this Supervisor's own `config/` directory.
    /// Verifies: ADR-draft-telegraf
    #[test]
    fn the_invocation_points_at_the_delivered_configuration() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_tx, shutdown) = crate::shutdown::shutdown_channel();
        let (events, _rx) = tokio::sync::mpsc::channel(1);
        let ctx = SupervisorContext {
            endpoint_token: zeroize::Zeroizing::new(String::new()),
            name: "telegraf".to_string(),
            supervisor_dir: dir.path().to_path_buf(),
            config_dir: dir.path().join("config"),
            program: dir.path().join("program").join(PROGRAM),
            install: crate::supervisor::ports::InstallTarget::binary(
                dir.path().join("program").join(PROGRAM),
            ),
            archive_key: None,
            process_account_group: None,
            settings: toml::Table::new(),
            stop_timeout: std::time::Duration::from_secs(1),
            apply_grace: std::time::Duration::from_secs(1),
            retain_previous: std::time::Duration::ZERO,
            events: crate::supervisor::ports::EventSender::new(0, events),
            shutdown,
        };
        assert_eq!(
            daemon_args(&ctx),
            vec![
                "--config".to_string(),
                dir.path()
                    .join("config")
                    .join(CONFIG_ENTRY)
                    .to_string_lossy()
                    .into_owned(),
            ]
        );
    }

    /// What the kind supplies is what `opamp-fleetctl package fetch` packs and uploads — the client
    /// half of `docs/artifacts/telegraf.md`, whose packing half is
    /// `telegraf_urls_carry_upstreams_spelling_and_the_platform_this_fleet_names`. An upstream that
    /// renames the program turns this red rather than a rollout.
    /// Verifies: ADR-draft-telegraf
    #[test]
    fn the_defaults_are_the_artifacts() {
        let defaults = TelegrafPlugin::default().defaults();
        assert_eq!(defaults.service_name, Some("telegraf"));
        assert_eq!(
            defaults.program_path, None,
            "a single-file package has no tree"
        );
        assert!(!defaults.endpoint_port);
        #[cfg(windows)]
        assert_eq!(defaults.program, Some("telegraf.exe"));
        #[cfg(not(windows))]
        assert_eq!(defaults.program, Some("telegraf"));
        // The member is the program at its place in the archive, and installs under its own name.
        assert_eq!(defaults.member, Some(MEMBER));
        assert_eq!(
            std::path::Path::new(MEMBER)
                .file_name()
                .and_then(std::ffi::OsStr::to_str),
            Some(PROGRAM)
        );
    }

    /// The archive as section 5 of `docs/artifacts/telegraf.md` describes it — a version-named
    /// directory, the program at `usr/bin/telegraf` among further entries named `telegraf` on Unix,
    /// at the root beside `telegraf.conf` on Windows — installs the program and nothing else,
    /// whatever order the archive keeps. The Unix fixture puts the logrotate rule and the
    /// directories named `telegraf` *ahead* of the program, the order in which a selection by file
    /// name installed the logrotate rule as the program.
    /// Verifies: ADR-draft-telegraf
    #[test]
    fn the_program_is_taken_from_its_place_in_upstreams_archive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = dir.path().join("telegraf-1.39.3");
        #[cfg(not(windows))]
        {
            let file = std::fs::File::create(&archive).expect("create");
            let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
            let mut builder = tar::Builder::new(encoder);
            for (name, content) in [
                ("./telegraf-1.39.3/", &b""[..]),
                (
                    "./telegraf-1.39.3/etc/logrotate.d/telegraf",
                    b"/var/log/telegraf/*.log {}",
                ),
                ("./telegraf-1.39.3/etc/telegraf/", b""),
                ("./telegraf-1.39.3/etc/telegraf/telegraf.conf", b"[agent]"),
                ("./telegraf-1.39.3/usr/lib/telegraf/", b""),
                ("./telegraf-1.39.3/var/log/telegraf/", b""),
                ("./telegraf-1.39.3/usr/bin/telegraf", b"the program"),
            ] {
                let mut header = tar::Header::new_gnu();
                if name.ends_with('/') {
                    header.set_entry_type(tar::EntryType::Directory);
                }
                header.set_size(content.len() as u64);
                header.set_mode(0o755);
                header.set_cksum();
                builder
                    .append_data(&mut header, name, content)
                    .expect("append");
            }
            builder
                .into_inner()
                .expect("finish tar")
                .finish()
                .expect("finish gzip");
        }
        #[cfg(windows)]
        {
            use std::io::Write as _;
            let file = std::fs::File::create(&archive).expect("create");
            let mut writer = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default();
            writer
                .add_directory("telegraf-1.39.3/", options)
                .expect("directory");
            for (name, content) in [
                ("telegraf-1.39.3/telegraf.conf", &b"[agent]"[..]),
                ("telegraf-1.39.3/telegraf.exe", b"the program"),
            ] {
                writer.start_file(name, options).expect("start");
                writer.write_all(content).expect("write");
            }
            writer.finish().expect("finish");
        }

        let program = dir.path().join(PROGRAM);
        crate::install::write_program(&archive, &program, MEMBER, None).expect("installed");
        assert_eq!(std::fs::read(&program).expect("read"), b"the program");
    }
}
