# ADR-draft-the-operator-plane-also-signs-in-with-oidc-beside-basic: Both transports over TLS 1.3, two planes split by audience, bounded input, and admission by client certificate and revocation, with the Operator plane also signing in with OIDC beside Basic

- **Status:** 🟡 proposed
- **Date:** 2026-11-15
- **Deciders:** Markus Brigl
- **Applies to:** the OpAMP endpoint and both transports in `crates/fleet-server/src/transport.rs` and `crates/fleet-agent/src/transport/`, TLS in `crates/opamp/src/tls.rs`, `crates/fleet-server/src/tls.rs` and `crates/fleet-agent/src/tls.rs`, the plaintext rule in `crates/opamp/src/client/connection.rs`, the reconnect delays in `crates/opamp/src/client/backoff.rs` and `crates/opamp/src/client/ws.rs`, how every listener binds and serves and how it reads a request body or a WebSocket message in `crates/opamp/src/server/`, the package upload in `crates/fleet-server/src/api.rs`, `crates/fleet-server/src/listen.rs` and `main.rs`, the `rustls` features in `Cargo.toml`, the `listen`, `max_connections`, `max_new_connections_per_sec`, `[rest]`, `[tls]` and `[rest.tls]` keys of `server.toml` and their parsing and startup checks in `crates/fleet-server/src/config.rs`, the certificates `server pki status` lists and the running Server watches in `crates/fleet-server/src/pki.rs`, `config/server.toml`, the message limit in `crates/fleet-server/src/agent_rate.rs`, `on_message` and `on_unreadable` of the Server's handler, the enrolment refusals in `Fleet::enrol` and the download guard `admit_download` in `crates/fleet-server/src/transport.rs`, every `Unavailable` reply in `crates/fleet-server/src/fleet.rs`, the `[agent_rate_limit]` section of `server.toml` and its startup warning, the `agent_rate.throttled` audit event, every outbound connection the Server opens, and every document that tells an operator which certificate the Operator plane serves or how a browser or `opamp-fleetctl` trusts it; Admission on `/v1/opamp`, the package download route and `/v1/gateway/revocations` in `crates/fleet-server/src/transport.rs`, `tls.rs` and `ca.rs`, the Operator plane's credential and guard in `crates/fleet-server/src/credentials.rs` and `api.rs`, admission throttling in `crates/fleet-server/src/throttle.rs`, the WebSocket session loop and the close frame `crates/opamp/src/server.rs` sends, the serial numbers `crates/fleet-server/src/ca.rs` assigns, the revocation list in `crates/fleet-server/src/revocation.rs` and `crates/fleet-server/src/fs/revocation.rs`, the `/api/v1/revocations` route, the reading of a CSR in `crates/fleet-server/src/ca.rs` and its handling in `crates/fleet-server/src/fleet.rs`, the Client's identity, CSR and renewal in `crates/fleet-agent/src/config.rs`, `tls.rs` and `csr.rs`, the identity it presents on a download in `crates/fleet-agent/src/packages.rs`, the `[tls]`, `[client_ca]`, `[enrolment]`, `[admission_throttle]` and `[rest.auth]` sections of `server.toml`, the `[tls]` section of `supervisor.toml`, an `[auth]` section in either file, the Server's `hash-credential` and `pki status` commands in `crates/fleet-server/src/main.rs`, the `CertificateSigner` port in `crates/fleet-server/src/fleet.rs` and every PKCS#11 adapter beside the local CA in `crates/fleet-server/src/pkcs11.rs` and `crates/fleet-core/src/pkcs11.rs`, the signing of bootstrap certificates in `crates/fleet-server/src/bootstrap.rs`, the `[pkcs11]` section of `server.toml` and its parsing in `crates/fleet-server/src/config.rs`, `config/server.toml`, the `pki init`, `pki token-init` and `pki bootstrap-ca` commands in `crates/fleet-tools/src/pki.rs`, the `cryptoki` dependency of the Server and of `crates/fleet-tools` in `Cargo.toml` and `deny.toml`, SoftHSM2 with its `softhsm2.conf` and token directory in the Dev Container and on the CI runner, `docs/ARCHITECTURE.md`, and every document that tells an operator where the CA keys live; the Operator plane's guard and its `401` in `crates/fleet-server/src/api.rs`, the token validation and the issuer's key cache beside it, the actor of every Operator-plane audit entry, the sign-in of the bundled UI under `crates/fleet-server/static/`, the Operator plane's Content Security Policy, the `[rest.auth]` and `[rest.auth.oidc]` sections of `server.toml` and their parsing in `crates/fleet-server/src/config.rs`, `config/server.toml`, the `jsonwebtoken` dependency in `Cargo.toml` and `deny.toml`, the outbound connection to the issuer, and every document that tells an operator how to sign in
- **Supersedes:** [ADR-0012](0012-tls-1-3-two-planes-and-bounded-input.md), [ADR-0026](0026-admission-by-client-certificate-and-revocation-with-the-ca-keys-in-a-file-or-on-a-token.md)

## Context

This ADR carries every clause of
[ADR-0012](0012-tls-1-3-two-planes-and-bounded-input.md) and
[ADR-0026](0026-admission-by-client-certificate-and-revocation-with-the-ca-keys-in-a-file-or-on-a-token.md)
and changes three of them, so that the Operator plane also signs in with OpenID Connect beside
Basic: the guard of the Operator plane and the rule that its credential carries no rights (ADR-0026
clauses 13 and 16), and the purposes the Server dials out for (ADR-0012 clause 21). It adds five
clauses: the bearer token, the issuer's keys, the library, the bundled UI's sign-in, and the audit
of who acted. The two fold into one ADR because the guard of the Operator plane, which ADR-0026
decides, and the outbound connection to the issuer, which ADR-0012 bounds, change together.

**Clause map.** ADR-0012's clauses keep their numbers, and ADR-0026's follow them, each its old
number plus 29. On acceptance, a citation of ADR-0012 clause *n* moves to clause *n* of this ADR,
and one of ADR-0026 clause *n* to clause *n* + 29.

| Old clauses | Clauses here | Change |
|---|---|---|
| ADR-0012 clauses 1–20 and 22–29 | 1–20 and 22–29 | none |
| ADR-0012 clause 21 | 21 | the issuer is a third purpose of the Server's outbound connections |
| ADR-0026 clauses 1–12 | 30–41 | none |
| ADR-0026 clause 13 | 42 | the Operator plane is guarded by Basic, OIDC or both |
| ADR-0026 clauses 14 and 15 | 43 and 44 | none |
| ADR-0026 clause 16 | 45 | the operators' credentials, a Basic credential and a token, carry no rights |
| ADR-0026 clauses 17–41 | 46–70 | none |
| none | 71–75 | new: the bearer token, the issuer's keys, the library, the bundled UI, the audit |

The Operator plane is guarded as a whole by `[rest.auth]`, Basic only
([ADR-0026](0026-admission-by-client-certificate-and-revocation-with-the-ca-keys-in-a-file-or-on-a-token.md)
clause 13). Every route, the UI at `/` included, answers `401` with
`WWW-Authenticate: Basic realm="opamp"`, and the browser's native prompt is the login. The
operators' credential is the only one and carries no roles (ADR-0026 clause 16). ADR-0026 rejected
pluggable authenticators for its own change and named the plane's guard as the seam where one
could be added later.

Companies that run a fleet sign their people in through an identity provider. Basic credentials
in `server.toml` are then a second list of people, kept apart from the one that decides who works
there, and the audit names a user name that nothing outside the Server knows. Keycloak is the
provider the project tests against; OpenID Connect is what it and every comparable provider speak.

Basic stays useful: `curl` and scripts send it without a token exchange, and it works while the
identity provider is down. The specification's Non-Goal "Authorization and multi-tenancy" stays as
it is: this change decides who gets in, not what a person may do. A later rights model will be
defined in the Server and must find each person again across sessions, so the person's key is the
token's issuer and subject, never a name or an e-mail address.

The build constrains the token library: TLS and signatures run on *ring*, and `deny.toml` bans
`aws-lc-rs` and OpenSSL
([ADR-0012](0012-tls-1-3-two-planes-and-bounded-input.md)
clause 5). `reqwest` is already a dependency and speaks TLS 1.3 alone.

A reader without this conversation would expect the UI's token in a cookie or browser storage, a
client secret in `server.toml`, and rights read from the token's roles. This ADR decides against
all three.

### Transport and the two planes

The Baseline defines two transports: *"Server implementations SHOULD accept both plain HTTP
connections and WebSocket connections. OpAMP Client implementations may choose to support
either."* The specification's strategy, to implement the protocol as completely as it allows,
turns that into an obligation on both ends. A Server accepting one transport locks out half the
third-party clients, and a Client speaking one cannot exercise both faces of its own Server.

- **Plain HTTP:** the Client POSTs a protobuf `AgentToServer` with
  `Content-Type: application/x-protobuf` and receives a `ServerToAgent`. It polls, by default
  every 30 seconds. The Server MUST honour `Content-Encoding: gzip`. Server-initiated messages
  wait for the next poll.
- **WebSocket:** one persistent connection, either side sends at will, each message a varint
  header plus the protobuf body. It gives the control loop its "within seconds" property, and
  Gateway multiplexing is stated for it ([ADR-0016](0016-client-modes-and-the-gateway.md)).
- **Detection:** one path (`/v1/opamp`, default port 4320). The Baseline describes the
  `Content-Type` header as how the Server tells the two apart.

TLS is part of the transport. G-17 requires every connection a Client opens to the Server to be
TLS 1.3, with a client certificate in its handshake. The Strategy *Security before convenience*
widens that to every connection that leaves the host, and accepts plaintext on the loopback alone.
Q-1 adds that a configuration breaking this is refused at startup, naming the setting, and never
warned about. The Dev Container ([ADR-0004](0004-dev-container-runtime.md)) has neither OpenSSL
headers nor cmake, which `aws-lc-rs`-backed rustls needs.

In this ADR, **LOOPBACK** means the IP literals `127.0.0.1` and `::1`. A host name is never
LOOPBACK, `localhost` included: what it resolves to is a resolver's answer, not the
configuration's.

The Server has two audiences with nothing in common but the process. Agents connect from the
whole estate, hold long-lived WebSocket sessions, speak protobuf, and prove fleet membership
(clauses 30–70).
Operators and portals connect from a few places, speak JSON over short requests, and act with
authority over every Agent: reading the fleet, rewriting Configurations, uploading and rolling out
Packages. One address for both means one exposure, one TLS policy, and one answer to "who may
connect" for two questions with different answers. It also forces client-certificate checks into
route code, since a browser presents none.

One route crosses that line: the package download. Its path is under `/api/v1`, but its audience
is Agents, and the offered `download_url` is a path the Client resolves against its own OpAMP
endpoint. Where the route is served, and what protects the artifact, is
[ADR-0039](0039-a-32-bit-arm-build-is-spelled-arm32.md)'s.

The Agent plane's certificate is made by `opamp-fleetctl pki` from the fleet's own server CA
([ADR-0036](0036-the-operator-program-opamp-fleetctl.md)). The Clients trust that CA because
their `[tls] ca_file` names it. A browser does not, unless the operator imports the server CA
into every browser that opens the UI. In an organization with its own PKI, the UI is expected to
present a certificate from that PKI, which every managed browser already trusts. The Agent plane
cannot take that certificate instead, because every Client trusts the fleet's server CA and
nothing else. One certificate for both planes also puts the key every Client's trust rests on
behind the listener that faces browsers. Both keys live in one process, so a compromise of the
process reaches both. What a second certificate separates is what an operator handles: the PKI's
certificate can be renewed, replaced or handed to the people who run the PKI without touching the
key the fleet trusts. An existing installation without a second certificate must keep starting
unchanged.

A connection is bounded by message size limits, admission, and on TLS listeners a handshake
deadline, but nothing of these bounds the time before a request exists. A peer can open a
connection, send `GET /v1/opamp HTTP/1.1\r\n` and fall silent, costing the Server a task and
hyper's read buffer per connection, before any route or credential check runs. hyper 1.x defaults
`header_read_timeout` to 30 s, but the default is inert unless a `Timer` is installed (its
`Time::check` warns and resolves to `None`), and configuring the timeout without a timer panics.
Neither `axum::serve` (0.8) nor `axum_server` (0.7) installs one; `axum::serve` exposes no builder
at all, while `axum_server::Server::http_builder()` does. A per-request timeout is the wrong
instrument: the three routes that matter (the WebSocket, the download, and the package upload
with its body limit off) are meant to take long, and middleware runs only after the headers are
parsed.

Two more bounds are needed. Nothing else limits how many connections a peer may hold open at
once, and `max_agents` bounds the fleet, not the sockets. The TLS listeners offer `h2` by ALPN,
and the header-read timeout is HTTP/1 only, so an HTTP/2 peer would be bounded by the message
size alone.

Once the headers are in, the body needs a bound too. A peer can send the headers of a `POST` and
then nothing, or a byte a minute, and hold the connection and its task for as long as it likes:
on the Agent plane, which faces the estate, up to `max_message_size_bytes`; on the Operator plane
up to axum's 2 MiB JSON limit, or `max_package_size_bytes` on the upload; on the Gateway endpoint
and the Supervisor Endpoint the same as the Agent plane. A WebSocket message can be trickled the
same way, frame by frame, before it is handed to the handler. The connection cap bounds how many
such peers there are, not how long each one stays. A deadline is the wrong instrument, since a
large upload over a slow link and a long-lived session are both legitimate, but a floor on the
pace is not: a body that has started arriving must keep arriving.

Admission on the Agent plane ends at the handshake
(clauses 34 and 35).
From there on, none of these
bounds how many messages an admitted peer sends:

- the connection cap (clause 19) bounds sockets;
- the message size limit (clause 3) bounds each message;
- the pace floor (clause 18) bounds how slowly a message may arrive;
- the throttle of failed admissions of clause 48 counts failures per peer address, and a member
  never fails.

None of them bounds how often. One WebSocket session can send a full report as fast as the Server
decodes it. Each report takes the fleet lock, may write the Agent's record to disk
([ADR-0029](0029-the-fleet-record-of-agents-and-hosts.md)), may compose an offer, and may ask for a
certificate to be signed. A plain-HTTP poller can do the same with one request per message. The
package download route is the Agent plane's other member route, and it costs more: each request
scans the fleet under its lock to find an Agent the artifact is offered to
([ADR-0039](0039-a-32-bit-arm-build-is-spelled-arm32.md)).

The specification holds that no vulnerability can be ruled out (Strategy *Security before
convenience*). A host taken over, or a Client with a bug that loops, is still a member, and
without a bound it can take the Server's time from every other Agent. Q-2 asks that untrusted
input be bounded, and an admitted peer's message is still input from the network.

Forces that shape the rate limit:

- **The host is the unit admission knows.** A certificate names the host it was issued to as
  `urn:opamp-fleet:host:<id>` (clause 41, the host a certificate names). `instance_uid` is
  self-asserted (clause 41), so a limit keyed on it alone binds nothing: a peer chooses a new uid
  for each message. A certificate an operator provisioned outside the CSR flow names no host until
  it is first renewed. It is still one certificate, told apart by its issuer and serial (`CertId` in
  [`revocation.rs`](../../crates/fleet-server/src/revocation.rs)).
- **A Gateway carries other hosts' Agents.** Behind a marked Gateway every Agent arrives under the
  Gateway's certificate (clause 40, [ADR-0016](0016-client-modes-and-the-gateway.md)). A per-host
  limit there would put a whole site under one host's budget. A per-uid limit alone bounds nothing,
  because a downstream peer can cycle uids. The Gateway's `max_carried_agents` does not stop that
  either: it bounds one downstream connection, and a plain-HTTP downstream exchange is a connection
  of its own ([`gateway/mod.rs`](../../crates/fleet-agent/src/gateway/mod.rs) creates a
  `Downstream` per exchange).
- **The protocol has one retryable refusal.** `ServerErrorResponseType` has three types. Only
  `UNAVAILABLE` tells the Agent to retry, and `RetryInfo.retry_after_nanoseconds` says when.
  - On a WebSocket, the Baseline's *Throttling* section says a Server that cannot process an
    `AgentToServer` message *"SHOULD respond with an ServerToAgent message"* whose
    `error_response` type is `UNAVAILABLE`, and that *"The Client SHOULD disconnect, wait, then
    reconnect again"*.
  - On plain HTTP the Server *"MAY return HTTP 503 … or HTTP 429"* with `Retry-After`, a header
    that *"SHOULD be used only for the client's attempts to reconnect"*.
  - *"The minimum recommended retry interval is 30 seconds."*
  - This Server answers `Unavailable` with `retry_info` of 30 s (`RETRY_AFTER` in
    [`fleet.rs`](../../crates/fleet-server/src/fleet.rs)) in four places besides the rate limit:
    at the Agent-record ceiling, when the enrolment queue is full, when no enrolment window is
    open (clause 38), and when the audit record holds back a CSR
    ([ADR-0028](0028-the-audit-record.md)).
- **The Client honours `Unavailable` on both transports.**
  [`opamp::client::protocol`](../../crates/opamp/src/client/protocol.rs) turns it into a wait,
  `retry_info` or 30 s without one.
  - On a WebSocket the Client closes the connection, waits, and reconnects with a full report from
    every Agent it carries ([`ws.rs`](../../crates/opamp/src/client/ws.rs)).
  - On plain HTTP it waits and resumes its routine reports
    ([`http.rs`](../../crates/opamp/src/client/http.rs)).
- **A refused message loses nothing.** The Server detects a gap in `sequence_num` and answers the
  next message it processes with `ReportFullState` (`fleet.rs`). The Baseline's *Retrying
  Messages* section lets a Client keep one up-to-date message of each kind rather than a queue.
- **A reply reaches an Agent only when it carries that Agent's `instance_uid`.** This project's
  own Client routes every reply by that field and drops one without it, on every connection
  (`Engine::handle` in [`engine.rs`](../../crates/fleet-agent/src/engine.rs)). A Gateway drops one
  too ([ADR-0016](0016-client-modes-and-the-gateway.md),
  [`pool.rs`](../../crates/fleet-agent/src/gateway/pool.rs)). An `Unavailable` reply built
  without an `instance_uid` reaches no Agent, and the Client never waits as told.
- **A host speaks for at most 256 Agents**
  ([ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)),
  and after a reconnect the Client sends one
  full report for each of them. At the Baseline's 30 s heartbeat or poll, 256 Agents send about 8.5
  messages a second.
- **Refusals already have a bounded record.**
  [ADR-0028](0028-the-audit-record.md)
  writes at most ten refusals of one event per peer address and second, and counts the rest. It
  counts every decision whose entry could not be queued in the next entry's `unrecorded_before`.

### Admission and revocation

A fleet credential as a second proof beside the client certificate would prove nothing the
certificate does not prove better, and would cost more than any other part of admission. Such a
credential is one value for the whole fleet. It proves that a peer belongs to the fleet, never
which host or Agent it is, and every host keeps it in clear beside its private key, so whoever
takes a host takes both proofs at once. The client certificate proves membership too, and also the
host: it is issued per host, lives 30 days, is renewed automatically and can be revoked one at a
time. Behind a terminating Gateway a credential every Agent shares does not tell one Agent from
another either. Carrying it would cost a configuration section on both ends, a secret in clear in
`supervisor.toml`, its rotation through connection-settings offers, its revocation by hash, and a
Gateway pool partitioned by the credential each peer presented. The
[specification](../SPECIFICATION.md)'s Strategy *Security before convenience* and Q-1 *Secure by
default* ask for an Agent that proves fleet membership with a client certificate in the TLS
handshake, issued to its host.

The Operator plane keeps its own credential. A browser cannot present a client certificate an
operator does not already manage, and `server.toml` must still hold no credential that
authenticates on its own. A Basic password is chosen by a person and needs a password hash:
Argon2id, at least the parameters OWASP names as its minimum (`m=19456`, `t=2`, `p=1`), compared
in constant time.

G-17 asks that every connection a Client opens to the Server be TLS 1.3 with a client certificate in
its handshake, that the Server admit an Agent only over an unrevoked certificate of the fleet's
client CA, and that a certificate speak only for the Agents of its own host. TLS
(clauses 1–29) protects the channel and authenticates
nobody. The Baseline treats authentication as transport-level: methods MAY be used and `401` MUST be
returned on failure. Its CSR flow — a regular TLS connection, a keypair and CSR generated by the
Agent, a certificate issued by the Server as a local CA or by a CA it proxies to, `BadRequest` when
issuance fails — keeps the private key on the host, and it answers the bootstrap chicken-and-egg
with *"a bootstrap client certificate that is already trusted by the Server"*, whose distribution it
leaves out of scope.

Forces that shape the answer:

- **Ecosystem practice is static headers, with client certificates optional.** `opamp-go` clients
  send static headers; the Collector's `opampextension` sets `headers` or delegates to
  `basicauthextension` or `bearertokenauthextension`. Every peer of the Agent plane presents a
  client certificate in the handshake, so admitting on it alone excludes no client.
- **Authorization and tenancy are non-goals** of the specification: *that* a peer belongs to the
  fleet is in scope, *which* Agent may do *what* is not.
- **`instance_uid` is self-asserted.** An Agent chooses it, and the Server may re-key it at any time
  with `AgentIdentification`. A certificate whose validity depended on matching it would die on a
  re-key the Server itself initiated.
- **A Gateway terminates.** A Client in Gateway Mode reads OpAMP to fold many Agents onto few
  connections ([ADR-0016](0016-client-modes-and-the-gateway.md)), so the certificate the Server
  sees belongs to the Gateway. The Gateway's own handshake is what proves a downstream peer.
- **A Client updates itself.** A Client may start with a `supervisor.toml` written for an
  earlier version, by the self-update of
  [ADR-0019](0019-the-client-updates-itself-from-a-signed-package.md), with nobody at the host.
- **The Agent plane serves the package download besides `/v1/opamp`**
  ([ADR-0039](0039-a-32-bit-arm-build-is-spelled-arm32.md)). A handshake that requires a certificate
  requires it there too, so the Client's downloader must present one — to the Server, never to a
  mirror.
- **An approval means something only when the enrolling peer can be told apart.** A bootstrap
  certificate that chains to the same CA as an issued one is indistinguishable from it; a CA of its
  own makes an enrolment connection recognisable from its handshake.
- **The Baseline asks a Server to throttle.** `ServerErrorResponse` `Unavailable` with
  `retry_info`, and a plain-HTTP `429` or `503` with `Retry-After`, are its forms; the Client
  honours both.
- **The Operator plane's UI is one embedded page with no frontend toolchain.** Whatever guards the
  API must guard that page without a login form, a session store or a cookie. Tools whose UI is an
  operational page (Prometheus, Alertmanager, Traefik's dashboard) use HTTP Basic behind TLS; tools
  whose UI is the product (Grafana, BindPlane) ship sessions.

The Baseline, in *Using instance_uid in the CSR*: *"The Server MUST verify that the instance_uid
field in AgentToServer message matches the instance_uid in the CSR fields"*, so an Agent cannot ask
for a certificate in another Agent's name. It prescribes no field: the `instance_uid` may sit in
*"one of the CSR fields (or part of the field)"*. The subject of an issued certificate is
descriptive
([ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)):
the Server does not require it to match `instance_uid` and drops requested SANs. That alone would
leave the conditional MUST unmet: a CSR that *does* carry an `instance_uid` would be signed whatever
it carries. [`CONFORMANCE.md`](../CONFORMANCE.md#mutual-tls-and-the-two-fields-still-refused)
records how it is met.

This project's own Client puts its `service.instance.name` in the common name and no
`instance_uid` anywhere, so it never triggers that check. A peer implementation may, and
interoperability with one is a stated target
([ADR-0011](0011-the-protocol-is-pinned-and-checked-against-opamp-go.md)). The difficulty is
recognising a claim without reading an ordinary descriptive subject as one. The Baseline's
`instance_uid` is 16 bytes and SHOULD be a UUID v7; its textual form is the canonical 8-4-4-4-12
hexadecimal form of RFC 9562, which a host name or a product name does not take by accident.

With an Agent admitted by a client certificate in the handshake and by nothing else, a
certificate is the one proof to revoke. Two gaps open without a revocation list:

- **A proof could not be withdrawn.** A stolen certificate would stay good until it expires.
- **A session could outlive its proof.** The certificate is checked once per WebSocket, at the
  upgrade. A session admitted yesterday would keep running after the certificate that admitted it
  expired, and nothing could end it early if the certificate were withdrawn.

The specification puts security before convenience and asks that the Server admit an Agent only
over a certificate that is not revoked (G-17, Q-1). An identity the operator no longer trusts is
not admitted, however recently it was.

The Baseline names the answer itself: *"Since the Server knows what access headers and a client
certificate the Client uses, the Server can revoke access to individual Agents by marking the
corresponding connection settings as 'revoked' and disconnecting the Client."* The Server is the
only party on this link that verifies anything, so revocation can be Server state; there is no
third party a CRL or OCSP responder would serve.

Three facts shape the revocation list:

- **`rcgen` derives a serial from the SHA-256 of the public key when none is set.** A re-sent CSR
  for the same key would get the same serial twice, and such a serial says nothing the key
  fingerprint does not.
- **Renewal changes the key.** A Client renews at two thirds of its certificate's life with a new
  key (clause 39). A host that renewed shortly before its old certificate was revoked would carry
  on with the new one.
- **A Gateway hides its downstream certificates.** The Server sees the Gateway's certificate on a
  pooled upstream connection ([ADR-0016](0016-client-modes-and-the-gateway.md)); the downstream
  certificate stays on the downstream hop, and nothing of the downstream Agent's proof reaches the
  Server.

The certificate is worth only what the key that signs it is worth. The Server signs two kinds of
certificate: as the local client CA it signs every host certificate (clause 37), and as the
bootstrap CA it issues bootstrap certificates on request
([ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)).
A key kept as a file is a PKCS#8 PEM file that `opamp-fleetctl pki init` writes into the Server's
directory and the Server reads unattended, unencrypted
([ADR-0036](0036-the-operator-program-opamp-fleetctl.md) clauses 7 and 16). Whoever reads
`client-ca-key.pem` once, from the host, from a backup or from a snapshot of its disk, can sign a
member certificate for any host until the CA is replaced, and nothing in the fleet would tell that
certificate from a real one. Several Server instances on one PostgreSQL database
([ADR-0025](0025-several-server-instances-share-one-postgresql-database-and-a-group-is-placed-again-at-once.md))
each need the CA keys, and copying a key file to every instance multiplies the places it can be
read from; an instance whose CA key is a file therefore runs alone (that ADR's clause 2). A key
on a token is made there by `opamp-fleetctl pki token-init` on the Server host and never leaves it
([ADR-0036](0036-the-operator-program-opamp-fleetctl.md) clauses 7, 8, 12 and 16).

The `CertificateSigner` port in `crates/fleet-server/src/fleet.rs` separates the signing of a host
certificate from the code that decides to sign it, and the bootstrap certificates are signed in
`crates/fleet-server/src/bootstrap.rs`. The CA keys are ECDSA P-256, which every PKCS#11 token
supports. `rcgen` 0.14 signs with any key that implements its `SigningKey` trait, so the
certificate is built by the same code and only the signature comes from elsewhere.

A hardware or cloud HSM costs money and operation that a first step does not need. SoftHSM2 speaks
the same PKCS#11 interface, is free, and runs wherever the Server runs, so the Server is built and
tested against it and moves to a hardware token by naming another module. Whether the Server keeps
its records in files or in PostgreSQL does not change what a token protects. The working note
"server split", recorded outside the ADRs, sets the target in D3 and D9.

## Decision

We will implement both OpAMP transports on both ends over rustls with the `ring` provider and TLS
1.3 alone, accept plaintext only on LOOPBACK, serve the Server on two TLS listeners split by
audience (an Agent plane and a loopback-default Operator plane that may serve a certificate of its
own), build each in one place that bounds connection setup, the number of connections, HTTP/2, and
the pace of every request body and WebSocket message, limit what an admitted peer sends on the
Agent plane with token buckets, admit a peer to the Agent plane on one proof, a client certificate
required in the TLS handshake and checked against a persisted revocation list that follows every
renewal, guard the Operator plane, beyond the loopback, with Basic credentials kept only as a hash,
a bearer token from an OpenID Connect provider that the Server validates against the provider's
keys, or both, with the bundled UI as a public client that holds its tokens in memory and with
neither credential deciding what an operator may do, refuse a CSR whose `instance_uid` claims are
not all its sender's, end each WebSocket session once the certificate that admitted it is revoked
or expires, and keep each CA key in a file or on a token the Server reaches over PKCS#11,
generated there and never leaving it, built and tested against SoftHSM2.

LOOPBACK below means the IP literals `127.0.0.1` and `::1`; a host name is never loopback,
`localhost` included.

### Transport and the two planes

1. **One endpoint, both transports, detected per request.** The Server serves `/v1/opamp` on its
   Agent plane. A WebSocket upgrade (`GET`) starts the WebSocket transport, a `POST` carrying
   `Content-Type: application/x-protobuf` is one plain-HTTP exchange, and a `POST` without that
   type is answered `415`. Both hand every decoded report to the same processing, so transport is
   carriage, never semantics.

2. **The Client selects the transport by its endpoint's scheme.** `ws://` and `wss://` are
   WebSocket, `http://` and `https://` are polling; any other scheme fails at startup. `ws://` and
   `http://` are accepted only when the endpoint's host is LOOPBACK; any other host fails at
   startup with a message naming the endpoint. The default endpoint is
   `wss://127.0.0.1:4320/v1/opamp`, so WebSocket over TLS is the default: it is the only transport
   that delivers Server-initiated changes without polling latency. `poll_interval_secs` (default
   30) governs polling. `heartbeat_interval_secs` (default 30; `0` disables heartbeats and
   undeclares `ReportsHeartbeat`) governs WebSocket. A heartbeat the Server offers replaces both
   ([ADR-0013](0013-connection-settings-that-never-lead-to-plaintext.md)).

3. **Gzip and a message size limit on both transports.** The Server accepts gzip request bodies
   (a Baseline MUST) and enforces `max_message_size_bytes` (default 64 MiB, `0` refused at
   startup) in both directions and on both transports, applied after decompression: `413` on plain
   HTTP, a `1009` close on WebSocket, and an oversized reply is discarded rather than sent. The
   shared helpers that implement this live in `crates/opamp`
   ([ADR-0040](0040-the-operator-tool-takes-from-the-client-what-this-adr-names.md)).

4. **Reconnection belongs to the Client's transport layer, and a fleet comes back spread out.**
   - The first reconnect after an established connection is lost — cut, or closed by a Server
     that shuts down — waits a delay drawn uniformly over `[0, 30 s)`. A failed attempt after it
     is retried with exponential backoff from one second, capped at one minute, with equal jitter,
     which the Baseline asks for so that a restarted Server is not hit by the whole fleet on the
     same instants. The first draw starts at zero, unlike the equal jitter of the later steps,
     because that jitter's floor is what puts a whole fleet into the same half second after a
     restart. Thirty seconds is the default heartbeat interval, and an Agent whose connection
     dropped alone waits 15 seconds on average.
   - The Server completes at most `max_new_connections_per_sec` (default 200) new Agent-plane
     connections a second, each a full TLS 1.3 handshake and admission; that is the rate it must
     absorb. It counts connections, not requests, so the routine polls of a large fleet are never
     refused. A connection past it completes its handshake, is offered HTTP/1.1 alone, and is
     answered `503` to every request, a WebSocket upgrade before it completes, with a
     `Retry-After` of whole seconds drawn uniformly from 1 to 30, and then closed; HTTP/2 has no
     way to close a connection after one answer, and a pooled one would be refused for as long as
     it lived. The refused peers come back spread rather than together. A fleet of more than 6 000
     Agents meets the bound after a restart, and its `Retry-After` spreads it further. The bound
     limits the admissions, not the handshakes: a refused connection still costs one.
   - The Client honours `Retry-After` on a refused upgrade as it does on plain HTTP: a `429` or
     `503` waits the `Retry-After` it carries, or the next backoff step when it carries none,
     whichever is longer.
   - After a reconnect the Client sends a full status report. The Server keys no state on the
     connection ([ADR-0016](0016-client-modes-and-the-gateway.md)).

5. **TLS is TLS 1.3 alone, on rustls with the `ring` provider, everywhere; no OpenSSL, no system
   TLS library.** `reqwest` requires the `tls12` feature of `rustls`, so the floor is held where
   every configuration is built rather than by the build. `opamp::tls::provider()` is the `ring`
   provider with the three TLS 1.3 suites and nothing else: `TLS13_AES_256_GCM_SHA384`,
   `TLS13_AES_128_GCM_SHA256` and `TLS13_CHACHA20_POLY1305_SHA256`. Every rustls configuration of
   both ends is built from it with the protocol versions pinned to TLS 1.3
   ([ADR-0040](0040-the-operator-tool-takes-from-the-client-what-this-adr-names.md)).
   Each binary installs it process-wide at
   startup, and every `reqwest` client is built with a minimum of TLS 1.3. This covers every
   connection: OpAMP, the REST API, downloads, mirrors, own telemetry and the Server's connections
   to its stores. `axum-server` (`tls-rustls-no-provider`) terminates TLS on the Server,
   `tokio-tungstenite` (`rustls-tls-webpki-roots`) carries `wss://`, and `reqwest`
   (`rustls-no-provider`, `gzip`) carries `https://`. The `*-no-provider` features keep
   `aws-lc-rs` out of the build, so a missing provider is an error, never a silent fallback.
   Without `[tls] ca_file` a Client trusts the built-in webpki roots on `wss://` and the
   platform's trust store on `https://`, the default `reqwest` takes. With it, that bundle
   replaces both, so self-signed deployments work; trust is the operator's file, never a Server's
   instruction.

6. **Plaintext on LOOPBACK alone, refused at startup elsewhere.** A listener without TLS is
   accepted only on a LOOPBACK address. `opamp`'s `Listener::serve` refuses a plaintext listener
   on any other address, and `opamp::client::connection` refuses `ws://` or `http://` to any other
   host. Each end refuses such a configuration at startup with a message naming the setting; it
   never warns and continues.

7. **Two listeners, split by audience rather than by path.**

   | Plane | Key | Default | TLS | Serves |
   |---|---|---|---|---|
   | Agent | `listen` | `127.0.0.1:4320` | always | `/v1/opamp`, the package download route, and the Gateways' revocation list (clause 63) |
   | Operator | `[rest] listen` | `127.0.0.1:4321` | always | `/api/v1/…`, `/api/v1/openapi.json`, `/api/v1/docs` with its vendored Redoc bundle, and the bundled UI at `/` |

   The Agent plane listens on LOOPBACK by default. Serving the estate is one deliberate line in
   `server.toml`, together with the TLS material it needs. `[rest]` is a table so that the plane's
   own settings (its authentication,
   clauses 42 and 71 to 75,
   and its connection cap)
   live inside it; unknown keys are refused as everywhere
   ([ADR-0040](0040-the-operator-tool-takes-from-the-client-what-this-adr-names.md)).
   The router is split accordingly:
   `fleet_server::agent_app(state, admission)` and `fleet_server::operator_app(state, auth)`.

8. **The Operator plane is loopback by default.** Its reachability is its first protection; a
   network-open default would export the fleet's full control surface. Reaching it from elsewhere
   is one deliberate line in `server.toml`, or an SSH tunnel. It serves TLS with the material
   clause 9 gives it, on every address. Off LOOPBACK it requires authentication as the admission
   decision states; a Server lacking it is refused at startup.

9. **`[tls]` is required and serves the Agent plane, and the Operator plane too unless
   `[rest.tls]` is present.** `[tls] cert_file` and `key_file` serve the Agent plane, with ALPN
   `h2` and `http/1.1`. A Server without `[tls]` is refused at startup with a message naming the
   section. `[tls] client_ca_file` applies to the Agent plane alone, and what it requires is
   decided by clauses 34 and 35.
   Without `[rest.tls]`, the Operator plane serves `[tls] cert_file` and `key_file` as well, with
   the same ALPN.

10. **`[rest.tls]` holds `cert_file` and `key_file`, both required, and nothing else.**
    `cert_file` is a PEM chain, the leaf first and any intermediates after it. `key_file` is its
    PEM private key. A `client_ca_file` or any other key in the section fails at startup: the
    Operator plane asks for no client certificate. The Operator plane serves the pair with ALPN
    `h2` and `http/1.1` and TLS 1.3 alone, as clause 5 decides for every listener.

11. **The `[rest.tls]` pair is checked at startup as `[tls]`'s is.** A file that cannot be read, a
    `cert_file` with no certificate, a `key_file` with no usable private key, and a key that does
    not match the certificate each refuse the start with a message naming the file. The Server
    checks nothing else about the certificate: not its names, not its issuer, and not its naming.
    It is the operator's PKI that answers for those.

12. **A new `[rest.tls]` certificate takes effect when the Server restarts.** The files are read
    once, at startup. Replacing them changes nothing that is served until the next start.

13. **`[rest.tls] cert_file` is a certificate the Server depends on.** `server pki status` lists
    it, and the running Server warns before it ends, at startup and daily, under the expiry rule
    of [ADR-0036](0036-the-operator-program-opamp-fleetctl.md). A file named in both `[tls]` and
    `[rest.tls]` is listed once.

14. **Two listeners always; addresses that cannot both be bound fail at startup.** There is no
    single-port mode. The same port on the same address, or on an address that covers every
    interface (`0.0.0.0:4320` and `127.0.0.1:4320`), is refused at load with a message naming both
    keys. Both listeners are bound before either serves, and a bind failure names the plane.

15. **Every plane's server is built in one place, with an HTTP/1 header-read timeout of 30
    seconds.** `listen::plane` serves both planes on `opamp::server::listen`
    ([ADR-0040](0040-the-operator-tool-takes-from-the-client-what-this-adr-names.md)),
    which serves plain and TLS through
    `axum_server`, installs `hyper_util::rt::TokioTimer` on the HTTP/1 builder, and sets
    `header_read_timeout` to `HEADER_READ_TIMEOUT` (30 s, hyper's own default and what axum
    applies once it installs the timer itself). The timeout is a parameter of the serving function
    so a test can drive it short. `hyper-util` is a dependency of `opamp`'s `server` feature for
    this.

16. **The TLS handshake deadline is stated, not inherited.** `TLS_HANDSHAKE_TIMEOUT` is 10 seconds
    on both planes' acceptors: `axum_server`'s default value, named in this code so it reads as a
    decision.

17. **Shutdown is one bounded drain for both planes.** One `Handle` is shared by both listeners. A
    stop closes both listeners at once and tells each WebSocket session to close at an instant
    drawn uniformly over `drain_secs`
    ([ADR-0025](0025-several-server-instances-share-one-postgresql-database-and-a-group-is-placed-again-at-once.md)
    clause 3); whatever has not ended 2 seconds after that window is cut, and the Agent-record flush
    of [ADR-0029](0029-the-fleet-record-of-agents-and-hosts.md) runs afterwards.

18. **No request timeout, but a floor on the pace of every body and every message; no
    `tower-http`, no `server.toml` key for these bounds.** The long routes stay long: nothing has
    a deadline, and a connection with nothing in flight (an idle WebSocket session between
    messages, a keep-alive connection between requests) is not bounded by this clause. A request
    body begins when its headers end, and a WebSocket message with its first byte. From then on
    it must deliver at least `MIN_PACE_BYTES` (64 KiB) within every `PACE_WINDOW` (60 s), each
    window measured from the end of the one before, or have ended. A body announced in the headers
    but never sent is therefore bounded as one that stops. A body that falls behind is answered
    `408` and whatever was staged from it is removed; a WebSocket message that falls behind closes
    its connection with `1008`. The floor holds on every listener `opamp`'s server serves (the
    Agent plane, the Operator plane, the Gateway endpoint and the Supervisor Endpoint), for every
    route, the package upload included, and on both HTTP versions. The two values are named
    constants, the same everywhere: a knob is added when a deployment needs a value other than
    these, not before.

19. **A connection cap per plane.** `max_connections` caps the Agent plane (default 10 000) and
    `[rest] max_connections` the Operator plane (default 256); `0` is refused at startup. The cap
    is applied in the accept loop of `opamp`'s listener. A connection past the cap is closed on
    accept, before the TLS handshake, while the connections already established keep working. A
    fleet larger than the default raises the key together with the process's file-descriptor
    limit. Where several instances serve one fleet
    ([ADR-0025](0025-several-server-instances-share-one-postgresql-database-and-a-group-is-placed-again-at-once.md)),
    each connection cap counts the connections of one instance, and `max_agents` counts the Agent
    records stored, for the whole fleet. A fleet with more connections than one instance's cap is
    spread over enough instances, or the cap is raised.

20. **HTTP/2 is bounded on every listener.** `opamp`'s listener sets the HTTP/2 builder of every
    listener it serves, so every TLS listener, where ALPN offers `h2`, carries the bounds.
    `H2_MAX_CONCURRENT_STREAMS` is 100 per connection. A keep-alive ping is sent every
    `H2_KEEP_ALIVE_INTERVAL` (30 s), and a peer that leaves one unanswered for
    `H2_KEEP_ALIVE_TIMEOUT` (20 s) is dropped. They are named constants, not keys, as clause 18
    states for the HTTP/1 bound.

21. **The Server dials only what its configuration names, for a purpose an ADR in force decides,
    over TLS 1.3.** Each outbound connection runs over the provider of clause 5, and plaintext
    only to a loopback endpoint the configuration names. The ADRs in force give it three such
    purposes, the S3 store and the PostgreSQL store
    ([ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md),
    clauses 17 and 24), and the issuer `[rest.auth.oidc] issuer` names, for its discovery document
    and the JSON Web Key Set that document names (clause 72). The Server sends no request for a
    package anywhere
    ([ADR-0039](0039-a-32-bit-arm-build-is-spelled-arm32.md), clause 4). This clause and clause 5
    bind the Server's own code. A library the operator has the Server load opens what its own
    configuration names, over its own stack, and the ADR that has the Server load it decides
    whether it may.

22. **`[agent_rate_limit]` in `server.toml`.** The rate limit covers an admitted peer's
    `AgentToServer` messages on `/v1/opamp` over both transports and its requests on the package
    download route. The section is optional, and its defaults are in force when it is absent. Any
    other key is refused, as in every section. No value switches the limit off.

    | Key | Default | Rule |
    |---|---|---|
    | `messages_per_sec` | `10` | Tokens added to a host's or an Agent's bucket per second, continuously. `0` is refused at startup with a message naming the key. |
    | `burst` | `300` | That bucket's capacity, enough for one full report from each of the 256 Agents a host may speak for (ADR-0027) after a reconnect. A new bucket starts full. `0` is refused at startup with a message naming the key. |
    | `gateway_messages_per_sec` | `500` | Tokens added per second to the aggregate bucket of a host marked as a Gateway. `0` is refused at startup with a message naming the key. |
    | `gateway_burst` | `10000` | The aggregate bucket's capacity: one full report from each Agent at the default `max_carried_agents` (ADR-0016). `0` is refused at startup with a message naming the key. |

23. **The Server warns when the limit is below what the fleet's heartbeat needs.**
    `messages_per_sec` times the offered `[connection_offer] heartbeat_interval_secs` is the number
    of Agents one host can report for at that interval. Without an offer, the Baseline's 30 s is
    used. When that product is below 256, the Server logs a warning at startup naming both keys.
    It is a warning, not a refusal: a limit set too low costs availability, not security. At the
    defaults the product is 300.

24. **What is counted.** Each of the following costs one token, on every connection that passed
    admission:
    - every message the endpoint hands to the Server's handler on `/v1/opamp`, on both
      transports. That covers a member's connection and an enrolment connection, a plain-HTTP
      request (one message each), and every message on a WebSocket session;
    - a message that cannot be decoded (`on_unreadable`);
    - every request on the package download route that admission let through.

    On `/v1/opamp` the check runs first in `on_message`: before enrolment, before a CSR is read,
    and before `process_presented` takes the fleet lock. On the download route it runs in
    `admit_download` once the certificate is admitted, before the route's handler. Which bucket a
    message or a download is counted in is [ADR-0016](0016-client-modes-and-the-gateway.md)'s.

    Not counted: a WebSocket upgrade that sends nothing, which the connection cap bounds, and the
    Gateways' revocation list.

25. **What a message past the limit gets.** A message that finds no whole token in a bucket it
    must pass is not processed. No record is created or updated, `last_seen_ms` does not move, no
    CSR is read, and nothing is offered. Its reply is a `ServerToAgent` with:
    - the message's own `instance_uid` (clause 27);
    - the Server's capabilities;
    - `error_response` of type `Unavailable`, with an `error_message` saying that too many
      messages arrive;
    - `retry_info.retry_after_nanoseconds` of 30 s, the `RETRY_AFTER` every other `Unavailable`
      of this Server carries and the Baseline's minimum recommended retry interval.

    On plain HTTP the reply is the protobuf body of a `200`, never a `429` or a `503`. The
    Baseline gives `Retry-After` to reconnect attempts, and this keeps the OpAMP answer apart from
    the failed-admission throttle of clause 48.

    The Server does not close the connection. A WebSocket Client that honours the Baseline closes
    it itself and comes back after 30 s; messages that arrive before it does are counted and
    answered the same way. A throttled message leaves `sequence_num` where it was, so the next
    message the Server processes from that Agent is answered with `ReportFullState`.

26. **What a download past the limit gets.** The download route is plain HTTP and not OpAMP, so it
    has no `ServerErrorResponse`. A request past the limit is answered `429` with
    `Retry-After: 30`, and the artifact is not read. This `429` differs from the one the
    failed-admission throttle of clause 48 sends:

    | | The failed-admission throttle (clause 48) | This clause |
    |---|---|---|
    | When it is sent | Before the certificate is judged | After the certificate is admitted |
    | Who gets it | A peer address in back-off for failed admissions | A member over its rate |
    | `Retry-After` | The seconds of back-off left | 30 |
    | Feeds the failure count | — | No, it is no failure |
    | Recorded as | `download.throttled` | `agent_rate.throttled` with `route` `download` (clause 29) |

    The Client's downloader treats either one as a failed download.

27. **Every `Unavailable` reply carries the `instance_uid` of the message it answers.** The Client
    and a Gateway route replies by that field alone and drop a reply without it. The throttled
    reply of clause 25 therefore names the Agent that sent the message, and only that Agent. The
    other `Unavailable` replies carry it too, within
    clause 41 and
    [ADR-0028](0028-the-audit-record.md).
    Those replies are:
    - the Agent-record ceiling in `fleet.rs`;
    - a CSR held back for its audit record;
    - the full enrolment queue and the closed enrolment window in `Fleet::enrol`.

    An undecodable message has no `instance_uid`, so its throttled reply carries none, and no Agent
    receives it.

28. **The tables are bounded.** The buckets live in memory and nothing persists them. They are
    kept in two tables:
    - Buckets keyed by host, by issuer and serial, or by an enrolling peer's address, and the
      Gateways' aggregates, hold at most `revocation::MAX_ISSUED` (100 000) entries, the
      certificate register's own capacity.
    - Buckets keyed by a Gateway's host and an `instance_uid` hold at most `max_agents` entries
      (default 100 000), the fleet's own ceiling.

    When a table is full, a new key evicts the bucket that was used least recently. Eviction can
    only grant tokens, never withhold them, because a new bucket starts full. Its worst case is a
    peer that refills its own bucket early, and filling a table to that end takes as many keys in
    use as the table holds.

29. **The audit records every throttled message and download as a refusal.** Each one is passed
    to the audit record
    ([ADR-0028](0028-the-audit-record.md))
    as event `agent_rate.throttled` with outcome `throttled`. The entry carries:
    - the peer address;
    - `host`, or `serial` and the CA role where the certificate names no host;
    - `instance_uid`, hex, where the message carries a valid one;
    - `bucket`: `agent`, `host` or `gateway`, naming the bucket that was empty;
    - `route`: `opamp` with the `transport`, or `download`.

    The entries are aggregated under ADR-0028's bound on refusals, by event and peer address. A
    host that floods costs at most ten entries and one `agent_rate.throttled.aggregated` count per
    address and second. The refusal never waits for its entry. A refusal that finds the audit
    channel full is counted in the next entry's `unrecorded_before` (ADR-0028). A message or
    download the limit lets through is recorded by nothing new.

### Admission and revocation

30. **The Agent plane has no credential of its own.** `server.toml` has no `[auth]` section; one
    that is present is refused at startup with a message naming `[auth]` and saying that the Agent
    plane admits by client certificate alone. `/v1/opamp` and the download route read no
    `Authorization` header. One that is sent is ignored, never refused, so a Client that still
    sends one is admitted on its certificate. The Agent plane sends no `WWW-Authenticate`
    challenge.
31. **The Operator plane's credential primitive.** A presented Basic password is verified against
    the named user's Argon2id hash, and an unknown user costs the same verification against a hash
    made at startup at the highest cost any user's hash has, so the answer's timing does not tell
    which users exist. A password hash runs on a blocking thread, at most four at once; past that a
    request is answered `503` with `Retry-After: 1`. Before a credential is checked, the attempt is
    counted against the throttle of clause 48 — failures and attempts under way together may not
    exceed `max_failures` — and an IPv6 peer is counted by its /64
    ([`credentials.rs`](../../crates/fleet-server/src/credentials.rs)). A verification that
    succeeded is remembered for ten minutes by the SHA-256 of the whole header value, in a table of
    at most 1 024 entries, and compared in constant time with `constant_time_eq`; a failed one is
    never remembered and counts toward the throttle of clause 48.
32. **The Client sends no credential.** `supervisor.toml` has no `[auth]` section. One that is
    present is ignored, with one notice at startup naming the section, and nothing from it is sent;
    a Client updated by the Server must keep connecting. An `Authorization` header persisted with
    connection settings is dropped when they are loaded
    ([ADR-0013](0013-connection-settings-that-never-lead-to-plaintext.md)). A `401` is logged as an
    admission failure and retried with the transport's backoff. The communication layer refuses
    `ws://` and `http://` to a host that is not LOOPBACK
    ([ADR-0040](0040-the-operator-tool-takes-from-the-client-what-this-adr-names.md)).
33. **What the Server offers reaches only admitted members.** A connection-settings offer and a
    `[telemetry_offer]` carrying headers are sent only on a connection whose certificate was issued
    by the client CA. An enrolment connection
    ([ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md))
    receives no offer but the certificate it asked for.
34. **Client certificates on both transports, required in the TLS handshake.** `[tls]
    client_ca_file` in `server.toml` is required; a configuration without it is refused at startup
    naming the key. The Agent plane's listener verifies with `ClientAuth::Required` in `opamp`,
    whose roots are the client CA and, while `[enrolment]` is configured, the bootstrap CA (clause
    46). A peer presenting no certificate, or one that chains to neither, fails the handshake and
    reaches no handler. `PeerCertificate` carries the handshake's certificate into the request;
    `/v1/opamp` still answers `401` without one, a second line behind the handshake. The Client
    presents its identity on `wss://` (its rustls `ClientConfig`) and on `https://` (a reqwest
    `Identity::from_pem`).
35. **The certificate is the whole of admission.** A connection is a member's when its certificate
    was issued directly by the client CA, is valid and is not revoked (clause 60); any other
    certificate that passes the handshake makes an enrolment connection (clause 46). No
    configuration admits a peer without a certificate. Behind a Gateway, the Gateway's handshake
    proves the downstream peer (clause 40).
36. **A Client has two sources for its identity, and the issued one wins.** An operator may
    provision one in `supervisor.toml`'s `[tls]` (`cert_file` and `key_file`, both or neither,
    beside `ca_file`); a Server-issued pair is stored in `state_dir` as `client-cert.pem` and
    `client-key.pem`. The stored pair outranks the file, the same precedence persisted connection
    settings have; deleting it reverts to what the operator wrote. The private key is written
    `0600` on Unix and inherits the state directory's ACL on Windows
    ([ADR-0020](0020-the-installed-service-and-its-state.md)).
37. **The Server signs CSRs as a local CA while `[client_ca]` is configured.** `[client_ca]` names
    `cert_file`, `validity_days` (default 30, `0` refused; a month bounds what a certificate stolen
    unnoticed is good for, and a host offline for a fortnight still renews on its own) and the CA's
    key, as exactly one of `key_file`, a PKCS#8 PEM file, or `key_label`, the label of a private key
    object on the `[pkcs11]` token. Naming both or neither fails startup naming the two keys. The
    key is its own, never the listener's, because the CA key is the fleet's trust anchor.
    `AcceptsConnectionSettingsRequest` is declared only while the section is present. A Client that
    holds no Server-issued certificate, or holds one in its renewal window, generates a keypair and
    a PEM CSR locally and sends it as `ConnectionSettingsRequest.opamp.certificate_request`, only
    once the Server has declared the capability (the pessimistic gate on sending in
    [ADR-0013](0013-connection-settings-that-never-lead-to-plaintext.md)). A CSR on a connection
    whose certificate chains to the client CA is signed at once; a CSR on an enrolment connection
    waits for an operator
    ([ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)).
    A CSR that does not parse, or that arrives while no `[client_ca]` is configured, is answered
    with a `ServerErrorResponse` of type `BadRequest`. The issued certificate travels back as an
    ordinary connection-settings offer with no private key in it.
38. **A bootstrap certificate carries a host to its first CSR, and an operator approves it.** It is
    issued by the bootstrap CA, never by the client CA, and the Server tells it apart from an issued
    one by that issuer. The Server issues it
    ([ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)).
    Any number may be valid at once, one per customer, site or batch as the operator chooses,
    because none opens anything on its own: it needs an open enrolment window
    ([ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md))
    and an operator's approval
    ([ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)).
    A short validity limits its reach. Handing it to a host is the operator's job.
39. **Renewal starts at two thirds of the validity, and the old certificate stays until the new
    one is proved.** The Client asks again once its certificate is two thirds through its life and
    keeps presenting the old one until the candidate has been verified by connecting with it (the
    verification by connecting in
    [ADR-0013](0013-connection-settings-that-never-lead-to-plaintext.md)); on failure the previous
    certificate stays in force and the offer is reported `FAILED`.
40. **Mutual TLS is per hop, and a Gateway's handshake admits the peers behind it.** Between an
    Agent and the Server through a Gateway there are two mutual-TLS connections, each proving the
    peer at that hop. The Gateway holds its own client identity for the upstream leg, obtained
    through the same CSR flow, and admits a downstream peer by its handshake against the fleet's
    client CA and the Server's revocation list, or, where it holds the bootstrap CA's certificate, a
    peer presenting a bootstrap certificate as an enrolling peer
    ([ADR-0016](0016-client-modes-and-the-gateway.md), which also owns the Gateway's downstream TLS
    keys). The Server admits the Gateway, and the Gateway's mark
    ([ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md))
    lets its certificate speak for the Agents it carries.
41. **Admission is a fleet-wide trust boundary, and within it the host is the only bound between
    Agents.** A report's `instance_uid` is self-asserted; an admitted peer may report under any
    `instance_uid` its host speaks for
    ([ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)),
    and through a Gateway under any at all. Per-Agent authorization is not added: binding a
    certificate to one `instance_uid` breaks Server-initiated re-keying, certificate renewal and
    Gateway Mode. Operators who need isolation between mutually distrusting Agents put them behind
    separate Servers or network segments. This is stated in [`SECURITY.md`](../../SECURITY.md) and
    in the Server's admission and identity module docs.
42. **The Operator plane is guarded by `[rest.auth]`, by Basic, OIDC or both, required beyond the
    loopback.**
    - `[rest.auth] basic_users` is a map of `user = "$argon2id$…"` (clause 50); several
      users allow rotation and the withdrawal of one operator's credential. `basic_users_file` names
      a file holding that map instead ([ADR-0024](0024-the-servers-configuration.md) clause 5). An
      entry with an empty name or password fails startup.
    - `[rest.auth.oidc]` names `issuer`, `client_id`, `required_claim` (a claim name and the value
      it must hold) and an optional `ca_file`. It names no client secret. Without `required_claim`
      it fails startup, naming the key: every account of the issuer would otherwise be an operator
      who can do everything (clause 45).
    - `[rest.auth]` holds Basic users, by `basic_users` or `basic_users_file`, `[rest.auth.oidc]`,
      or both; a section with neither fails startup. A `[rest] listen` that is not LOOPBACK without
      `[rest.auth]` is refused at startup with a message naming both keys. On a LOOPBACK `listen`
      the section stays optional, and the plane's loopback default
      (clause 8) is what protects it.
    - Without `[rest.auth.oidc]`, every route on the listener — `/api/v1/…`, `/api/v1/openapi.json`,
      `/api/v1/docs` and the UI at `/` — answers `401` with `WWW-Authenticate: Basic realm="opamp"`
      without a matching credential, and the browser's prompt is the login.
    - With `[rest.auth.oidc]`, the UI's shell at `/` and the files it loads, `/api/v1/docs` and its
      script, and `/api/v1/openapi.json` are served without a credential; none of them carries fleet
      data. Every other `/api/v1/…` route takes either a Basic credential or a bearer token. Its
      `401` carries `WWW-Authenticate: Bearer`, and the Basic challenge too when Basic users are
      configured and the request is not made by a page's script. The browser says which it is in
      `Sec-Fetch-Mode`, a header a page's script cannot set: a request with `cors`, `same-origin`
      or `no-cors` there gets no Basic challenge, and one with `navigate`, or without the header, as
      `curl` and scripts send it, does. The browser's prompt therefore never opens over the UI's
      sign-in.
    - The Agent plane, the package download included, is untouched by it.

43. **A re-sent credential is not a cross-site licence.** The body-less `POST` acts on the Operator
    plane refuse a request whose `Sec-Fetch-Site` says it is cross-site (`403`), and every other
    mutating route is a JSON `PUT`/`DELETE` a browser sends only after a preflight this Server
    answers for nobody.
44. **A cleartext operator password is refused, not warned about.** A `[rest] listen` that is not
    LOOPBACK must serve TLS; a configuration that would serve that plane in plaintext is refused at
    startup naming the setting. A TLS-terminating proxy in front reaches the plane on a LOOPBACK
    `listen`.
45. **The operators' credentials carry no rights.** A Basic credential and a token are the only
    credentials of the Operator plane, and neither reaches an Agent's host. Every authenticated
    operator can do everything the plane offers. The Server reads no role, group or scope from a
    token to decide what a request may do.

46. **Enrolment has a CA of its own.** `[enrolment] bootstrap_ca_file` in `server.toml` names the CA
    that issues bootstrap certificates, and its key is named by exactly one of
    `bootstrap_ca_key_file`, PKCS#8 PEM, or `bootstrap_ca_key_label`, a private key object on the
    `[pkcs11]` token. The handshake accepts a certificate from the client CA or the bootstrap CA
    (clause 34). While `[enrolment]` is set, a connection is a member's only when its certificate
    was issued directly by a CA in `client_ca_file`. Any other, from the bootstrap CA or from an
    intermediate below either CA, is an enrolment connection, so no chain lifts a bootstrap
    certificate into the fleet. The Server refuses to start on a bootstrap CA that shares a
    certificate with `client_ca_file`, a bootstrap CA key that is the client CA's key or does not
    match `bootstrap_ca_file`, both or neither of the two key settings when the Server is to issue,
    or an `[enrolment]` without `[client_ca]`. Without either key setting enrolment works with
    bootstrap certificates made elsewhere, and `POST /api/v1/bootstrap-certificates` is answered
    `409`, naming the missing key. Without `[enrolment]` no bootstrap certificate passes the
    handshake, and a host gets its first certificate from an operator.
47. **The package download is reached with a client certificate.** Its route on the Agent plane
    sits behind the same handshake as `/v1/opamp`
    ([ADR-0039](0039-a-32-bit-arm-build-is-spelled-arm32.md)). The route requires a certificate from
    the client CA; a bootstrap certificate is answered `401`. The Client presents its certificate
    only to the Server's own origin — the scheme, host and port of its OpAMP endpoint — and
    presents none to any other host or on a redirect hop that leaves that origin
    ([ADR-0039](0039-a-32-bit-arm-build-is-spelled-arm32.md)).
48. **Repeated admission failures from one peer address are throttled.** `[admission_throttle]` in
    `server.toml` sets `max_failures` (default 10), `window_secs` (default 60) and `backoff_secs`
    (default 300). Every `401` on `/v1/opamp` and on the download route counts as a failure of the
    peer's IP address. A peer address with `max_failures` failures within `window_secs` is in
    back-off for `backoff_secs`: each plain-HTTP request is answered `429` with `Retry-After` set
    to the remaining seconds, and each WebSocket upgrade `GET` is refused the same way before the
    upgrade completes. A success clears nothing — behind a shared address a member's success would
    wipe a guesser's count — so failures only age out of the window. The Operator plane counts its
    own `401`s the same way in a table of its own. Each table holds a bounded number of addresses;
    when it is full, the address heard from least recently that is not in back-off is dropped
    first. On the Agent plane a `401` follows a handshake that succeeded — a revoked certificate,
    or a bootstrap certificate on the download route — so the throttle does not stop guessing; it
    bounds what a refused host that keeps retrying costs in handshakes, revocation lookups and
    audit entries
    ([ADR-0028](0028-the-audit-record.md)).
49. **Enrolment reaches the Server directly or through a Gateway, and the Server decides it.** A
    host enrols by connecting to the Server, or through a Gateway that holds the bootstrap CA's
    certificate: the Gateway admits it as an enrolling peer and relays its one CSR, which carries a
    bootstrap proof, and nothing else it sends ([ADR-0016](0016-client-modes-and-the-gateway.md),
    clause 24). The Server decides a relayed enrolment as any other
    ([ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)).
    A CSR a Gateway forwards from a peer that holds a client certificate is signed as a renewal. A
    host can also be provisioned a client certificate by an operator.
50. **No credential in `server.toml` authenticates on its own.** An entry of `[rest.auth]
    basic_users` is an Argon2id PHC string with at least `m=19456`, `t=2` and `p=1`, a 32-byte
    output and a 16-byte salt. Any other value is refused at startup with a message naming the
    section and the user, and never echoing the value. `server hash-credential --basic` reads the
    password from standard input, without echo on a terminal, and prints the line to paste. The
    hashing adds the RustCrypto `argon2` crate and no system dependency.
51. **A claim is a canonical UUID in text.** Thirty-two hexadecimal digits in groups of 8, 4, 4, 4
    and 12 joined by hyphens, in either case, standing alone or as part of a longer value — the
    Baseline's "part of the field". The `urn:uuid:` prefix of RFC 9562 is read the same way. A run
    of 32 hex digits without hyphens is not a claim: it is what a fingerprint or a hash looks like.
52. **Every value is searched.** Each attribute of the subject, read as text whichever string type
    encodes it, and each requested SAN of a type that carries text — DNS name, URI, e-mail address
    — is searched for claims. A SAN is read for this check only; the Server still drops it from
    the certificate
    ([ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)).
53. **The sender is the message's `instance_uid`.** The `instance_uid` of the `AgentToServer` that
    carries the CSR, as the message states it and before any re-key the same message causes, is
    compared byte for byte with each claim. On an enrolment connection it is the same field.
54. **A mismatch is refused, a match or no claim is signed.** A CSR with one claim that differs
    from the sender's is answered `BadRequest`, naming the mismatch without echoing the CSR, and
    never reaches the signer or the enrolment queue. A CSR whose claims all match is signed as any
    other. A CSR with no claim is signed as clause 37 and the renewal proof
    ([ADR-0016](0016-client-modes-and-the-gateway.md)) decide; that is this project's own Client.
55. **The Server gives every certificate it signs a random serial.** 16 bytes from the system's
    secure random source, the top bit cleared so the encoding stays positive. Two certificates from
    one key have two serials.
56. **A revoked certificate is named by its CA and serial.** `POST /api/v1/revocations` with
    `{"certificate": {"authority": "client" | "bootstrap", "serial": "<hex>"}}` revokes one, under
    every certificate of that CA file. The client CA covers certificates the Server signed and
    those an operator provisioned from it; the bootstrap CA lets a lost bootstrap certificate be
    revoked. An authority this Server does not have is answered `400`, so a typo cannot pass for a
    revocation.
57. **A revocation follows renewal.** A certificate is revoked when it, or any predecessor in its
    chain in the certificate register
    ([ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)),
    is on the list. A connection that presents a revoked one is refused before its CSR is read, and
    an open session re-checks its certificate before a CSR it carries is signed.
58. **There is no credential to revoke.** `POST /api/v1/revocations` with `{"credential": …}` is
    answered `400`, naming the field and saying that the Agent plane admits by client certificate
    alone. A credential entry on the persisted list is dropped when the list is loaded, with one
    log line that names the number of entries dropped, and the list is written back without it;
    the Server starts either way.
59. **A revocation can be lifted.** `GET /api/v1/revocations` lists each entry with its id, kind,
    time, authority and serial; `DELETE /api/v1/revocations/{id}` removes one, answered `404` for
    an unknown id. Lifting one takes effect at the next connection; it reopens nothing.
60. **Admission checks the list after the handshake.** On `/v1/opamp` and on the download route, a
    peer whose certificate is revoked is answered `401` without a challenge, as every refusal
    behind the handshake is, and the failure counts toward its admission throttle. The answer does
    not say that the certificate was revoked.
61. **A revocation ends the sessions it concerns, and only those, on every instance.** Each instance
    records, per WebSocket session, the issuer and serial of the certificate that admitted it. On
    the filesystem store the one Server holds every session, and a revocation reaches them in
    process as it is committed, through the revocation list's subscription. On PostgreSQL a
    revocation committed on any instance reaches every instance by its notification
    ([ADR-0025](0025-several-server-instances-share-one-postgresql-database-and-a-group-is-placed-again-at-once.md)
    clause 9), or after a missed one by the re-read of that ADR's clause 10. Either way it is
    checked against every open session there at once; a session checks itself once
    more as it starts, so a revocation landing during its admission is not missed. A session it
    concerns is closed with WebSocket close code `1008` (policy violation) and the reason `revoked`.
    Other sessions are not touched. On plain HTTP every request is admitted anew, against the list
    the instance holds, so the next poll is refused.
62. **A session ends when its certificate expires.** A WebSocket session is closed with `1008` and
    the reason `certificate expired` at the `not_after` of the certificate that admitted it. A
    Client renews at two thirds of the life and reconnects to prove the new certificate, so a
    healthy fleet never meets this close; it ends what renewal did not replace.
63. **The list reaches the Gateways from a route of their own.** `GET /v1/gateway/revocations` on
    the Agent plane is admitted as `/v1/opamp` is — the handshake's client certificate from the
    client CA, checked against the list — and is answered only to a certificate whose host an
    operator marked as a Gateway (`PUT /api/v1/hosts/{host}/gateway`,
    [ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md));
    any other is answered `403`. The body is JSON: every certificate of the client CA that is
    revoked by clause 57, its own entry or a predecessor's, each by the SHA-256 of its issuer's name
    and its serial, so the Gateway resolves no chain; and a version that changes with every change
    to the list, carried as an `ETag`, so a fetch with `If-None-Match` is answered `304`. With
    PostgreSQL it is the list's one version in the database
    ([ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)
    clause 10), the same on every instance, so an `ETag` one instance gave is answered `304` by
    another. Bootstrap certificates are not on it: a Gateway admits no bootstrap certificate. The
    route is outside the OpenAPI document, as the package download is.
64. **One PKCS#11 token, named in `server.toml`.** `[pkcs11]` names `module`, the path of the
    PKCS#11 library; `token_label`; and `pin_file`, a file that holds the user PIN. The PIN file may
    be read by its owner and its group and written by its owner alone, as `password_file` may
    ([ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)
    clause 24). One that others may read, or its group may write, fails startup naming it. The PIN
    is never written into `server.toml` (clause 50). The Server loads the module at startup through
    `cryptoki` and keeps a pool of four sessions on the token, opened as they are needed; no setting
    changes the number. PKCS#11 initialises a module once per process, so instances that share a
    process, as tests run them, share one `cryptoki` context. A module that does not load, a token
    that is not found or a PIN that is refused fails startup, naming the key.
65. **The token is optional, whichever store holds the records.** With `[pkcs11]` set, each key may
    be named by its label; without it, by its file. `[database] store` does not decide it. A
    `key_label` while `[pkcs11]` is not set fails startup, naming both keys.
66. **The token signs; the Server builds.** A PKCS#11 key implements `rcgen`'s `SigningKey`: its
    public half is read from the token once, and each signature is one `C_Sign` with `CKM_ECDSA`
    over the SHA-256 the Server computes, its `r || s` encoded as the DER `ECDSA-Sig-Value` X.509
    asks for. The certificate's contents, its serial and its validity are made as with a key file.
    The adapter sits behind the `CertificateSigner` port and behind a signer port of the same shape
    for bootstrap certificates and audit checkpoints. A separate signing service can later take a
    port's place without a change to the use cases.
67. **The Server signs only what a use case justifies, and audits every signature.** A host
    certificate is signed for a valid renewal proof, an approved enrolment request or a member's
    CSR on its own connection. A bootstrap certificate is signed for an operator's request, and a
    checkpoint on the schedule of [ADR-0028](0028-the-audit-record.md) clause 11. Every certificate
    signed with a token key writes the audit entry `pkcs11.signed`. It names the key's label as
    `key_label`, the certificate's `issuer` and `serial`, and as `use_case` the one that asked for
    it: `renewal`, `enrolment_approval`, `member_csr` or `bootstrap_issue`. A checkpoint is its own
    signature's entry: it names the key and carries the signature, and no `pkcs11.signed` is
    written beside it. The entry's place in the record is held before the token is asked to sign,
    so without a record nothing is signed. It stands beside the entry the use case writes for its
    decision. A signature made with a key file writes none.
68. **SoftHSM2 is the planned token, and says so.** The Server is built and tested against
    SoftHSM2; a hardware or cloud HSM is a change of `[pkcs11] module`, nothing else. SoftHSM2 keeps
    its token in files on one host, so it serves the Server instances of that host alone
    (clause 70). The module the `[pkcs11]` section loads is a library the operator has the Server
    load, which clause 21 leaves to the ADR
    that loads it. This clause decides that the module may open a connection of its own to a
    network HSM: to the address the vendor's client configuration names, over the module's own TLS
    stack. The Server neither builds that connection nor sees it; it and the device's protection of
    it are the operator's. When the module's `C_GetInfo` names `SoftHSM` as its manufacturer, the
    Server starts, logs a warning that the keys have no hardware protection, and writes the audit
    entry `pkcs11.software_token` at every startup. The Dev Container and the CI runner install
    SoftHSM2 for the tests, beside the packages they already install, within
    [ADR-0004](0004-dev-container-runtime.md).
    - SoftHSM2 reads a configuration of its own, `softhsm2.conf`, from the path in `SOFTHSM2_CONF`
      or from `/etc/softhsm2.conf`. Its `directories.tokendir` names the directory that holds the
      tokens, encrypted under their PINs. The Server neither writes nor reads either; it loads the
      module, and the module finds its configuration as it always does.
    - In production `softhsm2.conf`, the token directory and the token's initialisation are the
      operator's, as an HSM's setup is. The token directory is the keys: it belongs in the backup,
      readable by the Server's account alone, and a copy of it with the PIN file is a copy of every
      key on the token.
    - In the Dev Container and on the CI runner the project creates both: a `softhsm2.conf` whose
      token directory lies outside the repository, and a token initialised for the tests, its PIN in
      a file beside it. Nothing of it is committed.
    - With a hardware or cloud HSM neither file exists; the vendor's client configures how the
      module reaches the device.
    - The Server's image carries no PKCS#11 module
      ([ADR-0038](0038-releases-attestations-and-the-server-image.md)). An operator who runs the
      image with a token mounts the module into the container, with what it loads, and names its
      path in `[pkcs11] module`; with SoftHSM2 also its configuration, named by `SOFTHSM2_CONF`, and
      its token directory. A module that does not load in the container fails startup (clause 64).
69. **An existing fleet moves to new CA keys at its ordinary renewal.** A key file cannot be
    imported into a token without leaving a copy outside it. So the move makes a new client CA on
    the token, with `opamp-fleetctl pki token-init` on the Server host. For a transition,
    `client_ca_file` names the old and the new CA certificate; the handshake trusts both, and the
    Server signs every renewal with the new key. A renewal proof is accepted when its certificate
    was issued by any CA in `client_ca_file`, so a host whose certificate the old CA signed renews
    to one the new key signs. Once the last certificate the old CA signed has ended, the operator
    removes it from `client_ca_file`. A new bootstrap CA takes the old one's place as
    [ADR-0036](0036-the-operator-program-opamp-fleetctl.md) clause 12 makes it.
70. **Instances that share the keys reach the same ones.** A key on the token is no file, so an
    instance whose CA keys are named by their labels need not be alone
    ([ADR-0025](0025-several-server-instances-share-one-postgresql-database-and-a-group-is-placed-again-at-once.md)
    clause 2). Instances that sign with shared keys name the same labels on a token every one of
    them reaches: a network HSM all of them reach, or a token that the HSM's own replication keeps
    identical on each host.
    - SoftHSM2 keeps its token in files on one host, and the instances on that host may share it:
      that is redundancy of processes, not of hosts. Instances on several hosts need a network
      HSM, or no CA key on the Server at all, the certificates then being issued outside it. The
      Server cannot tell which host a token's files are on, so keeping a SoftHSM2 token's
      instances on its host is the operator's. The Server does not copy keys between tokens.
    - The audit key's label, which every instance on one database names alike, is decided by
      [ADR-0028](0028-the-audit-record.md) clause 16.

### Signing in with OpenID Connect

71. **A bearer token is a JWT the Server validates on every request.**
    - The signature is checked against the issuer's JSON Web Key Set, with an allow-list of RS256,
      PS256 and ES256. The algorithm in the header must match the family of the key the `kid` names;
      `none`, every HMAC algorithm and a key of another family are refused.
    - `iss` equals `issuer`; `exp` and `nbf` are required and checked with a leeway of at most 30
      seconds; a `typ` header, when present, is `JWT` or `at+jwt`. The claim `required_claim` names
      must hold the configured value.
    - The audience a token must name depends on the route. On `/mcp`, where
      [ADR-draft-an-mcp-endpoint-on-the-operator-plane-reads-and-prepares-and-never-distributes](draft-an-mcp-endpoint-on-the-operator-plane-reads-and-prepares-and-never-distributes.md)
      serves it, `aud` names that endpoint's canonical URI (its clause 3). On every other route
      `aud` names `client_id`, and a token whose `aud` names the canonical URI of `/mcp` is refused
      there, even when it names `client_id` too. `azp` stands in for `aud` on no route.
    - An ID token is never a bearer token. A token that carries `nonce`, `at_hash` or `c_hash`, the
      claims OpenID Connect gives an ID token and RFC 9068 gives no access token, is refused,
      whatever its `typ`. The bundled UI sends a `nonce` with every authorization request
      (clause 74), so every ID token issued to it carries one.
    - A token that fails any check is answered `401` and audited as a refusal, the reason named and
      the token never echoed.

72. **The issuer's keys are fetched lazily, bounded, and the Server starts without them.**
    - The JWKS URI comes from the issuer's discovery document. Both are fetched over `https://` on
      the outbound TLS 1.3 client, with `ca_file` as an added root for a provider a company PKI
      signs, a timeout, and a size bound on each body. An `http://` issuer is accepted on LOOPBACK
      alone.
    - The issuer is the purpose of the Server's outbound connections that clause 21 adds to the
      two stores; it is the only one signing in with OIDC adds.
    - The key set is cached. A token naming an unknown `kid` triggers a refetch at most once per
      minute.
    - While the keys cannot be loaded, the Server runs, Basic works, and a request with a bearer
      token is answered `503`. Nothing about the provider is checked at startup beyond the
      configuration's shape.

73. **The library is `jsonwebtoken` without a crypto backend, on *ring*.** The Server enables none
    of the crate's `aws_lc_rs` and `rust_crypto` features and installs a `CryptoProvider` of its own
    whose verifiers are *ring*'s, for the three algorithms of clause 71. One `Validation` is built
    per key family and audience of clause 71, with `iss`, `aud`, `exp` and `nbf` required; the
    claims that mark an ID token are checked by the Server.

74. **The bundled UI is a public client.** It signs in with the authorization code flow and PKCE
    (`S256`, the verifier made with WebCrypto) and a fresh `nonce` in every authorization request,
    from its own script, with no script from elsewhere.
    It holds the access and the refresh token in memory only — no cookie, no `localStorage`, no
    `sessionStorage` — refreshes them in the background, and sends `Authorization: Bearer` on every
    API call. A reload goes through the provider again, whose own session answers without a prompt.
    Signing out goes to the provider's end-session endpoint. The Operator plane's Content Security
    Policy adds the issuer's origin to `connect-src`, computed at startup; every other directive
    stays as it is.

75. **The audit names who acted, and how.** An act through a token records the person as the
    token's `iss` and `sub`, with `preferred_username` as the readable name; an act through Basic
    records the user name. Each entry says which of the two it was. The person's
    identity is taken from the validated token, never from the request header again.

**Out of scope:**

- Where the package download route is served and what it requires
  ([ADR-0039](0039-a-32-bit-arm-build-is-spelled-arm32.md)), and which hosts a Client downloads from
  (ADR-0039).
- Connection settings and the heartbeat offer
  ([ADR-0013](0013-connection-settings-that-never-lead-to-plaintext.md)).
- Which bucket a message or a download is counted in
  ([ADR-0016](0016-client-modes-and-the-gateway.md)).
- The rate of admission attempts.
- Where the Client's own telemetry goes ([ADR-0017](0017-what-an-agent-reports-about-itself.md)).
- Response compression on plain HTTP.
- A connection cap on the Client's own listeners, the Gateway endpoint and the Supervisor
  Endpoint, which carry the bounds of `opamp`'s listener and no cap.
- A rate limit on the Operator plane, guarded by the Operator plane's authentication and the
  failed-admission throttle of clause 48.
- A rate limit of the Gateway's downstream endpoint, which applies none of its own and forwards
  the Server's `Unavailable` unchanged.
- Limits per message kind, or limits that weigh a message by its cost.
- The rate of WebSocket upgrades and TLS handshakes.
- Whether the Client's downloader honours `Retry-After`.
- Reloading a certificate without a restart.
- A client certificate for operators on the Operator plane.
- A certificate from `opamp-fleetctl` for the Operator plane. Its `pki server-cert` makes one from
  the server CA, which `[rest.tls]` may name like any other. Whether such a certificate is for
  development only is not decided here; the Operator plane's certificate would then always be the
  operator's own, from their PKI or a public CA, which browsers and MCP clients trust without
  extra setup.
- Selecting a certificate by SNI on one listener.
- Of admission and revocation: per-Agent identity beyond the host; an opt-in strict mode binding
  certificate subject to `instance_uid` where Gateway Mode and re-keying are off; proxying a CSR to
  an external CA (it would sit behind `[client_ca]`); how an installer obtains and writes the
  bootstrap certificate, which
  [ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)
  decides, as it decides the view of the pending requests in the bundled UI; counting failed TLS
  handshakes, and the `Unavailable` answer of an overloaded Server, which belong to the connection
  caps; enrolment by a per-host token, which a fleet that outgrows approval one host at a time may
  need; binding the issued certificate to the `instance_uid` (the certificate proves membership and
  its host, not an Agent's identity); the Client putting its `instance_uid` into its own CSR; claims
  in CSR extensions other than the SAN; CRL and OCSP for third parties; what a Gateway does with the
  list ([ADR-0016](0016-client-modes-and-the-gateway.md)); shortening certificate validity; a
  revocation view in the bundled UI; the audit record of revocations and of the sessions they end,
  and the audit key with its checkpoints and the export, which [ADR-0028](0028-the-audit-record.md)
  decides; the server CA's key and the package signing key, which stay offline with the operator;
  the TLS listener's own key; a separate signing service behind the signer port; keys on the Client.
- Of signing in: a bearer token from any issuer but the one `[rest.auth.oidc]` names; rights and
  levels of any kind, and the specification change they need; service accounts and the client
  credentials flow; a session cookie or a server-side confidential client; OIDC on the Agent plane;
  token introspection or revocation lists of the provider; signing in `opamp-fleetctl` with a device
  flow, so that it sends a bearer token as the UI does and automation runs through it on behalf of a
  person.

Local break-glass commands for an incident while the identity provider is down are not decided
here: `server revoke <serial>`, which revokes a certificate and ends its sessions, and
`server enrolment close`, which closes the enrolment window, beside `server audit-verify` and
`server pki status`. They act on the database with the Server's own configuration and credentials,
so their authentication is shell access on a Server host; the set stays that small because an
incident needs a peer cut off, not a rollout. How each run is audited, since it holds no lease
([ADR-0028](0028-the-audit-record.md)),
and that a run which writes to the database sends the notification of
[ADR-0025](0025-several-server-instances-share-one-postgresql-database-and-a-group-is-placed-again-at-once.md)
clause 9, so that every instance ends the sessions a revocation concerns, are decided with them.

End-to-end tests against a real Keycloak are not decided here either. Component tests keep the
in-process test issuer with its own JWKS; Keycloak would run beside PostgreSQL and the object
store as a service of `observability/compose.yaml`, the Compose project on the host that is
forwarded into the Dev Container, and as a workflow service in CI.

## Alternatives considered

### Signing in with OpenID Connect

- **OIDC replacing Basic.** One way in, but no access while the provider is down, and every
  script would need a token exchange. Rejected by the maintainer.
- **A server-side confidential client with a session cookie** (a backend for the frontend). The
  token would never reach JavaScript, but it brings a session store, CSRF defences and a cookie
  policy — what the login page with a session cookie below was rejected for — and a client secret in
  `server.toml`. Rejected.
- **A reverse proxy such as oauth2-proxy in front of the plane.** No code in the Server, but a
  second component to run and harden, and the audit would lose the person unless the Server
  trusted a header the proxy sets. Rejected.
- **Rights from the token's roles.** Levels are roles in the provider, read from a configurable
  claim (default `realm_access.roles`), and the Server stores no assignments and administers no
  users. `viewer` reads; `editor` also stores drafts, labels and Deployments, nothing that reaches
  an Agent; `releaser` also publishes, rolls out, revokes, restarts, approves enrolments, issues
  bootstrap certificates and reads the audit record. The levels are cumulative, so one role per
  person suffices, and a token carrying none of them is refused. The Non-Goal "Authorization and
  multi-tenancy" would narrow to "fine-grained authorization and multi-tenancy". It needs no store
  in the Server, but it fixes the rights model before it is decided and ties it to one provider's
  claim layout. Rejected; rights, once the specification has them, are the Server's.
- **`jsonwebtoken` with its `aws_lc_rs` or `rust_crypto` backend.** The first is banned; the second
  brings the `rsa` crate with an advisory that has no fix. Rejected in favour of a *ring* provider
  of our own.
- **`azp` standing in for `aud`.** It would admit a token issued to the UI's client for another
  resource, whose `aud` names that resource. RFC 9068 and RFC 8725 have a resource server accept
  only a token whose audience is its own; `azp` names the client a token was issued to, not the
  resource it is for. Rejected; the provider's audience mapper names `client_id` in `aud`.
- **Refusing every token whose `typ` is not `at+jwt`.** It is RFC 9068's own test, but not every
  provider issues that header by default: Auth0's default access token profile uses `JWT`.
  Rejected for the claims an ID token carries and an access token does not.
- **`biscuit`.** *ring*-native and enforcing the algorithm match itself, but a small community to
  depend on. Rejected.
- **`openidconnect`, `josekit`, `jwt-simple`, `aliri`.** ID tokens only, OpenSSL, BoringSSL, or
  OpenSSL for EC keys. Rejected.
- **No JWT crate, JWS verification with *ring* directly.** No new dependency, but the header and
  claim parsing and every claim check would be ours to write and keep correct. Rejected; the crate
  carries that part and the crypto stays ours.

### Transport and the two planes

- **One transport first.** WebSocket-only would put a deviation in `CONFORMANCE.md` for the
  simpler of the two transports and lock out HTTP-polling clients. HTTP-only caps the control loop
  at poll latency, and Gateway multiplexing is specified in WebSocket terms.
- **Separate endpoints or ports per transport.** The Baseline's default is one path, one port,
  per-request detection; splitting would be a deviation with no gain.
- **`native-tls`/OpenSSL.** System headers on Linux plus SChannel/SecureTransport variance on the
  Windows and macOS builds, against one pure-Rust stack identical everywhere.
- **rustls with the default `aws-lc-rs` provider.** It needs cmake, which the Dev Container
  deliberately lacks. Revisit if FIPS becomes a requirement.
- **A hand-rolled HTTP client on raw hyper instead of `reqwest`.** Polling with gzip, TLS and
  timeouts is reqwest's job; it is client-side only.
- **One listener, authenticated by path prefix.** It leaves both audiences on one exposure and one
  TLS policy, and the port, the only thing a firewall can act on, says nothing about who is
  talking.
- **Split strictly by path, everything under `/api/v1` on the Operator plane.** It puts the
  download on a port an Agent cannot derive from its endpoint, while the offered `download_url` is
  a path resolved against that endpoint.
- **Three listeners (OpAMP, downloads, Operator).** A moving part nothing needs.
- **A reverse proxy that splits the planes.** It makes a security property depend on an artifact
  this project does not ship or test, and a TLS-terminating proxy cannot give the Agent plane a
  handshake-level client-certificate requirement.
- **Operator plane on `0.0.0.0:4321` by default, or a merged single-port mode.** The first is the
  same exposure on a new port; the second is two operating modes, one of which is the one the
  split exists to avoid.
- **One certificate for both planes, with no per-listener certificates.** Every browser has to
  trust the fleet's server CA, and the UI cannot present the organization's own certificate.
- **`[rest.tls]` required whenever `[rest] listen` is beyond the loopback.** It would enforce the
  separation where browsers reach the UI, but an installation that serves the UI beyond the
  loopback would not start until the section is added. The section stays optional.
- **The UI certificate from `opamp-fleetctl`, signed by the fleet's server CA.** The keys would be
  separated, but browsers would still have to trust the fleet's server CA.
- **One certificate naming both audiences, from the organization's PKI.** The Clients would then
  have to trust the organization's PKI, which widens what a Client accepts as the Server to
  whatever that PKI signs.
- **A reverse proxy in front of the Operator plane that terminates TLS with the PKI's
  certificate.** It needs no change to the Server, but adds a component to run and harden. Unless
  the Server trusts a forwarded header, the audit and the throttling see the proxy's address in
  place of the operator's. An operator may still run one; clauses 9 to 13 make it unnecessary for
  the certificate alone.
- **Reloading the certificate files when they change.** It suits short-lived PKI certificates,
  but needs a file watch or a signal and a resolver that swaps the key at run time. A restart is
  enough; reloading stays open (see *Out of scope*).
- **A `tower-http` `TimeoutLayer`.** Middleware runs after hyper has parsed the request line and
  headers, so it cannot see the phase at issue, and it would need exclusions for the WebSocket,
  the download and the upload.
- **A deadline per body.** It refuses the large upload over a slow link, which is legitimate; any
  value long enough for it leaves the trickle in place.
- **An idle timeout between chunks.** It catches a body that stops, not one that sends a byte just
  inside the timeout, which is the cheaper attack.
- **The floor on the package upload alone.** The upload is the longest body, not the only one: the
  Agent plane's `POST`, which faces the estate, and the Operator plane's JSON routes can be
  trickled the same way.
- **A sliding window.** It bounds the gap between two bytes more tightly, at the price of keeping
  the arrival times of every chunk; consecutive windows bound the same attack to twice the window.
- **A `server.toml` and `supervisor.toml` key for the floor.** The value depends on what a link
  can carry, which is the same everywhere a program can be delivered at all.
- **Waiting for the axum release carrying the timer.** It fixes only `axum::serve`; the TLS
  listeners run on `axum_server` anyway.
- **Keeping `axum::serve` and hand-rolling the accept loop on `hyper-util`.** The same knob for
  the price of owning connection tracking and shutdown, which `axum_server` implements.
- **A per-plane `header_read_timeout_secs` key.** A key whose only sensible value is the framework
  default is a knob nobody turns.
- **The bound as a shared layer in `crates/opamp`.** It is one framework's builder knob in one
  binary; the Client's outbound transports have no counterpart
  ([ADR-0040](0040-the-operator-tool-takes-from-the-client-what-this-adr-names.md)).
- **The weaker posture: TLS 1.2 and 1.3, optional TLS, plaintext on any address, an Agent plane on
  `0.0.0.0:4320` and `ws://` as the Client's default.** A Server that starts with no configuration
  and a fleet that connects without certificates are convenient. Rejected: security comes before
  convenience, and Q-1 asks for a refusal at startup, not a working insecure default.
- **Plaintext allowed with a warning off LOOPBACK.** A warning is read after the credential has
  crossed the network. Q-1 asks for a refusal that names the setting.
- **`localhost` and private ranges as loopback.** A host name is whatever the resolver answers,
  and an RFC 1918 address leaves the host. Only the two literals cannot.
- **Plaintext on the Operator plane on LOOPBACK.** The loopback does not leave the host. Not
  chosen: `[tls]` is required anyway, and one rule for both planes leaves nothing to get wrong. A
  browser on the host reaches the plane by the Server's name over an SSH tunnel, or trusts a
  certificate that names `127.0.0.1`.
- **HTTP/2 bounds and connection caps as `server.toml` keys alike.** A cap depends on the size of
  the fleet, so an operator must be able to raise it. The HTTP/2 numbers depend on the protocol's
  use, which is the same in every deployment.
- **A rate-limit bucket per peer address.** Hosts behind one NAT, or every Agent behind a Gateway,
  would share one budget, and one busy neighbour would throttle the rest. The certificate names
  the host exactly, and admission has already proved it.
- **A bucket per `instance_uid` for every peer.** The uid is self-asserted (clause 41), so a peer
  that wants more messages names a fresh uid for each one, and the limit binds nothing.
- **Behind a Gateway, per-uid buckets alone.** A downstream peer that cycles uids gets a fresh,
  full bucket each time. `max_carried_agents` does not bound that, because each plain-HTTP
  exchange is a downstream connection of its own. The aggregate is what bounds the Gateway.
- **Behind a Gateway, one bucket for the host alone.** A whole site would share one host's budget,
  and one looping Agent behind the Gateway would throttle every other Agent it carries. The
  per-uid bucket inside the aggregate keeps that to the Agent that loops.
- **A bucket per connection.** A peer opens another connection, up to the cap. A poller has no
  connection that outlives one exchange, so there would be nothing to count it on.
- **`429` or `503` with `Retry-After` on `/v1/opamp` over plain HTTP.** The Baseline gives these
  to refusing a connection, and `Retry-After` to reconnect attempts. One protocol-level answer on
  both transports keeps an over-rate member apart from the failed-admission throttle. The download
  route has no protocol-level answer, so `429` is its answer (clause 26).
- **Leaving the download route uncounted.** The connection cap and the pace floor bound how many
  downloads are open and how slowly they run, but not how often a member asks. Each request scans
  the fleet under its lock, so a member asking in a loop holds that lock from every Agent.
- **Closing the WebSocket on the first message over the limit.** The Baseline leaves the
  disconnect to the Client, and closing gives the Agent no `retry_info`, only a reconnect at its
  backoff.
- **`retry_info` as the time until the next token.** That is at most 100 ms at the defaults, and
  a WebSocket Client reconnects at that pace. Every reconnect costs a TLS handshake and an
  admission entry, and it brings a full report from every Agent at once, which can run into the
  limit again. 30 s is the Baseline's recommended minimum and what every other `Unavailable` of
  this Server says.
- **A small `burst`, such as 50.** A host speaking for more Agents than `burst` could never get
  the full reports it sends after a reconnect through.
- **An enrolment connection counted by its bootstrap certificate's issuer and serial.** One
  bootstrap certificate may serve the whole fleet, so every enrolling host would share one bucket,
  and enrolment is already bounded by the window, the queue and the approval.
- **Refusing a `messages_per_sec` that cannot carry 256 Agents at the heartbeat.** A limit set too
  low costs availability, not security, and a fleet whose hosts carry few Agents may want it low.
  A warning names the keys.
- **Evicting a full bucket before any other.** Evicting a bucket that is not full only grants
  tokens, so there is nothing to protect.
- **Dropping the message silently.** The Agent would not learn that it should slow down, and on
  plain HTTP an exchange always has a reply.
- **`0` to switch the limit off.** A limit that one line disables is not a default an operator can
  rely on (Q-1). A fleet that needs more raises the numbers.
- **Probe a package source over `https://`, with `http://` on the loopback alone.** It sets rules
  for an outbound request the Server does not make: the Server sends no request for a package
  anywhere ([ADR-0039](0039-a-32-bit-arm-build-is-spelled-arm32.md), clause 4), and its outbound
  connections are those clause 21 names.
- **The first reconnect after a lost connection on the backoff's first step, one second with equal
  jitter, and no bound on the Server's new connections.** After a restart every Agent of a fleet
  would come back within the same half second, the floor of that jitter, and the Server would have
  to complete every handshake and admission at once. Rejected; clause 4 draws the first reconnect
  over thirty seconds and bounds the new connections a second.
- **One fixed drain of ten seconds in which every WebSocket session may end as it ends.** A stopped
  instance would send all its Agents to the others in the same moment. Rejected; clause 17 closes
  each session at an instant drawn over `drain_secs`.
- **The Server's outbound connections as a list in this ADR, its stores alone.** Every further
  purpose would have to change this ADR, although the ADR that decides the purpose already decides
  whether the Server dials for it. Rejected; clause 21 states the rule, and names the stores as
  the purposes the ADRs in force give it.

### Admission and revocation

- **Keep a fleet credential as a second required proof.** Rejected: it is one value
  for every host, so it identifies nobody, and it lies in clear beside the private key it is meant
  to back up. It costs a section on both ends, its rotation through offers, its revocation by hash
  and a Gateway pool partitioned by credential, and a fleet gains nothing from it that the
  certificate does not give.
- **Keep the credential optional, checked when configured.** Rejected: two admission paths to
  test and document, and every piece of machinery the credential needs stays in the tree for a
  proof that still identifies nobody.
- **Per-Agent or per-group credentials.** Rejected: identity management that brushes against the
  authorization and tenancy non-goal, beside a certificate that is already per host, short-lived
  and renewed without an operator.
- **A bootstrap token in a header instead of a bootstrap certificate.** Rejected: the token rides
  in HTTP, so the handshake would have to admit peers without a certificate. A listener has one
  client-certificate rule, so that means an optional certificate for the whole Agent plane or a
  third listener for enrolment. A token for the whole fleet has to be distributed exactly as the
  bootstrap certificate is.
- **A single-use enrolment token per host instead of the window and the approval.** Issuing the
  token would be the approval, and automation could fetch one per host. Not chosen: the handshake
  would again admit peers without a certificate, every host would need a secret of its own
  delivered to it, and a token intercepted on the way enrols a stranger unseen, where an approval
  shows the operator the key's fingerprint. It stays open as a follow-up.
- **Keep asking for the fleet credential at install, as optional, for a Server that still
  requires it.** Rejected: the Agent plane reads no credential, and the Server is upgraded first,
  so a question kept for older Servers would teach a setting nothing reads.
- **Remove a leftover `[auth]` section at install, or refuse the file there.** Rejected: `install`
  never rewrites a file that exists ([ADR-0020](0020-the-installed-service-and-its-state.md)), and
  refusing the section would stop a self-updated Client, whose file still has it, from connecting
  on a host nobody watches.
- **Refuse a leftover `[auth]` on the Client at startup**, as the Server does. Rejected: a Client
  updated by the Server starts with the old file and nobody at the host, so the whole fleet would
  stop at once. A section that is ignored weakens nothing.
- **Ignore a leftover `[auth]` on the Server too.** Rejected: an operator is present at a Server
  upgrade, and a section that silently does nothing would make that operator believe the fleet
  still needs the credential.
- **Optional proofs, required only by being configured** — an open default is convenient in a lab
  and admits anyone wherever an operator forgot a section; the specification puts security before
  convenience and asks for a refusal that names the setting.
- **Warning instead of refusing an exposed Operator plane** — a warning is read after the plane has
  been reachable; the specification asks for a refusal.
- **A bootstrap certificate from the client CA** — indistinguishable from an issued one at the
  handshake, so no approval can be asked of it and every leaked bootstrap pair is a member.
- **Approval without a window** — a pending queue that accepts requests at any time is a standing
  invitation; a window that closes by itself bounds both the queue and an operator's attention.
- **An enrolment queue whose requests outlive their window** — a request would stay pending after
  the window it was made in closed. The queue is persisted with the window's end
  ([ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)
  clause 8), and a closing window expires every request in it.
- **A download listener of its own instead of a certificate on the download** — a third listener
  serving artifacts to anyone who reaches it, where the Agent plane's handshake already proves the
  peer.
- **Rejecting a report whose `sequence_num` regresses** — an Agent restart legitimately resets it,
  and a forger can imitate a restart; it authorizes nothing.
- **Operator-provisioned certificates only, no CSR flow** — leaves every host's certificate to be
  created, delivered and renewed by hand, the errand the specification exists to remove. It
  survives as the first source of clause 36.
- **The Server hands out certificate and private key** — every Agent's key would exist on the
  Server, travel the wire and sit in its storage; one compromised Server would yield the fleet's
  identities.
- **Binding the certificate subject to `instance_uid`, or trust-on-first-use pinning of the two** —
  a re-key, a renewal or an Agent moving between Gateways would read as impersonation, and one
  Gateway certificate legitimately carries many UIDs.
- **Plaintext operator passwords accepted with a warning** — the secret stays in backups and diffs,
  and a warning is read after the file has travelled; the specification asks for a refusal.
- **SHA-256 for Basic passwords** — a person's password falls to a dictionary run against a leaked
  fast hash in minutes.
- **Operator passwords only by file reference, unhashed** — moves the secret out of `server.toml`
  but leaves it usable wherever that file travels; a hash is useless to whoever reads it.
- **Pluggable authenticators (OIDC, JWT, external IdP)** — a validator can be added behind the
  Operator plane's seam later. Clause 42 adds one there, OpenID Connect from the one issuer
  `[rest.auth.oidc]` names, and no other.
- **A login page with a session cookie on the Operator plane** — a session store, logout, cookie
  policy and CSRF protection inside a Server whose UI is an operational page; Basic gives the same
  browser experience for none of it.
- **Bearer only on the Operator plane** — a browser cannot send one without JavaScript holding a
  token, which drags the UI into session management. Clause 74 takes that on for OIDC beside
  Basic, not instead of it.
- **Leaving operator authentication to a reverse proxy** — a property depending on an artifact this
  project neither ships nor tests.
- **Stripping a mismatched `instance_uid` claim and signing anyway.** Rejected: it does not meet the
  Baseline's MUST, and a peer that made a false claim learns nothing about why its identity does
  not hold.
- **Recognising a claim only in one agreed field, such as a `urn:uuid:` SAN.** Rejected: the
  Baseline names no field, and a peer that put its `instance_uid` in the common name would pass
  unchecked.
- **Recognising any 32 hex digits as a claim.** Rejected: key fingerprints and hashes take that
  form, and a descriptive subject carrying one would be refused for nothing.
- **Leaving H3 undone.** Rejected: the MUST is conditional but unmet without it, and the cost is a
  parser over values the Server already reads.
- **A maximum session age instead of, or beside, targeted closing.** Rejected. A fixed age closes
  every session of the fleet on a clock, a reconnect storm's worth of handshakes for sessions whose
  proofs are fine, and still leaves a revoked one running until the age is reached. The
  certificate's own `not_after` is the bound that already exists and costs a healthy fleet nothing
  (clause 62).
- **Revocation by serial alone, without the renewal chain.** Rejected: a host that renewed shortly
  before the revocation keeps a valid certificate, and the operator has to revoke the successor
  they cannot see. The register costs one small entry per issued certificate, dropped on expiry.
- **Revocation by public key fingerprint.** Rejected: renewal changes the key, so it has the same
  hole as serial alone, and an operator reading a certificate with `openssl x509` sees the serial,
  not a key hash.
- **Naming the issuer by its distinguished name as text.** Rejected: RFC 4514 reverses the order
  of the parts and escapes commas, OpenSSL's default form does neither, and a parser's form is a
  third; a revocation typed in one would silently miss a certificate stored in another. The Server
  has two CAs at most, so their role names them without ambiguity.
- **Linking a renewal to the presented certificate only when the message names its owner.**
  Rejected: the `instance_uid` a message names is self-asserted, so one message naming another
  value would lift a renewal out of its chain. Linking always revokes too much behind a Gateway,
  never too little.
- **Dropping a register entry at its own expiry.** Rejected: the revocation of an expired
  certificate would go with it, and a renewal of that certificate would be admitted again.
- **Revocation by Agent, from the fleet view.** Rejected as the primitive: `instance_uid` is
  self-asserted and re-keyable, and behind a Gateway the Server sees only the Gateway's
  certificate. It may come later as a convenience on top of clause 56, using the register's
  `instance_uid`.
- **CRL or OCSP.** Rejected: both serve relying parties other than the issuer. Here the issuer is
  the only relying party, and an online responder would be infrastructure guarding nothing extra.
- **Distributing the list to Gateways through a custom message.** Rejected: a `CustomMessage` is
  `[Development]` in the Baseline and not implemented here, and it would tie the list to an Agent's
  session rather than to the Gateway as a host. A route of its own on the plane the Gateway
  already reaches carries it with the admission that plane already has.
- **The list for every member, not only marked Gateways.** Rejected: only a Gateway acts on it, and
  every member that could fetch it would learn which certificates of the fleet were withdrawn.
- **Ending a session by a re-authentication the Server pushes.** Rejected: the Baseline has no
  message for it, and the specification forbids a private side channel.
- **Refusing to start while a credential entry is on the persisted list.** Rejected: the entry
  names nothing the Server still accepts, so it guards nothing, and a Server that will not start
  after an upgrade would shut out the whole fleet for no gain in security.
- **A Gateway that trusts the fleet's client CA alone, and enrolment only directly against the
  Server.** A host that only a Gateway reaches could then join only with a client certificate an
  operator provisions, while G-15 asks that it enrol through the Gateway. A Gateway that holds the
  bootstrap CA's certificate admits an enrolling peer and relays its one CSR, and the Server decides
  that enrolment as any other ([ADR-0016](0016-client-modes-and-the-gateway.md), clause 24; clauses
  40 and 49).
- **A revocation that reaches the sessions of one Server's process alone, and a revocation list
  whose `ETag` is that Server's own.** It holds while one Server holds every session. Where several
  instances serve one fleet
  ([ADR-0025](0025-several-server-instances-share-one-postgresql-database-and-a-group-is-placed-again-at-once.md)),
  a session another instance holds would outlive the revocation, and a Gateway that fetches the
  list from another instance would download it again for an unchanged list. Rejected; clauses 61
  and 63 reach every instance and give the list one version in the database.
- **The CA keys as key files alone.** `[client_ca] key_file` and `[enrolment]
  bootstrap_ca_key_file`, PKCS#8 PEM read unattended, are the simplest form and need no library
  beside the Server. It holds for one Server whose host and backups nobody else reads. But whoever
  reads the file once can mint members unseen until the CA is replaced, and several instances
  would each hold a copy. Rejected as the only form; a key file stays one of the two (clauses 37,
  46 and 65).
- **PKCS#11 required for every installation.** One signing path to test, but every single-node
  Server would install and run at least SoftHSM2 for a key that sits on the same host either way.
  Rejected; it stays an option.
- **PKCS#11 required with the PostgreSQL store.** One Server on PostgreSQL keeps its key on one
  host as a Server on files does; the database does not make a key file more exposed. What needs
  shared keys is several instances, and clause 2 of
  [ADR-0025](0025-several-server-instances-share-one-postgresql-database-and-a-group-is-placed-again-at-once.md)
  makes an instance with a CA key file the only one. Rejected.
- **Encrypted key files with a passphrase from a file.** It protects a backup, but the passphrase
  sits on the same host as the file, and every instance holds both. Rejected.
- **A separate signing service from the start.** It could check each justification against the
  database itself and survive a compromised Server, but it is a second process to deploy and
  secure, with its own transport and admission. The signer port keeps it possible. Rejected, as in
  the working note.
- **A cloud KMS API instead of PKCS#11.** It binds the Server to one provider's API and SDK.
  PKCS#11 is what HSMs, cloud HSM services and SoftHSM2 all speak. Rejected.
- **Import the existing key files into the token.** It keeps every issued certificate valid, but
  the key has existed outside the token and may exist there still. Rejected; a new CA with a
  transition costs one renewal cycle (clause 69).
- **A hardware HSM from the start.** It protects the key in hardware, but every developer and CI
  run would need one, and the interface is the same as SoftHSM2's. Rejected for the first step.
- **SoftHSM2 refused in production.** It would force hardware on every installation that wants a
  token, including those whose operator accepts a software token for its other properties — keys
  that the Server cannot read and that no file copy carries. Rejected; the warning and the audit
  entry make the choice visible.
- **`cryptoki`'s alternatives** (`pkcs11`, `rust-pkcs11`). Older wrappers, unmaintained or with
  raw bindings only. Rejected for `cryptoki`.

## Sources / Prior art

### Signing in with OpenID Connect

- The working note of 2026-10-15 on the Server, section C, its analysis points and the decisions
  of 2026-10-16, recorded outside the ADRs.
- [OpenID Connect Core 1.0](https://openid.net/specs/openid-connect-core-1_0.html) and
  [Discovery 1.0](https://openid.net/specs/openid-connect-discovery-1_0.html).
- [RFC 7636](https://www.rfc-editor.org/rfc/rfc7636) (PKCE),
  [RFC 8725](https://www.rfc-editor.org/rfc/rfc8725) (JWT best current practices: algorithm
  allow-lists, key-family checks), [RFC 9068](https://www.rfc-editor.org/rfc/rfc9068) (`at+jwt`
  access tokens, and its `typ` check that keeps an ID token from passing as one), OpenID Connect
  Core 1.0 section 2 (`nonce`, `at_hash` and `c_hash` as ID token claims), Auth0's
  [access token profiles](https://auth0.com/docs/secure/tokens/access-tokens/access-token-profiles)
  (`at+jwt` only in its RFC 9068 profile), and the IETF draft "OAuth 2.0 for Browser-Based
  Applications" on public clients and token storage.
- [Fetch Metadata Request Headers](https://www.w3.org/TR/fetch-metadata/) (W3C):
  `Sec-Fetch-Mode`, set by the browser and not by a page's script.
- `jsonwebtoken` 11.1.0 ([docs.rs](https://docs.rs/jsonwebtoken/11.1.0/jsonwebtoken/)), MIT: no
  backend in its default features, `CryptoProvider` with `verifier_factory` and `install_default`
  in `src/crypto/mod.rs`, the `JwtVerifier` trait.
- [Keycloak: audience support](https://www.keycloak.org/docs/latest/server_admin/#audience-support):
  an access token names `account` as audience unless an audience mapper adds the client; the setup
  guide must say so.
- The code as it stands: `authenticate` and `record_act` in `crates/fleet-server/src/api.rs`, which
  read the Basic user name from the header; the plane's security headers in the same file.

### Transport and the two planes

- The OpAMP transports:
  [OpAMP specification — Transport, Plain HTTP Transport, WebSocket Transport](https://github.com/open-telemetry/opamp-spec/blob/v0.18.0/specification.md):
  the dual-transport SHOULD, `Content-Type: application/x-protobuf` and its use for detection, the
  gzip MUST, the 30 s default poll, port 4320 and `/v1/opamp`, varint framing, backoff with
  jitter; and the 64 MiB message-size recommendation recorded in
  [`CONFORMANCE.md`](../CONFORMANCE.md).
- [`opamp-go`](https://github.com/open-telemetry/opamp-go): both transports behind one client
  interface (`NewWebSocket`/`NewHTTP`); its `internal/examples/server` runs the OpAMP endpoint and
  the demo UI as two separate servers; its
  [`server/serverimpl.go`](https://github.com/open-telemetry/opamp-go/blob/main/server/serverimpl.go)
  sets no timeouts, so the connection bound goes beyond the oracle without diverging from it.
- [`tokio-tungstenite`](https://crates.io/crates/tokio-tungstenite),
  [`reqwest`](https://crates.io/crates/reqwest),
  [`axum-server`](https://crates.io/crates/axum-server),
  [`rustls`](https://crates.io/crates/rustls): the stack; the `ring` provider avoids cmake and
  aws-lc builds.
- The rustls provider:
  [`rustls::crypto::CryptoProvider`](https://docs.rs/rustls/0.23/rustls/crypto/struct.CryptoProvider.html)
  and the `tls12` crate feature: a provider without a TLS 1.2 suite cannot negotiate TLS 1.2;
  [`reqwest::ClientBuilder::min_tls_version`](https://docs.rs/reqwest/0.13/reqwest/struct.ClientBuilder.html#method.min_tls_version).
- rustls 0.23.46, `CertifiedKey::from_der` in `src/crypto/signer.rs`: loading a certificate and
  key compares the key's SubjectPublicKeyInfo with the certificate's and refuses a mismatch, which
  makes clause 11's mismatch a startup failure. Read in the crate's source.
- [RFC 8446](https://www.rfc-editor.org/rfc/rfc8446): TLS 1.3 and its five suites, three of which
  the `ring` provider implements; [RFC 8996](https://www.rfc-editor.org/rfc/rfc8996) deprecates
  TLS 1.0 and 1.1.
- The HTTP/2 bounds:
  [`hyper::server::conn::http2::Builder`](https://docs.rs/hyper/1/hyper/server/conn/http2/struct.Builder.html):
  `max_concurrent_streams`, `keep_alive_interval` and `keep_alive_timeout`, the last two needing
  the same `Timer` as the HTTP/1 bound.
- Several listeners:
  [HashiCorp Vault — TCP listener configuration](https://developer.hashicorp.com/vault/docs/configuration/listener/tcp):
  several listeners, each with its own client-certificate policy.
- [etcd — transport security model](https://etcd.io/docs/v3.6/op-guide/security/) and
  [configuration flags](https://etcd.io/docs/v3.4/op-guide/configuration/): client and peer
  listeners separated by audience, trust material following the audience.
- Loopback control surfaces:
  [Kubernetes — securing control-plane components](https://kubernetes.io/docs/tasks/administer-cluster/configure-upgrade-etcd/)
  and
  [Kubernetes API security fundamentals (Datadog Security
  Labs)](https://securitylabs.datadoghq.com/articles/kubernetes-security-fundamentals-part-2/):
  unauthenticated control surfaces (`kube-controller-manager`, `kube-scheduler`) bind loopback.
- One port elsewhere:
  [Bindplane — networking requirements](https://docs.bindplane.com/production-checklist/bindplane/networking-requirements):
  REST and OpAMP on one port, defensible there because that REST API is authenticated.
- [axum PR #3478](https://github.com/tokio-rs/axum/pull/3478), the
  [axum changelog](https://github.com/tokio-rs/axum/blob/main/axum/CHANGELOG.md) and
  [axum issue #2741](https://github.com/tokio-rs/axum/issues/2741): upstream installs the timer
  and applies hyper's 30 s default; an incomplete request costs about 1 MB per connection against
  about 2.5 kB for an idle one.
- The HTTP/1 header bound:
  [`hyper::server::conn::http1::Builder::header_read_timeout`](https://docs.rs/hyper/1.11.0/hyper/server/conn/http1/struct.Builder.html#method.header_read_timeout):
  requires a `Timer`, panics if configured without one; `axum-server` 0.7 `http_builder` and its
  10-second rustls handshake default, read from the vendored sources.
- Header timeouts elsewhere:
  [Diving into Go's HTTP server timeouts](https://adam-p.ca/blog/2022/01/golang-http-server-timeouts/)
  and nginx's `client_header_timeout` (60 s): the range 30 s sits in.
- [`cargo-deny` — `bans.features`](https://embarkstudios.github.io/cargo-deny/checks/bans/cfg.html):
  denies a crate feature anywhere in the graph.
- A body timeout:
  [nginx `client_body_timeout`](https://nginx.org/en/docs/http/ngx_http_core_module.html#client_body_timeout):
  a timeout between two successive reads of the body, not for the whole body; the same shape
  without a floor on the amount.
- [Apache `mod_reqtimeout`](https://httpd.apache.org/docs/2.4/mod/mod_reqtimeout.html):
  `body=20,MinRate=500`, a minimum data rate for the body, the measure clause 18 takes.
- [`HARDENING.md`](../HARDENING.md), *Where each bound applies today*: the handshake deadline,
  the header bound, the connection cap and the HTTP/2 bounds per listener, the Client's own
  listeners among them.
- The rate limit's protocol:
  [OpAMP specification v0.20.0](https://github.com/open-telemetry/opamp-spec/blob/v0.20.0/specification.md),
  for the rate limit:
  - *Throttling*, its WebSocket and Plain HTTP Transport subsections, with the `RetryInfo` message
    and the *"minimum recommended retry interval"* of 30 seconds;
  - `ServerErrorResponse.type`
    (`UNAVAILABLE: The Server is overloaded and unable to process the request`);
  - `ServerErrorResponse.retry_info`;
  - *Retrying Messages*.
- The vendored Baseline schema,
  [`opamp.proto`](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto): `ServerErrorResponse`,
  `ServerErrorResponseType`, `RetryInfo`.
- [RFC 6585 section 4](https://datatracker.ietf.org/doc/html/rfc6585#section-4) (`429`).
- The code the rate limit builds on:
  - [`transport.rs`](../../crates/fleet-server/src/transport.rs): `admit`, `admit_download`,
    `Proofs` and `Fleet::enrol`;
  - [`throttle.rs`](../../crates/fleet-server/src/throttle.rs): the per-address throttle and its
    bounded table;
  - [`fleet.rs`](../../crates/fleet-server/src/fleet.rs): `unavailable`, `RETRY_AFTER` and the
    `sequence_num` gap;
  - [`audit_log.rs`](../../crates/fleet-server/src/audit_log.rs): the `unrecorded_before` count;
  - [`server.rs`](../../crates/opamp/src/server.rs): the `Handler` dispatch;
  - the Client's handling of `Unavailable` in
    [`protocol.rs`](../../crates/opamp/src/client/protocol.rs),
    [`ws.rs`](../../crates/opamp/src/client/ws.rs) and
    [`http.rs`](../../crates/opamp/src/client/http.rs);
  - its routing by `instance_uid` in [`engine.rs`](../../crates/fleet-agent/src/engine.rs);
  - the Gateway's per-exchange `Downstream` in
    [`gateway/mod.rs`](../../crates/fleet-agent/src/gateway/mod.rs) and its downward routing in
    [`pool.rs`](../../crates/fleet-agent/src/gateway/pool.rs).
- [`CONFORMANCE.md`](../CONFORMANCE.md), the *Retrying, throttling, bad request* row.
- [`docs/manual/setup.md`](../manual/setup.md): the server CA in the browser's trust store, which
  `[rest.tls]` makes optional.

### Admission and revocation

- [OpAMP specification](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md) —
  authentication MAY, `401` MUST; the client-certificate CSR flow, its `BadRequest` MUST, its
  restriction to the OpAMP connection, the bootstrap certificate, and the "header-based
  authorization … and optionally also client-side certificates" layering; `instance_uid` chosen by
  the Agent and assignable by the Server; throttling with `retry_info` and `Retry-After`.
- The vendored Baseline schema: `TLSCertificate`, `ConnectionSettingsRequest`.
- [`opamp-go`
  `StartSettings`](https://github.com/open-telemetry/opamp-go/blob/main/client/types/startsettings.go)
  — static `Header` and per-request `HeaderFunc`.
-
  [`opampextension`](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/extension/opampextension),
  [`basicauthextension`](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/extension/basicauthextension),
  [`bearertokenauthextension`](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/extension/bearertokenauthextension)
  — the ecosystem's static Basic and Bearer building blocks.
- [RFC 7617](https://datatracker.ietf.org/doc/html/rfc7617) (Basic),
  [RFC 9110 section 11](https://datatracker.ietf.org/doc/html/rfc9110#name-http-authentication)
  (`401`, `WWW-Authenticate`, `realm`),
  [RFC 6585 section 4](https://datatracker.ietf.org/doc/html/rfc6585#section-4) (`429`) and
  [RFC 9110 section 10.2.3](https://datatracker.ietf.org/doc/html/rfc9110#name-retry-after)
  (`Retry-After`).
- [`constant_time_eq`](https://crates.io/crates/constant_time_eq).
- [OWASP Password Storage Cheat
  Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Password_Storage_Cheat_Sheet.html),
  *Argon2id*: "m=19456 (19 MiB), t=2, p=1" as the minimum configuration; [RFC
  9106](https://www.rfc-editor.org/rfc/rfc9106) (Argon2); the RustCrypto
  [`argon2`](https://crates.io/crates/argon2) crate and the [PHC string
  format](https://github.com/P-H-C/phc-string-format/blob/master/phc-sf-spec.md).
- [rcgen](https://docs.rs/rcgen/latest/rcgen/struct.CertificateSigningRequestParams.html) and its
  [feature flags](https://lib.rs/crates/rcgen/features); [`ring`'s
  build](https://docs.rs/crate/ring/latest);
  [`reqwest::tls::Identity`](https://docs.rs/reqwest/latest/reqwest/tls/struct.Identity.html)
  (`from_pem` under rustls takes key and certificate in one buffer);
  [`RustlsConfig::from_config`](https://docs.rs/axum-server/latest/axum_server/tls_rustls/struct.RustlsConfig.html)
  and the [`axum-server-mtls`](https://lib.rs/crates/axum-server-mtls) pattern for carrying the peer
  certificate into request extensions; rustls'
  [`WebPkiClientVerifier`](https://docs.rs/rustls/latest/rustls/server/struct.WebPkiClientVerifier.html)
  without `allow_unauthenticated`.
- [Kubernetes
  `certificates.k8s.io`](https://kubernetes.io/docs/reference/access-authn-authz/certificate-signing-requests/)
  and [`client-go` certificate
  rotation](https://github.com/kubernetes/client-go/blob/master/util/certificate/csr/csr.go) — local
  key, submitted CSR, bootstrap identity distinct from the issued one, renewal before expiry, and a
  pending request approved or denied by an operator.
- Shared-CA service meshes — a client CA proves membership, not per-caller identity, unless an
  identity document (SPIFFE SVID) is layered on.
- [Prometheus web configuration](https://prometheus.io/docs/prometheus/latest/configuration/https/),
  [Alertmanager](https://prometheus.io/docs/alerting/latest/configuration/), [Traefik's
  dashboard](https://doc.traefik.io/traefik/operations/dashboard/) — Basic in front of an
  operational UI;
  [Grafana](https://grafana.com/docs/grafana/latest/setup-grafana/configure-security/) and
  [BindPlane](https://docs.bindplane.com/) — session logins where the UI is the product.
- OpAMP specification v0.20.0, *Using instance_uid in the CSR*
  ([opamp-spec](https://github.com/open-telemetry/opamp-spec/blob/v0.20.0/specification.md)).
- [RFC 9562 section 4](https://www.rfc-editor.org/rfc/rfc9562#section-4): the UUID string
  representation and the `urn:uuid:` namespace.
- [`HARDENING.md`](../HARDENING.md), *What already holds*: the entries on the certificate in the
  handshake and enrolment by approval, on a CSR's claim to an `instance_uid`, on the credential in
  `server.toml` and the Basic password's hash, on revocation, and on the audit record.
- OpAMP specification v0.20.0, *Revoking Access*: *"the Server can revoke access to individual
  Agents by marking the corresponding connection settings as 'revoked' and disconnecting the
  Client."*
  ([opamp-spec](https://github.com/open-telemetry/opamp-spec/blob/v0.20.0/specification.md))
- [RFC 5280 section 4.1.2.2](https://www.rfc-editor.org/rfc/rfc5280#section-4.1.2.2): a serial is a
  positive integer of at most 20 octets, unique per issuer.
- [RFC 6455 section 7.4.1](https://www.rfc-editor.org/rfc/rfc6455#section-7.4.1): close code
  `1008`, *"an endpoint is terminating the connection because it has received a message that
  violates its policy"*.
- Kubernetes has no client-certificate revocation and relies on short lifetimes and rotation
  ([kubernetes#46287](https://github.com/kubernetes/kubernetes/issues/46287), [kubelet certificate
  rotation](https://kubernetes.io/docs/tasks/tls/certificate-rotation/)) — the position of admission
  without a revocation list, and the gap this closes.
- The working note "server split", sections D3 and D9, recorded outside the ADRs.
- [`cryptoki`](https://docs.rs/cryptoki/0.12.1/cryptoki/) 0.12.1
  ([repository](https://github.com/parallaxsecond/rust-cryptoki)), Apache-2.0: `Pkcs11::new` loads
  the module at runtime through `libloading` from pre-generated bindings, so no `bindgen` and no
  system header enter the build; `Mechanism::EccKeyPairGen`, `Mechanism::Ecdsa` and
  `Mechanism::EcdsaSha256`; `Info::manufacturer_id` from `C_GetInfo`.
- `rcgen` 0.14.10 (the version in `Cargo.lock`): the `SigningKey` and `PublicKeyData` traits in
  `src/key_pair.rs`, and `Issuer::new(params, signing_key)` for any `SigningKey`.
- [PKCS #11 Specification Version
  3.1](https://docs.oasis-open.org/pkcs11/pkcs11-spec/v3.1/pkcs11-spec-v3.1.html)
  (OASIS): `CKA_EXTRACTABLE`, `CKA_SENSITIVE`, `CKM_ECDSA` and its `r || s` signature format.
- [SoftHSM2](https://github.com/softhsm/SoftHSMv2), which reports `SoftHSM` as its manufacturer.
- RFC 5480 and RFC 5758: the DER `ECDSA-Sig-Value` an X.509 signature carries.
- The code as it stands: `CertificateSigner` in `crates/fleet-server/src/fleet.rs`, the local CA
  in `crates/fleet-server/src/ca.rs`, bootstrap issuance in `crates/fleet-server/src/bootstrap.rs`,
  key generation in `crates/fleet-tools/src/pki.rs`.

## Consequences

### Signing in with OpenID Connect

- Positive: operators sign in with the account their company already manages, and leaving the
  company ends their access without touching `server.toml`.
- Positive: the audit names a person by a key the provider vouches for, which a later rights model
  in the Server can attach to.
- Positive: Basic keeps scripts and the incident path working when the provider is down.
- Negative / trade-offs: a new dependency, and a crypto provider of our own to keep correct; the
  negative tests of clause 71 carry that.
- Negative / trade-offs: a token held in the page is reachable by script injected into it. The
  Content Security Policy, which allows no inline script but hashed ones and no foreign origin but
  the issuer's for `connect-src`, is what stands against that.
- Negative / trade-offs: the UI's shell, the API document and its viewer become readable without a
  credential when OIDC is set. They hold no fleet data, and the API's shape is public in the source.
- Negative / trade-offs: the provider's client must be set up correctly — public client, PKCE,
  redirect URI, audience mapper — and a wrong setup shows as `401` until it is fixed.
- Negative / trade-offs: one ADR of 75 clauses replaces two, because the guard and the outbound
  connection it needs change together; the clause map in the Context says where each old clause
  went.
- Follow-ups: rights defined in the Server and keyed by `iss` and `sub`, with the specification
  change that narrows its Non-Goal; signing in `opamp-fleetctl` through a device flow; the glossary
  terms identity provider and bearer token.

### Transport and the two planes

- Positive: full transport conformance on both ends; any third-party client or server pairs with
  this project regardless of its transport choice; one TLS stack, identical on Linux, macOS and
  Windows, and the client cross-builds need no system TLS library.
- Positive: no credential, configuration or package crosses the network unencrypted, and nothing
  older than TLS 1.3 is negotiated. A configuration that would break this does not start, and its
  message names the setting to fix.
- Positive: the default publishes neither plane on the network; authenticating the Operator plane
  is a decision about one listener, with no per-path exemption for Agent traffic.
- Positive: browsers trust the UI through the organization's PKI, with no fleet CA to import, when
  `[rest.tls]` names its certificate. The key every Client trusts is then not the one that the
  people who run the UI's certificate handle and renew. A `server.toml` without the section
  starts as it is.
- Positive: an unauthenticated peer cannot pin a connection open indefinitely on either plane,
  cannot hold more connections than the cap, and cannot hold an HTTP/2 connection with silent or
  unbounded streams. Both planes drain on shutdown within a bound.
- Positive: no peer holds a connection by stalling or trickling a body or a message, on any
  listener, while a large upload over a slow link and an idle session are left alone.
- Positive: one host, one Agent behind a Gateway, or one Gateway as a whole cannot take the
  Server's time from the rest of the fleet, on `/v1/opamp` or on the download route. What a
  looping or compromised member costs is bounded by numbers in `server.toml` (Q-2, Strategy
  *Security before convenience*).
- Positive: the rate limit's answer is the protocol's own on both transports, and the Client of
  this project honours it because the reply names its Agent. A throttled message loses nothing,
  because `ReportFullState` recovers the Agent's state at its next processed message.
- Positive: the limit follows the identity admission proved. Hosts behind one NAT do not share a
  budget, and a site behind a Gateway is not throttled as one host.
- Positive: every `Unavailable` reply of this Server reaches its Agent, directly and behind a
  Gateway, so the Client waits as it is told.
- Positive: the Server emits throttling itself, so the *Retrying, throttling, bad request* row and
  the Deviations table of `CONFORMANCE.md` follow the implementation.
- Negative / trade-offs: a Server with no configuration does not start. The operator provides a
  certificate and key before the first start, and sets `listen` before an Agent from another host
  can connect.
- Negative / trade-offs: a third-party client or server that speaks only TLS 1.2 cannot pair with
  this project, and a Client whose configuration names a `ws://` or `http://` endpoint off
  LOOPBACK, or a host name such as `localhost`, stops at startup until the endpoint is changed.
- Negative / trade-offs: two transports mean two code paths and reconnect behaviour to test on
  each end; `reqwest` is a sizeable dependency for a poll loop.
- Negative / trade-offs: every operator entry point is on port 4321, and driving the API from
  another host takes a deliberate `[rest] listen` with TLS and authentication.
- Negative / trade-offs: an operator who uses `[rest.tls]` renews two certificates on two
  schedules; the expiry warnings name both. A PKI certificate rarely names `127.0.0.1`, so an
  operator who reaches a loopback Operator plane through an SSH tunnel dials the certificate's host
  name, for example through an `/etc/hosts` entry; the manual says so.
  `opamp-fleetctl package fetch --cacert` names the CA that the Operator plane's certificate
  chains to, which with a PKI certificate is the PKI's CA, or nothing when the system's roots hold
  it.
- Negative / trade-offs: a body or a message over a link slower than about a kilobyte a second is
  refused, and a peer that holds the floor exactly still holds its connection for as long as its
  body lasts (about eleven days for a 1 GiB upload), bounded by the connection cap alone.
- Negative / trade-offs: a peer needing more than 30 s for its headers is hung up on. A fleet
  reconnecting after a restart meets the connection cap if it is larger than the cap, and the
  HTTP/2 numbers are chosen, not yet measured against a real fleet.
- Negative / trade-offs: a throttled WebSocket Client disconnects and returns after 30 s, as the
  Baseline asks, so throttling costs a host up to 30 s of reports from every Agent on that
  connection. On its return a `burst` of 300 lets one full report from each Agent through.
- Negative / trade-offs: the rate-limit defaults leave little headroom: 256 Agents at a 30 s
  heartbeat send about 8.5 messages a second against 10. A shorter offered heartbeat on a host
  near 256 Agents needs a higher `messages_per_sec`, and the startup warning says so.
- Negative / trade-offs: a downstream peer behind a Gateway that names a neighbour's
  `instance_uid` drains that Agent's bucket, and the neighbour is throttled. This is no worse than
  what [ADR-0016](0016-client-modes-and-the-gateway.md) accepts behind a Gateway, where such a peer
  takes over the neighbour's route. A peer that cycles uids is bounded by the Gateway's aggregate,
  which every Agent behind that Gateway shares.
- Negative / trade-offs: an over-limit download fails that download attempt on the Client, which
  does not honour `Retry-After` there.
- Negative / trade-offs: a throttled `agent_disconnect` is not processed. Over a WebSocket the
  record is marked disconnected when the socket closes. Over plain HTTP it stays connected until
  it goes stale.
- Negative / trade-offs: the Baseline describes `UNAVAILABLE` as an overloaded Server, and here it
  answers one host or Gateway over its rate while the Server has time to spare. The Agent sees no
  difference, and the protocol has no other retryable answer.
- Negative / trade-offs: an aggregate audit entry names the peer address, not the host, because
  ADR-0028 aggregates by address. Behind a NAT or a Gateway only the first ten entries of each
  second name the host or the Agent.
- Follow-ups:
  - a connection cap on the Client's Gateway endpoint;
  - measuring the HTTP/2 bounds against a real fleet;
  - reducing clauses 15 and 16 to the framework's defaults once axum installs the timer;
  - rate limits weighted by message kind;
  - the Client's downloader honouring `Retry-After`;
  - the running Server's daily expiry check reads the file, not the certificate it serves, so a
    file replaced without a restart silences the warning while the old certificate is still
    served; this holds for `[tls]` and `[rest.tls]` alike, and is a fix within ADR-0036's expiry
    rule;
  - whether to reload certificates without a restart;
  - whether operators should present client certificates on the Operator plane.

### Admission and revocation

- Positive: G-17 holds for every configuration — encrypted channel, a peer proved in the
  handshake by a certificate this project issues and renews, no configuration that admits a peer
  without one.
- Positive: a configuration that would weaken admission fails loudly at startup, naming the
  setting, never silently as an open endpoint.
- Positive: the trust model is explicit — one fleet is one trust domain — and the Operator plane
  can be published to a network only behind TLS and a password.
- Positive: a copy of `server.toml` admits no host and signs in no operator.
- Positive: a peer without a certificate dies in the handshake, before any handler parses a byte
  of its request; the route check is a second line rather than the only one.
- Positive: no secret for the Agent plane exists anywhere. Nothing in `supervisor.toml` admits a
  host, and the Client holds no credential in clear; its only secret is the private key, which
  never leaves it.
- Positive: no machinery for a fleet credential exists — no Agent plane `[auth]`, no
  `hash-credential --bearer`, no credential in connection-settings offers
  ([ADR-0013](0013-connection-settings-that-never-lead-to-plaintext.md)), no revocation by hash
  (clause 58), no audit events for one
  ([ADR-0028](0028-the-audit-record.md)),
  and no Gateway pool partitioned by credential ([ADR-0016](0016-client-modes-and-the-gateway.md)).
- Positive: a compromised host reports only for its own Agents and holds at most three valid
  certificates; it cannot take over another host's Agent record or its renewals.
- Positive: the Baseline's conditional MUST on `instance_uid` in the CSR is met; a peer cannot ask
  for a certificate in another Agent's name; nothing changes for this project's own Client.
- Positive: an operator can eject a host within seconds, not within a certificate's lifetime; a
  stolen bootstrap certificate can be withdrawn; a session cannot outlive its certificate.
  Validity can be shortened on evidence rather than as the only defence. The list holds one kind
  of entry, and no revocation shuts out the whole fleet at once.
- Negative / trade-offs: one proof. A stolen certificate and key admit until the certificate is
  revoked or expires.
- Negative / trade-offs: behind a Gateway the Server relies on the Gateway's handshake for the
  peers it carries. A compromised Gateway, or a compromised peer behind one, can poison the record
  of any Agent behind a Gateway. Accepted within one fleet; it is not a cross-fleet or
  unauthenticated exposure.
- Negative / trade-offs: the Server is upgraded before its Clients. A Server refuses to start
  while `server.toml` keeps `[auth]` or a credential key in `[connection_offer]`
  ([ADR-0013](0013-connection-settings-that-never-lead-to-plaintext.md)), and the manual says to
  delete them.
- Negative / trade-offs: an operator marks each Gateway by hand; until then the first Agents it
  carries bind to the Gateway's host, and the mark is the remedy.
- Negative / trade-offs: the Server is a CA, and its signing key can mint fleet members.
- Negative / trade-offs: no lab runs without setup. Every Server needs TLS material and a client
  CA, and every Client a certificate.
- Negative / trade-offs: an expired certificate locks a host out. A Client switched off longer than
  its validity comes back unable to connect; recovery is the operator's — delete its stored pair so
  it presents a bootstrap certificate, open a window, approve its request — and is written in the
  manual.
- Negative / trade-offs: a bootstrap certificate still has to be distributed, mitigated by it
  opening nothing without a window and an approval.
- Negative / trade-offs: every first enrolment needs an operator at the API while a window is
  open; a request nobody approves expires with the window.
- Negative / trade-offs: throttling counts by IP address, so hosts behind one NAT or one Gateway
  share a count, and one misconfigured host can put its neighbours into back-off.
- Negative / trade-offs: a Gateway whose client CA names the bootstrap CA would turn enrolment
  through it into automatic issuance; the Server cannot see this, and the rule lives in the
  Gateway's configuration and the manual.
- Negative / trade-offs: browsers cache Basic credentials and offer no clean logout.
- Negative / trade-offs: a descriptive subject that happens to contain a canonical UUID other than
  the sender's is refused. An operator who names hosts after UUIDs must name them after the
  Agent's own.
- Negative / trade-offs: the Server keeps two persisted stores and a check per admission; the
  register grows with the fleet times its renewals within one validity period. Behind a Gateway a
  revocation takes effect when the Gateway next fetches the list, and a Gateway needs its host
  marked before it admits anyone; the list is the only way the Server's revocation of a downstream
  certificate reaches that Agent. The register's bounds turn an admitted member that loops CSRs
  into a refusal of renewals within its chain, and a host holds at most three valid certificates.
  A downstream Agent that renews with a proof renews in its own chain and host; one without a
  proof renews in the Gateway's, and can stop renewal for the Gateway and every Agent it carries
  until the certificates it obtained expire. The bounds keep the damage to one chain and away from
  enrolment. An operator tool that posts `{"credential": …}` is answered `400` and has to be
  changed.
- Positive: where the token is used, Q-1 rests on a key no one can copy. A stolen backup, disk
  image or database dump holds no CA key.
- Positive: the Server is written against PKCS#11 once; SoftHSM2 now and a hardware HSM later
  differ in configuration only.
- Negative / trade-offs: an installation that uses the token needs SoftHSM2 or an HSM, a PIN file,
  and a CA move at its first switch, which costs one renewal cycle with two trusted CAs.
- Negative / trade-offs: a C library loaded at runtime, the PKCS#11 module, runs inside the
  Server's process, and `cryptoki` is a dependency for `cargo-deny`.
- Negative / trade-offs: every signature waits for the token; a slow or unreachable HSM delays or
  refuses renewals and enrolments, and the Server reports it as it reports any signing error.
- Negative / trade-offs: a compromised Server can still have the token sign while it runs; the
  audit entries of clause 67 and the export of [ADR-0028](0028-the-audit-record.md) clause 15 are
  what show it afterwards.
- Follow-ups: enrolment by a per-host token; a strict per-Agent identity mode; an external-CA
  backend; running the `instance_uid` claim check against opamp-go in the interoperability suite.
- Follow-ups: revocation by Agent as a convenience in the bundled UI; shortening certificate
  validity once renewal is proven; bounding issuance per Agent rather than per chain, which needs a
  per-Agent identity this decision leaves out.
- Follow-ups: a separate signing service behind the signer port; a glossary entry for the HSM and
  the token.

## Enforcement

### Signing in with OpenID Connect

Tests of signing in with OpenID Connect (clauses 42, 45 and 71 to 75), which will carry
`Verifies:` for this ADR, against an in-process test issuer with its own keys, discovery document
and JWKS endpoint:

- A valid token on `/api/v1/agents` is admitted; one with a wrong `iss`, an `aud` naming another
  audience while `azp` names `client_id`, an `aud` naming the canonical URI of `/mcp` beside
  `client_id`, an ID token of the UI's client with its `nonce`, or one with `at_hash` and no
  `nonce`, an expired `exp`, a future `nbf`, or a `required_claim` that does not hold is answered
  `401` and audited.
- A token signed with `none`, with HS256 over the RSA key's bytes, with RS256 against an EC key,
  or with ES256 on the wrong curve is refused.
- A token naming an unknown `kid` refetches the key set once, and a second one within the minute
  does not.
- With the issuer unreachable, the Server starts, Basic is admitted, and a bearer request is
  answered `503`.
- With `[rest.auth.oidc]`, `/` and `/api/v1/openapi.json` answer without a credential and
  `/api/v1/agents` answers `401` with a Bearer challenge, beside a Basic challenge when Basic users
  are configured and the request carries no `Sec-Fetch-Mode` or `navigate` there, and without one
  for `Sec-Fetch-Mode: cors`; without it, `/` answers `401` with the Basic challenge.
- `[rest.auth]` with neither `basic_users` nor `[rest.auth.oidc]`, `[rest.auth.oidc]` without
  `required_claim`, and a non-loopback `listen` without `[rest.auth]`, fail startup naming the
  keys.
- An act through a token is audited with `iss`, `sub` and `preferred_username`; an act through
  Basic with the user name; each says which.
- The Content Security Policy names the issuer's origin in `connect-src` with OIDC and does not
  without it.
- `cargo tree` of the Server contains no `aws-lc-rs` and no `rsa`.

**Not mechanically decidable:** that the UI never writes a token to browser storage. A review of
`crates/fleet-server/static/` checks it, and the test above that refuses foreign origins in the
policy limits what a mistake could reach.

### Transport and the two planes

- The transports on plain HTTP:
  [`crates/fleet-server/tests/http_transport.rs`](../../crates/fleet-server/tests/http_transport.rs):
  `gzip_request_bodies_are_accepted`, `an_oversized_request_body_is_refused_with_413`,
  `a_gzip_body_that_inflates_past_the_limit_is_refused_with_413`,
  `transport_detection_rejects_a_missing_protobuf_content_type` (clauses 1, 3).
- [`crates/fleet-server/tests/ws_transport.rs`](../../crates/fleet-server/tests/ws_transport.rs):
  `a_framed_report_is_answered`, `an_oversized_frame_closes_the_connection_with_1009`
  (clauses 1, 3).
- [`crates/fleet-agent/src/config.rs`](../../crates/fleet-agent/src/config.rs):
  `scheme_selects_the_transport`, `rejects_an_unknown_scheme_and_unknown_keys` (clause 2);
  [`crates/fleet-agent/tests/http_transport_e2e.rs`](../../crates/fleet-agent/tests/http_transport_e2e.rs):
  `a_configuration_rollout_reaches_a_polling_client` (clause 2).
- Clause 4:
  - [`crates/opamp/src/client/backoff.rs`](../../crates/opamp/src/client/backoff.rs):
    `backoff_doubles_and_caps_within_its_jittered_bounds`,
    `two_backoffs_do_not_produce_the_same_ladder` and
    `the_first_reconnect_after_a_lost_connection_is_drawn_over_thirty_seconds`;
  - [`crates/opamp/src/client/ws.rs`](../../crates/opamp/src/client/ws.rs):
    `a_refused_upgrade_waits_its_retry_after_or_the_next_step`;
  - [`crates/opamp/tests/server_listen.rs`](../../crates/opamp/tests/server_listen.rs):
    `new_connections_past_the_rate_are_answered_503_with_a_spread_retry_after`, at a rate of 20;
  - [`crates/opamp/src/server/listen.rs`](../../crates/opamp/src/server/listen.rs):
    `the_bucket_takes_its_rate_and_refills_at_it`, the rate of 200 a second without a listener;
  - [`crates/fleet-server/src/config.rs`](../../crates/fleet-server/src/config.rs):
    `the_new_connection_rate_and_the_drain_are_bounded`, the default of 200 and the refused `0`.

  No test drives a real listener at the default rate for a sustained period.
- Clause 5: reqwest's `rustls-no-provider` feature refuses to build a client without the
  process-wide `ring` provider, and a dependency pulling `aws-lc-rs` fails to build in the Dev
  Container, which has no cmake;
  [`crates/fleet-server/tests/mutual_tls.rs`](../../crates/fleet-server/tests/mutual_tls.rs)
  exercises the rustls stack on both ends.
- [`crates/fleet-server/src/config.rs`](../../crates/fleet-server/src/config.rs):
  `the_operator_plane_defaults_to_loopback_and_is_configurable`,
  `two_planes_on_one_address_are_refused` (clauses 7, 8, 14).
- [`crates/fleet-server/tests/packages.rs`](../../crates/fleet-server/tests/packages.rs):
  `the_artifact_is_served_where_the_agents_are_and_not_on_the_operator_plane`;
  [`crates/fleet-server/tests/auth.rs`](../../crates/fleet-server/tests/auth.rs):
  `the_rest_api_stays_open_on_its_own_listener_when_the_opamp_endpoint_is_guarded` (clause 7).
- Connection setup:
  [`crates/fleet-server/tests/connection_setup.rs`](../../crates/fleet-server/tests/connection_setup.rs):
  `a_connection_that_never_finishes_its_headers_is_hung_up_on`,
  `an_established_session_outlives_the_header_bound` (clauses 15, 18).
- Clause 18's floor, over real connections to `opamp`'s listener with the floor driven short, in
  real time, since a paused clock fires a socket's timeouts at once:
  [`crates/opamp/tests/server_listen.rs`](../../crates/opamp/tests/server_listen.rs):
  `a_body_that_never_arrives_is_answered_408`, `a_body_that_trickles_is_answered_408`,
  `a_slow_body_above_the_floor_is_taken_whole`,
  `a_websocket_message_that_trickles_closes_with_1008`,
  `a_message_kept_open_by_pings_between_its_fragments_closes_with_1008`,
  `an_idle_websocket_stays_open`;
  [`crates/fleet-server/tests/packages.rs`](../../crates/fleet-server/tests/packages.rs):
  `an_upload_that_stops_is_answered_408_and_leaves_nothing_staged`, on the Operator plane.

These tests carry `Verifies:` for this ADR:

- [`crates/opamp/src/tls.rs`](../../crates/opamp/src/tls.rs):
  `the_provider_offers_tls_1_3_suites_alone` (clause 5);
  [`crates/opamp/src/endpoint.rs`](../../crates/opamp/src/endpoint.rs):
  `only_the_loopback_literals_are_loopback` (clause 6).
- [`crates/opamp/tests/server_listen.rs`](../../crates/opamp/tests/server_listen.rs):
  `a_client_offering_only_tls_1_2_is_refused` (clause 5),
  `a_plaintext_listener_off_the_loopback_is_refused` (clause 6),
  `connections_past_the_cap_are_refused_while_established_ones_keep_working` (clause 19).
- [`crates/opamp/tests/client_connection.rs`](../../crates/opamp/tests/client_connection.rs):
  `a_tls12_only_server_fails_the_handshake` (clause 5).
- [`crates/opamp/src/client/connection.rs`](../../crates/opamp/src/client/connection.rs):
  `plaintext_is_refused_off_the_loopback_literals` and
  `plaintext_off_the_loopback_is_refused_before_connecting` (clauses 2, 6).
- [`crates/fleet-agent/src/config.rs`](../../crates/fleet-agent/src/config.rs):
  `the_default_endpoint_is_wss_on_the_loopback`,
  `a_plaintext_endpoint_off_the_loopback_is_refused_at_startup` (clause 2).
- [`crates/fleet-server/src/config.rs`](../../crates/fleet-server/src/config.rs):
  `the_agent_plane_defaults_to_the_loopback`, `a_server_without_tls_is_refused_at_startup`
  (clauses 7, 9), `the_operator_plane_requires_authentication_off_the_loopback` (clause 8),
  `max_connections_defaults_per_plane_and_zero_is_refused` (clause 19).
- The store connections of clause 21 are verified under
  [ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md),
  clauses 17 and 24.

The Operator plane's own certificate (clauses 9 to 13), each test marked as verifying this ADR:

- [`crates/fleet-server/tests/mutual_tls.rs`](../../crates/fleet-server/tests/mutual_tls.rs):
  `the_operator_plane_serves_its_own_certificate_and_the_agent_plane_keeps_the_fleets`. With
  `[rest.tls]`, the Operator plane presents that certificate and the Agent plane presents
  `[tls]`'s; without it, both present `[tls]`'s (clauses 9, 10).
- [`crates/fleet-server/src/config.rs`](../../crates/fleet-server/src/config.rs):
  `rest_tls_takes_a_certificate_and_its_key_and_nothing_else`. A `[rest.tls]` missing `key_file`
  and one carrying `client_ca_file` are refused at startup, naming the key (clause 10).
- [`crates/fleet-server/src/tls.rs`](../../crates/fleet-server/src/tls.rs):
  `a_rest_tls_pair_is_checked_at_startup_and_served_on_the_operator_plane_alone`. A key that does
  not match its certificate, or a file that cannot be read, refuses the start, naming the file
  (clause 11).
- [`crates/fleet-server/src/pki.rs`](../../crates/fleet-server/src/pki.rs):
  `the_operator_planes_certificate_is_watched_once`. `pki::depended_on` lists
  `[rest.tls] cert_file`, and lists a file named in both sections once (clause 13).

Tests of the rate limit, each marked `Verifies:` for this ADR:

- `crates/fleet-server/src/agent_rate.rs`:
  - `a_bucket_starts_full_and_refills_at_the_configured_rate` (clause 22);
  - `a_full_table_evicts_the_bucket_used_least_recently` and `both_tables_are_bounded`
    (clause 28).
- `crates/fleet-server/src/config.rs`:
  - `the_agent_rate_limit_defaults_and_refuses_zero`: the defaults are 10, 300, 500 and 10 000, a
    `0` in any key is refused naming it, and an unknown key is refused (clause 22);
  - `a_limit_below_the_heartbeat_for_256_agents_warns_naming_both_keys`: `messages_per_sec = 5`
    with `heartbeat_interval_secs = 30` warns, naming both keys, and starts; the defaults do not
    warn (clause 23).
- `crates/fleet-server/tests/ws_transport.rs`:
  `a_session_past_its_burst_is_answered_unavailable_with_retry_info`. A member sends `burst + 1`
  messages, and the last reply carries `Unavailable`, a `retry_info` of exactly 30 s and the
  message's `instance_uid`. The connection stays open, the Agent's record is unchanged, and once
  the bucket refills the next message is processed with `ReportFullState` set (clauses 24, 25,
  27).
- `crates/fleet-server/tests/http_transport.rs`:
  `a_poller_past_its_burst_is_answered_unavailable_in_the_body`: a `200` carrying the same
  `ServerToAgent`, never a `429` or a `503` (clause 25).
- `crates/fleet-server/tests/packages.rs`:
  `a_download_past_the_limit_is_answered_429_after_thirty_seconds`. A member over its rate gets
  `429` with `Retry-After: 30`, its next download is admitted once the bucket refills, and the
  refusal counts no failure toward the failed-admission throttle of clause 48 (clauses 24, 26).
- `crates/fleet-server/tests/mutual_tls.rs`:
  - `a_throttled_message_leaves_an_aggregated_refusal_naming_the_host`: beyond ten a second, the
    entries are counted in `agent_rate.throttled.aggregated` (clause 29);
  - `every_unavailable_reply_names_the_agent_it_answers`: each of these carries the message's
    `instance_uid` (clause 27):
    - the throttled reply;
    - the Agent-record ceiling;
    - a CSR held back for its audit record;
    - the full enrolment queue;
    - the closed enrolment window.
- `crates/fleet-server/src/audit_log.rs`:
  `a_throttle_refusal_that_finds_the_channel_full_is_counted_unrecorded` (clause 29).
- `crates/fleet-agent/tests/gateway_revocation_e2e.rs`:
  `a_throttled_agent_behind_a_gateway_hears_unavailable_and_its_neighbour_does_not`. Two Agents
  ride one folded connection to a marked Gateway, and one floods. It alone receives the
  `Unavailable`, routed by its `instance_uid`, and the other's reports keep being processed
  (clauses 25, 27, and the bucket rule of ADR-0016).

These tests verify the bucket a message is counted in, which
[ADR-0016](0016-client-modes-and-the-gateway.md) decides, and so carry `Verifies: ADR-0016`; they
pin what clause 24 counts as well: in `crates/fleet-server/src/agent_rate.rs`,
`a_host_is_one_bucket_whatever_its_agents_report`,
`a_member_certificate_without_a_host_is_counted_by_issuer_and_serial`,
`an_enrolment_connection_is_counted_by_its_peer_address`,
`marking_a_gateway_takes_effect_at_the_next_message`,
`behind_a_gateway_a_message_passes_its_agents_bucket_and_the_aggregate` and
`a_message_naming_no_agent_behind_a_gateway_counts_in_the_aggregate_alone`; in
`crates/fleet-server/tests/mutual_tls.rs`,
`two_certificates_of_one_host_share_a_bucket_and_two_hosts_do_not` and
`a_bootstrap_certificate_shared_by_two_addresses_is_two_buckets`.

Clause 17: `a_spread_drain_closes_the_sessions_over_its_window` in
[`crates/opamp/tests/server_listen.rs`](../../crates/opamp/tests/server_listen.rs).

**Not mechanically decidable:** clause 16. A 10-second handshake deadline is a named constant in
`listen.rs`, and no test waits it out; review keeps both planes on `listen::plane` and the shared
`Handle`. Clause 20: the HTTP/2 stream cap and keep-alive bounds are named constants set in
`opamp::server::listen`, and the workspace has no HTTP/2 client to drive a peer past them; review
checks that every listener goes through `Listener::serve`.

### Admission and revocation

Where a test below also covers a clause another ADR carries, that ADR is named beside it.

- [`crates/fleet-server/src/config.rs`](../../crates/fleet-server/src/config.rs) —
  `an_auth_section_is_refused_at_startup` (clause 30); `the_client_ca_is_required_at_startup`
  (clauses 34, 46); `rest_auth_verifies_its_hashes_and_carries_the_basic_challenge`,
  `an_unusable_rest_auth_section_is_rejected`,
  `the_operator_plane_requires_authentication_off_the_loopback` (clause 42),
  `a_server_without_tls_is_refused_at_startup` (clause 44),
  `a_plaintext_credential_is_refused_naming_its_entry`, which holds the Basic cases alone,
  `a_weak_argon2id_hash_is_refused` (clause 50).
- [`crates/fleet-server/tests/mutual_tls.rs`](../../crates/fleet-server/tests/mutual_tls.rs) —
  `a_member_is_admitted_on_its_certificate_alone` and `an_authorization_header_is_ignored` over both
  transports (clauses 30, 35);
  `a_client_certificate_is_required_in_the_handshake_on_the_agent_plane` (clauses 34, 47),
  `a_csr_is_answered_with_an_issued_certificate`,
  `a_csr_to_a_server_that_signs_nothing_is_a_bad_request` (clause 37),
  `a_bootstrap_ca_sharing_its_subject_with_the_client_ca_is_refused` (clause 46),
  `a_bootstrap_certificate_is_refused_outside_an_enrolment_window` (clauses 38, 48; the enrolment
  connection,
  [ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)),
  `an_enrolment_request_waits_for_an_operator_and_is_issued_on_approval` (clauses 33, 47; the
  window,
  [ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md);
  the enrolment connection and the pending list,
  [ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)),
  `a_rejected_request_is_refused_and_closing_the_window_shuts_enrolment` (the window,
  [ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md);
  the enrolment connection,
  [ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)),
  `an_unknown_request_is_not_approved_and_is_rejected_before_it_arrives` (the pending list,
  [ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)),
  `repeated_failures_from_one_address_are_throttled`,
  `the_operator_plane_counts_its_failures_in_a_table_of_its_own` (clause 48),
  `an_operator_sees_each_host_and_can_mark_a_gateway` (the host and its mark,
  [ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)).
- [`crates/fleet-server/src/credentials.rs`](../../crates/fleet-server/src/credentials.rs) —
  `a_basic_password_is_verified_against_its_argon2id_hash`,
  `an_unknown_user_costs_the_same_verification`, `a_basic_verification_is_remembered_and_bounded`,
  `password_hashes_are_bounded_and_off_the_async_workers`,
  `the_comparison_hash_matches_the_costliest_user` (clause 31).
- [`crates/fleet-server/src/throttle.rs`](../../crates/fleet-server/src/throttle.rs) —
  `attempts_under_way_count_and_a_slash_64_is_one_peer` (clause 31),
  `a_peer_that_fails_too_often_waits_out_the_back_off`, `a_full_table_keeps_an_address_in_back_off`,
  `the_table_is_bounded_and_drops_the_oldest` (clause 48).
- [`crates/fleet-server/src/main.rs`](../../crates/fleet-server/src/main.rs) —
  `hash_credential_hashes_a_basic_password_alone` (clause 50).
- [`crates/fleet-agent/src/config.rs`](../../crates/fleet-agent/src/config.rs) —
  `a_leftover_auth_section_is_ignored_with_a_notice` (clause 32);
  [`config_file.rs`](../../crates/fleet-agent/src/config_file.rs)
  `admission_needs_a_client_identity_and_nothing_else` (clause 34).
- [`crates/fleet-server/src/tls.rs`](../../crates/fleet-server/src/tls.rs) —
  `a_certificate_not_issued_by_the_client_ca_only_enrols` (clauses 35, 46; the enrolment connection,
  [ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)).
- [`crates/fleet-server/src/enrolment.rs`](../../crates/fleet-server/src/enrolment.rs) —
  `the_window_is_closed_by_default_and_bounded`, `closing_the_window_expires_every_request` (the
  window,
  [ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)),
  `a_request_waits_for_an_operator_and_is_answered_once_decided`, `the_queue_is_bounded` (the
  enrolment connection and the pending list,
  [ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)).
- [`crates/fleet-server/src/ca.rs`](../../crates/fleet-server/src/ca.rs) —
  `signs_a_request_into_a_certificate`, `the_request_cannot_dictate_the_certificates_powers`,
  `refuses_a_request_that_does_not_parse` (clause 37; the issued certificate's subject,
  [ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)),
  `a_renewal_proof_names_the_certificate_and_its_host` (the host,
  [ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md);
  the renewal proof, [ADR-0016](0016-client-modes-and-the-gateway.md)).
- [`crates/fleet-server/src/revocation.rs`](../../crates/fleet-server/src/revocation.rs) —
  `a_host_is_bounded_and_speaks_only_for_its_own_agents` (the host,
  [ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)).
- [`crates/fleet-server/src/fleet.rs`](../../crates/fleet-server/src/fleet.rs) —
  `a_host_cannot_report_for_another_hosts_agent` (clause 41; the host,
  [ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)),
  `unavailable_tells_the_agent_when_to_retry` (the enrolment queue,
  [ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)).
-
  [`crates/fleet-agent/tests/certificate_renewal_e2e.rs`](../../crates/fleet-agent/tests/certificate_renewal_e2e.rs)
  — `a_client_renews_each_certificate_before_it_expires` (clauses 37, 39; the renewal proof,
  [ADR-0016](0016-client-modes-and-the-gateway.md)).
- [`crates/fleet-agent/src/csr.rs`](../../crates/fleet-agent/src/csr.rs) —
  `a_client_without_a_certificate_asks_and_keeps_its_key` (clause 37; the enrolment connection,
  [ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)),
  `the_private_key_is_written_owner_only`, `an_issued_certificate_becomes_the_identity`,
  `a_certificate_without_a_pending_key_is_refused` (clause 36),
  `a_certificate_in_its_renewal_window_is_requested_again` (clause 39).
-
  [`crates/fleet-agent/tests/packages_download.rs`](../../crates/fleet-agent/tests/packages_download.rs)
  — `the_client_certificate_goes_to_the_servers_origin_alone` (clause 47).
- [`crates/fleet-agent/tests/gateway_tls.rs`](../../crates/fleet-agent/tests/gateway_tls.rs) —
  `a_downstream_agent_with_a_certificate_reaches_the_server_over_tls`,
  `a_downstream_peer_without_a_certificate_is_refused` (clause 40).
- [`crates/fleet-server/tests/rest_auth.rs`](../../crates/fleet-server/tests/rest_auth.rs) —
  `a_request_without_credentials_is_answered_401_with_a_basic_challenge`,
  `the_configured_operator_reaches_the_api`, `the_ui_and_the_api_docs_are_guarded_too`,
  `the_agent_plane_is_untouched_by_the_operator_credential` (clauses 42, 45);
  [`crates/fleet-server/tests/rest_api.rs`](../../crates/fleet-server/tests/rest_api.rs) —
  `a_cross_site_state_changing_post_is_refused` (clause 43).
- [`crates/fleet-server/src/ca.rs`](../../crates/fleet-server/src/ca.rs) —
  `a_canonical_uuid_anywhere_in_a_value_is_a_claim`, `hex_without_hyphens_is_no_claim`,
  `a_san_is_read_for_claims`, `a_claim_in_a_bmp_or_universal_string_is_read` (clauses 51, 52).
- [`crates/fleet-server/tests/mutual_tls.rs`](../../crates/fleet-server/tests/mutual_tls.rs) —
  `a_csr_claiming_another_instance_uid_is_a_bad_request`,
  `a_csr_claiming_its_own_instance_uid_is_signed`, `a_csr_claiming_nothing_is_signed_as_before`, and
  `an_enrolment_csr_claiming_another_instance_uid_never_reaches_the_queue` (clauses 53, 54).
- [`crates/fleet-server/src/revocation.rs`](../../crates/fleet-server/src/revocation.rs) tests:
  `a_revocation_follows_every_renewal` (clause 57), `the_list_survives_a_restart_and_can_be_lifted`
  (clause 59; the persisted list,
  [ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)),
  `a_renewal_stays_revoked_after_its_revoked_ancestor_expires` (clause 57; the persisted list,
  [ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)),
  `the_list_and_the_register_are_bounded`,
  `an_expired_register_entry_is_dropped_with_its_revocation` (the register and the persisted list,
  [ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)).
- [`crates/fleet-server/src/fs/revocation.rs`](../../crates/fleet-server/src/fs/revocation.rs)
  tests: `the_ledger_survives_a_reopen` (the register and the persisted list,
  [ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md));
  `a_persisted_credential_entry_is_dropped_on_load` (clause 58): a list holding a credential entry
  beside a certificate entry loads with the certificate entry alone, is written back without the
  credential entry, and the drop is logged.
- [`crates/fleet-server/src/ca.rs`](../../crates/fleet-server/src/ca.rs) test:
  `two_certificates_from_one_key_have_two_serials` (clause 55).
- [`crates/fleet-server/tests/mutual_tls.rs`](../../crates/fleet-server/tests/mutual_tls.rs) tests:
  `a_revoked_certificate_is_refused_on_both_transports_and_the_download` (clauses 56, 59, 60), which
  also asserts that the `401` carries no `WWW-Authenticate` header;
  `a_revocation_closes_the_session_it_concerns_and_no_other` (clause 61),
  `a_session_is_closed_when_its_certificate_expires` (clause 62),
  `a_revocation_reaches_a_certificate_renewed_before_it` (clauses 55, 57; the register,
  [ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)),
  `a_revocation_names_its_issuer_by_role_whatever_the_issuer_is_called` (clause 56),
  `a_csr_for_another_agent_still_descends_from_the_presented_certificate` (the register,
  [ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md);
  revocation behind a Gateway, [ADR-0016](0016-client-modes-and-the-gateway.md)),
  `a_marked_gateway_is_handed_the_revoked_certificates_with_their_renewals` (clause 63, an unmarked
  member answered `403` and an unchanged list `304`),
  `a_bootstrap_certificate_is_not_handed_the_list` (clause 63);
  `a_credential_revocation_is_answered_400` (clause 58): `{"credential": …}` is refused with `400`
  and the list is unchanged.
-
  [`crates/fleet-agent/tests/gateway_revocation_e2e.rs`](../../crates/fleet-agent/tests/gateway_revocation_e2e.rs)
  test: `a_certificate_the_server_revokes_is_refused_behind_the_gateway` (clause 63; revocation
  behind a Gateway, [ADR-0016](0016-client-modes-and-the-gateway.md)).
- The Server refuses to start on a bootstrap CA key that does not match `bootstrap_ca_file` or that
  is the client CA's key; without `bootstrap_ca_key_file` it starts, admits a bootstrap certificate
  made elsewhere, and answers `POST /api/v1/bootstrap-certificates` with `409` (clause 46).
- A request carrying a bootstrap certificate the Server issued is listed with the `bootstrap_serial`
  that `GET /api/v1/bootstrap-certificates` lists for it, and `PendingEnrolment` in the OpenAPI
  document carries the field (the pending list,
  [ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)).
- Clauses 32 and 34 across instances are verified by the tests
  [ADR-0025](0025-several-server-instances-share-one-postgresql-database-and-a-group-is-placed-again-at-once.md)
  names.

Tests that carry `Verifies:` for this ADR verify the CA keys on a token (clauses 37, 46 and 64 to
70), against SoftHSM2 where a token is needed:

- A host certificate and a bootstrap certificate signed through a SoftHSM2 token verify against
  the CA certificate on the token, and each signature wrote an audit entry naming the key label
  (clauses 37, 46, 66, 67).
- A `pin_file` that others may read, or its group may write, fails startup naming it (clause 64).
- `[client_ca]` with both `key_file` and `key_label`, or a `key_label` without `[pkcs11]`, fails
  startup naming the keys; with either store, a `key_file` alone starts without a token (clauses 37,
  65).
- A module whose manufacturer is `SoftHSM` starts with the warning and the
  `pkcs11.software_token` audit entry (clause 68).
- The Server starts against a SoftHSM2 token whose `softhsm2.conf` is named only by `SOFTHSM2_CONF`,
  and `server.toml` holds no path of SoftHSM2's own beside `[pkcs11] module` (clause 68).
- `opamp-fleetctl pki token-init` creates the client CA's, the bootstrap CA's and the audit key on
  the token, none of them extractable, signs both CA certificates through the token, writes no key
  file, and refuses a label the token already holds; `pki init --ca-keys-on-token` writes neither
  the client CA nor the bootstrap CA (the commands,
  [ADR-0036](0036-the-operator-program-opamp-fleetctl.md) clauses 7, 8 and 16).
- `client_ca_file` with two CA certificates admits a member of either, a renewal proof whose
  certificate the old CA signed is accepted, and the renewal is signed by the token's key
  (clause 69).
- Every certificate signed with a token key writes `pkcs11.signed` naming its label and use case,
  and with no place in the record nothing is signed (clause 67).
- On PostgreSQL, two instances whose CA keys are named by their labels on one SoftHSM2 token,
  sharing one `cryptoki` context in one test process, both start and both sign renewals
  (clauses 64, 70). The check of the audit key's label across instances is
  [ADR-0028](0028-the-audit-record.md)'s.

**Not mechanically decidable:** clause 41 decides what is *not* built beyond the host — no test
can show the absence of a per-Agent authorization the design rejects; and clause 49 rests on the
CAs a Gateway trusts, which the Server cannot see. Review holds them.
