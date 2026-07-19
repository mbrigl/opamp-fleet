# ADR-0007: A JSON REST API (with SSE) and a minimal HTML fleet view

- **Status:** 🟢 accepted
- **Date:** 2026-07-19
- **Deciders:** Maintainer

## Context

The [specification](../SPECIFICATION.md) makes the REST API the Server's public contract (Goal 5: "Any
UI can drive the fleet") and calls the Server **headless** — it ships no mandatory UI of its own. But
to *test the dev sidecars* a developer still needs to see, at a glance, which agents are connected and
what status each reports, without reading logs (the sidecars' logs are not visible from inside the Dev
Container, [ADR-0003](0003-compose-dev-environment-with-opamp-sidecars.md)).

So two surfaces are wanted, on the human-facing listener (`:4321`), kept off the agent-facing OpAMP
port (`:4320`, [ADR-0006](0006-rust-opamp-server-from-spec.md)) so they can be firewalled separately: a
**machine-readable REST API** (the contract any external UI builds on) and a **minimal built-in HTML
page** (a zero-setup view for development). The HTML page is a development convenience, not the product
UI the specification declines to ship.

## Decision

We will serve a **JSON REST API under `/api`** and a **minimal server-rendered HTML page** on the
`:4321` listener, over the same fleet state the OpAMP endpoint folds reports into.

- **REST API (the stable contract):**
  - `GET /api/fleet` — the fleet as a JSON array of agents (full config hashes, numeric timestamps,
    raw status strings — machine values, not the HTML display strings).
  - `GET /api/fleet/events` — a **Server-Sent-Events** stream: the fleet on connect, then again on
    every change (an agent connects, reports, or disconnects), with keep-alive comments.
  - `GET /api/config` — the distributed collector configuration as raw YAML, read from disk.
  - `PUT /api/config` — replace it; the body is the YAML. Writing the file is the only way to
    reconfigure the fleet ([ADR-0006](0006-rust-opamp-server-from-spec.md)), so this distributes
    nothing itself. On success `204`; a rejected write is `400` with a JSON error.
  - The JSON shape is a **stable contract** under `/api`; an incompatible change is versioned
    (`/api/v1`), not broken in place.
- **HTML page (a development view):** one server-rendered page at `/` listing each agent's reported
  state (identity, health, config status/sync, effective config) and an editor that `POST`s the
  configuration to the same file. Rendered with `askama`, whose **contextual auto-escaping** keeps
  agent-reported strings — which the Server does not control — from becoming markup; escaping is a
  security property here, not a convenience.
- **Unauthenticated, development-only.** Like the initial server ([ADR-0006](0006-rust-opamp-server-from-spec.md)),
  the `:4321` surface authenticates nobody; anyone who can reach it can reconfigure the whole fleet. The
  page says so, and both the page and the API must be kept off untrusted networks until authentication
  lands (a later ADR).

## Alternatives considered

- **API only, no HTML page.** Truest to "headless", but then seeing the sidecars means writing a client
  first. The minimal page is a zero-setup development view; it is explicitly not the product UI.
- **HTML page only (server-rendered, no API).** Fails Goal 5 — an external UI would have to scrape HTML.
  Rejected: the API is the contract; the page is the convenience.
- **Poll-only (no SSE).** Every UI would poll `GET /api/fleet` for liveness. SSE gives a live view at
  low cost and degrades to polling for clients that do not use it.
- **A WebSocket for live UI updates.** More than the UI needs (it only needs server→client push); SSE
  is simpler, proxy-friendly, and native in browsers.
- **GraphQL / a richer query layer.** Overkill for "list the fleet, read/write one config".
- **JSON-wrapping the configuration bytes.** The artifact is a YAML file; `GET`/`PUT` carry it raw
  (`text/yaml`) so it matches the file and the editor. Only errors and the fleet are JSON.

## Sources / Prior art

- The requirement: [`SPECIFICATION.md`](../SPECIFICATION.md) (Goal 5; "headless" Server; REST API as the
  contract). The state it projects and the listener split:
  [ADR-0006](0006-rust-opamp-server-from-spec.md).
- `axum` JSON and Server-Sent Events — <https://docs.rs/axum/latest/axum/response/sse/index.html>.
- Server-Sent Events (`text/event-stream`) —
  <https://developer.mozilla.org/en-US/docs/Web/API/Server-sent_events>.
- `askama` contextual auto-escaping — <https://docs.rs/askama/latest/askama/>.

## Consequences

- Positive: any UI can read the fleet and drive the configuration over a stable JSON contract with live
  SSE updates; the built-in page makes the dev sidecars and their status visible with zero setup — the
  immediate goal of the initial server.
- Negative / trade-offs: a public JSON contract to keep stable. The unauthenticated trust model is now
  exposed programmatically, which makes keeping `:4321` off untrusted networks more important, not less
  — authentication is the open flank, deferred to a later ADR.
- Follow-ups: authentication for `:4321`; optionally reimplementing the HTML page on top of the API;
  API versioning if the shape must evolve.
