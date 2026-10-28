# ADR-0015: Client modes, the Supervisor Endpoint and the Gateway

- **Status:** 🟢 accepted
- **Date:** 2026-10-24
- **Deciders:** Markus Brigl
- **Applies to:** crates/fleet-agent/src/gateway/ (endpoint, upstream pool and its release, downstream admission, the enrolling peer, forwarding and routing, bindings in `policy.rs` and `cache.rs`, their store under `<state_dir>/gateway-packages/bindings`), crates/fleet-agent/src/supervisor/endpoint.rs, the token threaded through `SupervisorContext` and `Runner` in crates/fleet-agent/src/supervisor/, the environment every Managed Process is started with, the `[gateway]` and `[gateway.tls]` sections of `supervisor.toml` and their parsing in crates/fleet-agent/src/config.rs, every place either end keeps per-Agent state, the renewal and enrolment CSR in crates/fleet-agent/src/csr.rs and crates/fleet-agent/src/service/enrol.rs, the reading of a CSR and its renewal and bootstrap proofs in crates/fleet-core/ and crates/fleet-server/src/ca.rs, the host and predecessor a CSR is issued to in crates/fleet-server/src/fleet.rs, the requester an enrolment request is filed under in crates/fleet-server/src/enrolment.rs and crates/fleet-server/src/transport.rs, the admission throttle's buckets in crates/fleet-server/src/transport.rs and crates/fleet-server/src/agent_rate.rs, the host derived for a certificate that names none in crates/fleet-core/src/host.rs, the `opampextension` configuration the documentation shows, and every document that tells an operator what a Gateway passes on or keeps on disk, how an Agent behind one is bound to its host or renews its certificate, or how a host behind one enrols

## Context

The [specification](../SPECIFICATION.md) asks the client side to cover three shapes: supervise
local processes, serve a Collector that speaks OpAMP itself, and act as a Gateway that carries
many other Clients upstream over a small Connection Pool (G-14, G-15, G-16). They are not three
peers.

**Supervising and serving an OpAMP-speaking Collector are inseparable.** The Collector's
`opampextension` is an OpAMP *client only*; it must be given something to connect to. The natural
counterpart is the Supervisor that owns that Collector, because it holds the configuration to
hand down and the upstream connection to relay onto. An endpoint without a Supervisor has nothing
to relay to, and a Supervisor without one cannot manage an extension-carrying Collector.
Gatewaying for other machines is genuinely independent: a host may supervise, gateway, or do both.

**The protocol provides for multiplexing.** The Baseline documents `ServerToAgent.instance_uid`:
*"When communication with multiple Agents is multiplexed into one WebSocket connection (for
example when a terminating proxy is used) the `instance_uid` field allows to distinguish which
Agent the ServerToAgent message is addressed to."* Over plain HTTP the same pooling follows from
each request carrying its own `instance_uid`. Any end that keys Agent state on the connection
misroutes messages as soon as a Gateway sits in front of it, and that assumption is the expensive
one to remove later.

**A Gateway terminates.** The Gateway terminates the downstream handshake and presents its own
certificate upstream, so the Server never sees a downstream certificate and cannot refuse it. A
certificate the Server has revoked would stay a member behind a Gateway until it expires unless
the Gateway refuses it. The specification puts security before convenience (Strategy, *Security
before convenience*; G-17, the Server accepts only authenticated Agent identities; Q-1, no Agent
is admitted onto the Server or a Gateway without a client certificate).

**Behind a Gateway the handshake is the only proof of membership.** No per-Agent proof crosses a
terminating Gateway: the Server sees the Gateway's certificate, and the downstream one stays on
the downstream hop. A host an operator marked as a Gateway speaks for any Agent
([ADR-0022](0022-bootstrap-certificates-and-enrolment.md)). Several things follow from that and
are closed here:

- A CSR without a renewal proof renews the certificate its connection presented. Behind a Gateway
  that is the Gateway's certificate, whose host carries the Gateway mark, so a member behind a
  Gateway could obtain a certificate with the Gateway's powers — speak for any `instance_uid`,
  fetch every offered artifact, read every Gateway's revocation list — and fill the Gateway
  host's places, blocking its own renewal.
- A member that reports under another host's `instance_uid` would take its route, receive its
  configuration, which may carry secrets, and its certificate, and speak in its name, unless
  routing consults the host the `instance_uid` is bound to.
- A binding that lasts only until the Gateway restarts — an update, a reboot, a crash — lets
  whichever host reports an `instance_uid` first after that take over its Agent, and the Server
  cannot see it. The same holds for a binding that can give way to another host's once the
  binding table is full. Trust on first use is only as good as the record of first use.
- A binding kept on disk and never given way to another host would hold an `instance_uid`
  forever once its host stops reporting, so it needs an end of its own: the host's ability to
  report, which ends with its certificate.
- A host that only a Gateway reaches must enrol through it (G-15), yet enrolment rests on the
  bootstrap certificate of a direct connection's handshake, and a Gateway forwards no downstream
  certificate. A proof inside the CSR holds over any hop, because the Server checks it itself.
- A CSR read by two PEM readers that may choose different blocks of one crafted input lets a
  proof be checked against one request while the key of another is signed.

**A Gateway must not invent messages.** The specification forbids resolving a protocol gap by
inventing semantics of this project's own. When something fails on one side of a hop, the
tempting fix is to synthesise a message toward the other, and that is exactly what is not
available. Nor is a connection an identity in either direction: upstream connection state says
nothing about whether a downstream Client is alive.

**A pool that is a fixed cost is a pool nobody keeps small.** The prior art opens ten connections
at startup; ten connections for a Gateway in front of three Agents is worse than three. A place
in the pool that outlives the Agent it was held for grows the pool's assignments for a peer that
polls under ever new `instance_uid`s.

**The Supervisor Endpoint is open to every local process unless it asks for a proof.** Every
Supervisor serves it on `127.0.0.1`, where a Collector's `opampextension` reports its
description, health and effective configuration. An endpoint that authenticates nothing lets any
process on the host connect and report in the Collector's name, and the fleet's view of that
Agent becomes forgeable from the inside. On a single-purpose host that is a small risk; on a
shared or multi-user host it is not. The Supervisor starts the process it supervises, so it can
hand that process a secret no other local process has: the environment of a child is readable by
the child and by root, not by other accounts. The `opampextension` sends configured headers on
its WebSocket, and a Collector configuration reads an environment variable with `${env:…}`.

## Decision

We will ship one Client binary with exactly two independent Client Modes, give every Supervisor a
Supervisor Endpoint that admits only the process its Supervisor started, route by `instance_uid`
on both ends, and run Gateway Mode as a mutual-TLS 1.3 hop that applies the Server's admission
and revocation, forwards and serves only for the host an `instance_uid` is durably bound to, keeps
a lazily grown, sticky upstream pool that synthesises no message, and relays enrolments whose CSR
proves the certificate it rests on, which the Server decides as any enrolment.

1. **Two modes, freely composable, neither implying the other.** Supervisor Mode (`[[supervisor]]`
   blocks) and Gateway Mode (`[gateway]`) run alone or together in one process. A mode is a
   composition of Ports, not a fork of the core: the supervision domain does not learn which mode
   it is in.

2. **Every Supervisor exposes a Supervisor Endpoint, unconditionally.** It binds `127.0.0.1` on
   the block's `endpoint_port` ([ADR-0017](0017-supervisor-mode-and-its-kinds.md)) before the
   Managed Process starts, so a taken port fails startup. It is WebSocket-only, declares
   `AcceptsStatus` and `AcceptsEffectiveConfig`, and folds what the process reports into the
   owning Agent, whose `instance_uid` stays the Supervisor's. For a Managed Process that speaks no
   OpAMP nothing ever connects, and that is the whole of the handling. The Supervisor Endpoint is
   not a mode.

3. **Neither end keys state on a connection.** The Server indexes Agents by `instance_uid` only;
   the Client maps Agents onto connections without assuming one-to-one. *n* Agents ride *m*
   connections (*n* ≥ *m* ≥ 1), and the pool size is a deployment choice, not a consequence of
   the Agent count.

4. **`[gateway]` arms Gateway Mode.**

   | Key | Default | Rule |
   |---|---|---|
   | `listen` | none | Required. A Gateway that binds nothing is a configuration error; loopback is the Supervisor Endpoint's job. A bind failure fails startup. |
   | `upstream_connections` | `10` | The pool's cap, not its size. `0` is refused at load. |
   | `max_carried_agents` | `10000` | The most distinct Agents one downstream connection may carry. Past it a report for a *new* Agent is dropped and logged; Agents already carried keep working. `0` is refused at load. |
   | `[gateway.tls]` | none | Required, with `cert_file`, `key_file` and `client_ca_file` for the downstream hop. A Gateway without it, or without `client_ca_file`, is refused at startup, on a loopback `listen` too. |

   The Client's own `endpoint` must be `ws://` or `wss://` when `[gateway]` is present: a polling
   connection cannot carry the Server's pushes to the Agents behind the Gateway.

5. **The downstream endpoint serves both transports.** A downstream Client picks its transport by
   the scheme of its endpoint
   ([ADR-0012](0012-tls-1-3-two-planes-and-bounded-input.md)), so the Gateway serves a WebSocket
   upgrade and a plain-HTTP POST on `/v1/opamp`, with the Baseline's message size limit, protobuf
   media type and gzip rules enforced per hop through the shared `opamp::endpoint` helpers
   ([ADR-0008](0008-five-crates-on-tokio-and-axum.md)).

6. **axum serves it, as an ordinary dependency of the Client crate behind no Cargo feature.** The
   mode lives in `crates/fleet-agent/src/gateway/` (`mod.rs` the endpoint, `pool.rs` the upstream
   pool, `registry.rs` the downstream routes), beside `supervisor/`. The HTTP engine under axum is
   linked through `reqwest` anyway; axum adds the routing layer. TLS on the downstream hop uses
   the same `axum-server` rustls terminator as the Server.

7. **An Agent is assigned to the least-loaded live connection, and stays there while the Gateway
   carries it.** Assignment is by `instance_uid`. It is sticky while that connection lives and the
   Gateway carries the Agent, which keeps an Agent's `sequence_num` stream and its
   `ReportFullState` exchanges on one upstream socket. The Gateway carries an Agent while a
   downstream WebSocket that reported for it is open, or while its binding stands (clause 15); an
   enrolling peer is carried for its one exchange. When the Gateway stops carrying an Agent, its
   place is released, and nothing is said upstream on its behalf (clause 10): its next report is
   assigned anew, to the least-loaded live connection, which may be another one. Any live
   upstream connection carries any downstream Agent: the pool is not divided by downstream peer,
   because nothing the Gateway sends upstream depends on which peer an Agent came through.

8. **The pool grows lazily to its cap, and never beyond.** The first Agent opens the first
   connection; a new one opens only when every live connection already carries an Agent and the
   cap is not reached. Past the cap, Agents share. Existing Agents are not re-balanced when a
   connection is added.

9. **A dropped upstream connection re-homes its Agents and says nothing on their behalf.** An
   Agent whose connection is gone is re-assigned by clause 7 on its next report, which is
   forwarded over the new connection. The Gateway sends no `agent_disconnect` for it. The Server
   marks those Agents disconnected because their owning connection dropped, and their next report
   corrects that.

10. **A downstream Client that vanishes is reported by its absence, not by a message.** An
    `agent_disconnect` it sends is forwarded like any other message. If it just goes away, its
    routes are released and the Gateway forwards nothing. Such an Agent stays connected in the
    fleet view with a `last_seen_ms` that stops advancing, and reads as stale once its staleness
    budget runs out when it declared `ReportsHeartbeat`
    ([ADR-0024](0024-the-fleet-record-of-agents-and-hosts.md)).

11. **The downstream handshake and the Server's revocation list are a downstream Agent's
    admission; the Gateway holds no policy of its own, and its own hop is its own.**
    - Mutual TLS is per hop and mandatory downstream: the handshake requires a client certificate
      that chains to `client_ca_file`, or, where `[gateway.tls] bootstrap_ca_file` names one, to
      that bootstrap CA (`ClientAuth::Required` in `opamp`), so a peer without one never reaches
      OpAMP.
    - `client_ca_file` must be the fleet's client CA, never a bootstrap CA. `bootstrap_ca_file`
      holds the public certificate of `[enrolment] bootstrap_ca_file` on the Server and no key,
      and a CA that is in both fails startup naming both keys. A peer whose certificate chains to
      the bootstrap CA alone is an **enrolling peer** (clause 24).
    - A peer that passed the handshake is admitted unless its certificate is on the list of
      clause 14; that is the one admission refusal the Gateway makes beyond the handshake, and it
      is the Server's. The list holds the client CA's revocations; a revoked bootstrap
      certificate is refused by the Server, which checks every proof against its own list
      (clause 21).
    - Beyond admission the Gateway refuses three things. It forwards no report for an
      `instance_uid` bound to another host, or that it cannot bind (clause 13). It forwards from
      an enrolling peer nothing but what clause 24 allows. And it passes on an uploaded artifact
      from its package cache only to a host whose Agent it relayed the Server's offer of that
      artifact to, and answers every other download request `404` (clause 15). What a host may
      receive is the Server's own bound
      ([ADR-0024](0024-the-fleet-record-of-agents-and-hosts.md), what a host's Agents inherit
      from it), applied to what the Server sent through this Gateway.
    - Four parts of these refusals are the Gateway's own: it binds an `instance_uid` to the host
      of the first report for it, it refuses a report under a binding that is another host's, it
      answers `404` for an artifact too large for its cache or whose fetch failed, and it answers
      `503` while a fetch runs ([ADR-0027](0027-packages-signatures-and-delivery.md), the
      Gateway's package cache; clause 15).
    - The client CA, the bootstrap CA and the list are the Server's, so the Gateway applies the
      Server's admission policy and holds no admission policy of its own
      ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md), mutual TLS per hop;
      [ADR-0022](0022-bootstrap-certificates-and-enrolment.md), what a certificate proves).
    - Upstream the Gateway presents the Client's own identity, so every downstream Agent arrives
      as a member, under the Gateway's certificate. The Gateway forwards no `Authorization`: one a
      downstream peer sends is ignored, never refused, and nothing is sent upstream in its place.
    - The downstream hop speaks TLS 1.3 only. A downstream certificate is never forwarded, and no
      header is invented for it. There is no plaintext Gateway, on the loopback neither, because
      a Gateway admits Agents.

12. **The Gateway is its own Agent, and only its own.** The Client presents its own Agent upstream
    exactly as any Client does, and never presents itself as an Agent it carries.

13. **Routing is by `instance_uid`, for the host it is bound to.**
    - Every downstream report of a member first binds its `instance_uid` (clause 15). It is
      forwarded upstream only when its `instance_uid` is bound to the host of the certificate that
      sent it, and only then does the connection or exchange that carried it become the Agent's
      route.
    - A report for an `instance_uid` bound to another host, or one the Gateway could not bind, is
      not forwarded and creates no route. Over WebSocket it is dropped. Over plain HTTP it is
      answered `403`, with a message saying that the `instance_uid` is not this host's to report
      for. Either way it is logged as every refusal of the Gateway is: the first five per host and
      minute one by one, the rest counted into one line
      ([ADR-0027](0027-packages-signatures-and-delivery.md), the Gateway's log of what it
      refuses).
    - A report of an enrolling peer binds nothing. It is forwarded as clause 24 says, and its
      exchange is its route for its one reply, gone when the reply is sent or the exchange ends.
    - Upward a forwarded report goes out on its Agent's connection. Downward the Gateway looks up,
      per `instance_uid`, the downstream connection or pending plain-HTTP exchange that last
      carried the Agent, which is always one of the bound host's or an enrolling peer's own
      exchange. A `ServerToAgent` for an Agent it does not carry is dropped with a log line, never
      broadcast.

14. **The Gateway refuses what the Server revoked, and admits nobody while it cannot know.** The
    Gateway fetches the revoked certificates of the fleet's client CA from its Server, on the
    Server's revocation route for Gateways
    ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)), over its own upstream
    origin, with its own certificate and no `Authorization`, every `REVOCATION_REFRESH` (30 s),
    conditionally on the list it holds. It keeps the list in memory only. A downstream peer whose
    certificate is on the list is answered `401` after the handshake and before anything is
    forwarded, on both transports; an open downstream WebSocket session whose certificate the list
    newly names is closed with `1008` and the reason `revoked`, as the Server closes its own, and a
    report arriving on a session the list no longer admits is not forwarded. The Gateway starts
    serving once its first fetch has answered or failed, and at most `REVOCATION_FIRST_WAIT`
    (5 s) after it started, so its first peers are not refused for want of a list. While it holds
    no list fetched within `REVOCATION_MAX_AGE` (300 s) — from then until a first fetch succeeds,
    and whenever its Server has not answered for that long — it answers every downstream request
    `503` and closes every downstream session with `1008` and the reason
    `revocation list stale`. The three values are named constants. A Gateway the Server refuses
    the list to holds no list, and admits nobody.

15. **Downstream, a host receives only what the Gateway relayed to its own Agents.** The Gateway
    serves `GET /api/v1/packages/{agent_type}/{version}/file` on its downstream listener, behind
    the handshake and the revocation verdict of its `/v1/opamp`: a certificate from
    `client_ca_file` is required, a revoked one is answered `401` and every request is answered
    `503` while the Gateway holds no current list (clauses 14, 11).
    - **Binding.** An `instance_uid` is bound to the host of the certificate of the first
      downstream connection that reports for it, as the Server binds an `instance_uid` to the
      host that first reports it ([ADR-0022](0022-bootstrap-certificates-and-enrolment.md)), and
      the binding is kept across the Gateway's restarts (clause 16) until it ends (clause 17). The
      host is the one the certificate names (`urn:opamp-fleet:host:<id>`). A certificate that
      names none is a host of its own: the host the Server derives from its issuer and serial
      when it first renews it (ADR-0022), computed the same way. So it neither shares a binding
      with another certificate nor takes one over, and the certificate its renewal brings names
      the same host. The derived host binds for routing only: offers and downloads are recorded
      and served only for a host a certificate names (*Offers*, *Requests*). A host holds at most
      `max_carried_agents` bindings, the number clause 4 bounds a connection by. Past that its
      report for a new `instance_uid` takes the place of that host's own least recently reported
      binding whose `instance_uid` has neither a downstream route nor a current offer, so a host
      whose Agents come and go is never shut out by its own past; when it has none, the report
      binds nothing, and that is logged. The Gateway holds at most 1 000 000 bindings in all;
      past that a new binding takes the place of the reporting host's own binding as above, and
      never another host's: when the reporting host has none to give, it binds nothing, and that
      is logged. Each report refreshes when its binding was last reported. What a report that
      binds nothing does is clause 13's.
    - **Offers.** An offer relayed to an `instance_uid` is recorded only when the downstream
      connection it is relayed over presents a certificate of the host the `instance_uid` is
      bound to; any other is not recorded, and that is logged. An Agent's current offer is the
      files of the last recorded `packages_available` relayed to it: a later one replaces it, one
      that names no Server-hosted file removes it, a message without `packages_available` leaves
      it standing ([ADR-0027](0027-packages-signatures-and-delivery.md), what an offer is), and a
      downstream disconnect does not end it. The offers held are bounded by the offers the Server
      makes through the Gateway. The offers are kept in memory only, indexed by host; a restart
      ends them.
    - **Requests.** A request is served only when the Gateway holds the artifact and a current
      offer of it (the same path and query, byte for byte) to an `instance_uid` bound to the host
      the request's certificate names. A certificate that names no host receives nothing. While
      an offered artifact is being fetched, or waits for a fetch slot, the request is answered
      `503` with `Retry-After: 30` instead of waiting, so a Client's read timeout does not run out
      while the Gateway fetches. An offered artifact that is neither held nor being fetched is
      fetched on a request at most once per relayed offer of it — the next relayed offer re-arms
      it — and that request is answered `503` with `Retry-After: 30`. After a failed fetch it is
      not fetched again until the Gateway's package cache says so
      ([ADR-0027](0027-packages-signatures-and-delivery.md)).
    - **Everything else** is answered `404`, with the same status, headers and body whether no
      such offer was recorded, it was recorded for another host, the artifact could not be
      fetched or verified, it is too large for the cache, or the request names nothing the route
      knows.

    The bound on what a host receives is the Server's
    ([ADR-0024](0024-the-fleet-record-of-agents-and-hosts.md)), applied to what the Server sent
    through the Gateway. The binding, the `404` for an artifact too large or not fetched, and the
    `503` while a fetch runs are the Gateway's own. Together they are the download refusal of
    clause 11.

16. **The bindings are kept in `<state_dir>/gateway-packages/bindings`, written before they are
    relied on.** The file lies in the Gateway's own directory, whose name no Supervisor may take,
    and the package cache holds nothing under that name.
    - **What is kept.** Each binding's `instance_uid` and host, and per host the end of its
      certificate: the latest `notAfter` among the valid certificates it has reported under, and
      that certificate's issuer and serial as the revocation list names a certificate — the
      SHA-256 of the issuer's DER name and the serial in lowercase hex without leading zeros. When
      a binding was last reported is kept too, but written in batches, at most once a minute and
      at shutdown: a restart may lose the order of the last minute's reports, never a binding.
    - **What a binding relies on is durable before its report is forwarded.** A new binding, and
      a host's certificate end where the report moves it forward, are appended to the store and
      synced before the Gateway forwards the report, and before that report's connection or
      exchange becomes the Agent's route (clause 13). A binding that cannot be written is not
      made: the report binds nothing, is treated as clause 13 treats such a report, and the
      failure is logged. A binding given up for another of the same host's, and a binding that
      ends (clause 17), is appended as its removal; a removal lost to a crash only brings back a
      binding that its own host could give up again.
    - **The store is a journal, compacted and bounded.** It begins with a line naming its format
      and version, and holds one entry per line, of at most 512 bytes. The Gateway appends to it,
      and rewrites it whole from what it holds when it starts and when it has grown to twice as
      many lines as it holds live bindings and hosts, as a new file synced and renamed over the
      old. A store is at most 512 MiB. It is written as the Client writes into its state
      directory, without following a link
      ([ADR-0037](0037-the-accounts-the-service-runs-under.md)), readable and writable by the
      Client's account alone: `0600` on Unix, under the restricted ACL the install gives the state
      directory on Windows.
    - **An unreadable store refuses every downstream Agent.** At start the Gateway reads the store
      without following a link. Where it is not a regular file — on Unix one the Client's account
      owns with mode `0600`, on Windows one that is not a reparse point — is larger than its
      bound, names a format or version this build does not read, or holds a line it cannot parse
      other than a last line without its terminating newline, which it drops as a write cut off
      by a crash, the Gateway carries no downstream Agent. It refuses every downstream WebSocket
      upgrade and answers every plain-HTTP report and every download `503`; an enrolling peer's
      one request is relayed still, since it binds nothing (clause 24). It logs, at its start and
      once an hour, why, and that removing the file and restarting the Client lets it start
      empty. Its own Supervisors and its upstream connection go on. A store that does not exist
      is an empty one.

17. **A host's bindings end with its certificate.** A report under a certificate whose `notAfter`
    has passed, by the Gateway's clock — the clock its handshake checks validity by — binds
    nothing and moves no certificate end. When the end the Gateway keeps for a host has passed, or
    the revocation list the Gateway holds (clause 11) lists that certificate, every binding of the
    host ends: its route, its current offer and its place upstream go with it, as with a binding
    that makes room (clause 7), and it is removed from the store; that is logged with the host and
    how many bindings went. The Gateway checks at start, whenever its revocation list changes, and
    once an hour. A host whose certificate is renewed reports under the new one, which moves the
    end forward; a host that can no longer report keeps no `instance_uid` bound, and the
    `instance_uid`'s next report binds it anew. A binding loaded at start holds no place upstream
    until its Agent reports.

18. **A renewal proves which certificate it renews.** A Client that holds a certificate and its
    key adds to its CSR the SAN URI `urn:opamp-fleet:renewal:v1:` followed by the base64url,
    without padding, of the certificate's DER and a signature made with its key over
    `opamp-fleet-renewal-v1\n<SHA-256 of the new SubjectPublicKeyInfo, lowercase hex>\n`, each
    prefixed by its length as four big-endian bytes. The Server accepts the proof only when the
    certificate was issued by the client CA, is valid now, is not revoked, and its key verifies
    the signature. The new certificate is then issued to that certificate's host, with it as the
    predecessor in the Server's register of what it signed
    ([ADR-0029](0029-the-servers-state-and-its-stores.md)). This holds through a Gateway, which
    presents its own certificate. A proof that does not hold is answered `BadRequest`. A CSR
    without a renewal proof — a third-party client's — renews the certificate its connection
    presented, and its host, unless that host is marked as a Gateway. Then a CSR that carries a
    bootstrap proof is an enrolment through the Gateway (clause 25), and any other is answered
    `BadRequest` with a message saying that a certificate requested through a Gateway needs a
    renewal or a bootstrap proof. Nothing is signed, and the refusal is recorded as every refused
    CSR is ([ADR-0023](0023-an-append-only-audit-record.md)). This holds whatever `instance_uid`
    the message names, and for the Gateway's own Agent too.

19. **Behind a Gateway the Gateway refuses what the Server revoked.** A downstream Agent is
    revoked by revoking the Gateway's certificate, which ends every Agent the Gateway carries; or
    by revoking its own certificate, which the Gateway refuses from the list of clause 14, and
    whose proof renews nothing. No certificate is renewed from the Gateway's (clause 18), so
    revoking the Gateway's certificate ends no certificate renewed through it.

20. **The bootstrap proof.** An enrolling Client adds to its CSR the SAN URI
    `urn:opamp-fleet:bootstrap:v1:` followed by the base64url, without padding, of the bootstrap
    certificate's DER and a signature made with its key over
    `opamp-fleet-bootstrap-v1\n<SHA-256 of the new SubjectPublicKeyInfo, lowercase hex>\n`, the
    certificate prefixed by its length as four big-endian bytes, as the renewal proof is
    (clause 18). The statement differs from the renewal's, so neither proof can stand for the
    other. The Client sends it on every enrolment, directly or not.

21. **The Server checks the proof itself.** It reads the CSR once, from exactly one PEM block with
    nothing but white space around it, and takes the proof, the key it covers, the claims and the
    key it signs from that one encoding; any other input is answered `BadRequest`. It accepts the
    proof only when the certificate was issued by the bootstrap CA of `[enrolment]`, is a
    client-authentication leaf as the handshake requires one (`clientAuth`, not a CA), is valid
    now, is not revoked, and its key verifies the signature over the CSR's own key. A proof that
    does not hold is answered `BadRequest`, and nothing is queued. A CSR that carries both a
    renewal proof and a bootstrap proof is answered `BadRequest`.

22. **The request is filed under the certificate it proves.** The requester of an enrolment
    request — the bootstrap certificate's issuer and serial, its fingerprint, its subject and the
    values an approval carries — is read from the certificate the request proves. On an enrolment
    connection a request proves the certificate of the handshake: a bootstrap proof there must
    name that certificate, or the request is answered `BadRequest`, and a request without one
    proves the handshake's certificate alone, so a client that sends no proof still enrols
    directly. Where a request may arrive other than on an enrolment connection is a decision of
    its own.

23. **An approval copies the subject from the bootstrap certificate the request proves, and
    validates it again.**
    - The Server reads the six attributes from the bootstrap certificate the request proves
      (clause 22). That certificate may come from a foreign PKI, so the Server applies again the
      rules by which the operator names the six attributes
      ([ADR-0022](0022-bootstrap-certificates-and-enrolment.md)).
    - An attribute that appears twice, has the wrong encoding or length, or is a `country` that
      is not two upper-case letters, and a missing `organization` or `organizational_unit`, make
      the approval fail (`409`) with a message naming the attribute. Nothing is signed, and the
      request stays waiting until the operator rejects it.
    - The right encoding is `PrintableString` for `country`. For the other five it is
      `UTF8String` or `PrintableString`, which older PKIs write and which is a subset of it; the
      host certificate carries them as `UTF8String`.
    - A `country` is checked by its shape, two upper-case letters, not against the ISO 3166-1
      list. The Enrolments tab offers the list.
    - Other attributes of the bootstrap certificate are not copied. Its common name is never
      copied, because the host certificate's common name is its CA's, with `CA` turned into
      `Certificate` ([ADR-0032](0032-the-operator-program-opamp-fleetctl.md)).

24. **An enrolling peer sends one thing, over plain HTTP.** A WebSocket upgrade from an enrolling
    peer is refused `403`, with a message saying that an enrolment through a Gateway is a
    plain-HTTP exchange. A plain-HTTP report of an enrolling peer is forwarded only when it
    carries a CSR with a bootstrap proof (clause 20) and nothing a member reports — no
    description, health, effective configuration, package statuses or connection settings beyond
    the CSR; any other is answered `403` and logged. The Gateway reads the CSR by the Server's
    rule of one PEM block (clause 21) and forwards it only when the certificate the proof embeds
    is, byte for byte, the one the peer presented in the handshake; any other is answered `403`
    and logged. So an enrolling peer proves only its own certificate, and spends no other
    certificate's tokens (clause 26). The Gateway does not check the proof's signature: the
    Server does. It records no offer for an enrolling peer and serves it no download. The Client
    re-sends its request until it is answered, as on a direct enrolment connection
    ([ADR-0022](0022-bootstrap-certificates-and-enrolment.md)), and collects its certificate in
    the reply to the request that finds it approved. A Client that holds no issued certificate
    and whose endpoint is `https://` or `http://` therefore sends its CSR alone from its first
    report on, so a Gateway relays it; answered `BadRequest`, it falls back to the reports of a
    member until an exchange fails, so a host that holds a provisioned member certificate is not
    held up. A Client behind a Gateway names it with an `https://` endpoint while it enrols.

25. **The Server decides an enrolment through a Gateway as any enrolment.** A CSR that carries a
    bootstrap proof over the certificate of a host marked as a Gateway is an enrolment request:
    the proof is checked (clause 21), the request is filed under the certificate it proves
    (clause 22), it waits for an open window, counts against the queue and against whatever share
    of it one bootstrap certificate may hold, and waits for an operator's approval, which mints a
    host of its own, never the Gateway's. Without an open window it is answered `Unavailable`, as
    on an enrolment connection. The pending request names the Gateway's host instead of a peer
    address, in the queue and in every audit entry of it. Through a Gateway the proof carries no
    chain, so its certificate must be issued directly by the bootstrap CA. A CSR with a bootstrap
    proof over a certificate whose host is not marked as a Gateway is answered `BadRequest`: a
    member does not enrol. A proof that names the certificate the connection presented is no
    proof of a bootstrap certificate but of that certificate itself, which a Client that cannot
    tell a provisioned member certificate from a bootstrap certificate sends; the CSR is then read
    as one without a proof (clause 18).

26. **Which bucket a message or a download is counted in.** The key comes from the certificate
    admission proved (`Proofs` in [`transport.rs`](../../crates/fleet-server/src/transport.rs)):
    - **A certificate that names a host:** that host.
    - **A member's certificate that names no host** (one an operator provisioned that has not
      been renewed yet): its issuer and serial.
    - **A bootstrap certificate on an enrolment connection:** the peer address, an IPv6 one by its
      /64 (`throttle::peer_key`). A bootstrap certificate may be one for the whole fleet
      ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)), so its issuer and
      serial would put every enrolling host in one bucket. The enrolment window
      ([ADR-0029](0029-the-servers-state-and-its-stores.md)), the bound on the queue and the
      operator's approval ([ADR-0022](0022-bootstrap-certificates-and-enrolment.md)) already bound
      what enrolment costs.
    - **A host marked as a Gateway** (`Revocations::is_gateway`), in two buckets, and a message
      must take a token from both:
      - the bucket of the Agent the message names, keyed by the host and the message's
        `instance_uid` and sized by `messages_per_sec` and `burst`;
      - the Gateway's aggregate, keyed by the host and sized by `gateway_messages_per_sec` and
        `gateway_burst`.

      A message carrying an enrolment request (clause 25) takes from neither, so enrolments
      cannot spend what the Gateway's members need. Once the bootstrap certificate its proof
      embeds is checked as the handshake checks one — issued by a bootstrap CA with a signature
      that verifies against it, valid now, not a CA, and `clientAuth` among its key usages where
      it names any — it takes a token from a bucket keyed by the host and that certificate, sized
      by `messages_per_sec` and `burst`, and one from the Gateway's **enrolment aggregate**, keyed
      by the host and sized by `gateway_messages_per_sec` and `gateway_burst`, so one bootstrap
      certificate empties its own bucket long before the aggregate and cannot spend another's
      tokens. One that fails that check takes a token from the Gateway's bucket for unchecked
      enrolments alone, keyed by the host and sized by `messages_per_sec` and `burst`: a forged
      certificate never makes a bucket of its own, and forged proofs spend neither the members'
      tokens nor the checked enrolments'.

      A message whose `instance_uid` is not 16 bytes, an undecodable message, and a download
      request through a Gateway name no Agent, so they are counted in the aggregate alone. The
      mark is read per message, so marking or unmarking a Gateway takes effect at the next
      message.

    A connection with no certificate exists only behind `Admission::open`, which no Server
    configuration serves ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)). It
    is counted by its peer address.

27. **A Supervisor Endpoint token per start.** 32 bytes from the system's secure random source,
    hex, made when the Supervisor starts and kept in memory only. A restart of the Supervisor
    makes a new one.

28. **Handed to the process, last.** The Runner sets `OPAMP_SUPERVISOR_TOKEN` in the environment
    of every Managed Process it starts, after the block's own `env`, so no block can replace it.

29. **Asked of every connection to the Supervisor Endpoint.** The endpoint compares the
    `Authorization` value of the upgrade request with `Bearer <token>` in constant time and
    answers `401` before the upgrade when it does not match. A refusal is logged at `warn`.

30. **The Collector presents it from its configuration.** A Collector configuration that reports
    through the endpoint names the header:
    `headers: { Authorization: "Bearer ${env:OPAMP_SUPERVISOR_TOKEN}" }`. One without it loses its
    own reports, not its supervision.

**Out of scope:**
- re-balancing Agents across a grown pool;
- what the Server does with an Agent that moves to another upstream connection, which it keys by
  `instance_uid` alone;
- consolidating the three places TLS material is configured on a Client;
- authenticating the Supervisor to the Managed Process, and a Supervisor Endpoint token on disk;
- binding at the Server the `instance_uid`s a Gateway carries, so that a member connecting
  directly cannot claim one first;
- a command to end a binding by hand;
- moving a binding to the identity the Server gives an Agent through `AgentIdentification`, whose
  old `instance_uid` stays bound to its host until that host gives it up or its certificate ends;
- keeping the offers across a restart, which the Server does not offer again while the Agent's
  `all_packages_hash` stands ([ADR-0027](0027-packages-signatures-and-delivery.md));
- bindings shared between Gateways, so a host that reports through two keeps an end at each;
- a rate per downstream member certificate at the Gateway;
- a bootstrap certificate issued below an intermediate CA, which cannot enrol through a Gateway
  (the proof carries no chain); a member certificate issued below an intermediate of the client
  CA, which a Gateway with `bootstrap_ca_file` takes for an enrolling peer, as the Server takes
  one for an enrolling host ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md));
- a proof for a bootstrap certificate from a CA other than `[enrolment]`'s;
- enrolment through nested Gateways, where the outer Gateway sees the inner one's certificate;
- a WebSocket push of the approval to an enrolling peer behind a Gateway, and telling a Gateway
  whether a window is open.

## Alternatives considered

- **Separate deployables (a supervisor binary, a gateway binary).** Both shapes share their whole
  OpAMP stack, so this multiplies packaging and release surface to express a startup choice, and
  it forecloses a machine that supervises its own processes and fronts others.
- **A third, independently selectable "local server" mode.** It would let an operator configure
  combinations that cannot work and put a conditional on a path that has no reason to vary.
- **Enable the Supervisor Endpoint only for Collector Supervisors, or per block.** More precise,
  but it buys that with a configuration surface and a branch to avoid an idle loopback listener.
  The `opampsupervisor` brings its local server up unconditionally too.
- **One connection per Agent, or multiplexing only in Gateway Mode.** The first makes Gateway Mode
  impossible and induces the connection-equals-agent assumption on the Server; the second leaves
  two routing models in the code while the Server must support the general one anyway.
- **An admission policy of the Gateway's own.** The specification places authentication policy on
  the Server; a Gateway with a policy of its own, such as an allow-list or a CA of its choosing,
  duplicates it and forces every change of it to reach every Gateway. The Gateway applies what is
  the Server's: the client CA the Server issues from, the bootstrap CA, and the revocation list.
- **A per-Agent proof of membership that travels end to end through the Gateway, or a fleet-wide
  credential forwarded in a header.** No proof of that kind exists on the Agent plane
  ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)). A fleet-wide credential
  proves membership, which the downstream handshake already proves, identifies no Agent, and is
  held by every host.
- **Keeping the upstream pool partitioned by downstream peer.** Nothing peer-specific travels
  upstream, so a partition buys nothing; it costs connections, and at the cap it refuses an Agent
  while connections of another partition sit there.
- **Forwarding the downstream certificate to the Server, in a header, and letting the Server
  decide.** It keeps every decision on the Server, but the Server would trust a certificate the
  Gateway asserts in a header of its own invention, and a pooled upstream connection carrying
  many Agents could no longer be judged by one certificate, so the pool would have to split per
  certificate. It also leaves a revoked peer on the Gateway's downstream hop until the Server
  answers.
- **Using the last revocation list for as long as the Server is away.** A Gateway that cannot
  reach its Server forwards nothing anyway, and a list kept indefinitely would admit a certificate
  revoked while it was away the moment the Server returns, until the next fetch. A bound costs
  nothing a working Gateway does.
- **The list pushed in an OpAMP message.** A `CustomMessage` is `[Development]` in the Baseline and
  not implemented here; a field of the connection-settings offer is meant for something else.
  Either ties the list to an Agent's session rather than to the Gateway as a host.
- **A plaintext or server-authenticated-only Gateway hop.** A Gateway admits Agents, and the
  specification admits none without a client certificate in the handshake and none over anything
  but TLS 1.3 beyond the loopback. A loopback exception would still let any local process ride the
  Gateway's upstream connections, and a warning in place of a refusal leaves the insecure
  configuration running.
- **A fixed pool opened at startup.** The prior art's shape; it makes the pool a cost paid before
  any Agent arrives.
- **Round-robin instead of least-connections.** With sticky assignment it distributes arrivals
  rather than load, so a Gateway that loses and regains a connection stays lopsided.
- **Re-balancing Agents when a connection is added.** It trades stickiness for an even spread
  nobody asked for, and moving a stream between sockets mid-flight invites reordering.
- **Keeping an Agent's place for as long as its upstream connection lives.** A plain-HTTP peer
  opens no socket that closes, so it could keep places for `instance_uid`s the Gateway no longer
  carries until the upstream connection drops, and grow the pool's assignments by polling under
  ever new `instance_uid`s.
- **Releasing a plain-HTTP Agent's place at the end of each exchange.** Its next poll could land
  on another connection every time, which breaks the one-socket stream clause 7 exists for.
- **Releasing a place after an idle timeout.** A new duration with effect outside the Gateway,
  unrelated to the bound clause 15 sets per host.
- **Capping the pool's assignments separately.** A second bound beside the bindings' would have to
  be chosen, configured and kept in step with them.
- **Synthesising `agent_disconnect` for a vanished downstream Client.** The message would say the
  Agent said goodbye when it did not. Server-side staleness is the honest signal.
- **A WebSocket-only downstream endpoint.** The Baseline lets a Client choose either transport; a
  Gateway that silently excludes polling Clients fails as a Client that connects to nothing.
- **Hand-rolling the plain-HTTP endpoint on `hyper`.** One route sounds small until the Baseline's
  `413`, `415` and gzip rules are counted.
- **Gateway Mode behind a Cargo feature.** The released binary must have it on (G-15, one binary
  for every shape), so it saves nothing where claimed, and a default-off feature is a branch that
  `cargo test` and `cargo clippy` never compile.
- **A crate of its own, `crates/gateway`.** It needs the configuration, TLS material, identity,
  transports and Agent state of the Client crate, so it would depend back on `client`. A crate is
  not a feature boundary either.
- **A Layer-4 passthrough Gateway.** It carries client certificates end to end but cannot read
  OpAMP, so it cannot fold *n* Agents onto *m* connections.
- **Reusing the Server crate for the downstream endpoint.** The Client would depend on the Server,
  and the downstream side forwards rather than processes.
- **Reading the download `404` as outside the Gateway's refusal set.** A clause that called a
  revoked certificate the one refusal beyond the handshake would leave a reader who meets the
  download bound in the code taking one of the two for a mistake. Saying in clause 11 what each
  refusal is, and which parts are the Server's, keeps the Gateway's whole refusal set in one
  place.
- **Routing by `instance_uid` alone, the last sender winning.** A member that reports under
  another host's `instance_uid` would take its route, its configuration and its certificate.
- **Forwarding an unbindable report.** That is the unbounded growth the per-host cap exists to
  stop, and an unbound `instance_uid` could later be bound by anyone.
- **Binding per connection instead of per host.** A reconnect would lose every binding; the host
  is what the certificate proves.
- **Keying a certificate without a host by its issuer and serial as such.** Its first renewal
  brings a certificate that names the derived host, which the Gateway would see as another host;
  the derived host is the key (clause 15).
- **A report over a certificate that names no host binds nothing.** Such a certificate could not
  be routed for at all; binding it under its derived host keeps it apart from every other and
  carries it across its first renewal.
- **A host past `max_carried_agents` binds nothing new.** A host whose Agents come and go would be
  shut out by its own past; it gives up its own least recently reported idle binding instead.
- **Keeping the bindings in memory only, since the Gateway started.** It spares the Gateway a file
  it must defend, but every restart of a Gateway — a routine update is enough — opens every
  Agent behind it to whichever member host reports for it first, and the Server cannot see it.
  The state directory is defended as a whole, and already holds the Client's private key; a
  binding table is less to defend than that.
- **Letting a new binding take another host's idle binding at the Gateway's total.** That is the
  takeover path without a restart. The cost of refusing it is that a Gateway full of bindings
  binds nothing new for a host that has none of its own to give, which is logged.
- **Binding at the Server.** The Server would bind each `instance_uid` behind a Gateway to a
  downstream host, which closes the restart and the direct claim alike, but it has to learn the
  downstream host from the Gateway's word, a header the Server would have to trust, and it needs
  a protocol extension and a Server store. It stays a follow-up, and a Gateway that keeps its
  bindings does not stand in its way.
- **Keeping a binding for a fixed time without a report** (for example 60 days). A host's ability
  to report ends with its certificate, which the Gateway sees; a fixed time is either longer than
  that and holds an `instance_uid` for a host that cannot use it, or shorter and lets a quiet
  Agent's binding go.
- **Ending a binding only by an operator's command.** It needs a privileged interface on the
  Gateway host, and an `instance_uid` stays held until someone notices. A command stays possible
  later, beside the certificate's end.
- **Starting empty when the store cannot be read, with a warning.** It reopens the restart window
  that the store closes, at the moment an attacker who can damage the file chooses. The Gateway
  fails closed for downstream Agents alone.
- **Rewriting the whole store at every new binding.** At up to 1 000 000 bindings that is a large
  write per report that binds; an append and a sync is one line.
- **A database (SQLite) for the bindings.** One more dependency and file format to defend for one
  table with two operations.
- **Issuing a proofless CSR through a Gateway to a host of its own.** A third-party client behind
  a Gateway could still renew, but each such CSR would mint a host that no operator approved.
- **Issuing it to the Gateway's host, but without the mark.** The mark belongs to the host, and
  the certificate would still fill one of the Gateway host's three places.
- **A downstream Agent without a proof renews from the Gateway's certificate.** What it renews
  would descend from the Gateway's chain and host, and carry the Gateway's powers; revoking the
  Gateway's certificate would have to end every certificate renewed through it.
- **Letting the Gateway refuse a CSR without a proof.** A Server that trusts the Gateway to do so
  is only as safe as the oldest Gateway in the fleet; the Server sees the presented certificate
  and decides on its own.
- **Reusing the renewal proof's statement for the bootstrap proof.** A statement of its own keeps
  the two apart by construction.
- **Requiring the bootstrap proof on direct enrolment too.** A third-party OpAMP client could no
  longer enrol at all; the handshake already proves the certificate on a direct connection.
- **Copying the subject from the bootstrap certificate the enrolment connection presented.** It
  holds for a direct enrolment, but through a Gateway the connection presents the Gateway's
  certificate; the certificate the request proves is the requester's on either path.
- **A downstream handshake that admits `client_ca_file` alone, the revocation list the one refusal
  beyond it.** A host that only a Gateway connects could then never enrol: its bootstrap
  certificate fails the handshake, and its only way in is a certificate the operator makes by
  hand.
- **A marked Gateway's buckets alone, with no buckets for enrolment.** An enrolment request
  through a Gateway would spend the tokens the Gateway's members need, and one bootstrap
  certificate could spend another's.
- **Admitting enrolling peers without a client certificate.** Anyone on the network would then
  reach the Gateway and, through it, the Server.
- **Letting the Gateway know whether a window is open; carrying enrolment over WebSocket; binding
  the enrolling peer and releasing the binding on approval.** Each needs state the Gateway does
  not hold.
- **Throttling enrolment by bootstrap certificate everywhere.** On a direct connection a
  fleet-wide bootstrap certificate would put every enrolling host in one bucket; the bucket per
  bootstrap certificate applies through a Gateway only.
- **A written statement that single-purpose hosts are assumed, instead of the endpoint token.** It
  leaves shared hosts forgeable, where the specification asks for the secure default.
- **A Unix socket with file permissions for the Supervisor Endpoint.** The `opampextension` speaks
  WebSocket over TCP, and Windows has no equivalent with the same reach.
- **Mutual TLS on the loopback for the Supervisor Endpoint.** A CA and certificates per Supervisor
  for a socket that never leaves the host; the token gives the same proof for a fraction of the
  setup.
- **Checking the peer's process ID.** Not portable, and a peer's PID says little once it can be
  reused.

## Sources / Prior art

- [OpAMP specification,
  `ServerToAgent.instance_uid`](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md)
  — the protocol's own provision for multiplexing through a terminating proxy; its
  Agent-initiated CSR flow, in which the Agent requests and the Server decides what it signs.
- [OpAMP Gateway
  Extension](https://bindplane.com/blog/opamp-for-opentelemetry-managing-collector-fleets-and-introducing-the-new-opamp-gateway-extension)
  — an OpAMP server downstream and a client upstream, a pool of ten by default with
  least-connections balancing, messages forwarded unchanged, authentication delegated to the
  Server. Alpha; a design reference, not a dependency.
- [`opampsupervisor`
  specification](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/specification/README.md),
  [`supervisor/supervisor.go`](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/supervisor/supervisor.go)
  and
  [`supervisor/config/config.go`](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/supervisor/config/config.go)
  — a local OpAMP server started unconditionally on `localhost:<port>`
  (`agent::opamp_server_port`, random free port when unset), with the extension's configuration
  injected from an embedded template.
-
  [`opampextension`](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/extension/opampextension)
  — a client only, with a deliberately small capability set; its configuration sends
  `server.ws.headers`.
- RFC 4210 section 5.3.1 (CMP): proof of possession by a signature with the key of the certificate a
  request rests on.
- RFC 5280 section 4.1.2.5 (*Validity*) and RFC 5280 section 5 (CRLs): a certificate past
  `notAfter` or on the issuer's list is not to be relied on, so neither is anything bound to it.
- OpenSSH `known_hosts` (`ssh(1)`, *VERIFYING HOST KEYS*): trust on first use is only as good as
  the record of first use; a record that is forgotten makes every connection the first.
- RFC 7435 (*Opportunistic Security*), which names trust on first use: a binding made at first
  contact protects only after that contact, and only as long as it is kept.
- The append-only journal with periodic compaction of Redis' AOF rewrite and of LevelDB's log;
  the sync before acknowledging is the write-ahead rule of PostgreSQL's WAL (*Reliability and the
  Write-Ahead Log*).
- This repository: `crates/fleet-agent/src/gateway/pool.rs` (stickiness by `instance_uid` and the
  release that skips an Agent still routed), `crates/fleet-agent/src/gateway/mod.rs` (the places
  released at the end of an enrolment exchange and when a downstream WebSocket closes),
  `crates/fleet-agent/src/gateway/policy.rs` (the bindings), and
  [`docs/HARDENING.md`](../HARDENING.md), *The local endpoint* and the Gateway entry of *Where
  each bound applies today*.

## Consequences

- Positive: one deployable to build, sign, install and update on every platform. A fleet scales
  connections independently of its Agent count, and the Server needs no change to sit behind a
  Gateway.
- Positive: the pool costs what it uses, so the default cap needs no tuning for small deployments
  and still bounds large ones. Every connection serves every Agent, so the cap is never reached
  with connections idle for want of the right peer. A place in the pool lasts no longer than the
  Gateway carries its Agent, so a host holds at most `max_carried_agents` places, as it holds at
  most that many bindings.
- Positive: binding the Supervisor Endpoint to the Supervisor removes a class of invalid
  configurations instead of validating against it, and another local account can no longer
  report in a Managed Process's name.
- Positive: a peer without a certificate from the Gateway's client CA, or from its bootstrap CA
  where it names one, is turned away in the handshake, before it reaches OpAMP or the upstream
  pool, and an insecure Gateway configuration cannot start.
- Positive: a revoked certificate is refused behind a Gateway within one refresh, and the session
  it holds there is ended, as it is on the Server.
- Positive: a member behind a Gateway cannot obtain a certificate that carries the Gateway's
  powers, take over another host's Agent — after a Gateway restart or by filling the binding table
  neither — or fill the Gateway host's places. Every certificate renewed through a Gateway stays
  in its own chain. A host that leaves the fleet frees its `instance_uid`s when its certificate
  ends, at the latest after one certificate life (30 days by default,
  [ADR-0021](0021-admission-by-client-certificate-and-revocation.md)), at once when it is revoked.
- Positive: a host that only a Gateway reaches can enrol, with the same window, queue and approval
  as one that connects directly, and its certificate is its own host's. A proof and the key the
  Server signs always come from the same request.
- Negative / trade-offs: behind a Gateway the Server judges no proof of a downstream Agent's
  membership; the Gateway's handshake and the list are the whole admission. A compromised Gateway
  speaks for any Agent, which the admission decision already accepts for a marked Gateway
  ([ADR-0022](0022-bootstrap-certificates-and-enrolment.md)). A Gateway whose `client_ca_file`
  names anything but the fleet's client CA admits what the Server never issued, and the Server
  cannot see that file; the manual and review hold it. An unmarked Gateway is refused the list and
  so admits nobody, which keeps that reliance to the hosts an operator marked.
- Negative / trade-offs: every Supervisor binds a loopback port whether or not its Managed Process
  uses it.
- Negative / trade-offs: losing one pooled connection marks every Agent riding it disconnected
  until each reports again: one heartbeat interval with a heartbeat configured, otherwise until
  the Agent has something to say. An Agent whose binding gave way and that reports again may move
  to another upstream connection while the old one lives; its `sequence_num` stream then continues
  on another socket, which the Server accepts by `instance_uid`.
- Negative / trade-offs: a downstream Client that vanishes without a goodbye stays connected in the
  fleet view; only staleness tells, and only for an Agent that declared `ReportsHeartbeat`.
- Negative / trade-offs: a revocation reaches a Gateway up to `REVOCATION_REFRESH` late. A Gateway
  whose host the operator has not marked as a Gateway, or whose Server is away for more than
  `REVOCATION_MAX_AGE`, admits nobody.
- Negative / trade-offs: a Gateway forwards no `Authorization`, so behind a Server that still
  demands one its Agents are refused; the Server is upgraded before its Gateways.
- Negative / trade-offs: every Gateway needs a server certificate and a client CA, and every Client
  behind it a certificate chaining to that CA, even on a single host or in a test.
- Negative / trade-offs: the binary carries an HTTP routing layer it uses only in Gateway Mode, and
  Supervisor Mode plus Gateway Mode in one process is a real test surface.
- Negative / trade-offs: the Gateway refuses a download request beyond admission. What a host may
  receive is the Server's bound applied to the offers the Server sent through the Gateway, and the
  `404` reads as the Server's for an artifact it does not hold. The binding, and the answers for
  an artifact too large, not fetched or still being fetched, are the Gateway's own decisions.
- Negative / trade-offs: the Gateway keeps a file of up to 512 MiB it must defend, writes and syncs
  one line per new binding, and refuses every downstream Agent while that file is damaged, until
  an administrator removes it and restarts the Client. An Agent that moves to a new host behind
  the same Gateway is refused there until the old host's certificate ends or is revoked; while the
  old host keeps renewing, revoking its certificate, which ends all of its Agents, is the only
  way. Hosts that together fill the total keep every new host out, across restarts, until their
  certificates end. A host whose `max_carried_agents` Agents are all connected or offered
  something cannot bring in another until one goes idle. Offers end with a restart, and an Agent
  behind the Gateway gets `404` for a download until the Server offers it something new.
- Negative / trade-offs: a third-party OpAMP client behind a Gateway renews only with a proof,
  which it cannot send; it enrols directly or gets its certificate from the operator. A Gateway
  with `bootstrap_ca_file` lets a holder of a bootstrap certificate reach the Server through it
  outside a window too; the Server answers `Unavailable`, and the throttle bounds the cost. An
  enrolling host behind a Gateway learns of its approval at its next poll. Hosts that enrol
  through one Gateway with one bootstrap certificate share that certificate's bucket, as hosts
  behind one address share one on a direct connection.
- Negative / trade-offs: every Collector configuration that reports through the Supervisor
  Endpoint must name the header; one that does not stops reporting its own state. A process
  running as root, or as the Client's own account, can still read the token from the child's
  environment.
- Follow-ups: re-balancing when the pool grows, if a real fleet is ever lopsided enough;
  consolidating TLS material on the Client if a fourth place appears; binding at the Server what a
  Gateway carries; a command to end a binding by hand; moving a binding with a re-keyed
  `instance_uid`; offers that outlast a restart; a rate per downstream member certificate;
  enrolment through nested Gateways; a push of the approval.

## Enforcement

- [`crates/fleet-agent/tests/gateway_e2e.rs`](../../crates/fleet-agent/tests/gateway_e2e.rs):
  `two_agents_reach_the_server_over_one_folded_connection` (clauses 3, 13),
  `one_agent_opens_one_upstream_connection` (clause 8),
  `a_downstream_connection_carries_no_more_than_its_agent_cap` (clause 4),
  `a_downstream_peer_without_the_protobuf_content_type_is_refused`,
  `a_downstream_peer_may_gzip_its_report`, `a_gzip_bomb_is_refused_by_the_gateway` and
  `an_oversized_downstream_message_closes_with_1009` (clause 5).
- [`crates/fleet-agent/tests/gateway_tls.rs`](../../crates/fleet-agent/tests/gateway_tls.rs):
  `a_downstream_agent_with_a_certificate_reaches_the_server_over_tls`,
  `a_downstream_peer_without_a_certificate_is_refused` and
  `the_tls_endpoint_does_not_answer_plaintext` (clause 11).
-
  [`crates/fleet-agent/tests/gateway_and_supervisor_e2e.rs`](../../crates/fleet-agent/tests/gateway_and_supervisor_e2e.rs):
  `a_host_supervises_and_gateways_at_the_same_time`,
  `a_verified_offer_restarts_the_gateway_and_leaves_the_supervisors_running` and
  `a_gateway_that_cannot_bind_is_loud` (clauses 1, 4).
- [`crates/fleet-server/tests/ws_transport.rs`](../../crates/fleet-server/tests/ws_transport.rs)
  `two_agents_share_one_connection` and
  [`crates/fleet-agent/tests/e2e.rs`](../../crates/fleet-agent/tests/e2e.rs)
  `a_config_change_reaches_both_supervised_agents_over_one_connection` (clause 3).
- `crates/fleet-agent/src/supervisor/endpoint.rs`:
  `extension_reports_are_folded_into_process_events` and `shutdown_stops_the_endpoint`
  (clause 2); `crates/fleet-agent/src/config.rs`:
  `the_gateway_agent_cap_defaults_and_rejects_zero` (clause 4).
- Each marked as verifying this ADR: `crates/fleet-agent/src/config.rs`
  `a_gateway_without_mutual_tls_is_refused_at_load` — no `[gateway.tls]`, and a section without
  `client_ca_file` (clause 4);
  [`crates/fleet-agent/tests/gateway_tls.rs`](../../crates/fleet-agent/tests/gateway_tls.rs)
  `a_gateway_without_tls_on_loopback_does_not_start`,
  `a_gateway_without_a_client_ca_does_not_start` and
  `a_downstream_peer_with_a_certificate_from_another_ca_is_refused` (clauses 4, 11);
  [`crates/opamp/tests/server_listen.rs`](../../crates/opamp/tests/server_listen.rs)
  `a_client_offering_only_tls_1_2_is_refused`, against the listener and `ServerTls` the downstream
  endpoint is served with (clause 11).
- Clause 14:
  [`crates/fleet-agent/tests/gateway_revocation_e2e.rs`](../../crates/fleet-agent/tests/gateway_revocation_e2e.rs)
  `a_certificate_the_server_revokes_is_refused_behind_the_gateway`,
  `an_unmarked_gateway_admits_nobody` and `a_gateway_whose_list_goes_stale_admits_nobody`, run
  against a Server that reads no `Authorization`; `crates/fleet-agent/src/gateway/revocations.rs`
  `the_verdict_follows_the_list_and_its_age`.
- Through the real Gateway and Server, each marked as verifying this ADR:
  [`crates/fleet-agent/tests/gateway_tls.rs`](../../crates/fleet-agent/tests/gateway_tls.rs)
  `downstream_peers_with_different_certificates_share_one_upstream_connection` — two peers, an
  `upstream_connections` of `1`, both Agents reach the Server (clauses 7, 11);
  `a_downstream_authorization_header_is_ignored_and_not_forwarded` — a peer that sends one is
  admitted and the Server receives none (clause 11);
  `crates/fleet-agent/src/gateway/revocations.rs`
  `the_revocation_list_is_fetched_without_authorization`, even with an `[auth]` section left in
  `supervisor.toml` (clause 14).
- The download bound of clauses 11 and 15, marked as verifying this ADR beside
  [ADR-0027](0027-packages-signatures-and-delivery.md):
  [`crates/fleet-agent/tests/gateway_package_cache.rs`](../../crates/fleet-agent/tests/gateway_package_cache.rs)
  `another_host_and_a_certificate_naming_no_host_are_answered_as_for_an_artifact_not_held` and
  `a_revoked_certificate_is_refused_on_the_download_route`.
- The Supervisor Endpoint token:
  [`crates/fleet-agent/src/supervisor/endpoint.rs`](../../crates/fleet-agent/src/supervisor/endpoint.rs)
  `the_endpoint_admits_only_the_token_it_handed_out` (clauses 27, 29);
  [`crates/fleet-agent/tests/supervisor_process.rs`](../../crates/fleet-agent/tests/supervisor_process.rs)
  `the_managed_process_is_handed_the_endpoint_token` (clause 28).
- Tests marked as verifying this ADR check, for the proofs, the routing for a host and the
  enrolment through a Gateway (clauses 13, 15, 18 to 26):
  - A CSR without a proof over a Gateway's certificate is answered `BadRequest` naming both
    proofs, signs nothing, and records `issuance.refused`; over an unmarked host's certificate it
    still renews that certificate; a CSR with a valid renewal proof through a Gateway is issued to
    the proven certificate's host, with that certificate as the predecessor.
  - A CSR that is not exactly one PEM block — text before or after it, text on its boundary
    lines, two blocks — is refused on the renewal path and on both enrolment paths, and no Server
    code reads a CSR through another PEM reader.
  - A report from a second host for an `instance_uid` bound to a first host is not forwarded,
    over WebSocket and over plain HTTP (`403`), and the Server's next reply reaches the first
    host. A report past a host's `max_carried_agents` is not forwarded while all its bindings have
    a route; once one of its own Agents is idle, the new report takes that binding's place. Two
    certificates that name no host bind separately, and a certificate that names none and the
    certificate its renewal brings report for the same `instance_uid` without a refusal.
  - The bootstrap proof round-trips, and its statement differs from the renewal proof's; the
    Client's enrolment CSR carries one that names its bootstrap certificate. On an enrolment
    connection a proof of another certificate, a bad signature or both proofs are answered
    `BadRequest`; a request without a proof is queued under the handshake's certificate; a
    revoked bootstrap certificate's proof is refused.
  - A Gateway with `bootstrap_ca_file` admits a bootstrap certificate over plain HTTP only,
    forwards only a CSR whose proof embeds the certificate the peer presented, and binds nothing
    for it; a Gateway without the key refuses the certificate in the handshake; a CA in both files
    fails startup. End to end through a Gateway, an enrolment waits in the queue naming the
    Gateway's host, its approval mints a new host, and the enrolling Client then reports as a
    member under its own `instance_uid`.
  - Enrolments through a Gateway spend none of its members' tokens; forged proofs make no bucket
    of their own; one bootstrap certificate cannot spend another's tokens.
- Tests marked as verifying this ADR check, for the pool and the bindings (clauses 7, 15 to 17):
  - In `crates/fleet-agent/src/gateway/mod.rs`: a plain-HTTP peer that polls under more new
    `instance_uid`s than `max_carried_agents` leaves exactly the bound ones assigned, and each of
    them on the connection it was first assigned to; a report on a downstream WebSocket that takes
    another Agent's binding releases that Agent's place; when a downstream WebSocket closes, the
    places of the Agents it carried are released.
  - A binding made before a Gateway restart holds after it: a second host's report for that
    `instance_uid` is refused (`403` over plain HTTP, dropped over WebSocket) and the first host's
    report is forwarded.
  - A report that makes a new binding is forwarded only after the binding is in the store; when
    the store cannot be written, the report binds nothing and is not forwarded.
  - At the Gateway's total, a report from a host without an idle binding of its own binds nothing,
    and no other host's idle binding is removed.
  - A host whose kept certificate end has passed, or whose certificate is on a revocation list
    the Gateway takes — a serial with a leading zero among them — loses every binding, with its
    route, offer and upstream place, in memory and in the store, and another host's report then
    binds the `instance_uid`. A host that reports under a renewed certificate keeps its bindings
    past the old certificate's `notAfter`, and a report under an expired certificate binds
    nothing.
  - A store that is a link, holds an unparsable line before its last, names an unknown version or
    exceeds its bound makes the Gateway refuse downstream upgrades and answer reports and
    downloads `503` while its upstream connection and its own Supervisors go on; on Unix so does
    one not `0600`; a last line without its newline is dropped and the rest loaded.
  - The journal is compacted at start and at twice its live lines, and the compacted store loads
    to the same bindings.

**Not mechanically decidable:** that no message is synthesised on an Agent's behalf (clauses 9,
10) is an absence; review holds it.
