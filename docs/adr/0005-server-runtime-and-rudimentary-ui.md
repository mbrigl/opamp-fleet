# ADR-0005: Server on axum with in-memory fleet state and a rudimentary UI

- **Status:** 🟢 accepted
- **Date:** 2026-07-20
- **Deciders:** Maintainer

## Context

The specification makes the Server **API-first** — its public contract is an OpenAPI-described REST API —
but also says it "carries only a rudimentary user interface of its own" so any external portal can
integrate it (Goal #5, Non-Goal "Shipping a production UI"). The first version's job is narrower: close
the control loop for one Agent (Goal #1) and let an operator **see** the fleet's state (Goal #2). That
requires a Server process that (a) serves the OpAMP HTTP endpoint from ADR-0004, (b) holds fleet state,
and (c) exposes that state to a minimal UI. Choosing the Server's web framework and how it holds state
are framework/persistence decisions that constrain future work, so per AGENTS.md §3 they need an ADR.

Forces: the transport is plain HTTP over `tokio` (ADR-0004, ADR-0003); the full OpenAPI REST API and any
durable persistence are larger goals not needed to close the loop, and the specification says "close the
loop before widening it"; the UI must be genuinely rudimentary and must not become a second product.

## Decision

We will build the Server on **`axum`** (with `tokio` and `tower-http` for static-file serving), keep
fleet state **in-memory** for the first version (a `HashMap` from Instance UID to an Agent record behind
a lock, plus the desired remote configuration and its Config hash — no persistence yet), and ship the
rudimentary UI as a **single static HTML+JS page** that polls one small JSON endpoint
(`GET /api/agents`). That JSON endpoint, together with `PUT /api/config` to set the desired remote
configuration, is the **seed of the future OpenAPI REST API** — the same shape a portal will later
consume. A full OpenAPI description, richer REST resources, and durable persistence are **deferred** to
future ADRs.

## Alternatives considered

- **`actix-web` / `warp` / raw `hyper`** — all capable; `axum` is the mainstream `tokio`-native choice
  that composes with the `tower`/`tower-http` middleware we already want (static files, tracing) and
  shares the runtime picked in ADR-0003, so it is the least-friction fit.
- **Server-rendered HTML templates** (e.g. `askama`, `maud`) — would couple the UI to the Server's
  render path; a static page over a JSON endpoint keeps the UI trivial *and* produces the API seed a
  portal will use, at no extra cost.
- **A persistent store now** (SQLite, sled, Postgres) — durability is not needed to close the loop and
  would lock in a storage choice prematurely (YAGNI); in-memory state is enough for the first version and
  a persistence ADR can supersede it when fleet state must survive a restart.
- **Building the full OpenAPI REST API now** — the API-first contract is a headline goal, but committing
  its full surface before the loop even holds risks designing it wrong; the two seed endpoints keep the
  door open without over-committing.

## Sources / Prior art

- `axum`: <https://github.com/tokio-rs/axum>; `tower-http` (static files, tracing):
  <https://github.com/tower-rs/tower-http>.
- OpAMP Server responsibilities and the config-hash control loop:
  <https://opentelemetry.io/docs/specs/opamp/> and <https://github.com/open-telemetry/opamp-go>.
- Specification Goals #1, #2, #5 and Non-Goal "Shipping a production UI"
  ([`docs/SPECIFICATION.md`](../SPECIFICATION.md)).

## Consequences

- Positive: minimal, `tokio`-native Server that closes the loop and shows fleet state; the UI stays
  rudimentary; the JSON endpoints seed the future REST API in the right shape; nothing durable is locked
  in prematurely.
- Negative / trade-offs: in-memory state is lost on Server restart (acceptable for the first version, not
  for production); the UI polls rather than streams, so it updates at the poll interval, not instantly.
- Follow-ups: future ADRs add the **OpenAPI-described REST API** surface, **durable persistence**, and
  optionally **SSE/WebSocket** live updates for the UI; none of these are needed to close the loop.
