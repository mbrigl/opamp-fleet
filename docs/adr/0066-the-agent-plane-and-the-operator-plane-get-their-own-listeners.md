# ADR-0066: The Agent plane and the Operator plane get their own listeners — OpAMP and package downloads on `4320`, REST API and UI on loopback `4321`

- **Status:** 🟢 accepted
- **Date:** 2026-08-16
- **Deciders:** Markus Brigl

## Context

[ADR-0005](0005-workspace-and-server-runtime.md) put the Server's three surfaces — the OpAMP
endpoint, the REST API, and the bundled UI — on **one** listener, and weighed the alternative
explicitly: *"Separate ports for OpAMP / API / UI — cleaner firewalling in some deployments, but it
triples the TLS and configuration surface and buys nothing now; a reverse proxy can still split
paths. Can be revisited if a deployment need appears."* The need has appeared, and it is not
firewalling: it is **authentication**.

What one listener costs today:

- **The Operator plane is open, and nobody decided that it should be.** `[auth]`
  ([ADR-0013](0013-opamp-endpoint-authentication.md)) and the client certificate
  ([ADR-0035](0035-mutual-tls-and-the-server-issued-client-certificate.md)) guard `/v1/opamp` and
  nothing else — `lib.rs` states it plainly: *"REST API and UI stay open, operator-facing auth being
  a separate decision"*. Anyone who can reach port `4320` can read the whole fleet, write and roll
  out Configurations, upload a package and distribute it. That is the strongest authority in the
  system, on the same address as the Agent traffic, with no credential in front of it. Any
  credential scheme added later would have to sit on a listener that must simultaneously stay open
  for Agents on `/v1/opamp` — a per-path exemption on a shared listener rather than a plane with a
  policy.
- **Mutual TLS cannot be required where it is verified.** `client_ca_file` is deliberately
  configured as *optional* client authentication at the TLS layer, because the same listener serves
  a browser, which presents nothing ([`tls.rs`](../../crates/server/src/tls.rs),
  [`CONFORMANCE.md`](../CONFORMANCE.md)); the requirement is then re-imposed per route in
  `Admission`. So the route check is the *only* line, where it should be the second one. This is
  measure **H9** in [`HARDENING.md`](../HARDENING.md), recorded there as needing an ADR for exactly
  this reason.

The two audiences have nothing in common but the process they talk to. Agents connect from the whole
estate, hold long-lived WebSocket sessions, speak protobuf, and prove *fleet membership* — a
fleet-wide credential, a fleet-wide certificate, and no authorization between them
([ADR-0047](0047-admission-is-a-fleet-wide-trust-boundary.md)). Operators and portals connect from a
few places, speak JSON over short requests, and act with authority over every Agent. One address for
both means one exposure, one TLS policy, and one answer to "who may connect" for two questions that
have different answers.

One route crosses that line, and it decides the shape of the split: **`GET
/api/v1/packages/{name}/{agent_type}/{version}/file`**. Its path prefix says Operator plane; its
audience is Agents. The `download_url` in a package offer is, by default, exactly this path, and the
Client resolves it **against its own OpAMP endpoint**
([`client/src/packages.rs::resolve_url`](../../crates/client/src/packages.rs)) — host *and port*.
A downloading Client sends no `Authorization` header and presents no client certificate (a
`download_url` may legitimately point at a mirror, [ADR-0018](0018-packages-imported-from-a-url.md)),
which is why ADR-0013 puts the download on an unauthenticated plane on purpose: the content hash and
the Ed25519 signature are what protect an installed binary
([ADR-0015](0015-package-delivery-for-managed-processes.md),
[ADR-0052](0052-a-package-is-a-versioned-set.md)). Any split that moves this route to a port the
Agent cannot derive turns `advertised_url` from an option into an obligation — and an omitted
obligation surfaces as a failed rollout, not as a startup error.

Fixed by the specification and standing ADRs: the REST API stays *the* contract (goal 5,
[ADR-0012](0012-selector-targeted-configurations-and-openapi-rest-api.md)), the OpAMP endpoint stays
one path serving both transports on the protocol's default port
([ADR-0007](0007-dual-transport-and-tls.md)), and `server.toml` stays TOML that rejects a typo
loudly ([ADR-0008](0008-toml-configuration.md)).

## Decision

We will serve the Server on **two listeners, split by audience rather than by path**:

- **The Agent plane** — `listen`, default `0.0.0.0:4320`: `/v1/opamp` (both transports, unchanged)
  **and the package download route**.
- **The Operator plane** — `[rest] listen`, default `127.0.0.1:4321`: `/api/v1/…`,
  `/api/v1/openapi.json`, `/api/v1/docs` (and its vendored Redoc bundle), and the bundled UI at `/`.

Bound by this decision:

- **The configuration key is a table.** `[rest] listen = "127.0.0.1:4321"`; absent means the
  default, and unknown keys are refused as everywhere else (ADR-0008). The table is the plane's
  name in the file, so the authentication decision that follows lands *inside* it instead of growing
  a parallel set of top-level keys.
- **Loopback by default.** As long as the Operator plane carries no authentication, its
  reachability *is* its protection, and a default that is open to the network would export the
  fleet's full control surface on a second port instead of fixing anything. Reaching it from
  elsewhere is one deliberate line in `server.toml` — or an SSH tunnel, which is the shape most
  operators already use for an unauthenticated admin surface.
- **The package download stays on the Agent plane, unauthenticated, outside `Admission`.** The
  offered `download_url` therefore remains a path the Client resolves against its own endpoint:
  **no Client change, no new obligation on `advertised_url`, and no rollout broken by the split.**
  It stays outside `Admission` because the Client presents neither credential nor certificate when
  downloading; what protects the artifact is its hash and signature (ADR-0013, ADR-0015). The route
  keeps its path — `/api/v1/…` — since the `download_url` of every published Set already names it
  and a path change would be a second, unrelated break.
- **The OpenAPI document describes the Operator plane.** Generated code-first from the registered
  routes (ADR-0012), it consequently no longer carries the download route; the manual documents that
  route as the Agent plane's, which is also the only place an operator would look for it.
- **One set of TLS material, one client CA.** `[tls] cert_file`/`key_file` serve **both** listeners;
  `client_ca_file` belongs to the **Agent plane alone**. Per-listener certificates are not
  introduced — nothing needs them yet.
- **No single-port mode.** Two listeners always. Addresses that cannot both be bound — the same
  port on the same address, or on one that covers every interface, so `0.0.0.0:4320` and
  `127.0.0.1:4320` count as colliding — are refused at startup with a message that says so, rather
  than producing an obscure second bind failure.
- **The router splits in two.** `server::app` becomes `server::agent_app(state, admission)` and
  `server::operator_app(state)`; `main` binds and serves both concurrently, each with its own TLS
  acceptor when `[tls]` is configured, shutting down together and flushing Agent records once
  ([ADR-0051](0051-agent-records-persist-across-a-server-restart.md)).
- **This ADR supersedes ADR-0005 on its single-listener clause only.** Everything else that ADR
  decides — the workspace layout, tokio, axum, the toolchain-free embedded UI — stands unchanged.
- **Authentication on the Operator plane is *not* decided here.** This ADR makes it a decision about
  one listener; taking it is the follow-up named below.

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
  moving part nothing needs today; `advertised_url` already covers the mirror case
  ([ADR-0018](0018-packages-imported-from-a-url.md)).
- **Register the download route on both listeners.** Convenient for an operator who curls an
  artifact to verify it. Rejected: one resource at two addresses under two exposure policies is a
  standing invitation to protect one and forget the other, and the offer names only one of them.
- **Put a reverse proxy in front and split there.** The answer ADR-0005 pointed at. Rejected: it
  makes a security property depend on a deployment artifact this project does not ship, cannot be
  tested here, and — since the proxy terminates TLS — still cannot give the Agent plane a
  handshake-level client-certificate requirement.
- **Default the Operator plane to `0.0.0.0:4321`.** Preserves today's remote access with a
  one-character change to existing commands. Rejected: it is today's exposure on a new port. The
  split is worth taking only if the default is the safe one; an operator who wants it open says so.
- **Keep a merged mode for compatibility (both keys equal → one listener).** Rejected: two
  operating modes to document and test, and the merged one is exactly the mode this ADR exists to
  end.

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
  there because that REST API is authenticated — which is precisely what this Server's is not.
- [`HARDENING.md`](../HARDENING.md) measure **H9** ("Give `/v1/opamp` its own listener") and its
  verification criterion; [`CONFORMANCE.md`](../CONFORMANCE.md) mutual-TLS row, which records the
  shared listener as the reason client authentication stays optional at the TLS layer; ADR-0005's
  own "can be revisited if a deployment need appears".

## Consequences

- Positive: **authenticating the Operator plane becomes a decision about one listener** — a
  credential scheme, a UI session, and a `401` on everything, with no per-path exemption for Agent
  traffic.
- Positive: the Agent plane can later **require the client certificate in the TLS handshake** (H9),
  where an unauthorized peer dies before it reaches a handler and the `Admission` check becomes the
  second line rather than the only one.
- Positive: the default stops publishing the fleet's control surface on the network at all. An
  operator who wants it remote states that, which is the shape a default should have.
- Positive: **no Client change** — `resolve_url`, `advertised_url`, and every published Set's
  `download_url` keep working exactly as they do.
- Negative / trade-offs: **every operator entry point moves to a new port.** `README.md`, the whole
  of `docs/manual/`, `config/server.toml`, `scripts/seed_test_configs.sh`, the operator tools'
  server prompt and `--server` examples ([ADR-0065](0065-the-operator-package-tools-live-in-their-own-crate.md)),
  and the UI's address all change; the `CHANGELOG` has to say so as a breaking change, and anyone
  driving the API from another host must now set `[rest] listen` deliberately. Accepted: that
  deliberate line is the point.
- Negative / trade-offs: the download route leaves the OpenAPI document, so a generated client no
  longer has a method for it. Accepted — no operator flow calls it, and the offer is what names it.
- Negative / trade-offs: two listeners mean two bind failures to report, a larger startup path, and
  the Server's own tests split from one router into two. `CONFORMANCE.md`'s mutual-TLS row and the
  transport rows that say "this listener" need rewording in the same change.
- Follow-ups: **authentication on the Operator plane** — which schemes, where credentials live given
  `server.toml` holds them in the clear today (H7), and whether the bundled UI gets a session —
  needs its own ADR, and is the reason this one exists. **Requiring the client certificate in the
  handshake on the Agent plane** needs its own decision too, and must account for the download route
  sitting on that listener: this project's Client presents no certificate when downloading, so
  requiring one in the handshake would break every download unless the downloader is changed to
  present it when the artifact host is its own Server. **Per-listener TLS material** if a deployment
  ever needs a public certificate for operators and a private one for Agents.
