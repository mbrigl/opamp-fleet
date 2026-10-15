//! What an Agent reports about itself (ADR-0016): its configured attributes, the host's facts,
//! and a Managed Process's own description folded in.

use opamp::attributes::{self, string_array_attr, string_attr};
use opamp::proto::{AgentDescription, KeyValue};
use opamp::uid::InstanceUid;

use super::{AgentState, Local};

impl AgentState {
    /// The Managed Process reported its own description (through the Supervisor Endpoint); fold
    /// it into ours — identity stays the Supervisor's (goal 16).
    pub fn set_process_description(&mut self, description: AgentDescription) {
        // Merged per attribute, not replaced wholesale: two sources describe the same process —
        // the version probe, which reports `service.version` and nothing else, and the
        // opampextension, which reports everything else — and whichever speaks second must not
        // erase what the first said. On the same key the later report wins, so a swapped binary's
        // probed version replaces the one it succeeded.
        let merged = self
            .local
            .process_description
            .get_or_insert_with(AgentDescription::default);
        for attr in &description.identifying_attributes {
            upsert_attr(&mut merged.identifying_attributes, attr);
        }
        for attr in &description.non_identifying_attributes {
            upsert_attr(&mut merged.non_identifying_attributes, attr);
        }
        self.protocol.force_full();
    }

    /// Attaches the operator-defined attributes this Agent reports (ADR-0016 clause 9).
    #[must_use]
    pub fn with_attributes(
        mut self,
        attributes: std::collections::BTreeMap<String, String>,
    ) -> Self {
        self.local.configured_attributes = attributes.into_iter().collect();
        self
    }

    /// Attaches the deployment's `service.namespace`, reported by every Agent this Client presents.
    /// `None` — the default — reports nothing, which is what the Baseline's "if it is used in the
    /// environment" amounts to for a deployment that does not use one.
    #[must_use]
    pub fn with_namespace(mut self, namespace: Option<String>) -> Self {
        self.local.namespace = namespace;
        self
    }
}

impl Local {
    pub(super) fn describe(&self, uid: &InstanceUid) -> AgentDescription {
        // `service.name` is the Agent *type* — the Baseline's "reverse FQDN that uniquely
        // identifies the Agent type" (ADR-0016). The instance name has its own key below, out of
        // the way of the fold, so a Managed Process reporting its own type cannot destroy it.
        let mut identifying_attributes =
            vec![string_attr(attributes::SERVICE_NAME, &self.service_name)];
        // The Baseline lists `service.namespace` second, among what identifies the Agent — it says
        // *which* deployment this service belongs to, so it belongs beside the name rather than
        // among the tags an operator hangs on it.
        if let Some(namespace) = &self.namespace {
            identifying_attributes.push(string_attr(attributes::SERVICE_NAMESPACE, namespace));
        }
        // `service.version` is the *Agent's* version. The self-Agent is the Client, so its baked
        // version is the truth; a Supervisor-backed Agent stands for its Managed Process, whose
        // version only the process itself can report (folded in below, goal 16) — never invented
        // from the Client's.
        if !self.managed {
            identifying_attributes.push(string_attr(
                attributes::SERVICE_VERSION,
                fleet_core::version::current(),
            ));
        }
        identifying_attributes.push(string_attr("service.instance.id", &uid.to_string()));
        // The rest of what the Baseline asks for "to describe where the Agent runs": `os.*` and
        // `host.*`. Every one of them is best effort, and what the platform cannot answer is left
        // out rather than filled in — an absent attribute says "unknown", where one carrying a
        // placeholder would say something false that a Selector could then match.
        let os = self.host.os();
        let mut non_identifying_attributes = vec![
            // The operator's name for this Agent (ADR-0016). Non-identifying because the Baseline
            // has no key for a human instance name and admits "any user-defined attributes the end
            // user would like to associate with this Agent" here; identity itself stays
            // `service.instance.id`. A Selector can match it, which is how ADR-0026's "pin one
            // host" is expressed for a machine running several Supervisors.
            string_attr(attributes::SERVICE_INSTANCE_NAME, &self.instance_name),
            string_attr(attributes::OS_TYPE, os_type()),
            string_attr(attributes::HOST_ARCH, host_arch()),
        ];
        for (key, value) in [
            ("os.name", os.name.as_deref()),
            ("os.version", os.version.as_deref()),
            ("os.build_id", os.build_id.as_deref()),
            (attributes::OS_DESCRIPTION, os.description.as_deref()),
            ("host.name", self.host.host_name()),
            ("host.id", self.host.host_id()),
            ("host.cpu.model.name", self.host.cpu_model()),
        ] {
            if let Some(value) = value {
                non_identifying_attributes.push(string_attr(key, value));
            }
        }
        // The host's network addresses — the conventions' `host.ip` and `host.mac` (ADR-0016),
        // both arrays and both "excluding loopback interfaces". Read live rather than once, so a
        // DHCP move is reported instead of the address the process happened to start with; a host
        // with nothing to say reports no attribute rather than an empty array.
        let (ips, macs) = self.host.addresses();
        for (key, values) in [("host.ip", ips), ("host.mac", macs)] {
            if !values.is_empty() {
                non_identifying_attributes.push(string_array_attr(key, &values));
            }
        }
        let mut description = AgentDescription {
            identifying_attributes,
            non_identifying_attributes,
        };
        // Operator-defined attributes (ADR-0023) — added only where nothing is reported under the
        // same key, so what the code (and below, the Managed Process) reports always wins.
        for (key, value) in &self.configured_attributes {
            let taken = |list: &[KeyValue]| list.iter().any(|kv| kv.key == *key);
            if !taken(&description.identifying_attributes)
                && !taken(&description.non_identifying_attributes)
            {
                description
                    .non_identifying_attributes
                    .push(string_attr(key, value));
            }
        }
        // Fold in what the Managed Process reported about itself — except the two attributes that
        // are the Supervisor's to state: the Agent the Server sees is the Supervisor, keyed by the
        // Supervisor's uid (goal 16) and called what the operator called it (ADR-0016). A process
        // cannot know either, so a value it reports under those keys is not an improvement. Its
        // `service.name` deliberately *does* win — a Collector's `dist.name` is a better type than
        // anything this file can infer.
        //
        // The exemption is by *key*, in both lists, and not by the list the Supervisor happens to
        // put each one in: the Server resolves a Selector against the identifying attributes first
        // and the non-identifying ones only after (`configs.rs`), so a process reporting either key
        // among the other list's would win every Selector while the fleet row went on showing the
        // Supervisor's value.
        if let Some(reported) = &self.process_description {
            let supervisors_own = |key: &str| {
                key == "service.instance.id" || key == attributes::SERVICE_INSTANCE_NAME
            };
            for attr in &reported.identifying_attributes {
                if !supervisors_own(&attr.key) {
                    upsert_attr(&mut description.identifying_attributes, attr);
                }
            }
            for attr in &reported.non_identifying_attributes {
                if !supervisors_own(&attr.key) {
                    upsert_attr(&mut description.non_identifying_attributes, attr);
                }
            }
        }
        description
    }
}

fn upsert_attr(attrs: &mut Vec<KeyValue>, attr: &KeyValue) {
    match attrs.iter_mut().find(|existing| existing.key == attr.key) {
        Some(existing) => existing.value = attr.value.clone(),
        None => attrs.push(attr.clone()),
    }
}

/// OpenTelemetry semantic-convention value for `os.type` (Rust says "macos", the convention
/// "darwin").
pub(super) fn os_type() -> &'static str {
    fleet_core::platform::canonical_os(std::env::consts::OS)
}

/// OpenTelemetry semantic-convention value for `host.arch` — the convention says `amd64`/`arm64`
/// where Rust's constant says `x86_64`/`aarch64` (ADR-0026).
///
/// The Baseline points at the conventions for these keys, and the Collector's `opampextension`
/// reports `runtime.GOARCH`, which is already this vocabulary. Reporting Rust's spelling instead
/// meant the *same host* changed architecture depending on whether a Collector was running on it:
/// a Managed Process's attributes are folded over the Supervisor's, so `amd64` overwrote `x86_64`
/// and any Selector written against one of them stopped matching.
fn host_arch() -> &'static str {
    fleet_core::platform::canonical_arch(std::env::consts::ARCH)
}
