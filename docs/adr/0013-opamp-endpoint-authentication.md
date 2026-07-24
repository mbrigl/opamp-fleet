# ADR-0013: Static Basic and Bearer authentication on the OpAMP endpoint, optional by default

- **Status:** 🟢 accepted
- **Date:** 2026-07-24
- **Deciders:** Markus Brigl

## Context

Goal 17 of the [specification](../SPECIFICATION.md) demands that *"the Server accepts only
authenticated Agent identities"*. TLS (ADR-0007) protects the channel but authenticates nobody:
today any process that can reach `/v1/opamp` becomes a fleet member. ADR-0007 explicitly deferred
this: *"Authentication (tokens, mutual TLS, `ConnectionSettings` rotation) stays out of scope
here."* The Protocol Baseline treats authentication as transport-level HTTP: auth methods MAY be
used, `401` MUST be returned on failure, and the mechanism is the standard `Authorization` header —
the Baseline's own connection-settings example shows `Authorization: Basic …`, and its
`ConnectionSettings.headers` field exists precisely so a Server can later rotate such headers
(goal 17's credential rotation, a follow-up capability).

The forces:

- **Both schemes are ecosystem practice.** `opamp-go` clients send static headers
  (`StartSettings.Header` / `HeaderFunc`); the Collector's `opampextension` sets `headers` or
  delegates to an auth extension, and the Collector ships both a `basicauthextension` and a
  `bearertokenauthextension`. Supporting **Basic and Bearer** keeps every mainstream OpAMP client
  connectable.
- **Zero-config operation must survive.** The project's configs are optional throughout
  (ADR-0008); a lab or evaluation setup without credentials must keep working unchanged.
- **One listener** (ADR-0005) serves OpAMP, REST API, and UI. Agent authentication and operator
  authentication are different principals; this decision must not silently couple them.
- **Authorization is a non-goal.** The specification defers roles, permissions, and tenancy:
  authenticating *that* a peer belongs to the fleet is in scope, *which* Agent may do *what* is not.

## Decision

We will add **optional static HTTP authentication to the OpAMP endpoint only**, on both ends:

- **Server.** A new optional `[auth]` section in `server.toml` holds the accepted credentials —
  `bearer_tokens = ["…"]` and/or a `[auth.basic_users]` table of `user = "password"` pairs; an
  `[auth]` section with no credential at all is rejected loudly (ADR-0008). When the section is
  present, every request to `/v1/opamp` — each plain-HTTP POST and the WebSocket upgrade GET,
  checked **before** the upgrade completes — must carry an `Authorization` header matching any
  configured credential; anything else is answered `401` with a `WWW-Authenticate` challenge
  (RFC 9110). Credential comparison is constant-time via the `constant_time_eq` crate (tiny,
  zero-dependency, exactly this job). Without `[auth]` the endpoint stays open — today's behaviour.
- **Client.** A new optional `[auth]` block in `client.toml` with either `bearer_token = "…"` or
  `username`/`password` (both schemes at once are rejected loudly). The Client sends the resulting
  `Authorization` header on every plain-HTTP request and on the WebSocket upgrade. A `401` is
  logged as a credential failure and retried with the existing capped backoff, so fixing the
  credentials on either side needs no restart choreography. The Client warns when it is about to
  send credentials over a non-TLS endpoint that is not loopback.
- **Scope.** REST API and UI remain unauthenticated — operator-facing auth is a separate decision
  (see Consequences). The Supervisor Endpoint stays loopback-only and unauthenticated (ADR-0011).
  A Gateway will forward credentials untouched — *"a Gateway makes no authentication decisions"*
  (ADR-0003).

Multiple accepted server-side credentials make overlapping rotation possible by hand today and are
exactly the shape the Baseline's `ConnectionSettings.headers` rotates tomorrow — this decision
implements the lock, not yet the key exchange.

## Alternatives considered

- **Mutual TLS as the authentication mechanism** — authenticates the peer at the channel, but
  demands a client-certificate PKI on day one, does nothing for the low-friction start, and the
  Baseline's own rotation story treats authorization headers as the first-class credential. mTLS
  remains complementary future work alongside connection settings.
- **Per-Agent credentials mapped to `instance_uid`** — turns authentication into identity
  management and brushes against authorization/tenancy, which the specification explicitly defers.
  One fleet-membership check is what goal 17 asks of this step.
- **Authenticating the whole listener (REST API + UI too)** — tempting for symmetry, but operator
  and Agent are different principals with different lifecycles; coupling them silently would
  prejudge the operator-auth decision.
- **Hashed passwords (htpasswd/argon2) in `server.toml`** — the config file is operator-owned and
  permission-protected exactly like the TLS private key next to it (ADR-0007); hashing adds a
  tooling step without changing that trust boundary. Revisit if credentials ever leave the file.
- **Pluggable authenticators (OIDC, JWT validation, external IdP)** — YAGNI; static credentials
  satisfy "belongs to the fleet", and the hexagonal seam to add a validator later is not made
  harder by shipping the simple thing first.

## Sources / Prior art

- [OpAMP specification](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md) —
  connection settings `headers`: *"Typically used to set access tokens or other authorization
  headers"*, with the `Authorization: Basic …` example; auth methods MAY be used and `401` MUST be
  returned on failure (tracked in [`CONFORMANCE.md`](../CONFORMANCE.md)).
- [`opamp-go` `StartSettings`](https://github.com/open-telemetry/opamp-go/blob/main/client/types/startsettings.go)
  — static `Header` plus per-request `HeaderFunc`; the server side authenticates in the
  connection callbacks before accepting.
- [`opampextension` README](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/extension/opampextension)
  — `server::ws|http::headers`, `auth` (authenticator extension ID), `tls`.
- Collector [`basicauthextension`](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/extension/basicauthextension)
  and [`bearertokenauthextension`](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/extension/bearertokenauthextension)
  — the ecosystem's client/server auth building blocks; both schemes, statically configured.
- RFC 7617 (Basic), RFC 6750 (Bearer), RFC 9110 §11 (`401` + `WWW-Authenticate`).
- [`constant_time_eq`](https://crates.io/crates/constant_time_eq) — constant-time comparison.

## Consequences

- Positive: goal 17's authentication half lands with one small config section per side; every
  mainstream OpAMP client (opamp-go, `opampextension`) can present the required header today; the
  credential shape is the one the Baseline's connection settings rotate, so the coming
  `OffersConnectionSettings` / `AcceptsOpAMPConnectionSettings` work builds on this instead of
  replacing it. The `Authentication` row in `CONFORMANCE.md` flips to implemented.
- Negative / trade-offs: credentials sit in plaintext in operator-owned config files (accepted —
  same trust boundary as the TLS key); Basic and Bearer over non-TLS transports are cleartext
  (mitigated by a Client warning, ultimately the operator's choice); a shared fleet credential
  identifies membership, not individual Agents.
- Follow-ups: server-driven credential rotation via connection settings; operator authentication
  for REST API and UI; mutual TLS as an additional peer proof.
