# ADR-0032: The Agent plane and the Operator plane get their own listeners — OpAMP and package downloads on `4320`, the REST API and UI on loopback `4321` behind optional Basic authentication, and both bound connection setup

- **Status:** 🟢 accepted
- **Date:** 2026-08-18
- **Deciders:** Markus Brigl

## Context

The Server has three surfaces — the OpAMP endpoint, the REST API, and the bundled UI.
[ADR-0005](0005-workspace-and-crates.md) weighed separate ports for them and named the
condition for revisiting one listener: *"Separate ports for OpAMP / API / UI — cleaner firewalling
in some deployments, but it triples the TLS and configuration surface and buys nothing now; a
reverse proxy can still split paths. Can be revisited if a deployment need appears."* The need is
not firewalling: it is **authentication**.

What one listener costs:

- **The Operator plane is open, and nobody decided that it should be.** `[auth]`
  ([ADR-0013](0013-opamp-endpoint-admission.md)) and the client certificate
  ([ADR-0013](0013-opamp-endpoint-admission.md)) guard `/v1/opamp` and
  nothing else. On one listener, anyone who can reach port `4320` can read the whole fleet, write and
  roll out Configurations, upload a package and distribute it. That is the strongest authority in the
  system, on the same address as the Agent traffic, with no credential in front of it. Any
  credential scheme would have to sit on a listener that must simultaneously stay open for Agents on
  `/v1/opamp` — a per-path exemption on a shared listener rather than a plane with a policy.
- **Mutual TLS cannot be required where it is verified.** On a listener that also serves a browser,
  which presents nothing, `client_ca_file` has to be *optional* client authentication at the TLS
  layer ([`tls.rs`](../../crates/server/src/tls.rs), [`CONFORMANCE.md`](../CONFORMANCE.md)); the
  requirement is then re-imposed per route in `Admission`. So the route check is the *only* line,
  where it should be the second one. This is measure **H9** in [`HARDENING.md`](../HARDENING.md).

The two audiences have nothing in common but the process they talk to. Agents connect from the whole
estate, hold long-lived WebSocket sessions, speak protobuf, and prove *fleet membership* — a
fleet-wide credential, a fleet-wide certificate, and no authorization between them
([ADR-0013](0013-opamp-endpoint-admission.md)). Operators and portals connect from a
few places, speak JSON over short requests, and act with authority over every Agent. One address for
both means one exposure, one TLS policy, and one answer to "who may connect" for two questions that
have different answers.

One route crosses that line, and it decides the shape of the split: **`GET
/api/v1/packages/{name}/{agent_type}/{version}/file`**. Its path prefix says Operator plane; its
audience is Agents. The `download_url` in a package offer is, by default, exactly this path, and the
Client resolves it **against its own OpAMP endpoint**
([`client/src/packages.rs::resolve_url`](../../crates/client/src/packages.rs)) — host *and port*.
A downloading Client sends no `Authorization` header and presents no client certificate (a
`download_url` may legitimately point at a mirror, [ADR-0015](0015-package-delivery-for-managed-processes.md)),
which is why ADR-0013 puts the download on an unauthenticated plane on purpose: the content hash and
the Ed25519 signature are what protect an installed binary
([ADR-0015](0015-package-delivery-for-managed-processes.md),
[ADR-0016](0016-a-package-is-a-versioned-set.md)). Any split that moves this route to a port the
Agent cannot derive turns `advertised_url` from an option into an obligation — and an omitted
obligation surfaces as a failed rollout, not as a startup error.

Fixed by the specification and standing ADRs: the REST API stays *the* contract (goal 5,
[ADR-0012](0012-selector-targeted-configurations-and-rest-api.md)), the OpAMP endpoint stays
one path serving both transports on the protocol's default port
([ADR-0007](0007-dual-transport-and-tls.md)), and `server.toml` stays TOML that rejects a typo
loudly ([ADR-0008](0008-toml-configuration.md)).

### What guards the Operator plane

Its own listener makes authenticating the Operator plane a decision about one listener. Its loopback
default keeps an open plane tolerable, not correct: an operator who publishes the plane to a network,
which is one configuration line, publishes the authority to read the whole fleet, rewrite every
Configuration, upload a package and roll it out.

What is already settled and constrains this:

- **The Agent plane's answer, and its shape.** `[auth]` (ADR-0013) accepts static Basic and Bearer
  credentials on `/v1/opamp`, precomputes the exact `Authorization` header values that authenticate,
  compares them in constant time, and answers `401` with a `WWW-Authenticate` challenge. Absent, the
  endpoint is open, so a fresh checkout runs with no configuration at all. That mechanism works and
  is tested; the question here is what guards a *different* plane, not how to compare a credential.
- **Authentication, not authorization.** The specification names multi-tenancy and authorization as
  non-goals, and ADR-0013 already fixes the Agent side as a fleet-wide trust boundary with no
  authorization inside it. Nothing here should invent operator roles.
- **The UI is a client of the API and nothing more** (ADR-0005): one embedded page, no frontend
  toolchain, deliberately rudimentary. Whatever guards the API has to guard that page too — and must
  not require a login form, a session store, or a cookie, because each of those is the toolchain and
  the state this project's UI does not have.
- **Credentials sit in `server.toml` verbatim**, which measure **H7** in
  [`HARDENING.md`](../HARDENING.md) records as a gap — and records with a warning worth repeating:
  Basic passwords want a password hash, Bearer tokens do not, and the two need different answers.

Prior art on browser-facing admin surfaces splits cleanly. Tools whose admin UI is an application in
its own right (Grafana, BindPlane) ship sessions, login pages, and user stores. Tools whose UI is an
operational page — Prometheus, Alertmanager, Traefik's dashboard, a `nginx`-fronted status endpoint
— use HTTP Basic behind TLS, and Prometheus's own web configuration is exactly that: a `basic_auth_users`
map with the password hashed, no session anywhere. The dividing line is not how sensitive the surface
is; it is whether the UI is a product.

### What bounds a connection before a request exists

The Agent plane defaults to `0.0.0.0:4320` (`server::config::DEFAULT_LISTEN`) — it is meant to be
reachable by every host in the fleet, so it is exposed by default; the Operator plane defaults to
loopback.

What a connection to either plane is already bounded by is substantial: the message size limit on
both transports and the Baseline's post-decompression rule (ADR-0007,
[ADR-0005](0005-workspace-and-crates.md)), `max_agents`, `max_package_size_bytes`, Admission
(ADR-0013), and — on the TLS listeners — the 10-second handshake deadline `axum_server`'s
`RustlsAcceptor` applies by default, which `PeerCertAcceptor` inherits by wrapping it.

**What is bounded by nothing is time before a request exists.** Without the bound below, a peer may
complete the TCP connection — and the TLS handshake — send `GET /v1/opamp HTTP/1.1\r\n`, and then
send nothing, for as long as it likes. It costs it one socket; it costs the Server a task and hyper's
read buffer per connection, and it happens *before* any route, any body limit and any Admission check
runs, so no credential is needed to do it many times over. This is the classic slow-header exposure.

**Why hyper's own default does not cover it.** hyper 1.11 defaults
`http1::Builder::header_read_timeout` to 30 seconds — but the default is inert unless a `Timer` is
installed. Its resolution is explicit about it (`hyper-1.11.0/src/common/time.rs`, `Time::check`):

```rust
Dur::Default(Some(dur)) => match self {
    Time::Empty => { warn!("timeout `{}` has default, but no timer set", name); None }
    Time::Timer(..) => Some(dur),
},
Dur::Configured(Some(dur)) => match self {
    Time::Empty => panic!("timeout `{name}` set, but no timer set"),
    ...
```

Neither of the two servers this project uses installs one: there is no `.timer(` call anywhere in
`axum-0.8.9/src/serve/mod.rs` or in `axum-server-0.7.3/src/`. Without the timer, both planes run
with no header-read timeout at all, and the second arm of that `match` is why the fix is not a
one-liner: configuring the timeout without also installing the timer replaces a silent gap with a
panic.

Upstream reached the same conclusion. axum [PR #3478](https://github.com/tokio-rs/axum/pull/3478)
("`axum::serve` now applies hyper's default `header_read_timeout`") installs the timer in
`axum::serve`, and follow-up work adds `Serve::header_read_timeout` /
`Serve::no_header_read_timeout`. It sits in the *Unreleased* section of the changelog; the newest
published axum is 0.8.9, which is what this workspace pins. That is prior art for the value and the
placement, not a fix we can consume.

**Why the bound belongs to connection setup, not to a request.** A per-request timeout would be the
wrong instrument here, and not only because it arrives too late to see the header phase: the three
routes that matter are all *meant* to take a long time. `/v1/opamp` is a long-lived WebSocket; the
package download streams an artifact of arbitrary size (ADR-0015); `put_package_entry` accepts an
upload with the body limit deliberately disabled (ADR-0008). A blanket timeout would break exactly
those and leave the slow peer untouched.

**What it costs to set.** `axum::serve` in 0.8.9 exposes no builder — it constructs its own
`hyper_util` `Builder` internally — so the plain-HTTP path cannot be configured where it stands.
`axum_server::Server::http_builder()` does expose it, and the TLS path already runs on
`axum_server`. Moving the plain path there is therefore consolidation rather than a new stack: both
planes, both transports, one serving path. Installing the timer needs `hyper_util::rt::TokioTimer`,
so `hyper-util` becomes a *direct* dependency of `crates/server` — it is already in the tree as
axum's own dependency and already linked into this binary, so nothing new is compiled or shipped.

Two things come along with that move, both improvements: a TLS listener served by a
`tokio::select!` on `ctrl_c` has **no graceful shutdown whatsoever** (the signal simply drops both
servers), while `axum::serve`'s plain path waits for in-flight connections without a bound — which
includes every open Agent WebSocket. `axum_server`'s `Handle::graceful_shutdown(Some(duration))`
gives both planes the same bounded drain.

## Decision

### Two listeners, split by audience

We will serve the Server on **two listeners, split by audience rather than by path**:

1. **The Agent plane** — `listen`, default `0.0.0.0:4320`: `/v1/opamp` (both transports, unchanged)
   **and the package download route**.
2. **The Operator plane** — `[rest] listen`, default `127.0.0.1:4321`: `/api/v1/…`,
   `/api/v1/openapi.json`, `/api/v1/docs` (and its vendored Redoc bundle), and the bundled UI at `/`.
3. **The configuration key is a table.** `[rest] listen = "127.0.0.1:4321"`; absent means the
   default, and unknown keys are refused as everywhere else (ADR-0008). The table is the plane's
   name in the file, so the authentication of clauses 11 to 18 lands *inside* it (`[rest.auth]`)
   instead of growing a parallel set of top-level keys.
4. **Loopback by default.** Where the Operator plane carries no authentication (clause 13), its
   reachability *is* its protection, and a default that is open to the network would export the
   fleet's full control surface on a second port instead of fixing anything. Reaching it from
   elsewhere is one deliberate line in `server.toml` — or an SSH tunnel, which is the shape most
   operators already use for an unauthenticated admin surface.
5. **The package download stays on the Agent plane, unauthenticated, outside `Admission`.** The
   offered `download_url` therefore remains a path the Client resolves against its own endpoint:
   **no Client change, no new obligation on `advertised_url`, and no rollout broken by the split.**
   It stays outside `Admission` because the Client presents neither credential nor certificate when
   downloading; what protects the artifact is its hash and signature (ADR-0013, ADR-0015). The route
   keeps its path — `/api/v1/…` — since the `download_url` of every Set already rolled out names it
   and a path change would be a second, unrelated break.
6. **The OpenAPI document describes the Operator plane.** Generated code-first from the registered
   routes (ADR-0012), it consequently does not carry the download route; the manual documents that
   route as the Agent plane's, which is also the only place an operator would look for it.
7. **One set of TLS material, one client CA.** `[tls] cert_file`/`key_file` serve **both** listeners;
   `client_ca_file` belongs to the **Agent plane alone**. Per-listener certificates are not
   introduced — nothing needs them yet.
8. **No single-port mode.** Two listeners always. Addresses that cannot both be bound — the same
   port on the same address, or on one that covers every interface, so `0.0.0.0:4320` and
   `127.0.0.1:4320` count as colliding — are refused at startup with a message that says so, rather
   than producing an obscure second bind failure.
9. **The router splits in two.** `server::agent_app(state, admission)` and
   `server::operator_app(state)`; `main` binds and serves both concurrently, each with its own TLS
   acceptor when `[tls]` is configured, shutting down together and flushing Agent records once
   ([ADR-0025](0025-agent-records-staleness-and-forgetting.md)).
10. **ADR-0005's one-port clause gives way to these two listeners, and nothing else of it does.**
    Everything else that ADR decides — the workspace layout, tokio, axum, the toolchain-free embedded
    UI — stands unchanged.

### Basic authentication on the Operator plane

We will guard the **whole Operator plane** with **HTTP Basic authentication**, configured as
`[rest.auth]` in `server.toml`, optional and absent by default.

11. **One section, one kind of credential.** `[rest.auth] basic_users` is a map of
    `user = "password"`, the same shape `[auth.basic_users]` already has. Several users are allowed,
    which is how a credential is rotated or an individual operator's is withdrawn. A `[rest.auth]`
    section with no user fails startup, as `[auth]` does — a section that locks everyone out is never
    what an operator meant.
12. **The plane, not the API.** Every route on that listener is guarded: `/api/v1/…`,
    `/api/v1/openapi.json`, `/api/v1/docs`, and the UI at `/`. Basic is what makes that free — the
    browser prompts natively and re-sends the header, so the rudimentary UI needs no login page, no
    session, and no cookie. Like a cookie, an automatically re-sent credential is what makes a
    cross-site request dangerous — which the plane already answers: the body-less `POST` acts carry
    the `Sec-Fetch-Site` guard, and every other mutating route is a JSON `PUT`/`DELETE` that a
    browser may only send after a preflight this Server answers for nobody. The Agent plane is
    untouched, which is the point of the split: the package download an Agent fetches keeps working
    with no credential at all.
13. **Absent means open.** No `[rest.auth]`, no guard — the zero-configuration lab of ADR-0013 keeps
    running, and the loopback default of clause 4 keeps being what protects it. The two are
    independent: authentication does not publish the plane, and publishing it does not require
    authentication. It should, and the manual says so, but the Server will not decide it for an
    operator who has a reason.
14. **The refusal is the Baseline's shape, reused.** `401` with `WWW-Authenticate: Basic realm="opamp"`
    on every guarded route, credentials precomputed into the exact accepted header values, compared
    in constant time. The primitive `[auth]` uses lives in one place used by both planes rather than
    being written a second time.
15. **A cleartext credential is surfaced, not refused.** With `[rest.auth]` configured, no `[tls]`,
    and a listener that is not loopback, the Server logs a warning at startup: a Basic password on a
    plain HTTP listener is on the wire in base64. Refusing to start would break the one legitimate
    case — a plane already fronted by a TLS-terminating proxy.
16. **No Bearer on this plane.** The audience is a browser and `curl`; Basic covers both, and a
    second credential shape with no client asking for it is a second thing to get wrong. Adding
    `bearer_tokens` later is an additive configuration key, not a new decision.
17. **No roles, no per-user scope.** Every authenticated operator can do everything the plane offers.
    Authorization stays the non-goal it is; what this decides is *whether the caller is an operator*.
18. **Passwords stay as `[auth]` stores them** — verbatim in `server.toml`, readable by whoever reads
    the file. They are not hashed for this section alone: doing it for one section and not the other
    would leave two credential formats in one file, and H7 is the decision that changes both
    together.

### Bounded connection setup on both listeners

We will **serve both planes through `axum_server`, with hyper's HTTP/1 timer installed and a
30-second header-read timeout**, and we will keep every per-request timeout out of it.

19. One place builds a plane's server — plain or TLS, Agent or Operator — installs
    `TokioTimer` on the HTTP/1 builder and sets `header_read_timeout` to 30 seconds. 30 s is hyper's
    own default and what axum will apply once #3478 ships, so this decision becomes "keep the
    default" rather than "hold a private opinion" on that day.
20. The TLS handshake deadline stays at `axum_server`'s 10 seconds, but is **stated in our code**
    rather than inherited silently, so it reads as a decision.
21. Shutdown is one bounded drain per plane via `axum_server`'s `Handle`, never a drop-on-signal and
    never an unbounded wait. `flush_agents` (ADR-0025) still runs after both.
22. **No request timeout, no body timeout, no `tower-http`.** The long routes stay long.
23. **No new `server.toml` key.** The value equals the framework default an operator would otherwise
    never see; a knob is added when a deployment needs a different one, not before.

The header-read timeout is a parameter of the serving function so an integration test can drive it
short: connect, send a partial request line, assert the Server hangs up within the deadline. That
test is what makes this behaviour rather than configuration.

## Alternatives considered

- **Keep one listener and authenticate by path prefix.** A middleware over `/api/v1` and `/` would
  deliver the credential check without moving a port. Rejected: it leaves the two audiences sharing
  one exposure and one TLS policy, so client certificates still cannot be required in the handshake
  (H9 stays open), and the only thing a firewall or a network policy can act on — the port — still
  says nothing about who is talking. It also keeps the Operator plane reachable from every host in
  the estate by construction.
- **Split strictly by path prefix — everything under `/api/v1` moves.** The clean-looking rule, and
  the reason it is wrong: it moves the package download onto a port an Agent cannot derive, so every
  deployment must set `advertised_url` correctly or discover the mistake at rollout time. It trades
  a working zero-config path for tidiness.
- **Three listeners: OpAMP, downloads, Operator.** Gives the artifact plane its own exposure policy
  — genuinely useful for a deployment that wants downloads on a mirror interface. Rejected as a
  moving part nothing needs today; `advertised_url` already covers the mirror case (ADR-0015).
- **Register the download route on both listeners.** Convenient for an operator who curls an
  artifact to verify it. Rejected: one resource at two addresses under two exposure policies is a
  standing invitation to protect one and forget the other, and the offer names only one of them.
- **Put a reverse proxy in front and split there**, or **leave authentication to a reverse proxy.**
  The answer ADR-0005 pointed at, and every deployment that fronts the Server can already do it.
  Rejected as the answer: it makes a security property depend on a deployment artifact this project
  does not ship, cannot be tested here, and — since the proxy terminates TLS — still cannot give the
  Agent plane a handshake-level client-certificate requirement.
- **Default the Operator plane to `0.0.0.0:4321`.** Preserves remote access with a one-character
  change to existing commands. Rejected: it is the one-listener exposure on a new port. The split is
  worth taking only if the default is the safe one; an operator who wants it open says so.
- **Keep a merged mode for compatibility (both keys equal → one listener).** Rejected: two
  operating modes to document and test, and the merged one is exactly the mode clause 8 exists to
  end.
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
  are separate on purpose (clauses 1 and 2); giving them one credential undoes that at the only point
  that matters.
- **Hash the passwords now (Argon2/bcrypt).** The right end state, and what Prometheus does.
  Rejected *here*, not on merit but on scope: it adds a dependency and a second storage format
  beside `[auth]`'s cleartext, and running a KDF per request is its own denial-of-service question.
  H7 is where both sections change together.
- **Refuse to start on cleartext Basic.** Tempting, and wrong: a TLS-terminating proxy in front is a
  legitimate deployment the Server cannot see from where it stands.
- **A `tower-http` `TimeoutLayer` on the routes** — rejected, and it is the alternative worth being
  explicit about: middleware runs *after* hyper has parsed the request line and headers, so it
  cannot see the phase clauses 19 to 23 are about. It would add a dependency, need exclusions for the
  WebSocket, the download and the upload, and still leave the hole open.
- **Wait for the axum release carrying #3478** — rejected. It is unreleased, and it fixes only the
  plain path: the TLS listeners run on `axum_server`, which would still have no timer.
- **Keep `axum::serve` and hand-roll the accept loop on `hyper-util`** — rejected. It buys the same
  knob for the price of owning connection tracking and shutdown, both of which `axum_server` already
  implements and this project already depends on.
- **A `header_read_timeout_secs` key per plane in `server.toml`** — deferred. YAGNI, and a key whose
  only sensible value is the framework default is a knob nobody turns.
- **Put the hardening in `crates/opamp` as a shared layer** — rejected under ADR-0005's rule. This is
  one framework's builder knob in one binary; the Client's outbound transports have no counterpart to
  share with. The protocol-level hardening that *is* identical on both ends already lives there
  (`frame`, `endpoint`).

## Sources / Prior art

- [HashiCorp Vault — TCP listener configuration](https://developer.hashicorp.com/vault/docs/configuration/listener/tcp):
  multiple `listener "tcp"` stanzas, each with its own port and its own client-certificate policy
  (`tls_require_and_verify_client_cert` vs `tls_disable_client_certs`, mutually exclusive) — the
  established shape for exactly this problem, a browser-facing surface and a certificate-authenticated
  one on the same process.
- [etcd — transport security model](https://etcd.io/docs/v3.6/op-guide/security/) and
  [configuration flags](https://etcd.io/docs/v3.4/op-guide/configuration/): `--listen-client-urls`
  (`2379`) and `--listen-peer-urls` (`2380`), with separate certificates per plane and a separate
  `--peer-client-cert-auth`. Separation by audience, with the trust material following the audience.
- [Kubernetes — securing control-plane components](https://kubernetes.io/docs/tasks/administer-cluster/configure-upgrade-etcd/)
  and [Kubernetes API security fundamentals (Datadog Security Labs)](https://securitylabs.datadoghq.com/articles/kubernetes-security-fundamentals-part-2/):
  the API server is the authenticated public surface (`6443`), while `kube-controller-manager`
  (`10257`) and `kube-scheduler` (`10259`) bind loopback — the precedent for defaulting an
  unauthenticated control surface to `127.0.0.1`.
- [`opamp-go`](https://github.com/open-telemetry/opamp-go) `internal/examples/server`: the OpAMP
  endpoint (`0.0.0.0:4320/v1/opamp`) and the demo UI run as **two separate servers** in one process
  — the reference implementation's own example does not put them on one listener.
- [Bindplane — networking requirements](https://docs.bindplane.com/production-checklist/bindplane/networking-requirements):
  REST *and* OpAMP on one port (`3001`). The counter-example, and instructive: it is defensible
  there because that REST API is authenticated with a session-based login.
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
- [ADR-0013](0013-opamp-endpoint-admission.md) — the mechanism reused in clause 14 (accepted
  header precomputation, constant-time comparison, `401` with a challenge, optional by default).
- [axum PR #3478](https://github.com/tokio-rs/axum/pull/3478) and the
  [axum changelog](https://github.com/tokio-rs/axum/blob/main/axum/CHANGELOG.md) — upstream installs
  the timer and applies hyper's 30 s default; unreleased as of 0.8.9, the version this workspace
  pins. Also the motivation measured there: an incomplete request costs ~1 MB per connection against
  ~2.5 kB for an idle one, and it blocks graceful shutdown.
- [axum issue #2741, "How to avoid Slowloris DoS Attack?"](https://github.com/tokio-rs/axum/issues/2741)
  — the same question asked from the outside, with the same answer: it is a connection-setup knob.
- [`hyper::server::conn::http1::Builder::header_read_timeout`](https://docs.rs/hyper/1.11.0/hyper/server/conn/http1/struct.Builder.html#method.header_read_timeout)
  — *"Requires a `Timer` set by `Builder::timer` to take effect. Panics if `header_read_timeout` is
  configured without a `Timer`."* — and `hyper-1.11.0/src/common/time.rs`, `Time::check`, quoted
  above, for what the untimed default actually resolves to.
- `axum-server-0.7.3/src/server.rs` (`http_builder`) and `src/tls_rustls/mod.rs` (the 10-second
  handshake default) — read from the vendored sources, since neither is prominent in the docs.
- Prior art for the value: Go's `http.Server` leaves `ReadHeaderTimeout` unset by default and the
  field exists precisely for this attack ([Diving into Go's HTTP server timeouts](https://adam-p.ca/blog/2022/01/golang-http-server-timeouts/)),
  with ~20 s a commonly recommended setting; nginx's `client_header_timeout` defaults to 60 s. 30 s
  sits inside that range and matches hyper.
- [`opamp-go`](https://github.com/open-telemetry/opamp-go/blob/main/server/serverimpl.go), this
  project's behavioural oracle under [ADR-0004](0004-protocol-baseline-and-conformance.md), builds
  its `http.Server` with `Handler`, `Addr`, `TLSConfig` and `ConnContext` and **no timeouts at all**.
  The oracle is silent here rather than contrary: hardening a deployment is not protocol behaviour,
  so clauses 19 to 23 go beyond it without diverging from it.
- [`HARDENING.md`](../HARDENING.md) — measure **H9** ("Give `/v1/opamp` its own listener") and its
  verification criterion; H7 (credentials stored in the clear, and why Basic and Bearer need
  different answers); the scope note drawing the authentication/authorization line clauses 11 to 18
  stay inside. [`CONFORMANCE.md`](../CONFORMANCE.md) mutual-TLS row. ADR-0005's own "can be
  revisited if a deployment need appears".

## Consequences

- Positive: **authenticating the Operator plane is a decision about one listener** — a credential
  scheme and a `401` on everything, with no per-path exemption for Agent traffic.
- Positive: the Operator plane can be published to a network without publishing the fleet's control
  surface, and the UI is guarded by the same line of configuration as the API, with no login system,
  no session state, and no new attack surface of its own.
- Positive: the Agent plane can **require the client certificate in the TLS handshake** (H9), where
  an unauthorized peer dies before it reaches a handler and the `Admission` check becomes the second
  line rather than the only one.
- Positive: the default does not publish the fleet's control surface on the network at all. An
  operator who wants it remote states that, which is the shape a default should have.
- Positive: **no Client change** — `resolve_url`, `advertised_url`, and every rolled-out Set's
  `download_url` keep working, and the package download stays reachable without a credential, which
  is what keeps rollouts working.
- Positive: an unauthenticated peer can no longer pin a connection open indefinitely on either
  plane. The exposure closes on the Agent plane, which is the one that is public by default.
- Positive: one serving path for plain and TLS, a bounded graceful drain on both planes, and a
  10-second TLS handshake deadline that is a stated choice rather than an inherited default.
- Negative / trade-offs: **every operator entry point is on its own port.** `README.md`, the whole
  of `docs/manual/`, `config/server.toml`, `scripts/seed_test_configs.sh`, the operator tools'
  server prompt and `--server` examples ([ADR-0005](0005-workspace-and-crates.md)),
  and the UI's address name `4321`, and anyone driving the API from another host must set
  `[rest] listen` deliberately. Accepted: that deliberate line is the point.
- Negative / trade-offs: the download route is not in the OpenAPI document, so a generated client
  has no method for it. Accepted — no operator flow calls it, and the offer is what names it.
- Negative / trade-offs: two listeners mean two bind failures to report, a larger startup path, and
  the Server's tests split into two routers. `CONFORMANCE.md`'s mutual-TLS row and the transport rows
  speak of the listener each row is about.
- Negative / trade-offs: Basic sends a reusable password on every request, so it is only as good as
  the TLS under it. Mitigated by a startup warning and by the manual, not by the protocol.
- Negative / trade-offs: passwords remain readable in `server.toml`, reaching backups and
  configuration management. Accepted deliberately, and tracked as H7 — which has two sections to
  change.
- Negative / trade-offs: browsers cache Basic credentials for the origin and offer no clean logout;
  an operator "signs out" by closing the browser. Accepted for an operational page.
- Negative / trade-offs: `hyper-util` is a direct dependency of `crates/server`. It is already in the
  lockfile and already linked into this binary through axum, so the cost is the entry in
  `Cargo.toml` and one more crate whose version bumps this workspace notices.
- Negative / trade-offs: the header-read bound is **HTTP/1 only**. The TLS listeners offer `h2` by
  ALPN, and HTTP/2 has no header-read equivalent; its analogues are keep-alive pings and
  `max_concurrent_streams`. Left undecided deliberately rather than guessed at.
- Negative / trade-offs: a peer that needs more than 30 s to send its request headers is hung up on.
  No Client in this project comes close, and the value is the one axum will impose anyway.
- Negative / trade-offs: connection *count* stays unbounded — a peer may still open many sockets,
  each now cheap and short-lived. Bounding that is a different instrument.
- Negative / trade-offs: clauses 19 to 23 bound the **Server's** two planes and nothing else. The
  Client serves the same protocol on two listeners of its own — the Gateway endpoint (ADR-0024) and
  the Supervisor Endpoint — and both lack the bound; the Supervisor Endpoint worse, since it serves
  connections one at a time and a stalled handshake blocks the Managed Process behind it. That is
  measure **H18** in [`HARDENING.md`](../HARDENING.md), and deliberately not folded in here: it is
  the same fix in another binary, not another decision.
- Follow-ups: hashing stored credentials (H7), for `[auth]` and `[rest.auth]` in one decision.
  **Requiring the client certificate in the handshake on the Agent plane** needs its own decision,
  and must account for the download route sitting on that listener: this project's Client presents
  no certificate when downloading, so requiring one in the handshake would break every download
  unless the downloader is changed to present it when the artifact host is its own Server.
  **Per-listener TLS material** if a deployment ever needs a public certificate for operators and a
  private one for Agents. An audit record of operator actions (H15), which is meaningful now that a
  request has a name attached to it. `bearer_tokens` on the Operator plane if a portal ever needs
  one, which is additive. The Client's two listeners (H18); a concurrent-connection cap per plane
  (H16); HTTP/2 keep-alive and stream limits (H17); and revisiting clauses 19 and 20 when axum ships
  #3478, at which point they may reduce to *not* opting out of the framework's defaults.
