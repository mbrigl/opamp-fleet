# ADR-0014: Server-driven OpAMP connection settings — credential rotation, offered heartbeat, movable endpoint

- **Status:** 🟡 proposed
- **Date:** 2026-07-24
- **Deciders:** Markus Brigl

## Context

ADR-0013 gave both ends static credentials: the Server accepts a configured set, the Client sends
one from `client.toml`. Rotating them today means editing every machine's config file by hand —
exactly the fleet-wide chore this project exists to remove. The Baseline's answer is
**connection settings management**: `ConnectionSettingsOffers.opamp` carries an
`OpAMPConnectionSettings` with a destination endpoint, headers (*"typically used to set access
tokens or other authorization headers"*), a heartbeat interval, and TLS material; the Client
verifies an offer **by actually connecting** — *"The Agent MUST verify the offered connection
settings by actually connecting before accepting the setting to ensure it does not loose access
to the OpAMP Server due to invalid settings"* — then persists and switches, reverting on failure.
`ConnectionSettingsStatus` (Development) closes the loop: the Agent reports the last offer hash
plus `APPLYING`/`APPLIED`/`FAILED`, and *"if the hashes are different the Server MUST include the
connection_settings field in the response"*.

The heartbeat interval belongs to the same message: for an Agent with `ReportsHeartbeat` the
Server *"MAY respond by setting an interval in the heartbeat_interval_seconds field"*, the Agent
MUST then use it — and an HTTP Client *"MUST use the value as polling interval"*. The heartbeat
row in [`CONFORMANCE.md`](../CONFORMANCE.md) has awaited exactly this since ADR-style work on
`ReportsHeartbeat`.

Forces: the REST API carries no operator authentication yet (deferred in ADR-0013), so it is no
place to hand in credentials; server-infrastructure secrets already live in `server.toml` next to
the TLS key (ADR-0007, ADR-0008). The Client already persists protocol state losslessly in its
`state_dir` (`opamp::uid`, `remote-config.pb`). Capability negotiation obliges the Server to
offer only to Agents declaring `AcceptsOpAMPConnectionSettings`. Three of the pieces —
`ConnectionSettingsStatus`, `ReportsConnectionSettingsStatus`, `heartbeat_interval_seconds` —
are **Development** maturity: implementing them is the same deliberate risk acceptance already
taken for `ReportsHeartbeat` and `ReportsAvailableComponents`.

## Decision

We will implement Server-driven OpAMP connection settings, scoped to what the fleet can use today:

- **Server** (`OffersConnectionSettings`). A new optional `[connection_offer]` section in
  `server.toml` names any of: the **canonical client credential** — `bearer_token`, or `username`
  and `password` (exactly one scheme, as in the Client's `[auth]`) — an optional
  `heartbeat_interval_secs`, and an optional `endpoint` (`ws(s)://` or `http(s)://`, e.g. for a
  Server move). At least one must be present — an empty section fails loudly (ADR-0008); a
  credential-less offer legitimately retunes only heartbeat or endpoint. The section compiles into one `OpAMPConnectionSettings` whose SHA-256 hash gates
  delivery: the Server compares each Agent's reported `last_connection_settings_hash` and
  includes the offer on mismatch, only for Agents declaring `AcceptsOpAMPConnectionSettings`.
  Unless `endpoint` points elsewhere, the offered credential MUST be in the `[auth]` accepted
  set — a rotation that would lock out the fleet fails loudly at startup (ADR-0008).
- **Client** (`AcceptsOpAMPConnectionSettings`, `ReportsConnectionSettingsStatus`). On an offer:
  report `APPLYING`, **verify by actually connecting** with the offered endpoint/headers (a real
  handshake on the WebSocket transport, a real exchange on plain HTTP), then persist the settings
  losslessly in `state_dir` (`connection-settings.pb`, candidate → valid, the `remote-config.pb`
  pattern), reconnect with them, and report `APPLIED` with the offer hash. Verification failure
  keeps the old settings and reports `FAILED` with the error. Persisted settings override
  `client.toml`'s `endpoint`, `[auth]`, and `heartbeat_interval_secs` at startup — the operator
  reverts by deleting the file, and `client.toml` stays what the operator wrote (ADR-0008's
  loud-typo contract untouched).
- **Heartbeat.** An offered non-zero `heartbeat_interval_seconds` replaces the configured
  interval — as heartbeat period on WebSocket, as polling interval on plain HTTP, per the
  Baseline's MUSTs.
- **One connection, n Agents.** Offers are per Agent on the wire (ADR-0003), but the settings
  are connection-scoped: the Client applies an offer once per hash and reports status for every
  Agent it carries — the Server offers to each, the switch happens once.

**Out of scope**, staying `planned` in the matrix: client-certificate/TLS/proxy rotation and the
CSR flow (`AcceptsConnectionSettingsRequest`) — no client-side PKI exists (ADR-0013 kept mTLS
future work); `AcceptsOtherConnectionSettings` and the telemetry connection settings — nothing
consumes them before own-telemetry lands.

The operational story this buys: add the new token to `[auth].bearer_tokens` (old and new both
accepted), point `[connection_offer]` at the new token, restart the Server; the fleet migrates
itself, verified connection by verified connection; then drop the old token from `[auth]`.

## Alternatives considered

- **Handing in rotation credentials through the REST API** — the natural "any UI drives the
  fleet" shape, but the REST API is unauthenticated until operator auth lands; pushing fleet
  credentials through an open endpoint is indefensible. Server-infrastructure secrets stay in
  `server.toml` beside the TLS key. Revisit when operator authentication exists.
- **Offering per-Agent credentials from a credentials store** — the specification's fullest
  flow, but per-Agent identity management brushes against the deferred authorization/tenancy
  non-goal, and ADR-0013 deliberately chose fleet-membership credentials. One canonical
  credential rotates one fleet.
- **Gating offers on the presented `Authorization` header instead of the status hash** — looks
  elegant (no new capability needed) but breaks on the shared WebSocket connection (one header,
  n Agents) and substitutes invented semantics for the Baseline's own hash-comparison MUST.
- **Skipping `ReportsConnectionSettingsStatus` (Development) and offering blind** — without the
  reported hash the Server cannot stop re-offering, and rejection becomes invisible — the very
  failure mode goals 3 and 4 exclude for configuration. Same risk acceptance as `ReportsHeartbeat`.
- **Waiting for config-file hot reload instead of a Server restart to start a rotation** — hot
  reload is a feature this Server does not have; a restart is the established way `server.toml`
  changes take effect and adds no new machinery.

## Sources / Prior art

- [OpAMP specification — Connection Settings Management](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md#connection-settings-management)
  — the server-initiated flow (offer → verify by connecting → persist candidate → reconnect →
  report status → server retires old credentials), TOFU, and the heartbeat obligations quoted
  above; `ConnectionSettingsOffers`, `OpAMPConnectionSettings`, `ConnectionSettingsStatus` in the
  pinned Baseline proto (`crates/opamp/proto/v0.18.0/opamp.proto`).
- [`opamp-go` client callbacks](https://pkg.go.dev/github.com/open-telemetry/opamp-go/client/types)
  — `OnOpampConnectionSettings` hands the offer to the Agent, which accepts after its own
  verification; the library then reconnects with the new settings.
- [OpAMP Supervisor specification](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/specification/README.md)
  — the Collector's Supervisor persists server-offered connection settings across restarts, the
  same persistence shape chosen here.
- ADR-0013 — the static credential base this rotates; ADR-0008 — TOML, loud validation;
  ADR-0003 — n Agents over one connection, which forces the once-per-hash application.

## Consequences

- Positive: goal 17's rotation half lands — credentials rotate fleet-wide without touching a
  single `client.toml`; the Server gains a sanctioned way to retune every Agent's heartbeat and
  polling cadence and to move the fleet to a new endpoint; three matrix rows
  (`OffersConnectionSettings`, `AcceptsOpAMPConnectionSettings`,
  `ReportsConnectionSettingsStatus`) flip to implemented and the heartbeat row's open note
  closes.
- Negative / trade-offs: two more Development-maturity surfaces may shift upstream (accepted, as
  before); persisted settings overriding `client.toml` adds a state file an operator must know
  about (documented, deletable); the canonical credential is fleet-wide, not per Agent — rotation
  is all-or-nothing by design.
- Follow-ups: client-certificate rotation and the CSR flow once mTLS arrives; telemetry and
  other connection settings once own-telemetry exists; moving rotation control into the REST API
  once operator authentication lands.
