//! The `telegraf` plugin (CLT-telegraf-kind): Telegraf, delivered as a package and run out of this
//! Supervisor's own directory.
//!
//! There is nothing to decide about how Telegraf is invoked. It is a single-file program, it takes
//! its configuration from `--config`, it prints a Semantic Versioning version for `--version`, and
//! it re-reads its configuration on `SIGHUP`. All four are Telegraf's own properties, and as block
//! keys every host would repeat them — the reload signal not at all, since no block may declare
//! one (CLT-wrappers-act-through-the-core §7), so a `command` block restarts Telegraf on every
//! apply. Here the reload is the kind's, and platform-correct by construction: the signal where
//! there are signals, and the core's restart where there are none.
//!
//! The kind states all of it as data on the Plugin port and does nothing itself
//! (CLT-wrappers-act-through-the-core §1): the core runs, reloads and probes Telegraf.

use crate::supervisor::port::{
    parse_settings, Entry, Instance, KindDefaults, KindTiming, Manifest, Plugin, Probe, Process,
    Root, RootPath, Spec, Value,
};

/// The program's file name in this Supervisor's `program/` directory
/// (CLT-wrappers-act-through-the-core).
#[cfg(windows)]
const PROGRAM: &str = "telegraf.exe";
#[cfg(not(windows))]
const PROGRAM: &str = "telegraf";

/// Where InfluxData's archive holds the program, below the version-named directory that wraps it
/// (CLT-telegraf-kind §3). The Unix tarballs carry further entries named `telegraf` — the
/// logrotate rule `etc/logrotate.d/telegraf`, and the directories `etc/telegraf/`,
/// `usr/lib/telegraf/` and `var/log/telegraf/` — so the file name alone does not say which member
/// is the program; the path does, and the installer refuses an archive in which it matches more
/// than one (PKG-signed-packages-and-delivery §55).
#[cfg(windows)]
const MEMBER: &str = "telegraf.exe";
#[cfg(not(windows))]
const MEMBER: &str = "usr/bin/telegraf";

/// The Agent type every Telegraf Configuration is aimed at (CLT-what-an-agent-reports), a property
/// of the packing side rather than of a host.
const SERVICE_NAME: &str = "telegraf";

/// The name of the Configuration the kind points `--config` at, which is the name
/// `opamp-fleetctl package fetch` uploads its default Configuration under (CLT-telegraf-kind §5):
/// the Agent type plus `-conf`. A rename here alone leaves Telegraf reading a file that does not
/// exist, and `the_default_configuration_is_named_as_the_kind_reads_it` in `fetch.rs` reads this
/// declaration's literal to fail it.
const CONFIG_ENTRY: &str = "telegraf-conf";

/// How Telegraf is asked for its version — and, run against a *staged* program before the running
/// one is stopped, this kind's preflight (CLT-wrappers-act-through-the-core §33). The same
/// arguments serve both, because what makes them a version probe is what makes them a safe check:
/// cheap, and touching no state.
const VERSION_ARGS: &[&str] = &["--version"];

/// `SIGHUP`, Telegraf's documented way of re-reading its configuration — the same signal
/// `systemctl reload` sends it. Its number is 1 on every Unix (POSIX), and the kind names the
/// number rather than a system crate it may not reach (CLT-wrappers-act-through-the-core §1).
/// Windows has no signal at all, and the core applies by restart there
/// (CLT-wrappers-act-through-the-core §7), which is the whole reason this is a property of the
/// kind rather than a key: the one block then serves both platforms.
#[cfg(unix)]
const RELOAD_SIGNAL: Option<i32> = Some(1);
#[cfg(not(unix))]
const RELOAD_SIGNAL: Option<i32> = None;

/// The keys a `command` recipe for Telegraf carries and this kind supplies itself
/// (CLT-telegraf-kind), each with what answers it — refused by name rather than met with serde's
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

/// What a start of Telegraf runs — derived whole, so the test below is the statement of how a
/// host starts it: pointed at the one Configuration the fleet delivers for it, in this
/// Supervisor's own `config/` directory, with no environment and its program's own directory as
/// its working directory (CLT-wrappers-act-through-the-core §6).
fn spec() -> Spec {
    Spec {
        args: vec![
            Value::Text("--config".to_string()),
            Value::Rooted(RootPath {
                root: Root::Config,
                path: CONFIG_ENTRY.to_string(),
            }),
        ],
        env: Vec::new(),
        working_dir: None,
    }
}

/// The `telegraf` kind.
#[derive(Default)]
pub struct Telegraf;

impl Plugin for Telegraf {
    fn manifest(&self) -> Manifest {
        Manifest {
            // A single file, so there is no tree and no `program_path`: the installer takes the
            // member at [`MEMBER`] out of the archive as InfluxData published it
            // (CLT-telegraf-kind).
            defaults: KindDefaults {
                program: Some(PROGRAM),
                program_path: None,
                member: Some(MEMBER),
                // A single file, so no tree and no link it keeps.
                tree_links: &[],
                service_name: Some(SERVICE_NAME),
                // Wrapped, with nothing to correct: the fleet's `[supervisors]`/`[updates]` policy
                // stands, and the block says nothing about it (CLT-wrappers-act-through-the-core §23).
                timing: Some(KindTiming::default()),
                // Telegraf speaks no OpAMP to us; its Endpoint is bound and nothing connects to it.
                endpoint_port: false,
            },
            reload_signal: RELOAD_SIGNAL,
            // Telegraf starts children: the programs its `exec` and `execd` inputs and outputs
            // name, which `execd` keeps running. Leading a group lets the bounded stop take them
            // with it (CLT-wrappers-act-through-the-core §32, CLT-telegraf-kind §8); the reload
            // signal still goes to Telegraf's pid alone.
            own_process_group: true,
            // Telegraf writes to its outputs, not to a directory of its own.
            data_dirs: &[],
            ..Manifest::of("telegraf", "command")
        }
    }

    fn check(&self, name: &str, settings: toml::Table) -> Result<(), String> {
        parse_settings::<TelegrafSettings>(name, "telegraf", RETIRED, settings).map(|_| ())
    }

    fn instantiate(&self, name: &str, settings: toml::Table) -> Result<Box<dyn Instance>, String> {
        // Strictly empty: a kind that knows its agent has no escape hatch
        // (CLT-wrappers-act-through-the-core §20), so anything left here is a key nobody supplies.
        let _: TelegrafSettings = parse_settings(name, "telegraf", RETIRED, settings)?;
        Ok(Box::new(TelegrafInstance))
    }
}

/// This kind has no settings at all — the strict parse accepts an empty table and refuses every key
/// (CLT-telegraf-kind). Written as a type rather than a length check so the refusal reads the same
/// as every other plugin's.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TelegrafSettings {}

/// One Telegraf Supervisor. It holds nothing: everything it states is the kind's.
struct TelegrafInstance;

impl Instance for TelegrafInstance {
    fn process(&self) -> Process {
        Process {
            probe: Some(Probe {
                args: VERSION_ARGS
                    .iter()
                    .map(|a| Value::Text((*a).to_string()))
                    .collect(),
                ..Probe::default()
            }),
            // Telegraf's banner carries a plain Semantic Versioning version, so the strict default
            // read finds it and no parser of its own is needed.
            read_version: None,
        }
    }

    /// Telegraf starts whether or not the fleet has delivered anything: with no configuration it
    /// exits and the core reports that, which is the honest state.
    fn spec(&self, _entries: &[Entry]) -> Option<Spec> {
        Some(spec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole block is `type` and `name`, so anything else is a key nobody supplies.
    /// Verifies: CLT-telegraf-kind
    #[test]
    fn the_block_has_no_settings() {
        Telegraf
            .check("telegraf", toml::Table::new())
            .expect("empty");

        let extra: toml::Table = toml::from_str("interval = \"10s\"").expect("table");
        let err = Telegraf.check("telegraf", extra).expect_err("refused");
        assert!(err.contains("interval"), "{err}");
    }

    /// Each key a `command` recipe carries is refused by name with what supplies it — including
    /// through `check`, so a Supervisor set the Server offers is refused before any running process
    /// is touched (CLT-wrappers-act-through-the-core).
    /// Verifies: CLT-telegraf-kind
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
            let err = Telegraf.check("telegraf", table).expect_err(key);
            assert!(err.contains(key), "{err}");
            assert!(err.contains("no longer a supervisor key"), "{err}");
        }
    }

    /// A value the kind derives is refused where a block restates it
    /// (CLT-wrappers-act-through-the-core §20), through the check an offered Supervisor set passes:
    /// the program key `command`, and `program_path`, which says nothing for a single-file package.
    /// Verifies: CLT-telegraf-kind
    #[test]
    fn restating_a_derived_value_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let block = |line: &str| -> crate::config::ClientConfig {
            toml::from_str(&format!(
                "endpoint = \"ws://127.0.0.1:1/v1/opamp\"\nstate_dir = {state:?}\n\
                 [[supervisor]]\ntype = \"telegraf\"\nname = \"telegraf\"\n{line}",
                state = dir.path().join("state").to_string_lossy(),
            ))
            .expect("parse")
        };
        let plain = block("");
        crate::supervisor::validate_block(&plain, &plain.supervisors[0]).expect("two lines");
        for (key, line) in [
            ("command", "command = \"/usr/bin/telegraf\"\n"),
            ("program_path", "program_path = \"usr/bin/telegraf\"\n"),
        ] {
            let config = block(line);
            let err =
                crate::supervisor::validate_block(&config, &config.supervisors[0]).expect_err(key);
            assert!(err.contains(key), "{err}");
            assert!(err.contains("no longer a supervisor key"), "{err}");
        }
    }

    /// The whole invocation: Telegraf is pointed at the one Configuration the fleet delivers for
    /// it, in this Supervisor's own `config/` directory, by the name `opamp-fleetctl package fetch`
    /// uploads it under (CLT-telegraf-kind §5), and starts with nothing else of ours. The name is
    /// written out, not read from the constant, so a rename of the constant fails here.
    /// Verifies: CLT-telegraf-kind
    #[test]
    fn the_invocation_points_at_the_delivered_configuration() {
        let instance = Telegraf
            .instantiate("telegraf", toml::Table::new())
            .expect("instance");
        let spec = instance.spec(&[]).expect("starts without a delivery");
        assert_eq!(
            spec.args,
            vec![
                Value::Text("--config".to_string()),
                Value::Rooted(RootPath {
                    root: Root::Config,
                    path: "telegraf-conf".to_string(),
                }),
            ]
        );
        assert!(spec.env.is_empty() && spec.working_dir.is_none());
    }

    /// Telegraf leads its own process group, so the bounded stop takes the children its `exec`
    /// and `execd` plugins start (CLT-telegraf-kind §8). It re-reads its configuration on
    /// `SIGHUP` where there are signals, and the core restarts it where there are none. It asks
    /// for no directory to be made.
    /// Verifies: CLT-telegraf-kind
    #[test]
    fn telegraf_leads_its_own_process_group_and_reloads_on_sighup() {
        let manifest = Telegraf.manifest();
        assert!(
            manifest.own_process_group,
            "a stop takes Telegraf's children"
        );
        #[cfg(unix)]
        assert_eq!(manifest.reload_signal, Some(libc::SIGHUP));
        #[cfg(not(unix))]
        assert_eq!(manifest.reload_signal, None);
        assert!(manifest.data_dirs.is_empty());
        assert_eq!(
            (manifest.kind, manifest.program_key),
            ("telegraf", "command")
        );
    }

    /// The version probe asks `--version`, the same arguments the preflight runs against a staged
    /// program, and reads the version strictly.
    /// Verifies: CLT-telegraf-kind
    #[test]
    fn the_version_is_asked_for_with_version() {
        let process = Telegraf
            .instantiate("telegraf", toml::Table::new())
            .expect("instance")
            .process();
        let probe = process.probe.expect("a probe");
        assert_eq!(probe.args, vec![Value::Text("--version".to_string())]);
        assert!(probe.env.is_empty() && probe.working_dir.is_none());
        assert!(process.read_version.is_none(), "strict Semantic Versioning");
    }

    /// The kind acts only through the host: every input reaches the host function of its name,
    /// the start and the apply on the derived invocation (CLT-wrappers-act-through-the-core §1).
    /// Verifies: CLT-wrappers-act-through-the-core, CLT-telegraf-kind
    #[tokio::test]
    async fn telegraf_acts_only_through_the_host() {
        use crate::supervisor::port::recorded::Recorded;
        use crate::supervisor::port::Input;
        let mut instance = Telegraf
            .instantiate("telegraf", toml::Table::new())
            .expect("instance");
        let host = Recorded::default();
        for input in [
            Input::Start {
                entries: Vec::new(),
            },
            Input::ApplyConfig {
                hash: vec![1],
                entries: Vec::new(),
            },
            Input::Uninstall,
        ] {
            instance.handle(input, &host).await.expect("handled");
        }
        let spec = spec();
        assert_eq!(
            host.calls(),
            vec![
                format!("start {spec:?}"),
                format!("apply_config [1] {:?}", Some(&spec)),
                "uninstall".to_string(),
            ]
        );
    }

    /// What the kind supplies is what `opamp-fleetctl package fetch` packs and uploads — the client
    /// half of `docs/artifacts/telegraf.md`, whose packing half is
    /// `telegraf_urls_carry_upstreams_spelling_and_the_platform_this_fleet_names`. An upstream that
    /// renames the program turns this red rather than a rollout.
    /// Verifies: CLT-telegraf-kind
    #[test]
    fn the_defaults_are_the_artifacts() {
        let defaults = Telegraf.defaults();
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
    /// Verifies: CLT-telegraf-kind
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
