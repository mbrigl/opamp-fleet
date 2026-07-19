# ADR-0013: An optional health-check endpoint on the Supervisor Host

- **Status:** 🟢 accepted
- **Date:** 2026-07-19
- **Deciders:** Maintainer

## Context

The upstream Go OpAMP Supervisor exposes a `healthcheck.endpoint` that reports whether the supervisor
itself can do its job — specifically that it "is able to persist state to disk" and "generate the agent's
configuration". Our Supervisor Host ([ADR-0009](0009-plugin-hexagonal-supervisor-host.md)) has no such
self-health endpoint: an orchestrator (Docker Compose, Kubernetes, a process manager) can only tell that
the Host *process* is up, not that it can actually persist its instance UID / applied config or generate
a Collector configuration. A Host wedged on a read-only storage directory looks identical to a healthy
one. This is a named parity gap against the Go reference
([ADR-0008](0008-collector-supervisor-go-reference-compat.md)).

The Supervisor crate deliberately has a **lean dependency footprint** (`tokio`, `tokio-tungstenite`,
`serde_yaml`, …); it does **not** depend on an HTTP server framework. Adding one is architecture-relevant,
which is why this small feature gets an ADR.

## Decision

We will expose an **optional HTTP health-check endpoint on the Supervisor Host** that reports whether the
Host can do its job, returning **`200` when healthy and `503` otherwise**.

- **What it checks:** (a) the storage directory is **writable** (write and remove a probe file), and
  (b) each managed agent can **generate its configuration** (the adapter's config-preparation succeeds on
  the running / fallback config). The Host runs many supervisors ([ADR-0009](0009-plugin-hexagonal-supervisor-host.md)),
  so the endpoint reports healthy only when **all** of them pass — the same two conditions the Go
  supervisor checks, aggregated across the fleet the Host manages.
- **Configuration:** a host-level `healthcheck: { endpoint: 127.0.0.1:13133 }` in `supervisors.yaml`;
  **absent → disabled**, matching the Go supervisor's default-off. Bound to loopback by default.
- **No new HTTP framework dependency.** The endpoint is served by a **minimal hand-rolled HTTP/1.1
  responder** over a `tokio` `TcpListener` — one route, `GET`, a status line plus a tiny body. One
  endpoint does not justify pulling `axum`/`hyper` into the Supervisor crate, and hand-rolling keeps the
  footprint the crate has held since [ADR-0005](0005-cargo-workspace-layout.md).
- **Scope: plain HTTP, unauthenticated, loopback.** It is a **liveness/readiness probe** for the local
  orchestrator, not a public surface. If it is ever exposed beyond loopback, TLS/auth for it falls under
  [ADR-0012](0012-tls-and-shared-token-auth.md); until then it stays local and plain, consistent with a
  probe endpoint.

## Alternatives considered

- **Add `axum`/`hyper` to the Supervisor for the endpoint.** Rejected: a whole HTTP framework for a single
  `GET` route is a large dependency for a lean crate; the hand-rolled responder is a few dozen lines and
  has no new dependency. (`axum` is a workspace dependency of the *server* crate, but the Supervisor crate
  does not and need not depend on it.)
- **No endpoint — rely on process liveness.** Rejected: that is exactly the gap. A process-up check cannot
  distinguish a Host that can persist and configure from one wedged on a read-only volume — which is the
  failure the probe exists to catch.
- **Reuse the OpAMP-reported health.** Rejected: that is the *Collector's* health, surfaced to the fleet
  server; this endpoint is about the *Supervisor Host's own* ability to function, a different concern and
  a different consumer (the local orchestrator, not the fleet).
- **A richer JSON health document from the start.** Deferred: `200`/`503` with a tiny body is enough for a
  probe; a per-supervisor JSON breakdown can be added later without changing the contract.

## Sources / Prior art

- The upstream OpAMP Supervisor's `healthcheck` (persist-state + generate-config conditions):
  <https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/README.md>.
- Kubernetes liveness/readiness probe semantics (why `200`/`503` on a loopback HTTP endpoint):
  <https://kubernetes.io/docs/tasks/configure-pod-container/configure-liveness-readiness-startup-probes/>.
- The Host this extends: [ADR-0009](0009-plugin-hexagonal-supervisor-host.md); the parity requirement:
  [ADR-0008](0008-collector-supervisor-go-reference-compat.md); the lean-crate stance:
  [ADR-0005](0005-cargo-workspace-layout.md).

## Consequences

- Positive: an orchestrator gets a real readiness/liveness signal; a Host that cannot persist state or
  generate a config fails its probe instead of masquerading as healthy. Closes a named Go-reference parity
  gap with no new dependency.
- Negative / trade-offs: a second (loopback) listener on the Host and a small hand-rolled HTTP responder
  to maintain; aggregating many supervisors into one `200`/`503` is coarse — which supervisor is unhealthy
  is not (yet) in the response.
- Follow-ups: a richer per-supervisor JSON health body; authenticating / TLS-ing the endpoint via
  [ADR-0012](0012-tls-and-shared-token-auth.md) if it is ever exposed beyond loopback; optionally a
  matching health endpoint on the fleet **server** (trivially, on its existing `:4321` listener) if an
  orchestrator needs to probe it too.
