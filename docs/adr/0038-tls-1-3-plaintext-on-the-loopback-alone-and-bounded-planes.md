# ADR-0038: Both OpAMP transports on both ends over TLS 1.3 alone, plaintext on the loopback alone, and a Server on two listeners split by audience with bounded connections

- **Status:** 🟢 accepted
- **Date:** 2026-10-03
- **Deciders:** Markus Brigl
- **Applies to:** the OpAMP endpoint and both transports in `crates/fleet-server/src/transport.rs` and `crates/fleet-agent/src/transport/`, TLS in `crates/opamp/src/tls.rs`, `crates/fleet-server/src/tls.rs` and `crates/fleet-agent/src/tls.rs`, the plaintext rule in `crates/opamp/src/client/connection.rs`, how the Server binds and serves in `crates/opamp/src/server/listen.rs`, `crates/fleet-server/src/listen.rs` and `main.rs`, the package URL probe in `crates/fleet-server/src/api.rs`, the `rustls` features in `Cargo.toml`, and the `listen`, `max_connections`, `[rest]` and `[tls]` keys of `server.toml`
- **Supersedes:** [ADR-0012](0012-transports-tls-and-the-servers-two-planes.md)

## Context

Supersedes [ADR-0012](0012-transports-tls-and-the-servers-two-planes.md) because the
[specification](../SPECIFICATION.md) puts security before convenience (Strategy "Security before
convenience", quality goal Q-1 "Secure by default"). Clauses 2, 5, 6, 7, 8, 9 and 11 change, and
clauses 15 to 18 are new; the rest of the decision stands as it was.

The Baseline defines two transports: *"Server implementations SHOULD accept both plain HTTP
connections and WebSocket connections. OpAMP Client implementations may choose to support
either."* The specification's strategy — implement the protocol as completely as it allows — turns
that into an obligation on both ends: a Server accepting one transport locks out half the
third-party clients, and a Client speaking one cannot exercise both faces of its own Server.

- **Plain HTTP** — the Client POSTs a protobuf `AgentToServer` with
  `Content-Type: application/x-protobuf` and receives a `ServerToAgent`; it polls (default
  30 seconds). The Server MUST honour `Content-Encoding: gzip`. Server-initiated messages wait for
  the next poll.
- **WebSocket** — one persistent connection, either side sends at will, each message a varint
  header plus the protobuf body. It is the transport that gives the control loop its "within
  seconds" property and the one Gateway multiplexing is stated for
  ([ADR-0009](0009-client-modes-and-the-gateway.md)).
- **Detection** — one path (`/v1/opamp`, default port 4320); the Baseline itself describes the
  `Content-Type` header as how the Server tells the two apart.

TLS is part of the transport: goal 17 of the [specification](../SPECIFICATION.md) requires
Client–Server traffic to be TLS-protected. The Strategy narrows that: every connection that leaves
the host is TLS 1.3, and plaintext is accepted on the loopback alone. Q-1 adds that a configuration
breaking this is refused at startup, naming the setting, never warned. The Dev Container
([ADR-0002](0002-dev-container-runtime.md)) has neither OpenSSL headers nor cmake, which
`aws-lc-rs`-backed rustls needs.

In this ADR, **LOOPBACK** means the IP literals `127.0.0.1` and `::1`. A host name is never
LOOPBACK, `localhost` included: what it resolves to is a resolver's answer, not the
configuration's.

The Server has two audiences with nothing in common but the process. Agents connect from the whole
estate, hold long-lived WebSocket sessions, speak protobuf, and prove fleet membership
([ADR-0017](0017-admission-and-authentication.md)). Operators and portals connect from a few
places, speak JSON over short requests, and act with authority over every Agent — reading the
fleet, rewriting Configurations, uploading and rolling out packages. One address for both means one
exposure, one TLS policy, and one answer to "who may connect" for two questions with different
answers; it also forces client-certificate checks into route code, since a browser presents none.

One route crosses that line: the package download. Its path is under `/api/v1`, but its audience is
Agents. The `download_url` in a package offer is by default a path the Client resolves against its
own OpAMP endpoint, host and port ([ADR-0019](0019-package-delivery-on-the-agent.md)). The
downloading Client presents no credential. It reaches the route through the Agent plane's TLS
handshake, so it presents its client certificate there as it does for `/v1/opamp`. The content
hash and signature protect the artifact.

Finally, a connection is bounded by message size limits, Admission, and on TLS listeners a
handshake deadline — but nothing bounds the time before a request exists. A peer can open a
connection, send `GET /v1/opamp HTTP/1.1\r\n` and fall silent, costing the Server a task and
hyper's read buffer per connection, before any route or credential check runs. hyper 1.x defaults
`header_read_timeout` to 30 s, but the default is inert unless a `Timer` is installed (its
`Time::check` warns and resolves to `None`), and configuring the timeout without a timer panics.
Neither `axum::serve` (0.8) nor `axum_server` (0.7) installs one; `axum::serve` exposes no builder
at all, while `axum_server::Server::http_builder()` does. A per-request timeout is the wrong
instrument: the three routes that matter — the WebSocket, the download, the package upload with
its body limit off — are meant to take long, and middleware runs only after the headers are parsed.

Two more bounds are missing. Nothing limits how many connections a peer may hold open at once, and
`max_agents` bounds the fleet, not the sockets. The TLS listeners offer `h2` by ALPN, and the
header-read timeout is HTTP/1 only; an HTTP/2 peer is bounded by the message size and by nothing
else. [`HARDENING.md`](../HARDENING.md) names these H16 and H17.

## Decision

We will implement both OpAMP transports on both ends over rustls with the `ring` provider and TLS
1.3 alone, accept plaintext only on LOOPBACK, and serve the Server on two listeners split by
audience — an Agent plane that always serves TLS and a loopback-default Operator plane — each built
in one place that bounds connection setup, the number of connections and HTTP/2.

1. **One endpoint, both transports, detected per request.** The Server serves `/v1/opamp` on its
   Agent plane: a WebSocket upgrade (`GET`) starts the WebSocket transport, a `POST` carrying
   `Content-Type: application/x-protobuf` is one plain-HTTP exchange, and a `POST` without that
   type is answered `415`. Both hand every decoded report to the same processing, so transport is
   carriage, never semantics.

2. **The Client selects the transport by its endpoint's scheme.** `ws://`/`wss://` is WebSocket,
   `http://`/`https://` is polling; any other scheme fails at startup. `ws://` and `http://` are
   accepted only when the endpoint's host is LOOPBACK; any other host fails at startup with a
   message naming the endpoint. The default endpoint is `wss://127.0.0.1:4320/v1/opamp`, so
   WebSocket over TLS is the default — it is the only transport that delivers Server-initiated
   changes without polling latency. `poll_interval_secs` (default 30) governs polling;
   `heartbeat_interval_secs` (default 30, `0` disables heartbeats and undeclares
   `ReportsHeartbeat`) governs WebSocket. A Server-offered heartbeat replaces both
   ([ADR-0018](0018-connection-settings-and-server-capabilities.md)).

3. **Gzip and a message size limit on both transports.** The Server accepts gzip request bodies
   (a Baseline MUST) and enforces `max_message_size_bytes` (default 64 MiB, `0` refused at startup)
   in both directions and on both transports, applied after decompression: `413` on plain HTTP, a
   `1009` close on WebSocket, and an oversized reply is discarded rather than sent. The shared
   helpers that implement this live in `crates/opamp`
   ([ADR-0011](0011-workspace-crates-and-configuration.md)).

4. **Reconnection belongs to the Client's transport layer.** A lost connection is retried with
   exponential backoff from one second, capped at one minute, with jitter (the Baseline asks for
   jitter so a restarted Server is not hit by the whole fleet on the same instants); after a
   reconnect the Client sends a full status report. The Server keys no state on the connection
   ([ADR-0009](0009-client-modes-and-the-gateway.md)).

5. **TLS is TLS 1.3 alone, on rustls with the `ring` provider, everywhere; no OpenSSL, no system
   TLS library.** `reqwest` requires the `tls12` feature of `rustls`, so the floor is held where
   every configuration is built rather than by the build. `opamp::tls::provider()` is the
   `ring` provider with the three TLS 1.3 suites and nothing else: `TLS13_AES_256_GCM_SHA384`,
   `TLS13_AES_128_GCM_SHA256` and `TLS13_CHACHA20_POLY1305_SHA256`. Every rustls configuration of
   both ends is built from it with the protocol versions pinned to TLS 1.3
   ([ADR-0036](0036-the-whole-opamp-communication-layer-in-the-opamp-crate.md)). Each binary
   installs it process-wide at startup, and every `reqwest` client is built with a minimum of TLS
   1.3. This covers every connection: OpAMP, the REST API, downloads, mirrors, own telemetry and
   the Server's outbound probe. Without `[tls] ca_file` a Client trusts the built-in webpki roots on
   `wss://` and the platform's trust store on `https://`, the default `reqwest` takes; with it,
   that bundle replaces both. `axum-server` (`tls-rustls-no-provider`) terminates TLS on the
   Server, `tokio-tungstenite` (`rustls-tls-webpki-roots`) carries `wss://`, and `reqwest`
   (`rustls-no-provider`, `gzip`) carries `https://`. The `*-no-provider` features keep `aws-lc-rs`
   out of the build, so a missing provider is an error, never a silent fallback.
   On the Client, `[tls] ca_file` replaces the built-in webpki roots, so self-signed deployments
   work; trust is the operator's file, never a Server's instruction.

6. **Two listeners, split by audience rather than by path.**

   | Plane | Key | Default | TLS | Serves |
   |---|---|---|---|---|
   | Agent | `listen` | `127.0.0.1:4320` | always | `/v1/opamp` and the package download route |
   | Operator | `[rest] listen` | `127.0.0.1:4321` | always | `/api/v1/…`, `/api/v1/openapi.json`, `/api/v1/docs` with its vendored Redoc bundle, and the bundled UI at `/` |

   The Agent plane listens on LOOPBACK by default. Serving the estate is one deliberate line in
   `server.toml`, together with the TLS material it needs. `[rest]` is a table so that the plane's
   own settings (its authentication, [ADR-0017](0017-admission-and-authentication.md), and its
   connection cap) live inside it; unknown keys are refused as everywhere
   ([ADR-0011](0011-workspace-crates-and-configuration.md)). The router is split accordingly:
   `server::agent_app(state, admission)` and `server::operator_app(state, auth)`.

7. **The Operator plane is loopback by default.** Its reachability is its first protection; a
   network-open default would export the fleet's full control surface. Reaching it from elsewhere
   is one deliberate line in `server.toml`, or an SSH tunnel. It serves TLS with the material of
   clause 9 on every address. Off LOOPBACK it requires authentication as the admission decision
   states; a Server lacking it is refused at startup.

8. **The package download stays on the Agent plane, outside the credential check, under its
   `/api/v1` path.** The offered `download_url` therefore remains a path the Client resolves
   against its own endpoint, and `advertised_url` stays an option for mirrors and other hosts,
   never an obligation. The download is reached through the same TLS handshake as `/v1/opamp`, so
   the Client presents its client certificate to its own Server's origin and to no other host.
   What the handshake requires of that certificate is the admission decision's
   ([ADR-0017](0017-admission-and-authentication.md) and the ADR that supersedes it). The route is
   served on that one listener only — the Operator plane answers it `404` — and the OpenAPI
   document, generated from the Operator plane's routes, does not carry it; the manual documents it
   as the Agent plane's.

9. **One set of TLS material for both listeners, and it is required.** `[tls] cert_file`/`key_file`
   serve both planes, each with ALPN `h2` and
   `http/1.1`. A Server without `[tls]` is refused at startup with a message naming the section.
   `[tls] client_ca_file` applies to the Agent plane alone, and what it requires is
   [ADR-0017](0017-admission-and-authentication.md)'s and its successor's. There are no
   per-listener certificates.

10. **Two listeners always; addresses that cannot both be bound fail at startup.** There is no
    single-port mode. The same port on the same address, or on an address that covers every
    interface (`0.0.0.0:4320` and `127.0.0.1:4320`), is refused at load with a message naming both
    keys. Both listeners are bound before either serves, and a bind failure names the plane.

11. **Every plane's server is built in one place, with an HTTP/1 header-read timeout of 30
    seconds.** `listen::plane` serves both planes on `opamp::server::listen`
    ([ADR-0036](0036-the-whole-opamp-communication-layer-in-the-opamp-crate.md)), which serves plain
    and TLS through `axum_server`, installs `hyper_util::rt::TokioTimer` on the HTTP/1 builder, and
    sets `header_read_timeout` to `HEADER_READ_TIMEOUT` (30 s — hyper's own default, and what axum
    applies once it installs the timer itself). The timeout is a parameter of the serving function
    so a test can drive it short. `hyper-util` is a dependency of `opamp`'s `server` feature for
    this.

12. **The TLS handshake deadline is stated, not inherited.** `TLS_HANDSHAKE_TIMEOUT` is 10 seconds
    on both planes' acceptors — `axum_server`'s default value, named in this code so it reads as a
    decision.

13. **Shutdown is one bounded drain for both planes.** One `axum_server::Handle` is shared by both
    listeners; an interrupt calls `graceful_shutdown` with `SHUTDOWN_DRAIN` (10 s), whatever has not
    ended by then is cut, and the Agent-record flush of
    [ADR-0026](0026-the-fleet-record.md) runs afterwards.

14. **No request timeout, no body timeout, no `tower-http`, no `server.toml` key for these
    bounds.** The long routes stay long. A knob is added when a deployment needs a value other than
    the framework default, not before.

15. **Plaintext on LOOPBACK alone, refused at startup elsewhere.** A listener without TLS is
    accepted only on a LOOPBACK address. `opamp`'s `Listener::serve` refuses a plaintext listener
    on any other address, and `opamp::client::connection` refuses `ws://` or `http://` to any other
    host. Each end refuses such a configuration at startup with a message naming the setting; it
    never warns and continues.

16. **A connection cap per plane.** `max_connections` caps the Agent plane (default 10 000) and
    `[rest] max_connections` the Operator plane (default 256); `0` is refused at startup. The cap
    is applied in the accept loop of `opamp`'s listener. A connection past the cap is closed on
    accept, before the TLS handshake, while the connections already established keep working. A
    fleet larger than the default raises the key together with the process's file-descriptor
    limit.

17. **HTTP/2 is bounded on every listener.** `opamp`'s listener sets the HTTP/2 builder of every
    listener it serves, so every TLS listener, where ALPN offers `h2`, carries the bounds.
    `H2_MAX_CONCURRENT_STREAMS` is 100 per connection. A keep-alive ping is sent every
    `H2_KEEP_ALIVE_INTERVAL` (30 s), and a peer that leaves one unanswered for
    `H2_KEEP_ALIVE_TIMEOUT` (20 s) is dropped. They are named constants, not keys, as clause 14
    states for the HTTP/1 bound.

18. **The Server's outbound HTTP is TLS 1.3 and `https://`.** The probe that asks a package source
    whether it holds an artifact speaks `https://` over the provider of clause 5. An `http://`
    source is probed only on LOOPBACK; off LOOPBACK it is refused with a message naming the URL.
    The probe keeps its refusal of internal addresses and redirects.

**Out of scope:** authentication on either plane and what a client certificate must prove,
including whether the Agent plane's handshake requires it
([ADR-0017](0017-admission-and-authentication.md) and the admission ADR that supersedes it);
connection settings and the heartbeat offer
([ADR-0018](0018-connection-settings-and-server-capabilities.md) and
[ADR-0041](0041-connection-settings-offered-securely-and-server-capabilities.md)); the rate of
admission attempts; which hosts a Client downloads from
([ADR-0042](0042-signed-package-delivery-from-allowed-sources.md)); where its own telemetry goes
([ADR-0048](0048-own-telemetry-over-tls-1-3-and-plaintext-only-to-the-loopback.md)); response
compression on plain HTTP; and a connection cap on the Client's own listeners — the Gateway
endpoint and the Supervisor Endpoint — which carry the bounds of `opamp`'s listener and no cap.

## Alternatives considered

- **One transport first** — WebSocket-only would put a deviation in `CONFORMANCE.md` for the simpler
  of the two transports and lock out HTTP-polling clients; HTTP-only caps the control loop at poll
  latency, and Gateway multiplexing is specified in WebSocket terms.
- **Separate endpoints or ports per transport** — the Baseline's default is one path, one port,
  per-request detection; splitting would be a deviation with no gain.
- **`native-tls`/OpenSSL** — system headers on Linux plus SChannel/SecureTransport variance on the
  Windows and macOS builds, against one pure-Rust stack identical everywhere.
- **rustls with the default `aws-lc-rs` provider** — needs cmake, which the Dev Container
  deliberately lacks. Revisit if FIPS becomes a requirement.
- **A hand-rolled HTTP client on raw hyper instead of `reqwest`** — polling with gzip, TLS and
  timeouts is reqwest's job; it is client-side only.
- **One listener, authenticated by path prefix** — leaves both audiences on one exposure and one
  TLS policy, and the port, the only thing a firewall can act on, says nothing about who is talking.
- **Split strictly by path — everything under `/api/v1` moves** — puts the download on a port an
  Agent cannot derive, turning `advertised_url` into an obligation whose omission shows up as a
  failed rollout, not a startup error.
- **Three listeners (OpAMP, downloads, Operator)** — a moving part nothing needs; `advertised_url`
  covers the mirror case.
- **The download route on both listeners** — one resource at two addresses under two policies
  invites protecting one and forgetting the other, and the offer names only one.
- **A reverse proxy that splits the planes** — makes a security property depend on an artifact
  this project does not ship or test, and a TLS-terminating proxy cannot give the Agent plane a
  handshake-level client-certificate requirement.
- **Operator plane on `0.0.0.0:4321` by default, or a merged single-port mode** — the first is the
  same exposure on a new port; the second is two operating modes, one of which is the one the split
  exists to avoid.
- **A `tower-http` `TimeoutLayer`** — middleware runs after hyper has parsed the request line and
  headers, so it cannot see the phase at issue, and it would need exclusions for the WebSocket, the
  download and the upload.
- **Waiting for the axum release carrying the timer** — fixes only `axum::serve`; the TLS listeners
  run on `axum_server` anyway.
- **Keeping `axum::serve` and hand-rolling the accept loop on `hyper-util`** — the same knob for the
  price of owning connection tracking and shutdown, which `axum_server` implements.
- **A per-plane `header_read_timeout_secs` key** — a key whose only sensible value is the framework
  default is a knob nobody turns.
- **The bound as a shared layer in `crates/opamp`** — it is one framework's builder knob in one
  binary; the Client's outbound transports have no counterpart
  ([ADR-0011](0011-workspace-crates-and-configuration.md)).
- **The weaker posture: TLS 1.2 and 1.3, optional TLS, plaintext on any address, an Agent plane on
  `0.0.0.0:4320` and `ws://` as the Client's default** — a Server that starts with no
  configuration and a fleet that connects without certificates are convenient. Rejected: security
  comes before convenience, and Q-1 asks for a refusal at startup, not a working insecure default.
- **Plaintext allowed with a warning off LOOPBACK** — a warning is read after the credential has
  crossed the network. Q-1 asks for a refusal that names the setting.
- **`localhost` and private ranges as loopback** — a host name is whatever the resolver answers,
  and an RFC 1918 address leaves the host. Only the two literals cannot.
- **Plaintext on the Operator plane on LOOPBACK** — the loopback does not leave the host. Not
  chosen: `[tls]` is required anyway, and one rule for both planes leaves nothing to get wrong. A
  browser on the host reaches the plane by the Server's name over an SSH tunnel, or trusts a
  certificate that names `127.0.0.1`.
- **HTTP/2 bounds and connection caps as `server.toml` keys alike** — a cap depends on the size of
  the fleet, so an operator must be able to raise it. The HTTP/2 numbers depend on the protocol's
  use, which is the same in every deployment.

## Sources / Prior art

- [OpAMP specification — Transport, Plain HTTP Transport, WebSocket Transport](https://github.com/open-telemetry/opamp-spec/blob/v0.18.0/specification.md)
  — the dual-transport SHOULD, `Content-Type: application/x-protobuf` and its use for detection,
  the gzip MUST, the 30 s default poll, port 4320 and `/v1/opamp`, varint framing, backoff with
  jitter; and the post-Baseline 64 MiB message-size recommendation recorded in
  [`CONFORMANCE.md`](../CONFORMANCE.md).
- [`opamp-go`](https://github.com/open-telemetry/opamp-go) — both transports behind one client
  interface (`NewWebSocket`/`NewHTTP`); its `internal/examples/server` runs the OpAMP endpoint and
  the demo UI as two separate servers; its
  [`server/serverimpl.go`](https://github.com/open-telemetry/opamp-go/blob/main/server/serverimpl.go)
  sets no timeouts, so the connection bound goes beyond the oracle without diverging from it.
- [`tokio-tungstenite`](https://crates.io/crates/tokio-tungstenite),
  [`reqwest`](https://crates.io/crates/reqwest), [`axum-server`](https://crates.io/crates/axum-server),
  [`rustls`](https://crates.io/crates/rustls) — the stack; the `ring` provider avoids cmake and
  aws-lc builds.
- [`rustls::crypto::CryptoProvider`](https://docs.rs/rustls/0.23/rustls/crypto/struct.CryptoProvider.html)
  and the `tls12` crate feature — a provider without a TLS 1.2 suite cannot negotiate TLS 1.2;
  [`reqwest::ClientBuilder::min_tls_version`](https://docs.rs/reqwest/0.13/reqwest/struct.ClientBuilder.html#method.min_tls_version).
- [RFC 8446](https://www.rfc-editor.org/rfc/rfc8446) — TLS 1.3 and its five suites, three of which
  the `ring` provider implements; [RFC 8996](https://www.rfc-editor.org/rfc/rfc8996) deprecates
  TLS 1.0 and 1.1.
- [`hyper::server::conn::http2::Builder`](https://docs.rs/hyper/1/hyper/server/conn/http2/struct.Builder.html)
  — `max_concurrent_streams`, `keep_alive_interval` and `keep_alive_timeout`, the last two needing
  the same `Timer` as the HTTP/1 bound.
- [HashiCorp Vault — TCP listener configuration](https://developer.hashicorp.com/vault/docs/configuration/listener/tcp)
  — several listeners, each with its own client-certificate policy.
- [etcd — transport security model](https://etcd.io/docs/v3.6/op-guide/security/) and
  [configuration flags](https://etcd.io/docs/v3.4/op-guide/configuration/) — client and peer
  listeners separated by audience, trust material following the audience.
- [Kubernetes — securing control-plane components](https://kubernetes.io/docs/tasks/administer-cluster/configure-upgrade-etcd/)
  and [Kubernetes API security fundamentals (Datadog Security Labs)](https://securitylabs.datadoghq.com/articles/kubernetes-security-fundamentals-part-2/)
  — unauthenticated control surfaces (`kube-controller-manager`, `kube-scheduler`) bind loopback.
- [Bindplane — networking requirements](https://docs.bindplane.com/production-checklist/bindplane/networking-requirements)
  — REST and OpAMP on one port, defensible there because that REST API is authenticated.
- [axum PR #3478](https://github.com/tokio-rs/axum/pull/3478), the
  [axum changelog](https://github.com/tokio-rs/axum/blob/main/axum/CHANGELOG.md) and
  [axum issue #2741](https://github.com/tokio-rs/axum/issues/2741) — upstream installs the timer and
  applies hyper's 30 s default; an incomplete request costs about 1 MB per connection against about
  2.5 kB for an idle one.
- [`hyper::server::conn::http1::Builder::header_read_timeout`](https://docs.rs/hyper/1.11.0/hyper/server/conn/http1/struct.Builder.html#method.header_read_timeout)
  — requires a `Timer`, panics if configured without one; `axum-server` 0.7 `http_builder` and its
  10-second rustls handshake default, read from the vendored sources.
- [Diving into Go's HTTP server timeouts](https://adam-p.ca/blog/2022/01/golang-http-server-timeouts/)
  and nginx's `client_header_timeout` (60 s) — the range 30 s sits in.
- [`cargo-deny` — `bans.features`](https://embarkstudios.github.io/cargo-deny/checks/bans/cfg.html)
  — denies a crate feature anywhere in the graph.
- [`HARDENING.md`](../HARDENING.md) — H9 (the handshake-level client-certificate requirement), H11
  (the TLS version floor), H16 (connection cap), H17 (HTTP/2 bounds), H18 (the Client's own
  listeners).

## Consequences

- Positive: full transport conformance on both ends; any third-party client or server pairs with
  this project regardless of its transport choice; one TLS stack, identical on Linux, macOS and
  Windows, and the client cross-builds need no system TLS library.
- Positive: no credential, configuration or package crosses the network unencrypted, and nothing
  older than TLS 1.3 is negotiated. A configuration that would break this does not start, and its
  message names the setting to fix.
- Positive: the default publishes neither plane on the network; authenticating the Operator plane
  is a decision about one listener, with no per-path exemption for Agent traffic.
- Positive: an unauthenticated peer cannot pin a connection open indefinitely on either plane,
  cannot hold more connections than the cap, and cannot hold an HTTP/2 connection with silent or
  unbounded streams. Both planes drain on shutdown within a bound.
- Negative / trade-offs: a Server with no configuration no longer starts. The operator provides a
  certificate and key before the first start, and sets `listen` before an Agent from another host
  can connect.
- Negative / trade-offs: a third-party client or server that speaks only TLS 1.2 cannot pair with
  this project, and a Client whose configuration names a `ws://` or `http://` endpoint off
  LOOPBACK, or a host name such as `localhost`, stops at startup until the endpoint is changed.
- Negative / trade-offs: two transports mean two code paths and reconnect behaviour to test on each
  end; `reqwest` is a sizeable dependency for a poll loop.
- Negative / trade-offs: every operator entry point is on port 4321, and driving the API from
  another host takes a deliberate `[rest] listen` with TLS and authentication. On LOOPBACK the
  Operator plane serves plaintext even when `[tls]` is set. The download route is absent from the
  OpenAPI document, so a generated client has no method for it — no operator flow calls it.
- Negative / trade-offs: a peer needing more than 30 s for its headers is hung up on. A fleet
  reconnecting after a restart meets the connection cap if it is larger than the cap, and the
  HTTP/2 numbers are chosen, not yet measured against a real fleet.
- Follow-ups: per-listener TLS material if a deployment needs a public certificate for operators
  and a private one for Agents; a connection cap on the Client's Gateway endpoint; measuring the
  HTTP/2 bounds against a real fleet; reducing clauses 11 and 12 to the framework's defaults once
  axum installs the timer.

## Enforcement

- [`crates/fleet-server/tests/http_transport.rs`](../../crates/fleet-server/tests/http_transport.rs) —
  `gzip_request_bodies_are_accepted`, `an_oversized_request_body_is_refused_with_413`,
  `a_gzip_body_that_inflates_past_the_limit_is_refused_with_413`,
  `transport_detection_rejects_a_missing_protobuf_content_type` (clauses 1, 3).
- [`crates/fleet-server/tests/ws_transport.rs`](../../crates/fleet-server/tests/ws_transport.rs) —
  `a_framed_report_is_answered`, `an_oversized_frame_closes_the_connection_with_1009` (clauses 1, 3).
- [`crates/fleet-agent/src/config.rs`](../../crates/fleet-agent/src/config.rs) —
  `scheme_selects_the_transport`, `rejects_an_unknown_scheme_and_unknown_keys` (clause 2);
  [`crates/fleet-agent/tests/http_transport_e2e.rs`](../../crates/fleet-agent/tests/http_transport_e2e.rs) —
  `a_configuration_rollout_reaches_a_polling_client` (clause 2).
- [`crates/fleet-agent/src/transport/mod.rs`](../../crates/fleet-agent/src/transport/mod.rs) —
  `backoff_doubles_and_caps_within_its_jittered_bounds`, `two_backoffs_do_not_produce_the_same_ladder`
  (clause 4).
- Clause 5: reqwest's `rustls-no-provider` feature refuses to build a client without the
  process-wide `ring` provider, and a dependency pulling `aws-lc-rs` fails to build in the Dev
  Container, which has no cmake; [`crates/fleet-server/tests/mutual_tls.rs`](../../crates/fleet-server/tests/mutual_tls.rs)
  exercises the rustls stack on both ends.
- [`crates/fleet-server/src/config.rs`](../../crates/fleet-server/src/config.rs) —
  `the_operator_plane_defaults_to_loopback_and_is_configurable`, `two_planes_on_one_address_are_refused`
  (clauses 6, 7, 10).
- [`crates/fleet-server/tests/packages.rs`](../../crates/fleet-server/tests/packages.rs) —
  `the_artifact_is_served_where_the_agents_are_and_not_on_the_operator_plane`;
  [`crates/fleet-server/tests/auth.rs`](../../crates/fleet-server/tests/auth.rs) —
  `the_rest_api_stays_open_on_its_own_listener_when_the_opamp_endpoint_is_guarded` (clauses 6, 8).
- [`crates/fleet-server/tests/connection_setup.rs`](../../crates/fleet-server/tests/connection_setup.rs) —
  `a_connection_that_never_finishes_its_headers_is_hung_up_on`,
  `an_established_session_outlives_the_header_bound` (clauses 11, 14).

These tests carry `Verifies: ADR-0038`:

- [`crates/opamp/src/tls.rs`](../../crates/opamp/src/tls.rs) —
  `the_provider_offers_tls_1_3_suites_alone` (clause 5);
  [`crates/opamp/src/endpoint.rs`](../../crates/opamp/src/endpoint.rs) —
  `only_the_loopback_literals_are_loopback` (clause 15).
- [`crates/opamp/tests/server_listen.rs`](../../crates/opamp/tests/server_listen.rs) —
  `a_client_offering_only_tls_1_2_is_refused` (clause 5),
  `a_plaintext_listener_off_the_loopback_is_refused` (clause 15),
  `connections_past_the_cap_are_refused_while_established_ones_keep_working` (clause 16).
- [`crates/opamp/tests/client_connection.rs`](../../crates/opamp/tests/client_connection.rs) —
  `a_tls12_only_server_fails_the_handshake` (clause 5).
- [`crates/opamp/src/client/connection.rs`](../../crates/opamp/src/client/connection.rs) —
  `plaintext_is_refused_off_the_loopback_literals`,
  `plaintext_off_the_loopback_is_refused_before_connecting` (clauses 2, 15).
- [`crates/fleet-agent/src/config.rs`](../../crates/fleet-agent/src/config.rs) —
  `the_default_endpoint_is_wss_on_the_loopback`,
  `a_plaintext_endpoint_off_the_loopback_is_refused_at_startup` (clause 2).
- [`crates/fleet-server/src/config.rs`](../../crates/fleet-server/src/config.rs) —
  `the_agent_plane_defaults_to_the_loopback`, `a_server_without_tls_is_refused_at_startup`
  (clauses 6, 9), `the_operator_plane_requires_authentication_off_the_loopback` (clause 7),
  `max_connections_defaults_per_plane_and_zero_is_refused` (clause 16).
- [`crates/fleet-server/tests/mutual_tls.rs`](../../crates/fleet-server/tests/mutual_tls.rs) —
  `a_client_certificate_is_required_in_the_handshake_on_the_agent_plane` (clause 8).
- [`crates/fleet-server/tests/packages.rs`](../../crates/fleet-server/tests/packages.rs) —
  `a_plaintext_source_off_the_loopback_is_refused` (clause 18).

**Not mechanically decidable:** clauses 12 and 13 — a 10-second handshake deadline and a 10-second
shutdown drain are named constants in `listen.rs`, and no test waits them out; review keeps both
planes on `listen::plane` and the shared `Handle`. Clause 17 — the HTTP/2 stream cap and keep-alive
bounds are named constants set in `opamp::server::listen`, and the workspace has no HTTP/2 client to
drive a peer past them; review checks that every listener goes through `Listener::serve`.
