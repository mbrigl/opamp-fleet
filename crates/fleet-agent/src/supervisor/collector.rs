//! The `collector` plugin: the Collector Supervisor, on the Plugin port of
//! CLT-wrappers-act-through-the-core. It owns an OpenTelemetry Collector: started with one
//! `--config` flag per written configuration entry (the Collector merges multiple configs itself,
//! so no YAML is touched here) and restarted when a new remote configuration arrives. Until a
//! configuration exists nothing runs and the Agent reports "awaiting configuration".
//!
//! A Collector carrying the `opampextension` additionally reports its own description, health,
//! and effective configuration to the Supervisor Endpoint; one without it is observed from the
//! outside. Either way it is the same plugin (goal 16 versus plain supervision).
//!
//! The kind lists no directory and starts no program: the core hands it the written entries, and
//! it answers with the process specification that names them (CLT-wrappers-act-through-the-core
//! §10).

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::supervisor::port::{
    Entry, Instance, Manifest, Plugin, Process, Root, RootPath, Spec, Value,
};
use crate::supervisor::ports::{parse_settings, KindDefaults};

/// The block's plugin-specific keys, parsed strictly — a typo fails startup, per
/// DEV-six-crates-tokio-axum.
///
/// `binary` is not among them: the core takes it out and resolves it
/// (CLT-wrappers-act-through-the-core §27).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CollectorSettings {
    /// Extra arguments, appended after the `--config` flags.
    #[serde(default)]
    args: Vec<String>,
    /// Additional environment for the Collector process. Environment is not a `command`-only
    /// feature — a Collector's config reads `${env:VAR}` too — so it is honoured here just as the
    /// command plugin honours it, values expanded through the same placeholders
    /// (CLT-wrappers-act-through-the-core §31).
    #[serde(default)]
    env: BTreeMap<String, String>,
}

/// The Collector kind.
#[derive(Default)]
pub struct Collector;

impl Plugin for Collector {
    fn manifest(&self) -> Manifest {
        Manifest {
            kind: "collector",
            program_key: "binary",
            // Nothing else: a Collector's distribution is a decision the block states, and a
            // Foreign Agent is by definition one nobody has written a wrapper for. The one
            // exception: something *does* connect to a Collector's Endpoint, the
            // `opampextension`, so pinning its port is a decision that has a place.
            defaults: KindDefaults {
                endpoint_port: true,
                ..KindDefaults::none()
            },
        }
    }

    fn check(&self, name: &str, settings: toml::Table) -> Result<(), String> {
        parse_settings::<CollectorSettings>(name, "collector", &[], settings).map(|_| ())
    }

    fn instantiate(&self, name: &str, settings: toml::Table) -> Result<Box<dyn Instance>, String> {
        let settings = parse_settings(name, "collector", &[], settings)?;
        Ok(Box::new(CollectorInstance { settings }))
    }
}

/// One Collector Supervisor: its block's extra arguments and environment.
struct CollectorInstance {
    settings: CollectorSettings,
}

impl Instance for CollectorInstance {
    /// The Collector states its version on `--version`, so even one without the opampextension,
    /// which never self-reports, shows its own version rather than none. The same `--version` is
    /// asked of a staged program before the running one is stopped
    /// (CLT-wrappers-act-through-the-core §33): a build the host cannot run is refused before
    /// anything stops. A configuration is applied by restart, as the reference supervisor does.
    fn process(&self) -> Process {
        Process {
            version_args: Some(vec!["--version".to_string()]),
            reload_signal: None,
        }
    }

    /// One `--config` per entry that is configuration itself, then the operator's extra
    /// arguments, with its environment. Supplementary content lies beside it for the Collector to
    /// read by path, and handing it over as `--config` is what the role exists to prevent
    /// (SRV-configurations-compose-one-map). `None` until a configuration exists: the Collector
    /// does not run on nothing.
    fn spec(&self, entries: &[Entry]) -> Option<Spec> {
        let configuration: Vec<&Entry> = entries.iter().filter(|e| e.role.is_none()).collect();
        if configuration.is_empty() {
            return None;
        }
        let mut args = Vec::with_capacity(configuration.len() * 2 + self.settings.args.len());
        for entry in configuration {
            args.push(Value::Text("--config".to_string()));
            args.push(Value::Rooted(RootPath {
                root: Root::Config,
                path: entry.name.clone(),
            }));
        }
        args.extend(self.settings.args.iter().cloned().map(Value::Template));
        Some(Spec {
            args,
            env: self
                .settings
                .env
                .iter()
                .map(|(k, v)| (k.clone(), Value::Template(v.clone())))
                .collect(),
            working_dir: None,
            // One process, no worker of its own: signalling a group would gain nothing
            // (CLT-wrappers-act-through-the-core §32).
            own_process_group: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::port::recorded::Recorded;
    use crate::supervisor::port::Input;

    /// `binary` is gone from these settings — the core resolves it — so a block that still
    /// carries it here would be an unknown key, which is exactly what must fail.
    #[test]
    fn settings_parse_strictly() {
        let table: toml::Table = toml::from_str("args = [\"--feature-gates=x\"]\n").expect("table");
        let settings: CollectorSettings = table.try_into().expect("settings");
        assert_eq!(settings.args, vec!["--feature-gates=x".to_string()]);

        let typo: toml::Table = toml::from_str("arg = [\"--x\"]").expect("table");
        assert!(typo.try_into::<CollectorSettings>().is_err());
    }

    /// `[supervisor.env]` is accepted on a collector block — environment is not a `command`-only
    /// feature.
    #[test]
    fn env_parses() {
        let table: toml::Table =
            toml::from_str("[env]\nOTLP_HTTP_ENDPOINT = \"127.0.0.1:4318\"\n").expect("table");
        let settings: CollectorSettings = table.try_into().expect("settings");
        assert_eq!(
            settings.env.get("OTLP_HTTP_ENDPOINT").map(String::as_str),
            Some("127.0.0.1:4318")
        );
    }

    fn entry(name: &str, role: Option<&str>) -> Entry {
        Entry {
            name: name.to_string(),
            role: role.map(str::to_string),
            digest: Vec::new(),
        }
    }

    fn text(s: &str) -> Value {
        Value::Text(s.to_string())
    }

    fn config(name: &str) -> Value {
        Value::Rooted(RootPath {
            root: Root::Config,
            path: name.to_string(),
        })
    }

    /// The process specification `collector` asks the host for, per set of written entries: one
    /// `--config` per entry that is configuration, in order, then the block's arguments; nothing
    /// to start while no entry is configuration.
    /// Verifies: CLT-wrappers-act-through-the-core
    #[tokio::test]
    async fn the_spec_names_each_configuration_entry() {
        let table: toml::Table =
            toml::from_str("args = [\"--feature-gates=x\"]\n[env]\nOTLP = \"${config_dir}/o\"\n")
                .expect("table");
        let flag = Value::Template("--feature-gates=x".to_string());
        let cases: Vec<(Vec<Entry>, Option<Vec<Value>>)> = vec![
            (Vec::new(), None),
            (vec![entry("ca", Some("certificate"))], None),
            (vec![entry("ca", Some(""))], None),
            (
                vec![entry("fleet", None)],
                Some(vec![text("--config"), config("fleet"), flag.clone()]),
            ),
            (
                vec![
                    entry("a", None),
                    entry("ca", Some("certificate")),
                    entry("fleet", None),
                ],
                Some(vec![
                    text("--config"),
                    config("a"),
                    text("--config"),
                    config("fleet"),
                    flag.clone(),
                ]),
            ),
        ];
        for (entries, args) in cases {
            let mut instance = Collector
                .instantiate("otel", table.clone())
                .expect("instance");
            let host = Recorded::default();
            instance
                .handle(
                    Input::Start {
                        entries: entries.clone(),
                    },
                    &host,
                )
                .await
                .expect("started");
            instance
                .handle(
                    Input::ApplyConfig {
                        hash: vec![7],
                        entries: entries.clone(),
                    },
                    &host,
                )
                .await
                .expect("applied");
            let spec = args.map(|args| Spec {
                args,
                env: vec![(
                    "OTLP".to_string(),
                    Value::Template("${config_dir}/o".to_string()),
                )],
                working_dir: None,
                own_process_group: false,
            });
            let mut expected = Vec::new();
            if let Some(spec) = &spec {
                expected.push(format!("start {spec:?}"));
            }
            expected.push(format!("apply_config [7] {spec:?}"));
            assert_eq!(host.calls(), expected, "for the entries {entries:?}");
        }
    }

    /// The spec carries the operator's environment, not an empty one. A configuration entry has
    /// to exist first, since the Collector does not run on nothing.
    #[test]
    fn the_spec_carries_the_environment() {
        let table: toml::Table =
            toml::from_str("[env]\nOTLP_HTTP_ENDPOINT = \"127.0.0.1:4318\"\n").expect("table");
        let instance = Collector.instantiate("otel", table).expect("instance");
        assert!(instance.spec(&[]).is_none(), "no configuration, no spec");
        let spec = instance
            .spec(&[entry("fleet", None)])
            .expect("a spec once a configuration exists");
        assert_eq!(
            spec.env,
            vec![(
                "OTLP_HTTP_ENDPOINT".to_string(),
                Value::Template("127.0.0.1:4318".to_string())
            )],
            "the operator's environment reaches the process"
        );
    }

    /// The version is asked on `--version`, which also proves a staged package.
    #[test]
    fn the_version_is_asked_on_version() {
        let instance = Collector
            .instantiate("otel", toml::Table::new())
            .expect("instance");
        assert_eq!(
            instance.process(),
            Process {
                version_args: Some(vec!["--version".to_string()]),
                reload_signal: None,
            }
        );
    }
}
