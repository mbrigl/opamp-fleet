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

use crate::proto::{any_value, AnyValue, KeyValue};

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
