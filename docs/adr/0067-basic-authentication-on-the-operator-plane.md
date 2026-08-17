# ADR-0067: Basic authentication on the Operator plane — one credential set for the REST API and the UI, optional by default

- **Status:** 🟡 proposed
- **Date:** 2026-08-16
- **Deciders:** Markus Brigl

## Context

[ADR-0066](0066-the-agent-plane-and-the-operator-plane-get-their-own-listeners.md) split the Server
into two listeners and named this decision as its reason: *"authenticating the Operator plane
becomes a decision about one listener"*. That plane — the REST API, its OpenAPI document, the API
docs, and the bundled UI — is still open to anyone who reaches it. Its loopback default keeps that
tolerable, not correct: an operator who publishes the plane to a network, which is one configuration
line, publishes the authority to read the whole fleet, rewrite every Configuration, upload a package
and roll it out.

What is already settled and constrains this:

- **The Agent plane's answer, and its shape.** `[auth]`
  ([ADR-0013](0013-opamp-endpoint-authentication.md)) accepts static Basic and Bearer credentials on
  `/v1/opamp`, precomputes the exact `Authorization` header values that authenticate, compares them
  in constant time, and answers `401` with a `WWW-Authenticate` challenge. Absent, the endpoint is
  open, so a fresh checkout runs with no configuration at all. That mechanism works and is tested;
  the question here is what guards a *different* plane, not how to compare a credential.
- **Authentication, not authorization.** The specification names multi-tenancy and authorization as
  non-goals, and [ADR-0047](0047-admission-is-a-fleet-wide-trust-boundary.md) already fixes the
  Agent side as a fleet-wide trust boundary with no authorization inside it. Nothing here should
  invent operator roles.
- **The UI is a client of the API and nothing more** ([ADR-0005](0005-workspace-and-server-runtime.md)):
  one embedded page, no frontend toolchain, deliberately rudimentary. Whatever guards the API has to
  guard that page too — and must not require a login form, a session store, or a cookie, because
  each of those is the toolchain and the state this project's UI does not have.
- **Credentials sit in `server.toml` verbatim today**, which measure **H7** in
  [`HARDENING.md`](../HARDENING.md) already records as a gap — and records with a warning worth
  repeating: Basic passwords want a password hash, Bearer tokens do not, and the two need different
  answers.

Prior art on browser-facing admin surfaces splits cleanly. Tools whose admin UI is an application in
its own right (Grafana, BindPlane) ship sessions, login pages, and user stores. Tools whose UI is an
operational page — Prometheus, Alertmanager, Traefik's dashboard, a `nginx`-fronted status endpoint
— use HTTP Basic behind TLS, and Prometheus's own web configuration is exactly that: a `basic_auth_users`
map with the password hashed, no session anywhere. The dividing line is not how sensitive the surface
is; it is whether the UI is a product.

## Decision

We will guard the **whole Operator plane** with **HTTP Basic authentication**, configured as
`[rest.auth]` in `server.toml`, optional and absent by default.

Concretely this binds:

- **One section, one kind of credential.** `[rest.auth] basic_users` is a map of `user = "password"`,
  the same shape `[auth.basic_users]` already has. Several users are allowed, which is how a
  credential is rotated or an individual operator's is withdrawn. A `[rest.auth]` section with no
  user fails startup, as `[auth]` does — a section that locks everyone out is never what an operator
  meant.
- **The plane, not the API.** Every route on that listener is guarded: `/api/v1/…`,
  `/api/v1/openapi.json`, `/api/v1/docs`, and the UI at `/`. Basic is what makes that free — the
  browser prompts natively and re-sends the header, so the rudimentary UI needs no login page, no
  session, and no cookie. Like a cookie, an automatically re-sent credential is what makes a
  cross-site request dangerous — which the plane already answers: the body-less `POST` acts carry
  the `Sec-Fetch-Site` guard, and every other mutating route is a JSON `PUT`/`DELETE` that a browser
  may only send after a preflight this Server answers for nobody. The Agent plane is untouched,
  which is the point of ADR-0066: the package download an Agent fetches keeps working with no
  credential at all.
- **Absent means open.** No `[rest.auth]`, no guard — the zero-configuration lab of ADR-0013 keeps
  running, and the loopback default of ADR-0066 keeps being what protects it. The two are
  independent: authentication does not publish the plane, and publishing it does not require
  authentication. It should, and the manual says so, but the Server will not decide it for an
  operator who has a reason.
- **The refusal is the Baseline's shape, reused.** `401` with `WWW-Authenticate: Basic realm="opamp"`
  on every guarded route, credentials precomputed into the exact accepted header values, compared in
  constant time. The primitive `[auth]` already uses moves into one place used by both planes rather
  than being written a second time.
- **A cleartext credential is surfaced, not refused.** With `[rest.auth]` configured, no `[tls]`, and
  a listener that is not loopback, the Server logs a warning at startup: a Basic password on a plain
  HTTP listener is on the wire in base64. Refusing to start would break the one legitimate case — a
  plane already fronted by a TLS-terminating proxy.
- **No Bearer on this plane.** The audience is a browser and `curl`; Basic covers both, and a second
  credential shape with no client asking for it is a second thing to get wrong. Adding
  `bearer_tokens` later is an additive configuration key, not a new decision.
- **No roles, no per-user scope.** Every authenticated operator can do everything the plane offers.
  Authorization stays the non-goal it is; what this decides is *whether the caller is an operator*.
- **Passwords stay as `[auth]` stores them** — verbatim in `server.toml`, readable by whoever reads
  the file. This ADR deliberately does not hash them: doing it for one section and not the other
  would leave two credential formats in one file, and H7 is the decision that changes both together.

## Alternatives considered

- **A login page with a session cookie.** What a product UI does, and what BindPlane and Grafana do.
  Rejected: it needs a session store, a logout, a cookie policy, and CSRF protection on every
  mutating route — a login system inside a Server whose UI is capped at "rudimentary" by charter, and
  whose REST API is the actual product. Basic gets the same browser experience for none of it.
- **Bearer tokens only.** The cleaner fit for a portal integrating the API. Rejected as the *only*
  scheme: a browser cannot send one without JavaScript holding a token, which drags the UI back into
  session management through a side door.
- **Reuse `[auth]` for both planes.** One credential, less configuration. Rejected: it makes the
  fleet's credential — which lives in `client.toml` on every host in the estate, and is rotated
  through connection-settings offers (ADR-0014) — also the operator's password. The two audiences
  are separate on purpose since ADR-0066; giving them one credential undoes that at the only point
  that matters.
- **Hash the passwords now (Argon2/bcrypt).** The right end state, and what Prometheus does.
  Rejected *here*, not on merit but on scope: it adds a dependency and a second storage format
  beside `[auth]`'s cleartext, and running a KDF per request is its own denial-of-service question.
  H7 is where both sections change together.
- **Leave it to a reverse proxy.** Every deployment that fronts the Server can already do this.
  Rejected as the only answer: it makes the property depend on an artifact this project does not
  ship or test, and it cannot be verified here — the same reasoning ADR-0066 used.
- **Refuse to start on cleartext Basic.** Tempting, and wrong: a TLS-terminating proxy in front is a
  legitimate deployment the Server cannot see from where it stands.

## Sources / Prior art

- [RFC 7617 — The 'Basic' HTTP Authentication Scheme](https://datatracker.ietf.org/doc/html/rfc7617)
  and [RFC 9110 §11](https://datatracker.ietf.org/doc/html/rfc9110#name-http-authentication) — the
  `401` / `WWW-Authenticate` exchange, the `realm`, and the standing warning that Basic without a
  confidential channel exposes the password.
- [Prometheus web configuration](https://prometheus.io/docs/prometheus/latest/configuration/https/) —
  `basic_auth_users` guarding the API *and* the built-in UI, no session anywhere; the closest
  comparable to this Server's operational-page UI, and the source of the hashed-password shape H7
  should take.
- [Alertmanager](https://prometheus.io/docs/alerting/latest/configuration/) and
  [Traefik's dashboard](https://doc.traefik.io/traefik/operations/dashboard/) — the same pattern:
  Basic (or a middleware) in front of an operational UI, rather than a login system inside it.
- [Grafana](https://grafana.com/docs/grafana/latest/setup-grafana/configure-security/) and
  [BindPlane](https://docs.bindplane.com/) — the counter-examples with real session-based logins,
  and the reason they are: their UI *is* the product.
- [ADR-0013](0013-opamp-endpoint-authentication.md) — the mechanism reused here (accepted header
  precomputation, constant-time comparison, `401` with a challenge, optional by default).
- [`HARDENING.md`](../HARDENING.md) — H7 (credentials stored in the clear, and why Basic and Bearer
  need different answers) and the scope note drawing the authentication/authorization line this ADR
  stays inside.

## Consequences

- Positive: the Operator plane can be published to a network without publishing the fleet's control
  surface — the combination this project could not offer before ADR-0066 and this decision.
- Positive: the UI is guarded by the same line of configuration as the API, with no login system, no
  session state, and no new attack surface of its own.
- Positive: the Agent plane is unaffected — no Client change, and the package download stays
  reachable without a credential, which is what keeps rollouts working.
- Negative / trade-offs: Basic sends a reusable password on every request, so it is only as good as
  the TLS under it. Mitigated by a startup warning and by the manual, not by the protocol.
- Negative / trade-offs: passwords remain readable in `server.toml`, reaching backups and
  configuration management. Accepted deliberately, and tracked as H7 — which now has two sections to
  change instead of one.
- Negative / trade-offs: browsers cache Basic credentials for the origin and offer no clean logout;
  an operator "signs out" by closing the browser. Accepted for an operational page.
- Follow-ups: hashing stored credentials (H7), for `[auth]` and `[rest.auth]` in one decision; an
  audit record of operator actions (H15), which only becomes meaningful now that a request has a
  name attached to it; and `bearer_tokens` on this plane if a portal ever needs one, which is
  additive.
