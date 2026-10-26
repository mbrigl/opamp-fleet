# ADR-0013: Both transports over TLS 1.3, two planes split by audience, and bounded input

- **Status:** 🟢 accepted
- **Date:** 2026-10-17
- **Deciders:** Markus Brigl
- **Applies to:** the OpAMP endpoint and both transports in `crates/fleet-server/src/transport.rs` and `crates/fleet-agent/src/transport/`, TLS in `crates/opamp/src/tls.rs`, `crates/fleet-server/src/tls.rs` and `crates/fleet-agent/src/tls.rs`, the plaintext rule in `crates/opamp/src/client/connection.rs`, how every listener binds and serves and how it reads a request body or a WebSocket message in `crates/opamp/src/server/`, the package upload and the package URL probe in `crates/fleet-server/src/api.rs`, `crates/fleet-server/src/listen.rs` and `main.rs`, the `rustls` features in `Cargo.toml`, the `listen`, `max_connections`, `[rest]`, `[tls]` and `[rest.tls]` keys of `server.toml` and their parsing and startup checks in `crates/fleet-server/src/config.rs`, the certificates `server pki status` lists and the running Server watches in `crates/fleet-server/src/pki.rs`, `config/server.toml`, the message limit in `crates/fleet-server/src/agent_rate.rs`, `on_message` and `on_unreadable` of the Server's handler, the enrolment refusals in `Fleet::enrol` and the download guard `admit_download` in `crates/fleet-server/src/transport.rs`, every `Unavailable` reply in `crates/fleet-server/src/fleet.rs`, the `[agent_rate_limit]` section of `server.toml` and its startup warning, the `agent_rate.throttled` audit event, and every document that tells an operator which certificate the Operator plane serves or how a browser or `opamp-fleetctl` trusts it

## Context

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

TLS is part of the transport. G-17 requires Client-to-Server traffic to be TLS-protected. The
Strategy *Security before convenience* narrows that: every connection that leaves the host is
TLS 1.3, and plaintext is accepted on the loopback alone. Q-1 adds that a configuration breaking
this is refused at startup, naming the setting, and never warned about. The Dev Container
([ADR-0004](0004-dev-container-runtime.md)) has neither OpenSSL headers nor cmake, which
`aws-lc-rs`-backed rustls needs.

In this ADR, **LOOPBACK** means the IP literals `127.0.0.1` and `::1`. A host name is never
LOOPBACK, `localhost` included: what it resolves to is a resolver's answer, not the
configuration's.

The Server has two audiences with nothing in common but the process. Agents connect from the
whole estate, hold long-lived WebSocket sessions, speak protobuf, and prove fleet membership
([ADR-0022](0022-admission-by-client-certificate-and-revocation.md)). Operators and portals
connect from a few places, speak JSON over short requests, and act with authority over every
Agent: reading the fleet, rewriting Configurations, uploading and rolling out Packages. One
address for both means one exposure, one TLS policy, and one answer to "who may connect" for two
questions with different answers. It also forces client-certificate checks into route code, since
a browser presents none.

One route crosses that line: the package download. Its path is under `/api/v1`, but its audience
is Agents, and the offered `download_url` is a path the Client resolves against its own OpAMP
endpoint. Where the route is served, and what protects the artifact, is
[ADR-0028](0028-packages-signatures-and-delivery.md)'s.

The Agent plane's certificate is made by `opamp-fleetctl pki` from the fleet's own server CA
([ADR-0033](0033-the-operator-program-opamp-fleetctl.md)). The Clients trust that CA because
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
size alone. [`HARDENING.md`](../HARDENING.md) names these H16 and H17.

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
([ADR-0022](0022-admission-by-client-certificate-and-revocation.md)). From there on, none of these
bounds how many messages an admitted peer sends:

- the connection cap (clause 19) bounds sockets;
- the message size limit (clause 3) bounds each message;
- the pace floor (clause 18) bounds how slowly a message may arrive;
- the throttle of failed admissions in ADR-0022 counts failures per peer address, and a member
  never fails.

None of them bounds how often. One WebSocket session can send a full report as fast as the Server
decodes it. Each report takes the fleet lock, may write the Agent's record to disk
([ADR-0025](0025-the-fleet-record-of-agents-and-hosts.md)), may compose an offer, and may ask for
a certificate to be signed. A plain-HTTP poller can do the same with one request per message. The
package download route is the Agent plane's other member route, and it costs more: each request
scans the fleet under its lock to find an Agent the artifact is offered to
([ADR-0028](0028-packages-signatures-and-delivery.md)).

The specification holds that no vulnerability can be ruled out (Strategy *Security before
convenience*). A host taken over, or a Client with a bug that loops, is still a member, and
without a bound it can take the Server's time from every other Agent. Q-2 asks that untrusted
input be bounded, and an admitted peer's message is still input from the network.

Forces that shape the rate limit:

- **The host is the unit admission knows.** A certificate names the host it was issued to as
  `urn:opamp-fleet:host:<id>` (ADR-0022, the host a certificate names). `instance_uid` is
  self-asserted (ADR-0022), so a limit keyed on it alone binds nothing: a peer chooses a new uid
  for each message. A certificate an operator provisioned outside the CSR flow names no host until
  it is first renewed. It is still one certificate, told apart by its issuer and serial (`CertId`
  in [`revocation.rs`](../../crates/fleet-server/src/revocation.rs)).
- **A Gateway carries other hosts' Agents.** Behind a marked Gateway every Agent arrives under the
  Gateway's certificate (ADR-0022, [ADR-0016](0016-client-modes-and-the-gateway.md)). A per-host
  limit there would put a whole site under one host's budget. A per-uid limit alone bounds
  nothing, because a downstream peer can cycle uids. The Gateway's `max_carried_agents` does not
  stop that either: it bounds one downstream connection, and a plain-HTTP downstream exchange is a
  connection of its own ([`gateway/mod.rs`](../../crates/fleet-agent/src/gateway/mod.rs) creates a
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
    open (ADR-0022), and when the audit record holds back a CSR
    ([ADR-0024](0024-an-append-only-audit-record.md)).
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
- **A host speaks for at most 256 Agents** (ADR-0022), and after a reconnect the Client sends one
  full report for each of them. At the Baseline's 30 s heartbeat or poll, 256 Agents send about
  8.5 messages a second.
- **Refusals already have a bounded record.** [ADR-0024](0024-an-append-only-audit-record.md)
  writes at most ten refusals of one event per peer address and second, and counts the rest. It
  counts every decision whose entry could not be queued in the next entry's `unrecorded_before`.

## Decision

We will implement both OpAMP transports on both ends over rustls with the `ring` provider and TLS
1.3 alone, accept plaintext only on LOOPBACK, serve the Server on two TLS listeners split by
audience (an Agent plane and a loopback-default Operator plane that may serve a certificate of its
own), build each in one place that bounds connection setup, the number of connections, HTTP/2, and
the pace of every request body and WebSocket message, and limit what an admitted peer sends on the
Agent plane with token buckets.

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
   ([ADR-0014](0014-connection-settings-that-never-lead-to-plaintext.md)).

3. **Gzip and a message size limit on both transports.** The Server accepts gzip request bodies
   (a Baseline MUST) and enforces `max_message_size_bytes` (default 64 MiB, `0` refused at
   startup) in both directions and on both transports, applied after decompression: `413` on plain
   HTTP, a `1009` close on WebSocket, and an oversized reply is discarded rather than sent. The
   shared helpers that implement this live in `crates/opamp`
   ([ADR-0009](0009-five-crates-on-tokio-and-axum.md)).

4. **Reconnection belongs to the Client's transport layer.** A lost connection is retried with
   exponential backoff from one second, capped at one minute, with jitter, which the Baseline asks
   for so that a restarted Server is not hit by the whole fleet on the same instants. After a
   reconnect the Client sends a full status report. The Server keys no state on the connection
   ([ADR-0016](0016-client-modes-and-the-gateway.md)).

5. **TLS is TLS 1.3 alone, on rustls with the `ring` provider, everywhere; no OpenSSL, no system
   TLS library.** `reqwest` requires the `tls12` feature of `rustls`, so the floor is held where
   every configuration is built rather than by the build. `opamp::tls::provider()` is the `ring`
   provider with the three TLS 1.3 suites and nothing else: `TLS13_AES_256_GCM_SHA384`,
   `TLS13_AES_128_GCM_SHA256` and `TLS13_CHACHA20_POLY1305_SHA256`. Every rustls configuration of
   both ends is built from it with the protocol versions pinned to TLS 1.3
   ([ADR-0009](0009-five-crates-on-tokio-and-axum.md)). Each binary installs it process-wide at
   startup, and every `reqwest` client is built with a minimum of TLS 1.3. This covers every
   connection: OpAMP, the REST API, downloads, mirrors, own telemetry and the Server's outbound
   probe. `axum-server` (`tls-rustls-no-provider`) terminates TLS on the Server,
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
   | Agent | `listen` | `127.0.0.1:4320` | always | `/v1/opamp`, the package download route, and the Gateways' revocation list ([ADR-0022](0022-admission-by-client-certificate-and-revocation.md)) |
   | Operator | `[rest] listen` | `127.0.0.1:4321` | always | `/api/v1/…`, `/api/v1/openapi.json`, `/api/v1/docs` with its vendored Redoc bundle, and the bundled UI at `/` |

   The Agent plane listens on LOOPBACK by default. Serving the estate is one deliberate line in
   `server.toml`, together with the TLS material it needs. `[rest]` is a table so that the plane's
   own settings (its authentication,
   [ADR-0022](0022-admission-by-client-certificate-and-revocation.md), and its connection cap)
   live inside it; unknown keys are refused as everywhere
   ([ADR-0009](0009-five-crates-on-tokio-and-axum.md)). The router is split accordingly:
   `server::agent_app(state, admission)` and `server::operator_app(state, auth)`.

8. **The Operator plane is loopback by default.** Its reachability is its first protection; a
   network-open default would export the fleet's full control surface. Reaching it from elsewhere
   is one deliberate line in `server.toml`, or an SSH tunnel. It serves TLS with the material
   clause 9 gives it, on every address. Off LOOPBACK it requires authentication as the admission
   decision states; a Server lacking it is refused at startup.

9. **`[tls]` is required and serves the Agent plane, and the Operator plane too unless
   `[rest.tls]` is present.** `[tls] cert_file` and `key_file` serve the Agent plane, with ALPN
   `h2` and `http/1.1`. A Server without `[tls]` is refused at startup with a message naming the
   section. `[tls] client_ca_file` applies to the Agent plane alone, and what it requires is
   [ADR-0022](0022-admission-by-client-certificate-and-revocation.md)'s. Without `[rest.tls]`, the
   Operator plane serves `[tls] cert_file` and `key_file` as well, with the same ALPN.

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
    of [ADR-0033](0033-the-operator-program-opamp-fleetctl.md). A file named in both `[tls]` and
    `[rest.tls]` is listed once.

14. **Two listeners always; addresses that cannot both be bound fail at startup.** There is no
    single-port mode. The same port on the same address, or on an address that covers every
    interface (`0.0.0.0:4320` and `127.0.0.1:4320`), is refused at load with a message naming both
    keys. Both listeners are bound before either serves, and a bind failure names the plane.

15. **Every plane's server is built in one place, with an HTTP/1 header-read timeout of 30
    seconds.** `listen::plane` serves both planes on `opamp::server::listen`
    ([ADR-0009](0009-five-crates-on-tokio-and-axum.md)), which serves plain and TLS through
    `axum_server`, installs `hyper_util::rt::TokioTimer` on the HTTP/1 builder, and sets
    `header_read_timeout` to `HEADER_READ_TIMEOUT` (30 s, hyper's own default and what axum
    applies once it installs the timer itself). The timeout is a parameter of the serving function
    so a test can drive it short. `hyper-util` is a dependency of `opamp`'s `server` feature for
    this.

16. **The TLS handshake deadline is stated, not inherited.** `TLS_HANDSHAKE_TIMEOUT` is 10 seconds
    on both planes' acceptors: `axum_server`'s default value, named in this code so it reads as a
    decision.

17. **Shutdown is one bounded drain for both planes.** One `axum_server::Handle` is shared by both
    listeners. An interrupt calls `graceful_shutdown` with `SHUTDOWN_DRAIN` (10 s), whatever has
    not ended by then is cut, and the Agent-record flush of
    [ADR-0025](0025-the-fleet-record-of-agents-and-hosts.md) runs afterwards.

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
    limit.

20. **HTTP/2 is bounded on every listener.** `opamp`'s listener sets the HTTP/2 builder of every
    listener it serves, so every TLS listener, where ALPN offers `h2`, carries the bounds.
    `H2_MAX_CONCURRENT_STREAMS` is 100 per connection. A keep-alive ping is sent every
    `H2_KEEP_ALIVE_INTERVAL` (30 s), and a peer that leaves one unanswered for
    `H2_KEEP_ALIVE_TIMEOUT` (20 s) is dropped. They are named constants, not keys, as clause 18
    states for the HTTP/1 bound.

21. **The Server's outbound HTTP is TLS 1.3 and `https://`.** The probe that asks a package source
    whether it holds an artifact speaks `https://` over the provider of clause 5. An `http://`
    source is probed only on LOOPBACK; off LOOPBACK it is refused with a message naming the URL.
    The probe keeps its refusal of internal addresses and redirects.

22. **`[agent_rate_limit]` in `server.toml`.** The rate limit covers an admitted peer's
    `AgentToServer` messages on `/v1/opamp` over both transports and its requests on the package
    download route. The section is optional, and its defaults are in force when it is absent. Any
    other key is refused, as in every section. No value switches the limit off.

    | Key | Default | Rule |
    |---|---|---|
    | `messages_per_sec` | `10` | Tokens added to a host's or an Agent's bucket per second, continuously. `0` is refused at startup with a message naming the key. |
    | `burst` | `300` | That bucket's capacity, enough for one full report from each of the 256 Agents a host may speak for (ADR-0022) after a reconnect. A new bucket starts full. `0` is refused at startup with a message naming the key. |
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
    the failed-admission throttle of ADR-0022.

    The Server does not close the connection. A WebSocket Client that honours the Baseline closes
    it itself and comes back after 30 s; messages that arrive before it does are counted and
    answered the same way. A throttled message leaves `sequence_num` where it was, so the next
    message the Server processes from that Agent is answered with `ReportFullState`.

26. **What a download past the limit gets.** The download route is plain HTTP and not OpAMP, so it
    has no `ServerErrorResponse`. A request past the limit is answered `429` with
    `Retry-After: 30`, and the artifact is not read. This `429` differs from the one the
    failed-admission throttle of ADR-0022 sends:

    | | The failed-admission throttle (ADR-0022) | This clause |
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
    [ADR-0022](0022-admission-by-client-certificate-and-revocation.md) and
    [ADR-0024](0024-an-append-only-audit-record.md). Those replies are:
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
    to the audit record ([ADR-0024](0024-an-append-only-audit-record.md)) as event
    `agent_rate.throttled` with outcome `throttled`. The entry carries:
    - the peer address;
    - `host`, or `serial` and the CA role where the certificate names no host;
    - `instance_uid`, hex, where the message carries a valid one;
    - `bucket`: `agent`, `host` or `gateway`, naming the bucket that was empty;
    - `route`: `opamp` with the `transport`, or `download`.

    The entries are aggregated under ADR-0024's bound on refusals, by event and peer address. A
    host that floods costs at most ten entries and one `agent_rate.throttled.aggregated` count per
    address and second. The refusal never waits for its entry. A refusal that finds the audit
    channel full is counted in the next entry's `unrecorded_before` (ADR-0024). A message or
    download the limit lets through is recorded by nothing new.

**Out of scope:**

- Authentication on either plane and what a client certificate must prove, including whether the
  Agent plane's handshake requires it
  ([ADR-0022](0022-admission-by-client-certificate-and-revocation.md)).
- Where the package download route is served and what it requires
  ([ADR-0028](0028-packages-signatures-and-delivery.md)), and which hosts a Client downloads from
  (ADR-0028).
- Connection settings and the heartbeat offer
  ([ADR-0014](0014-connection-settings-that-never-lead-to-plaintext.md)).
- Which bucket a message or a download is counted in
  ([ADR-0016](0016-client-modes-and-the-gateway.md)).
- The rate of admission attempts.
- Where the Client's own telemetry goes ([ADR-0017](0017-what-an-agent-reports-about-itself.md)).
- Response compression on plain HTTP.
- A connection cap on the Client's own listeners, the Gateway endpoint and the Supervisor
  Endpoint, which carry the bounds of `opamp`'s listener and no cap.
- A rate limit on the Operator plane, guarded by the Operator plane's authentication and the
  failed-admission throttle of ADR-0022.
- A rate limit of the Gateway's downstream endpoint, which applies none of its own and forwards
  the Server's `Unavailable` unchanged.
- Limits per message kind, or limits that weigh a message by its cost.
- The rate of WebSocket upgrades and TLS handshakes.
- Whether the Client's downloader honours `Retry-After`.
- Reloading a certificate without a restart.
- A client certificate for operators on the Operator plane.
- A certificate from `opamp-fleetctl` for the Operator plane. Its `pki server-cert` makes one from
  the server CA, which `[rest.tls]` may name like any other.
- Selecting a certificate by SNI on one listener.

## Alternatives considered

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
  ([ADR-0009](0009-five-crates-on-tokio-and-axum.md)).
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
- **A bucket per `instance_uid` for every peer.** The uid is self-asserted (ADR-0022), so a peer
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

## Sources / Prior art

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
  [Kubernetes API security fundamentals (Datadog Security Labs)](https://securitylabs.datadoghq.com/articles/kubernetes-security-fundamentals-part-2/):
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
- [`HARDENING.md`](../HARDENING.md): H9 (the handshake-level client-certificate requirement), H11
  (the TLS version floor), H16 (connection cap), H17 (HTTP/2 bounds), H18 (the Client's own
  listeners).
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
- [RFC 6585 §4](https://datatracker.ietf.org/doc/html/rfc6585#section-4) (`429`).
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
- [ADR-0007](0007-an-adr-may-supersede-named-clauses-of-another.md): the numbered clauses of this
  Decision, which a later ADR may supersede one by one.

## Consequences

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
  ADR-0024 aggregates by address. Behind a NAT or a Gateway only the first ten entries of each
  second name the host or the Agent.
- Follow-ups:
  - a connection cap on the Client's Gateway endpoint;
  - measuring the HTTP/2 bounds against a real fleet;
  - reducing clauses 15 and 16 to the framework's defaults once axum installs the timer;
  - rate limits weighted by message kind;
  - the Client's downloader honouring `Retry-After`;
  - the running Server's daily expiry check reads the file, not the certificate it serves, so a
    file replaced without a restart silences the warning while the old certificate is still
    served; this holds for `[tls]` and `[rest.tls]` alike, and is a fix within ADR-0033's expiry
    rule;
  - whether to reload certificates without a restart;
  - whether operators should present client certificates on the Operator plane.

## Enforcement

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
- [`crates/fleet-agent/src/transport/mod.rs`](../../crates/fleet-agent/src/transport/mod.rs):
  `backoff_doubles_and_caps_within_its_jittered_bounds`,
  `two_backoffs_do_not_produce_the_same_ladder` (clause 4).
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

These tests carry `Verifies: ADR-0013`:

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
  `plaintext_is_refused_off_the_loopback_literals`,
  `plaintext_off_the_loopback_is_refused_before_connecting` (clauses 2, 6).
- [`crates/fleet-agent/src/config.rs`](../../crates/fleet-agent/src/config.rs):
  `the_default_endpoint_is_wss_on_the_loopback`,
  `a_plaintext_endpoint_off_the_loopback_is_refused_at_startup` (clause 2).
- [`crates/fleet-server/src/config.rs`](../../crates/fleet-server/src/config.rs):
  `the_agent_plane_defaults_to_the_loopback`, `a_server_without_tls_is_refused_at_startup`
  (clauses 7, 9), `the_operator_plane_requires_authentication_off_the_loopback` (clause 8),
  `max_connections_defaults_per_plane_and_zero_is_refused` (clause 19).
- [`crates/fleet-server/tests/packages.rs`](../../crates/fleet-server/tests/packages.rs):
  `a_plaintext_source_off_the_loopback_is_refused` (clause 21).

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

Tests of the rate limit, each marked `Verifies: ADR-0013`:

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
  refusal counts no failure toward the failed-admission throttle of ADR-0022 (clauses 24, 26).
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
pin what clause 24 counts as well:
in `crates/fleet-server/src/agent_rate.rs`, `a_host_is_one_bucket_whatever_its_agents_report`,
`a_member_certificate_without_a_host_is_counted_by_issuer_and_serial`,
`an_enrolment_connection_is_counted_by_its_peer_address`,
`marking_a_gateway_takes_effect_at_the_next_message`,
`behind_a_gateway_a_message_passes_its_agents_bucket_and_the_aggregate` and
`a_message_naming_no_agent_behind_a_gateway_counts_in_the_aggregate_alone`; in
`crates/fleet-server/tests/mutual_tls.rs`,
`two_certificates_of_one_host_share_a_bucket_and_two_hosts_do_not` and
`a_bootstrap_certificate_shared_by_two_addresses_is_two_buckets`.

**Not mechanically decidable:** clauses 16 and 17. A 10-second handshake deadline and a 10-second
shutdown drain are named constants in `listen.rs`, and no test waits them out; review keeps both
planes on `listen::plane` and the shared `Handle`. Clause 20: the HTTP/2 stream cap and keep-alive
bounds are named constants set in `opamp::server::listen`, and the workspace has no HTTP/2 client
to drive a peer past them; review checks that every listener goes through `Listener::serve`.
