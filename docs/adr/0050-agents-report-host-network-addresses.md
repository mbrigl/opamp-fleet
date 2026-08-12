# ADR-0050: Agents report the host's network addresses, CPU model, and OS build

- **Status:** 🟢 accepted
- **Date:** 2026-08-11
- **Deciders:** Markus Brigl

## Context

The Client describes where an Agent runs with the Baseline's `os.*` and `host.*` attributes —
`host.name`, `host.id`, `host.arch` and the `os.*` set, every one best effort and absent rather
than blank when the platform cannot answer. The host's network addresses are not among them, and
an operator looking at the fleet cannot see which addresses a host holds. The Server does see a
peer address on every connection, but it is the wrong fact to show: behind NAT it is the
translator's, and behind a Gateway (ADR-0037) it is the Gateway's — a hop that invents nothing
must not become the source of an attribute about someone else's host.

The OpenTelemetry semantic conventions already define both keys: `host.ip` and `host.mac`, each a
**string array**, each "excluding loopback interfaces", IPv6 in RFC 5952 form and MACs "in IEEE RA
hexadecimal form: as hyphen-separated octets in uppercase". The fleet view renders every reported
non-identifying attribute generically, so anything reported under these keys is displayed, and
searched, without a UI change.

## Decision

We will report `host.ip` and `host.mac` from every Agent this Client presents, as the conventions
define them.

- **Enumeration** comes from `sysinfo`, which the Client already carries for its own telemetry
  (ADR-0036) — the `network` feature is enabled on the existing dependency; no new crate.
- **Loopback interfaces are excluded whole** — their MACs too; an unspecified MAC (`00-…-00`) is
  no answer. Everything else is best effort: a host with nothing to report omits the key rather
  than reporting an empty array.
- **Formats are the conventions'**: IPv4 dotted-quad and IPv6 RFC 5952 (both what `IpAddr`'s
  `Display` writes), MACs hyphen-separated uppercase. Both lists are deduplicated and sorted so an
  unchanged host never re-reports a description over enumeration order.
- **Read live on each description**, not once at start, so a DHCP move is reported.
- **Arrays stay arrays on the wire** (`ArrayValue` of strings — the typed original); the Server's
  read-only view joins string arrays with a comma. Selectors keep matching string values only —
  an array-valued attribute is displayed and searched, not matched.

Two more conventions ride along, both plain strings and both from sources already paid for:

- **`host.cpu.model.name`** — the processor's model designation, from the same `sysinfo`; read
  once, since hardware does not change under a running process.
- **`os.build_id`** — the build behind `os.version`, read where the platform's existing `os.*`
  answer already carries it: os-release's `BUILD_ID` (absent on distributions that stamp none),
  `sw_vers`' BuildVersion, and on Windows the build components of the version line
  (`10.0.26100.2033` → `26100.2033`, matching the conventions' Windows example `22621`).

## Alternatives considered

- **A dedicated interface-enumeration crate** (`if-addrs`, `mac_address`, `network-interface`) —
  a new dependency for what an existing one's feature flag already provides.
- **Joined strings on the wire** — renders everywhere without a Server change, but bakes a display
  choice into the protocol and departs from the conventions' declared type; the view is the place
  to join.
- **The Server records the connection's peer address** — no Client change, but it reports the NAT
  or the Gateway rather than the host, precisely the invention ADR-0037 forbids the hop.

## Sources / Prior art

- OpenTelemetry semantic conventions, host registry (`host.ip`, `host.mac`, `host.cpu.model.name`,
  formats and loopback exclusion):
  <https://opentelemetry.io/docs/specs/semconv/registry/attributes/host/> (retrieved 2026-08-11).
- OpenTelemetry semantic conventions, os registry (`os.build_id`, examples including Windows'
  `22621`): <https://opentelemetry.io/docs/specs/semconv/registry/attributes/os/> (retrieved
  2026-08-11).
- `sysinfo` 0.37 `Networks` API (`mac_address()`, `ip_networks()`), gated behind its `network`
  feature.

## Consequences

- Positive: the fleet view shows each host's addresses — generically in the attribute chips, and
  at a glance in a dedicated Network column (first address of each kind, the rest in the tooltip);
  the attributes are searchable; the wire stays convention-shaped for any OTel-aware consumer.
- Negative / trade-offs: network addresses now leave the host and sit in the fleet view — visible
  to whoever can read the API (the fleet is an operator tool, that is its purpose, but it is data
  that was not transmitted before). Virtual adapters (bridges, tunnels) appear alongside physical
  ones — best effort reports what the platform says, it does not curate. Selectors cannot match
  array-valued attributes.
- Follow-ups: the cloud-shaped conventions (`host.image.*`, `host.type`) remain unreported — they
  come from provider metadata services, a separate decision if the fleet ever needs them.
