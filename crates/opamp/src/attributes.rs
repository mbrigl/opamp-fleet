//! The Baseline's attribute keys, and reading a string out of a set of them (ADR-0044).
//!
//! An `AgentDescription` carries its identity as `KeyValue` pairs, and the keys are fixed strings
//! the Baseline names. Both ends match on the same ones —
//! [ADR-0033](../../../docs/adr/0033-an-agents-type-and-its-instance-name-are-two-attributes.md)
//! gives `service.name` and `service.instance.name` their meaning, and
//! [ADR-0031](../../../docs/adr/0031-per-platform-package-variants.md) and
//! [ADR-0034](../../../docs/adr/0034-a-package-states-the-agent-type-it-is-built-for.md) make them
//! decide *which binary a host is offered*.
//!
//! Spelled as literals at each use, a typo in one of them is not a compile error: it is a Selector
//! that quietly matches nothing, or a package that reaches nobody. As constants it is a name the
//! compiler knows. That is the whole reason they live here rather than in each end.

use crate::proto::{any_value, AnyValue, ArrayValue, KeyValue};

/// The Agent *type* — a Collector distribution, this Client — never an operator's name for one
/// (ADR-0033). A package is matched against it (ADR-0034).
pub const SERVICE_NAME: &str = "service.name";
/// The operator's name for one Agent (ADR-0033).
pub const SERVICE_INSTANCE_NAME: &str = "service.instance.name";
/// The namespace an Agent runs in, reported only where the environment uses one.
pub const SERVICE_NAMESPACE: &str = "service.namespace";
/// The version the Agent reports for itself.
pub const SERVICE_VERSION: &str = "service.version";
/// The operating system in the semantic-convention spelling (`linux`, `darwin`, `windows`) — half
/// of the platform a package artifact is chosen by (ADR-0031).
pub const OS_TYPE: &str = "os.type";
/// The human-readable operating system description, e.g. `Ubuntu 26.04 LTS`.
pub const OS_DESCRIPTION: &str = "os.description";
/// The architecture in the semantic-convention spelling (`amd64`, `arm64`) — the other half of the
/// platform (ADR-0031).
pub const HOST_ARCH: &str = "host.arch";

/// The string value of `key`, or `None` when the attribute is absent, holds another type, or is
/// empty.
///
/// **An empty string is not a value.** That rule is load-bearing rather than tidy: ADR-0034 refuses
/// to offer a package to an Agent of another type, and an Agent reporting `service.name = ""` must
/// therefore match no package at all rather than match every untyped one. Stating it here is the
/// point of the module — one of the two copies this replaces enforced it and the other did not.
#[must_use]
pub fn string_value<'a>(attributes: &'a [KeyValue], key: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|kv| kv.key == key)
        .and_then(|kv| kv.value.as_ref())
        .and_then(|value| value.value.as_ref())
        .and_then(|value| match value {
            any_value::Value::StringValue(s) if !s.is_empty() => Some(s.as_str()),
            _ => None,
        })
}

/// Spellings of an operating system that mean a canonical [`OS_TYPE`] value.
///
/// Deliberately short: it exists for what this project does **not** control — an older release file
/// name, a foreign build system, an Agent that predates the convention — not as a general
/// vocabulary. Everything this project produces is already canonical.
const OS_ALIASES: &[(&str, &str)] = &[
    ("macos", "darwin"),
    ("osx", "darwin"),
    ("win", "windows"),
    ("win32", "windows"),
    ("win64", "windows"),
];

/// Spellings of an architecture that mean a canonical [`HOST_ARCH`] value. Rust's own
/// `std::env::consts::ARCH` is among them, which is why an Agent reporting its platform reads the
/// same table the Server matches it against.
const ARCH_ALIASES: &[(&str, &str)] = &[
    ("x86_64", "amd64"),
    ("x86-64", "amd64"),
    ("x64", "amd64"),
    ("aarch64", "arm64"),
];

/// The canonical `os.type` for a spelling of it — the input unchanged when the table has never
/// heard of it.
///
/// One table for both ends, because they are two halves of one comparison: the Client writes this
/// value into its `os.type` attribute and the Server matches an artifact's platform against it
/// (ADR-0031). Two tables that disagreed would not fail — they would offer a host the wrong binary,
/// or none, and say nothing.
#[must_use]
pub fn canonical_os(raw: &str) -> &str {
    canonical(raw, OS_ALIASES)
}

/// The canonical `host.arch` for a spelling of it — the input unchanged when the table has never
/// heard of it. See [`canonical_os`] for why this is shared.
#[must_use]
pub fn canonical_arch(raw: &str) -> &str {
    canonical(raw, ARCH_ALIASES)
}

/// Unknown tokens pass through rather than being refused: the fleet may run a system this table has
/// never heard of, and serving it under its own name is a better failure than not serving it.
fn canonical<'a>(raw: &'a str, aliases: &[(&'static str, &'static str)]) -> &'a str {
    aliases
        .iter()
        .find(|(from, _)| from.eq_ignore_ascii_case(raw))
        .map_or(raw, |(_, to)| *to)
}

/// One attribute as the protocol carries it: a key and a string value.
#[must_use]
pub fn string_attr(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue {
            value: Some(any_value::Value::StringValue(value.to_string())),
        }),
    }
}

/// One attribute carrying a string array — the shape the conventions give `host.ip` and
/// `host.mac`. The wire keeps the typed original; a viewer decides how to render it.
#[must_use]
pub fn string_array_attr(key: &str, values: &[String]) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue {
            value: Some(any_value::Value::ArrayValue(ArrayValue {
                values: values
                    .iter()
                    .map(|value| AnyValue {
                        value: Some(any_value::Value::StringValue(value.clone())),
                    })
                    .collect(),
            })),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attrs() -> Vec<KeyValue> {
        vec![
            string_attr(SERVICE_NAME, "otelcol-contrib"),
            string_attr(SERVICE_INSTANCE_NAME, "edge-01"),
            string_attr(SERVICE_VERSION, ""),
            KeyValue {
                key: "port".to_string(),
                value: Some(AnyValue {
                    value: Some(any_value::Value::IntValue(4320)),
                }),
            },
            KeyValue {
                key: "no.value".to_string(),
                value: None,
            },
        ]
    }

    #[test]
    fn a_string_array_attribute_carries_each_value_typed() {
        let attr = string_array_attr("host.ip", &["10.0.0.7".into(), "192.168.1.140".into()]);
        assert_eq!(attr.key, "host.ip");
        let Some(any_value::Value::ArrayValue(list)) =
            attr.value.as_ref().and_then(|v| v.value.as_ref())
        else {
            panic!("expected an array value");
        };
        let values: Vec<_> = list
            .values
            .iter()
            .filter_map(|v| match v.value.as_ref() {
                Some(any_value::Value::StringValue(s)) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(values, ["10.0.0.7", "192.168.1.140"]);
    }

    #[test]
    fn reads_the_string_an_agent_reported() {
        let attrs = attrs();
        assert_eq!(string_value(&attrs, SERVICE_NAME), Some("otelcol-contrib"));
        assert_eq!(string_value(&attrs, SERVICE_INSTANCE_NAME), Some("edge-01"));
    }

    /// The rule ADR-0034 leans on: an Agent that reports its type as an empty string reports no
    /// type, so it matches no package rather than every untyped one.
    #[test]
    fn an_empty_string_is_not_a_value() {
        assert_eq!(string_value(&attrs(), SERVICE_VERSION), None);
    }

    #[test]
    fn what_is_absent_or_not_a_string_reads_as_nothing() {
        let attrs = attrs();
        assert_eq!(string_value(&attrs, "port"), None, "an int is not a string");
        assert_eq!(string_value(&attrs, "no.value"), None);
        assert_eq!(string_value(&attrs, HOST_ARCH), None, "never reported");
        assert_eq!(string_value(&[], SERVICE_NAME), None);
    }

    /// The first match wins rather than the last, so a duplicate key cannot change an Agent's
    /// identity by being appended to its description.
    #[test]
    fn the_first_of_a_duplicated_key_wins() {
        let attrs = vec![
            string_attr(SERVICE_NAME, "first"),
            string_attr(SERVICE_NAME, "second"),
        ];
        assert_eq!(string_value(&attrs, SERVICE_NAME), Some("first"));
    }

    /// The pairs both ends depend on agreeing: the Client writes the left, the Server matches an
    /// artifact's platform against the right (ADR-0031).
    #[test]
    fn folds_the_spellings_this_project_does_not_control() {
        assert_eq!(canonical_os("macos"), "darwin");
        assert_eq!(canonical_os("osx"), "darwin");
        for win in ["win", "win32", "win64"] {
            assert_eq!(canonical_os(win), "windows");
        }
        assert_eq!(canonical_arch("x86_64"), "amd64");
        assert_eq!(canonical_arch("x86-64"), "amd64");
        assert_eq!(canonical_arch("x64"), "amd64");
        assert_eq!(canonical_arch("aarch64"), "arm64");
    }

    /// Rust names the host one way and the semantic conventions another, and this is the table that
    /// bridges them — so what a Client compiled by rustc reports is a token the Server knows.
    #[test]
    fn what_rust_calls_this_machine_folds_onto_a_canonical_token() {
        assert_eq!(
            canonical_os(std::env::consts::OS),
            canonical_os(canonical_os(std::env::consts::OS)),
            "canonicalising twice is canonicalising once"
        );
        assert_eq!(
            canonical_arch("x86_64"),
            canonical_arch("amd64"),
            "rustc's spelling and the convention's are one machine"
        );
        assert_eq!(canonical_os("macos"), canonical_os("darwin"));
    }

    /// A system the table has never heard of is served under its own name rather than refused: a
    /// fleet may run one, and offering it nothing would be the worse failure.
    #[test]
    fn an_unknown_token_passes_through_unchanged() {
        assert_eq!(canonical_os("plan9"), "plan9");
        assert_eq!(canonical_arch("riscv64"), "riscv64");
        assert_eq!(canonical_os(""), "");
        // Already canonical stays put — the table never folds a token onto another canonical one.
        for os in ["linux", "darwin", "windows"] {
            assert_eq!(canonical_os(os), os);
        }
        for arch in ["amd64", "arm64"] {
            assert_eq!(canonical_arch(arch), arch);
        }
    }

    #[test]
    fn round_trips_through_the_pair_the_protocol_carries() {
        let kv = string_attr(OS_TYPE, "linux");
        assert_eq!(kv.key, OS_TYPE);
        assert_eq!(
            string_value(std::slice::from_ref(&kv), OS_TYPE),
            Some("linux")
        );
    }
}
