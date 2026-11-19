# Ticket: An Agent's attributes are read through one lookup

## What to build

The lookup "a key from either attribute set, identifying first" is written out five times in the
Server: `deployments::matches` (`crates/fleet-server/src/deployments.rs` near line 46),
`labels.rs` near line 249, the `reported` closure of `hosts_of` (`fleet/views.rs` near line 37),
`listing.rs` near line 370, and `configs::fits` (`configs.rs` near line 160).
`Platform::reported` (`packages.rs` near line 69) reads the sets the other way round, on purpose.
`opamp::attributes` once took five such helpers into one (3105ca89e), and they grew back.

`service.name` and `service.version` are read three ways besides:
- `listing::names` and `listing::display_values` read them from the identifying set through
  `attr_map`, where a duplicate key takes the last value and an empty value counts.
- `packages::reported_agent_type` and `reported_service_version` read them from the identifying
  set through `string_value`, where the first value wins and an empty value is none.
- `configs::fits` also reads them from the non-identifying set when the identifying set has none.

So a Configuration fits an Agent that the list shows with no type, that the `service_name` filter
misses, and that no Package reaches. CLT-what-an-agent-reports §1 makes `service.name`
identifying. The Hosts view drifted the same way a third time after revision-2026-11-18's ticket
01 had derived the display values once: 36e9af5b3, fixed in 8f61f397b, reworked in eef2c090b.

`opamp::attributes` gets the lookups the Server's paths share: one for a key from both sets, and
one each for the Agent type and the version from the set that ADR gives them. Every path above calls
them, and a test refuses a read of those keys anywhere else in the Server.

## Acceptance criteria

- [ ] Given an Agent that reports `service.name` only among its non-identifying attributes, when
  it is listed, filtered by `service_name`, matched against a Configuration, and offered a
  Package, then all four answer alike, as the human decided for `configs::fits` (see Mode).
- [ ] Given an Agent that reports `service.name` twice or empty, when it is listed and when a
  Package is offered to it, then both read the same value.
- [ ] Given the Server's production code, when a test reads it, then an Agent's reported
  `SERVICE_NAME` and `SERVICE_VERSION` are read from an `AgentDescription` only through the shared
  functions of `opamp::attributes`, and so is a lookup of one key in the identifying set and then
  the non-identifying one. Not reads: a write of the key, such as the OpenTelemetry resource of
  `audit_export.rs`, and a walk over both sets that looks up no key (`listing.rs` near line 361,
  `labels.rs` near lines 109 and 141). `Platform::reported` is the named exception, with its
  reason.
- [ ] Given the change, when the fleet's unit tests and the Server's suites run, then they pass,
  and a changed answer of `configs::fits` has its `CHANGELOG.md` entry.

## Read first

- Touches: `crates/opamp/src/attributes.rs`, `crates/fleet-server/src/configs.rs`,
  `crates/fleet-server/src/deployments.rs`, `crates/fleet-server/src/labels.rs`,
  `crates/fleet-server/src/listing.rs`, `crates/fleet-server/src/fleet/views.rs`,
  `crates/fleet-server/src/packages.rs`
- CLT-what-an-agent-reports §1 and §2; SRV-configurations-compose-one-map (what a Configuration
  fits); SRV-deployments-and-pinned-rollouts (the Selector).
- `docs/CONVENTIONS.md`: "A rule two paths apply is one function".

## Blocked by

- None (can start immediately)

## Mode

with the human: DEV-maintainer-decides-agents-implement §5 keeps every part of the system with the
human. The human also decides whether `configs::fits` keeps its fallback to the non-identifying
set, which today lets a Configuration fit what no other path sees. Taking it out changes what a
deployment sees.
