# ADR-0015: An Agent reports its type, its operator's name, and its host as the conventions define them

- **Status:** 🟢 accepted
- **Date:** 2026-08-11
- **Deciders:** Markus Brigl
- **Applies to:** AgentDescription building in crates/fleet-agent/src/supervisor/agent.rs, the Agent type resolution in crates/fleet-agent/src/supervisor/mod.rs, [attributes] and service_namespace in supervisor.toml, the fleet view's name and network columns

## Context

Everything a Selector matches, every artifact a Package fit chooses, and every row of the fleet view
comes from the `AgentDescription` an Agent reports. What goes into it is therefore a contract.

**The Baseline reserves `service.name` for the Agent *type*.** It "should be set to a reverse FQDN
that uniquely identifies the Agent type, e.g. `io.opentelemetry.collector`", and names
`service.instance.id` separately as what identifies the Agent. A `[[supervisor]]` block's `name`
is the operator's name for one instance — a directory, a uniqueness key — and putting it into
`service.name` would give one key two meanings. The collision is not theoretical: a Collector
carrying the `opampextension` reports its own description, folded over the Supervisor's, and its
`service.name` is the `dist.name` it was built with (`otelcol-contrib`), identical on every host.
An operator's name kept in that key is erased the moment the extension connects, and three
Collectors become three rows of the same name.

The type is the more useful of the two for targeting — "reach the Collectors and nothing else" —
and where no extension reports it (the core `otelcol` distribution, every Foreign Agent) something
else has to. The Baseline has no attribute for a human-readable instance name; it admits "any
user-defined attributes the end user would like to associate with this Agent" among the
non-identifying ones. Bindplane, the closest shipping server, derives its Agent Type from
`dist.name` and keeps the human name in a separate field.

**Where the Agent runs** is described with the semantic conventions' `os.*` and `host.*` keys. An
operator also wants to see which addresses a host holds. The connection's peer address is the
wrong fact: behind NAT it is the translator's, behind a Gateway the Gateway's, and a hop must not
become the source of an attribute about someone else's host. The conventions define `host.ip` and
`host.mac` as string arrays, "excluding loopback interfaces", IPv6 in RFC 5952 form and MACs "in
IEEE RA hexadecimal form: as hyphen-separated octets in uppercase".

## Decision

We will report `service.name` as the Agent type and a separate `service.instance.name` as the
operator's name for the instance, describe the host with the conventions' `os.*` and `host.*`
attributes in their declared shapes, and let a Managed Process improve everything except who the
Agent is.

1. **`service.name` is the Agent type**, identifying, resolved in this order, first hit wins:

   | Source | When |
   |---|---|
   | What the Managed Process reports | the fold of clause 8 — `dist.name` for a Collector with the extension |
   | The kind's own type | a wrapped kind states it (`icinga2`, `telegraf`, `glpi-agent`); a block of such a kind carrying `service_name` fails at startup naming the derived value ([ADR-0017](0017-supervisor-mode-kinds-directories-and-what-the-server-may-change.md)) |
   | The block key `service_name` | `collector` and `command`; must not be empty, and is not bound by the instance-name grammar, so a reverse FQDN is accepted |
   | The program's file name | the fallback: `otelcol`, `promtail` |

   The fallback is read from the configuration, never parsed out of a program's output. The
   Baseline's reverse-FQDN shape is a recommendation and is not enforced. The Client's own Agent
   reports the constant `supervisor` ([ADR-0021](0021-the-client-supervisor-installed-service-releases-and-installers.md)),
   never its configured name. A Selector or a Configuration's type
   ([ADR-0025](0025-configurations-and-the-rest-api.md) clause 4) on `service.name` aims at a type.

2. **`service.instance.name` is the operator's name for the Agent**, non-identifying: the
   `[[supervisor]]` block's `name`, or the top-level `name` for the Client's own Agent. It is
   always reported. The key is this project's — the conventions have none for a human instance
   name — chosen to read as the partner of `service.instance.id`.

3. **The block `name` keeps its local meaning.** It is the Supervisor's directory name and the
   uniqueness key across blocks, bound by the instance-name grammar of
   [ADR-0021](0021-the-client-supervisor-installed-service-releases-and-installers.md) clause 2: lowercase, digits, `-`, no dots. A type
   may be a reverse FQDN; an instance name is a path component on three operating systems. That
   is why the two cannot share a key.

4. **The identifying attributes** are `service.name`; `service.namespace`, only when the top-level
   `service_namespace` key in `supervisor.toml` is set, since only an operator knows the
   deployment; `service.version` — for the Client's own Agent its baked version
   ([ADR-0011](0011-versions-resolved-in-the-internal-crate.md)), for a Supervisor-backed Agent only what the Managed Process
   reports, never invented from the Client's; and `service.instance.id`, the Instance UID.

5. **Where the Agent runs is reported best effort, and absent rather than blank.** Beside
   `service.instance.name`, the non-identifying attributes are `os.type` and `host.arch` in the
   conventions' vocabulary ([ADR-0028](0028-packages-signed-deployments-offered-downloads-and-verified-delivery.md)), `os.name`, `os.version`,
   `os.build_id`, `os.description`, `host.name`, `host.id`, `host.cpu.model.name`, `host.ip`, and
   `host.mac`. What the platform cannot answer is left out: a placeholder would be something false
   a Selector could match. Nothing is taken from the connection.

6. **`host.ip` and `host.mac` follow the conventions.**
   - Enumerated with `sysinfo`'s `network` feature, the crate the Client already carries for its
     own telemetry — no new dependency.
   - An interface whose every address is loopback is excluded whole, its MAC too; an all-zero MAC
     is no answer.
   - IPv4 dotted-quad, IPv6 RFC 5952 (what `IpAddr`'s `Display` writes), MACs hyphen-separated
     uppercase. Both lists are deduplicated and sorted, so enumeration order never re-reports an
     unchanged host.
   - Read live on every description, so a DHCP move is reported. An empty list omits the key.
   - On the wire an `ArrayValue` of strings. The Server's view joins a string array with `, `;
     Selectors match string values only, so an array is displayed and searched, never matched.

7. **`host.cpu.model.name` and `os.build_id` come from sources already paid for.** The CPU model
   from the same `sysinfo`, read once per process. The build behind `os.version` from what the
   platform's `os.*` answer already carries: os-release's `BUILD_ID` (absent where a distribution
   stamps none), `sw_vers`' BuildVersion, and on Windows the build components of the version line
   (`10.0.26100.2033` → `26100.2033`).

8. **A Managed Process's own description is folded in, except who the Agent is.** What the
   process reports — through the Supervisor Endpoint, or the version probe — is merged per
   attribute, a later report winning on the same key, and laid over the Supervisor's. Its
   `service.name` wins: `dist.name` is a better type than anything a file can infer.
   `service.instance.id` and `service.instance.name` are the Supervisor's and are exempt, **by key
   and in both lists**: a process cannot know the Supervisor's identity or what the operator
   called it, and a key reported in the other list would otherwise win every Selector while the
   fleet row showed the Supervisor's value.

9. **Operator attributes describe the host.** The Client-wide `[attributes]` table (string to
   string; any other value type is refused) is reported as non-identifying attributes of every
   Agent this Client presents, each only where nothing is reported under the same key — what the
   code or the Managed Process reports always wins, so an attribute can never restate the type or
   the instance name. Tagging one Agent among several is a Server label's job
   ([ADR-0026](0026-the-fleet-record.md) clause 16): a `[[supervisor]]` block carrying an
   `attributes` table fails at startup with a message naming the Server label. The Client's own
   Agent also reports the kinds it was compiled with
   ([ADR-0017](0017-supervisor-mode-kinds-directories-and-what-the-server-may-change.md)).

10. **The fleet view shows the instance name first and the type beneath it.** `AgentView` carries
    `service_name` (the type) and `service_instance_name`; the bundled UI's name column falls back
    from instance name to type to Instance UID, so a row is never blank and never collapses onto
    its neighbours. A Network column shows the first address of each kind with the rest in the
    tooltip; every attribute is shown as a chip and searchable.

**Out of scope:** the cloud-shaped conventions (`host.image.*`, `host.type`), which come from
provider metadata services; reporting kinds through `AvailableComponents`; bootstrapping a type
by starting a Collector once with a generated extension-only configuration.

## Alternatives considered

- **Keep `service.name` as the instance name and add `service.type`.** A Collector's extension
  reports `service.name` and knows nothing of `service.type`, so it would keep overwriting the
  instance name with its type — the collapse this decision prevents.
- **No human instance name at all.** The most conformant reading, but a UID is not a name a person
  can use, and several Supervisors share one `host.name`.
- **`host.name` as the instance name.** A property of the machine, while Supervisor Mode runs *n*
  Agents on one machine ([ADR-0014](0014-client-modes-and-a-gateway-that-passes-packages-only-to-the-hosts-they-were-offered-to.md)).
- **The instance name outside `AgentDescription`**, as Bindplane does. Needs a channel this
  project does not have, and makes the name invisible to Selectors, which pin one Agent by it.
- **Derive the type from `<program> --version` output.** A name has no grammar the way a SemVer
  does (`Fluent Bit v3.1.0`, `promtail, version 3.0.0 (branch: main)`); the configuration
  fallback gets the same value without guessing.
- **Bootstrap the type by starting the Collector with an extension-only configuration**, as the
  upstream Supervisor does. Adds a process start and works only for distributions that already
  report their type through the fold.
- **A dedicated interface-enumeration crate** (`if-addrs`, `mac_address`). A new dependency for
  what an existing one's feature flag provides.
- **Joined strings on the wire.** Bakes a display choice into the protocol and departs from the
  conventions' declared type; the view is the place to join.
- **The Server records the connection's peer address.** Reports the NAT or the Gateway, not the
  host.

## Sources / Prior art

- [`opamp.proto`](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto), `AgentDescription` —
  `service.name` as the type, `service.instance.id` as the identity, and the non-identifying
  clause admitting user-defined attributes.
- [Bindplane — Bring Your Own Collector](https://docs.bindplane.com/feature-guides/deployment-and-management/bring-your-own-collector)
  — Agent Type from `dist.name`, the human name kept apart.
- [`opampextension` `opamp_agent.go`](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/extension/opampextension/opamp_agent.go)
  — the three identifying attributes a Collector reports, which the fold coexists with.
- [OpAMP Supervisor `config.go`](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/supervisor/config/config.go)
  — operator-stated identifying attributes, the need the `service_name` key answers.
- [PR #38809 — Control Collectors with only the OpAMP extension](https://github.com/open-telemetry/opentelemetry-collector-contrib/pull/38809)
  — the bootstrap mechanism weighed and declined.
- [Semantic conventions — `service`](https://github.com/open-telemetry/semantic-conventions/blob/main/docs/registry/attributes/service.md)
  — no attribute for a human instance name.
- [Semantic conventions — `host`](https://opentelemetry.io/docs/specs/semconv/registry/attributes/host/)
  and [`os`](https://opentelemetry.io/docs/specs/semconv/registry/attributes/os/) — `host.ip`,
  `host.mac`, `host.cpu.model.name`, `os.build_id`, their formats and loopback exclusion.
- `sysinfo` 0.37 `Networks` API (`mac_address()`, `ip_networks()`), behind its `network` feature.

## Consequences

- Positive: an operator's name for an Agent survives its Managed Process reporting for itself;
  the fleet can be aimed by what an Agent *is*, with no per-host attribute to maintain.
- Positive: the fleet view shows each host's addresses, the attributes are searchable, and the wire
  stays convention-shaped for any OpenTelemetry-aware consumer.
- Negative / trade-offs: `service.instance.name` is a key this project invents; a future
  convention may contradict it, and renaming it would break Selectors matching it.
- Negative / trade-offs: an Agent's reported type changes when a Collector gains the extension —
  from the configured or file-name value to `dist.name` — and a Selector on the old value stops
  matching at that moment.
- Negative / trade-offs: network addresses leave the host and are visible to whoever can read the
  API; virtual adapters appear beside physical ones, because best effort reports what the platform
  says and does not curate.

## Enforcement

- [`agent.rs`](../../crates/fleet-agent/src/supervisor/agent.rs) unit tests:
  `a_process_reporting_its_type_does_not_take_the_operators_name_with_it`,
  `the_supervisors_own_attributes_survive_whichever_list_a_process_reports_them_in`,
  `the_reported_type_is_the_processs_own_word_where_it_gives_one`,
  `the_clients_own_agent_reports_its_type_and_its_configured_name_separately`,
  `the_clients_own_agent_type_is_the_one_name_this_program_has`,
  `configured_attributes_are_reported_but_never_shadow_reported_ones`,
  `configured_attributes_cannot_restate_the_type_or_the_instance_name`,
  `the_service_namespace_is_absent_until_configured_and_then_identifies`,
  `host_addresses_follow_the_conventions`, `os_release_parses_the_fields_the_description_reports`,
  `the_cpu_model_is_reported`, `the_agent_reports_the_host_name_a_selector_would_pin_it_by`,
  `the_os_is_reported_as_a_name_and_a_version_not_only_as_prose`.
- [`config.rs`](../../crates/fleet-agent/src/config.rs) unit tests:
  `a_block_may_state_its_agent_type_and_a_reverse_fqdn_is_accepted`,
  `an_empty_agent_type_is_refused_rather_than_treated_as_absent`,
  `attributes_describe_the_host_and_a_block_no_longer_tags_one_agent`,
  `non_string_attributes_are_rejected`; [`supervisor/mod.rs`](../../crates/fleet-agent/src/supervisor/mod.rs)
  `a_wrapped_block_that_restates_a_derived_value_is_refused`.
- Server: [`fleet.rs`](../../crates/fleet-server/src/fleet.rs) unit test
  `the_view_joins_a_string_array_attribute`.
