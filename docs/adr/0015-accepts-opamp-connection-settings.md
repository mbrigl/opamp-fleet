# ADR-0015: The Supervisor accepts Server-offered OpAMP connection settings (re-pointing), with revert on failure

- **Status:** 🟢 accepted
- **Date:** 2026-07-19
- **Deciders:** Maintainer

## Context

OpAMP lets the Server offer the Agent **new settings for its own OpAMP connection** —
`ServerToAgent.connection_settings.opamp` (`OpAMPConnectionSettings`) carries a `destination_endpoint`,
`headers`, `tls`, and a `certificate`. An Agent that declares **`AcceptsOpAMPConnectionSettings`** applies
the offer: it reconnects to the offered endpoint with the offered credentials. This is how a fleet
migrates agents to a new Server, or rotates the OpAMP endpoint's token / TLS.

Our Supervisor reads only `heartbeat_interval_seconds` out of `connection_settings.opamp`
([ADR-0010](0010-collector-supervisor-own-telemetry.md), [ADR-0011](0011-server-agent-control-beyond-config.md));
it ignores `destination_endpoint`, `headers`, and `tls`, and does not declare
`AcceptsOpAMPConnectionSettings`. So the fleet cannot redirect a managed Supervisor to a different OpAMP
endpoint, nor rotate its connection token/TLS from the Server. [ADR-0012](0012-tls-and-shared-token-auth.md)
just built the token/TLS machinery this needs.

Declaring the capability is a wire-level promise and requires a **reconnection state machine**, so this is
architecture-relevant.

## Decision

We will declare **`AcceptsOpAMPConnectionSettings`** and honour an `OpAMPConnectionSettings` offer by
**re-pointing the OpAMP connection**, reverting to the previous settings if the new connection fails.

- **Apply an offer.** On a `connection_settings.opamp` offer that changes the endpoint, headers, or TLS,
  the Supervisor stores the new settings, closes the current session, and reconnects to the offered
  `destination_endpoint` with the offered `headers` (bearer token) and `tls` — reusing the connect path
  from [ADR-0012](0012-tls-and-shared-token-auth.md). `heartbeat_interval_seconds` keeps its existing
  meaning ([ADR-0011](0011-server-agent-control-beyond-config.md)).
- **Revert on failure.** If the new connection cannot be established (or does not complete the OpAMP
  handshake) within a bounded time, the Supervisor **reverts to the previous connection settings** and
  reconnects to them, so a bad offer cannot strand the agent off the fleet — the same
  fail-safe stance as `automatic_config_rollback` ([ADR-0008](0008-collector-supervisor-go-reference-compat.md)).
- **Persist the accepted settings.** Once a new connection succeeds, the settings are persisted in the
  storage dir so a Supervisor restart resumes on the new endpoint, not the bootstrap one.
- **Server side (minimal).** The Server may *offer* new OpAMP connection settings from configuration (a
  flag, like the ADR-0011 offers) — primarily to rotate the token or TLS. Re-pointing to a *different*
  Server is an agent-side capability tested against any conforming Server; our Server offering a foreign
  endpoint is out of scope (a migration tool, not the control loop).
- **The `certificate` field is deferred to [ADR-0016](0016-mtls-client-certificate-issuance.md)** (mTLS /
  client-certificate issuance), which owns cert handling; this ADR covers endpoint/headers/TLS-CA
  re-pointing.

## Alternatives considered

- **Apply the offer without a revert.** Rejected: a typo'd endpoint or a wrong token would drop the agent
  off the fleet with no way for the Server to correct it (the agent is now unreachable). Revert-on-failure
  keeps a bad offer recoverable.
- **Implement only the Server-side offer, not the agent-side accept.** Backwards: the named capability is
  agent-side; the agent accepting offers is the deliverable, the Server making them is optional.
- **Ignore it as a migration-only feature.** Rejected: token/TLS **rotation** on the existing endpoint is
  a routine security operation, not just migration, and it is the same mechanism.

## Sources / Prior art

- OpAMP specification — `OpAMPConnectionSettings` and `AcceptsOpAMPConnectionSettings` (connection
  settings management, revert on failed connection):
  <https://github.com/open-telemetry/opamp-spec/blob/main/specification.md>.
- The connect path and credentials this reuses: [ADR-0012](0012-tls-and-shared-token-auth.md); the
  connection-settings offer channel: [ADR-0011](0011-server-agent-control-beyond-config.md); the
  fail-safe stance: [ADR-0008](0008-collector-supervisor-go-reference-compat.md).

## Consequences

- Positive: the fleet can rotate the OpAMP endpoint's token/TLS and migrate agents to a new Server; a bad
  offer is self-correcting via revert.
- Negative / trade-offs: a reconnection state machine with a revert path and persisted connection state —
  more moving parts in the client loop, and more to verify against the oracle. Persisting connection
  credentials (a token) to the storage dir widens what the storage dir holds (a secret), which the
  storage dir's permissions must protect.
- Follow-ups: [ADR-0016](0016-mtls-client-certificate-issuance.md) for the `certificate` field;
  a Server-side migration/rotation UI if operators need to drive it from the fleet console.
