# ADR-0053: The Supervisor Endpoint admits only the process its Supervisor started, by a token handed to that process

- **Status:** 🟡 proposed
- **Date:** 2026-10-04
- **Deciders:** Markus Brigl
- **Applies to:** crates/fleet-agent/src/supervisor/endpoint.rs, the token threaded through `SupervisorContext` and `Runner` in crates/fleet-agent/src/supervisor/, the environment every Managed Process is started with, and the `opampextension` configuration the documentation shows

## Context

Every Supervisor serves a Supervisor Endpoint on `127.0.0.1`
([ADR-0040](0040-client-modes-and-a-gateway-that-admits-over-mutual-tls.md) clause 2,
[ADR-0015](0015-supervisor-mode-and-its-kinds.md) clause 5), where a Collector's `opampextension`
reports its description, health and effective configuration. The endpoint authenticates nothing:
any process on the host can connect and report in the Collector's name, and the fleet's view of that
Agent becomes forgeable from the inside. On a single-purpose host that is a small risk; on a shared
or multi-user host it is not. [`HARDENING.md`](../HARDENING.md) asked for a decision either way,
and the specification puts security before convenience.

The Supervisor starts the process it supervises, so it can hand that process a secret no other
local process has: the environment of a child is readable by the child and by root, not by other
accounts. The `opampextension` sends configured headers on its WebSocket, and a Collector
configuration reads an environment variable with `${env:…}`.

## Decision

We will have each Supervisor make a fresh random token at every start, hand it to its Managed
Process as `OPAMP_SUPERVISOR_TOKEN`, and refuse any connection to its Supervisor Endpoint that does
not present it as `Authorization: Bearer <token>`.

1. **A token per start.** 32 bytes from the system's secure random source, hex, made when the
   Supervisor starts and kept in memory only. A restart of the Supervisor makes a new one.
2. **Handed to the process, last.** The Runner sets `OPAMP_SUPERVISOR_TOKEN` in the environment of
   every Managed Process it starts, after the block's own `env`, so no block can replace it.
3. **Asked of every connection.** The endpoint compares the `Authorization` value of the upgrade
   request with `Bearer <token>` in constant time and answers `401` before the upgrade when it does
   not match. A refusal is logged at `warn`.
4. **The Collector presents it from its configuration.** A Collector configuration that reports
   through the endpoint names the header:
   `headers: { Authorization: "Bearer ${env:OPAMP_SUPERVISOR_TOKEN}" }`. One without it loses its own
   reports, not its supervision.

**Out of scope:** authenticating the Supervisor to the Managed Process; a token on disk; the
Gateway's downstream endpoint, which admits over mutual TLS
([ADR-0040](0040-client-modes-and-a-gateway-that-admits-over-mutual-tls.md)).

## Alternatives considered

- **A written statement that single-purpose hosts are assumed** — leaves shared hosts forgeable,
  where the specification asks for the secure default.
- **A Unix socket with file permissions** — the `opampextension` speaks WebSocket over TCP, and
  Windows has no equivalent with the same reach.
- **Mutual TLS on the loopback** — a CA and certificates per Supervisor for a socket that never
  leaves the host; the token gives the same proof for a fraction of the setup.
- **Checking the peer's process ID** — not portable, and a peer's PID says little once it can be
  reused.

## Sources / Prior art

- The `opampextension` configuration, `server.ws.headers`
  ([opentelemetry-collector-contrib](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/extension/opampextension)).
- [`HARDENING.md`](../HARDENING.md), *The local endpoint*.

## Consequences

- Positive: another local account can no longer report in a Managed Process's name.
- Negative / trade-offs: every Collector configuration that reports through the endpoint must name
  the header; one that does not stops reporting its own state. A process running as root, or as the
  Client's own account, can still read the token from the child's environment.
- Follow-ups: none.

## Enforcement

- [`crates/fleet-agent/src/supervisor/endpoint.rs`](../../crates/fleet-agent/src/supervisor/endpoint.rs)
  test: `the_endpoint_admits_only_the_token_it_handed_out` (clauses 1, 3).
- [`crates/fleet-agent/tests/supervisor_process.rs`](../../crates/fleet-agent/tests/supervisor_process.rs)
  test: `the_managed_process_is_handed_the_endpoint_token` (clause 2).
