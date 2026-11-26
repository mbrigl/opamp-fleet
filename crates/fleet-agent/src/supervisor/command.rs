//! The `command` plugin: the example Custom Supervisor (CLT-wrappers-act-through-the-core). It brings a Foreign Agent —
//! any process started by a command-line invocation — under management: spawned as configured,
//! restarted when a remote configuration arrives (the files land in the Supervisor's
//! `config/` directory for the process to re-read), health derived from the outside.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::supervisor::port::{Entry, Instance, Manifest, Plugin, Process, Spec, Value};
use crate::supervisor::ports::{parse_settings, KindDefaults};

/// The block's plugin-specific keys, parsed strictly — a typo fails startup, per DEV-six-crates-tokio-axum.
///
/// `command` is not among them: the core takes it out and resolves it (CLT-wrappers-act-through-the-core), and what
/// arrives here is [`SupervisorContext::program`].
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandSettings {
    /// Its arguments, verbatim.
    #[serde(default)]
    args: Vec<String>,
    /// Additional environment for the process.
    #[serde(default)]
    env: BTreeMap<String, String>,
    /// Arguments that make the command print its version (e.g. `["--version"]`). When set, the
    /// command is invoked once with exactly these arguments and the first Semantic Versioning
    /// 2.0.0 version in its output becomes the Agent's `service.version`. A Foreign Agent's
    /// version flag is its own convention — hence opt-in, unlike the Collector's.
    ///
    /// They are also this kind's **preflight** (CLT-wrappers-act-through-the-core §33): a package's staged program is run with
    /// them before the running one is stopped, and a non-zero exit refuses the package with the
    /// program's own message. Same arguments, same contract — a check that is cheap and touches
    /// no state — asked where a refusal costs nothing rather than after the swap.
    #[serde(default)]
    version_args: Option<Vec<String>>,
}

/// The keys this kind does not take (CLT-wrappers-act-through-the-core), each with what answers it. Refused by name rather
/// than met with serde's "unknown field", because a block carrying one expects a Client that needs
/// it, and the operator deleting the line deserves to be told where the value went.
const RETIRED: &[(&str, &str)] = &[
    (
        "working_dir",
        "a Managed Process starts in the directory its program lives in",
    ),
    (
        "reload_signal",
        "whether a program re-reads its configuration on a signal is the program's own convention \
         and belongs in a kind that knows it — an unwrapped agent applies by restarting",
    ),
];

/// The `command` kind, on the Plugin port of CLT-wrappers-act-through-the-core: it states how
/// the Foreign Agent runs, and the core does the rest.
#[derive(Default)]
pub struct Command;

impl Plugin for Command {
    fn manifest(&self) -> Manifest {
        Manifest {
            kind: "command",
            program_key: "command",
            defaults: KindDefaults::none(),
        }
    }

    fn check(&self, name: &str, settings: toml::Table) -> Result<(), String> {
        parse_settings::<CommandSettings>(name, "command", RETIRED, settings).map(|_| ())
    }

    fn instantiate(&self, name: &str, settings: toml::Table) -> Result<Box<dyn Instance>, String> {
        let settings = parse_settings(name, "command", RETIRED, settings)?;
        Ok(Box::new(CommandInstance { settings }))
    }
}

/// One `command` Supervisor: its block's arguments, environment and version arguments.
struct CommandInstance {
    settings: CommandSettings,
}

impl Instance for CommandInstance {
    fn process(&self) -> Process {
        Process {
            version_args: self.settings.version_args.clone(),
            reload_signal: None,
        }
    }

    /// The operator's arguments and environment, as written, the placeholders of the
    /// Supervisor's own directories expanded by the core (CLT-wrappers-act-through-the-core §31).
    fn spec(&self, _entries: &[Entry]) -> Option<Spec> {
        Some(Spec {
            args: self
                .settings
                .args
                .iter()
                .map(|a| Value::Template(a.clone()))
                .collect(),
            env: self
                .settings
                .env
                .iter()
                .map(|(k, v)| (k.clone(), Value::Template(v.clone())))
                .collect(),
            working_dir: None,
            own_process_group: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `command` is gone from these settings — the core resolves it (CLT-wrappers-act-through-the-core) — so a block that
    /// still carries it here would be an unknown key, which is exactly what must fail.
    #[test]
    fn settings_parse_strictly() {
        let table: toml::Table = toml::from_str(
            r#"
            args = ["--a"]
            version_args = ["--version"]
            [env]
            K = "v"
            "#,
        )
        .expect("table");
        let settings: CommandSettings = table.try_into().expect("settings");
        assert_eq!(settings.env.get("K").map(String::as_str), Some("v"));
        assert_eq!(settings.version_args, Some(vec!["--version".to_string()]));

        let typo: toml::Table = toml::from_str("comand = \"/x\"").expect("table");
        assert!(typo.try_into::<CommandSettings>().is_err());
    }

    /// The two keys CLT-server-manages-the-supervisor-set retires are refused by name, on both sides of the seam: at startup,
    /// and in an offered Supervisor set before any running process is touched (CLT-server-manages-the-supervisor-set). Each
    /// message says what supplies the value now, because a block carrying one was written against
    /// a Client that took it.
    #[test]
    fn the_retired_keys_are_refused_by_name() {
        for (key, line) in [
            ("working_dir", "working_dir = \"/tmp\""),
            ("reload_signal", "reload_signal = \"HUP\""),
        ] {
            let table: toml::Table = toml::from_str(line).expect("table");
            let err = Command.check("agent", table).expect_err(key);
            assert!(err.contains(key), "{err}");
            assert!(err.contains("no longer a supervisor key"), "{err}");
        }
    }

    /// What `command` asks of the host for each input: it starts, applies, installs, restarts
    /// and uninstalls through the host function of the same name, and starts its program on the
    /// operator's arguments and environment as templates.
    /// Verifies: CLT-wrappers-act-through-the-core
    #[tokio::test]
    async fn command_acts_only_through_the_host() {
        use crate::supervisor::port::recorded::Recorded;
        use crate::supervisor::port::Input;

        let table: toml::Table =
            toml::from_str("args = [\"-c\", \"${config_dir}/a\"]\n[env]\nK = \"v\"\n")
                .expect("table");
        let mut instance = Command.instantiate("agent", table).expect("instance");
        let host = Recorded::default();
        for input in [
            Input::Start {
                entries: Vec::new(),
            },
            Input::ApplyConfig {
                hash: vec![1],
                entries: Vec::new(),
            },
            Input::ApplyPackage {
                version: "1.0.0".to_string(),
                hash: vec![2],
            },
            Input::Restart,
            Input::Uninstall,
        ] {
            instance.handle(input, &host).await.expect("handled");
        }
        let spec = Spec {
            args: vec![
                Value::Template("-c".to_string()),
                Value::Template("${config_dir}/a".to_string()),
            ],
            env: vec![("K".to_string(), Value::Template("v".to_string()))],
            working_dir: None,
            own_process_group: false,
        };
        assert_eq!(
            host.calls(),
            vec![
                format!("start {spec:?}"),
                format!("apply_config [1] {:?}", Some(&spec)),
                "install [2]".to_string(),
                "restart".to_string(),
                "uninstall".to_string(),
            ]
        );
        assert_eq!(
            instance.process(),
            Process::default(),
            "no version arguments, no reload signal"
        );
    }
}
