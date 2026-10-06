# ADR-0013: Admission on the OpAMP endpoint — optional Basic and Bearer credentials, mutual TLS with a Server-issued client certificate, and no authorization between admitted Agents

- **Status:** 🟢 accepted
- **Date:** 2026-08-11
- **Deciders:** Markus Brigl

## Context

Goal 17 of the [specification](../SPECIFICATION.md) asks for three things: TLS on both ends, mutual
TLS, and a Server that *"accepts only authenticated Agent identities"*. TLS
([ADR-0007](0007-dual-transport-and-tls.md)) protects the channel but authenticates nobody: without
authentication, any process that can reach `/v1/opamp` becomes a fleet member. The Protocol Baseline
treats authentication as transport-level HTTP: auth methods MAY be used, `401` MUST be returned on
failure, and the mechanism is the standard `Authorization` header — the Baseline's own
connection-settings example shows `Authorization: Basic …`, and its `ConnectionSettings.headers`
field exists precisely so a Server can rotate such headers
([ADR-0014](0014-server-driven-connection-settings.md)).

**The protocol puts header authorization in the middle and certificates beside it.** The upstream
specification's own sentence is *"Agents (or OpAMP Clients connecting on Agent's behalf) will use
some sort of header-based authorization mechanism (e.g. an 'Authorization' HTTP header or an access
token in a custom header) and **optionally also** client-side certificates."* Mutual TLS is
therefore an addition to the header credential, not a replacement for it — and an implementation
that treats the two as interchangeable is further from the specification than one that stacks them.

**The protocol provides the whole certificate mechanism, and it is not optional-shaped.**
`TLSCertificate` carries a PEM certificate *and* its private key
([`opamp.proto:478-496`](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto#L478-L496)), so a
Server can hand an Agent a complete identity. Alongside it the Baseline defines an Agent-initiated
alternative in which the private key never moves: `AgentToServer.connection_settings_request`
carries a PEM CSR ([`opamp.proto:130-157`](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto#L130-L157)),
and the Server answers with an `OpAMPConnectionSettings` whose `certificate.cert` holds the issued
certificate. The upstream specification's steps are explicit: the Client connects over regular TLS,
generates a keypair and a CSR, sends it, and the Server "creates a client certificate … either by
issuing a self-signed certificate (acting as a local CA) or proxies the CSR to a CA". A failure in
issuing MUST be answered with a `ServerErrorResponse` of type `BadRequest`. An offer that carries a
`certificate`, `tls`, or `proxy` field the Client drops must not be acknowledged `APPLIED`: as
[`CONFORMANCE.md`](../CONFORMANCE.md) puts it, "an ignored field that reports success is worse than
one that reports nothing, because the Server has no way to find out."

The forces:

- **Both header schemes are ecosystem practice.** `opamp-go` clients send static headers
  (`StartSettings.Header` / `HeaderFunc`); the Collector's `opampextension` sets `headers` or
  delegates to an auth extension, and the Collector ships both a `basicauthextension` and a
  `bearertokenauthextension`. Supporting **Basic and Bearer** keeps every mainstream OpAMP client
  connectable.
- **Zero-config operation must survive.** The project's configs are optional throughout
  (ADR-0008); a lab or evaluation setup without credentials must keep working unchanged.
- **Agent and operator are different principals.** The Agent plane and the Operator plane have
  their own listeners ([ADR-0032](0032-agent-and-operator-planes-on-their-own-listeners.md)),
  and the Operator plane has its own authentication
  ([ADR-0032](0032-agent-and-operator-planes-on-their-own-listeners.md)). The Agent plane also serves the
  package download route, which the Client fetches with neither credential nor certificate
  (ADR-0032). Client-certificate enforcement therefore has to happen per route, not per listener.
- **The bootstrap is a chicken and egg, and the specification answers it with a certificate.** A
  Client with no certificate cannot obtain one over a connection that requires one. The upstream
  flow says as much — step 1 is a *regular* TLS connection — and it names its own way out: *"The
  Agent may also use a bootstrap client certificate that is already trusted by the Server"*, whose
  *"distribution and installation method … is not part of this specification"*. The bootstrap is a
  certificate, not a token.
- **Identity is not stable enough to bind a certificate to.** The Baseline makes `instance_uid`
  self-asserted: an Agent chooses its own UID (UUID v7 recommended), and the Server may re-key it at
  any time with `AgentIdentification`, which the Client adopts. Identity is a routing key, not a
  secret ([ADR-0004](0004-protocol-baseline-and-conformance.md)). A certificate whose
  validity depended on the subject matching the current `instance_uid` would be invalidated by a
  re-key the Server itself initiated.
- **A Gateway terminates.** [ADR-0003](0003-client-modes-and-connection-multiplexing.md) has a Client
  in Gateway Mode fold many downstream connections onto few upstream ones, which means reading OpAMP
  and therefore ending the TLS session — the same shape as the OpAMP Gateway Extension, which is an
  OpAMP server downstream and an OpAMP client upstream and delegates all authentication to the Server
  above it. A peer certificate cannot survive that hop; a header can, and ADR-0003 already requires
  that headers be forwarded untouched. One Gateway certificate carries the reports of every
  downstream Agent ([ADR-0024](0024-gateway-mode.md)), so "one connection, one Agent identity" is
  false by design.
- **Authorization is a non-goal.** The specification defers roles, permissions, and tenancy:
  authenticating *that* a peer belongs to the fleet is in scope, *which* Agent may do *what* is not.

These forces leave one property that has to be stated rather than fixed. Any peer that passes
admission may send an `AgentToServer` under *any* `instance_uid`. The Server's `process`
(`crates/server/src/fleet.rs`) takes the reported `instance_uid` at face value and updates that
record's `sequence_num`, `health`, `effective_config`, and `remote_config_status`. Because
`remote_config_status` is the hash that gates whether a Configuration is (re-)offered, a peer polling
as a victim's `instance_uid` can suppress or force config delivery to that Agent, or poison the fleet
view. Over plain HTTP the Server is unable to tell two pollers apart; the WebSocket path rekeys a
*duplicate* `instance_uid` seen on a second connection, but that is collision handling, not
authorization. The credential set and the client CA both prove *fleet membership*; there is no
per-Agent secret anywhere in the model to authorize against.

Prior art (see Sources) is consistent on the certificate shape. The upstream specification defines
the CSR flow as the certificate-rotation story and restricts it to the OpAMP connection: "It is not
possible for the Agent to send a CSR request for own telemetry connections or for other connection
types." Kubernetes' `certificates.k8s.io` CSR flow is the same shape at fleet scale — a client
generates a key locally, submits a CSR, an authenticated-but-unprivileged identity bootstraps the
request, and the control plane signs — which is also where the "who approves" question is normally
answered by policy rather than by a human queue.

## Decision

We will admit peers to the OpAMP endpoint with **optional static HTTP credentials** and **optional
mutual TLS with a Server-issued client identity that stacks on top of the credential rather than
replacing it**: every configured proof must succeed, a bootstrap certificate carries a host to its
first CSR, the private key never leaves the host, and the ADR-0014 offer flow delivers, rotates, and
renews what comes back. Mutual TLS secures **each hop**; the credential travels **end to end**.
Admission proves fleet membership, and **within the admitted fleet there is no authorization between
Agents**.

### Credentials

1. **Server.** An optional `[auth]` section in `server.toml` holds the accepted credentials —
   `bearer_tokens = ["…"]` and/or a `[auth.basic_users]` table of `user = "password"` pairs; an
   `[auth]` section with no credential at all is rejected loudly (ADR-0008). When the section is
   present, every request to `/v1/opamp` — each plain-HTTP POST and the WebSocket upgrade GET,
   checked **before** the upgrade completes — must carry an `Authorization` header matching any
   configured credential; anything else is answered `401` with a `WWW-Authenticate` challenge
   (RFC 9110). Credential comparison is constant-time via the `constant_time_eq` crate (tiny,
   zero-dependency, exactly this job). Without `[auth]` the endpoint stays open.

   Multiple accepted server-side credentials make overlapping rotation possible by hand, and they
   are exactly the shape the Baseline's `ConnectionSettings.headers` rotates (ADR-0014).

2. **Client.** An optional `[auth]` block in `client.toml` with either `bearer_token = "…"` or
   `username`/`password` (both schemes at once are rejected loudly). The Client sends the resulting
   `Authorization` header on every plain-HTTP request and on the WebSocket upgrade. A `401` is
   logged as a credential failure and retried with the existing capped backoff, so fixing the
   credentials on either side needs no restart choreography. The Client warns when it is about to
   send credentials over a non-TLS endpoint that is not loopback.

3. **Scope.** Admission guards `/v1/opamp` only. The REST API and the UI are on the Operator plane
   (ADR-0032), which has its own authentication (ADR-0032). The package download route on the Agent
   plane stays outside admission (ADR-0032). The Supervisor Endpoint stays loopback-only and
   unauthenticated (ADR-0011). A Gateway forwards credentials untouched — *"a Gateway makes no
   authentication decisions"* (ADR-0003).

### Mutual TLS

4. **Client certificates on both transports, on both ends.** The Client presents its certificate on
   `wss://` (its own rustls `ClientConfig`, [`tls.rs`](../../crates/client/src/tls.rs)) and on
   `https://` (a reqwest identity). The Server verifies with a rustls `WebPkiClientVerifier` built
   from an optional `client_ca_file` in `server.toml`'s `[tls]` section, passed to the existing
   listener as a prebuilt `ServerConfig`. No new TLS dependency: rustls, axum-server, reqwest, and
   tokio-tungstenite are all already in the tree (ADR-0007).

5. **The verifier allows unauthenticated peers; the OpAMP route does not.** Client auth is optional
   at the TLS layer, because the package download route on the Agent plane is fetched without a
   certificate (ADR-0032). A small acceptor wrapping the rustls one puts the peer's certificate — or
   its absence — into the request extensions. `/v1/opamp` is the only route that reads it. A
   presented certificate is always verified: a bad one is refused by rustls before any route is
   reached.

6. **Every configured proof must succeed.** Not "either one": with `[auth]` alone the endpoint
   behaves as clause 1 describes; with `client_ca_file` alone it is certificate-only; with both, a
   request must carry a valid credential **and** arrive over a connection bearing a valid client
   certificate. This is the specification's own layering — header authorization, and *optionally
   also* client certificates — and it is the only rule under which adding mutual TLS cannot make a
   fleet less safe than it was. There is no separate "required" switch: configuring a mechanism is
   what requires it, and removing `[auth]` is what makes a fleet certificate-only.

   **So yes, the credential can be dropped entirely — for a fleet without Gateways.** Behind a
   terminating Gateway the credential is the only per-Agent proof that survives the hop (clause 13),
   so a fleet that gateways keeps `[auth]` permanently. The Server records which proofs a peer
   satisfied and shows them on the fleet row, so that choice is made on evidence rather than on
   belief.

7. **A certificate proves membership, not identity.** The Server does not require the subject to
   match the Agent's `instance_uid`, `service.instance.name`, or anything else it reports. Binding
   them would mean a certificate dies whenever the Server re-keys an Agent through
   `AgentIdentification` — an outage this project would inflict on itself. The subject is
   descriptive (the Client's `service.instance.name`), it is recorded and displayed, and it
   authorizes nothing: authorization and tenancy stay the specification's non-goal.

8. **Two sources for a client identity, with ADR-0014's precedence.** An operator may provision one
   in `client.toml`'s `[tls]` section (`cert_file`, `key_file`, beside `ca_file`), and the Client
   stores a Server-issued one in `state_dir` (`client-cert.pem`, `client-key.pem`). The stored pair
   wins over the file, exactly as `connection-settings.pb` wins over `client.toml`, and deleting it
   reverts to what the operator wrote. The private key is written `0600` on Unix; on Windows it
   inherits the `%ProgramData%` layout's ACL, which is what already protects the state directory
   (ADR-0010).

9. **The CSR flow, bootstrapped by a bootstrap certificate.** A Client that wants a certificate and
   has none — or holds one inside its renewal window — generates a keypair and a PEM CSR locally and
   sends it as `ConnectionSettingsRequest.opamp.certificate_request`. The Server signs it as a
   **local CA**: an optional `[client_ca]` section in `server.toml` (`cert_file`, `key_file`,
   `validity_days`, default 90), and `AcceptsConnectionSettingsRequest` is declared **only while
   that section is present** — the same "declare what is actually armed" rule ADR-0014 uses for
   `[connection_offer]` and ADR-0015 for the package store. A CSR that cannot be parsed, or that
   arrives while no `[client_ca]` is configured, is answered with the Baseline's
   `ServerErrorResponse` of type `BadRequest` — its MUST. The issued certificate is returned through
   the ordinary offer path of ADR-0014, so nothing new carries it; a `[client_ca]` therefore also
   makes the Server declare `OffersConnectionSettings` (ADR-0023 point 11).

   The host reaches that first exchange with a **bootstrap client certificate**, the specification's
   own answer to the chicken and egg. It is an ordinary client certificate — the same
   `client_ca_file` verifies it and the Server does not tell it apart from an issued one — and it may
   be **one certificate for the whole fleet**, which is only defensible because of clause 6: under
   "every configured proof must succeed" a bootstrap certificate opens nothing without a valid
   credential beside it. What limits its blast radius is a short validity, not a special case in the
   Server. Distributing it is the operator's, exactly as upstream says.

   **Admission is the approval**: a CSR from a peer that satisfied clause 6 is signed, and there is
   no manual queue. The specification's "awaits for an approval" is met by the proofs already
   required to be on that connection.

10. **Renewal is the same flow, early enough to fail twice.** The Client requests a new certificate
    once the current one is two thirds through its validity, and keeps using the old one until the
    new one is verified — so a Server that cannot sign for a while costs nothing until the last
    third. Verification is ADR-0014's: **connect with the candidate identity before accepting it**,
    the Baseline's MUST; on failure the previous certificate stays in force and the outcome is
    reported `FAILED`.

11. **`tls` and `proxy` are not honoured — and the acknowledgement does not claim otherwise.** An
    offer is applied for every field the Client honours (endpoint, headers, heartbeat, certificate)
    and then reported **`FAILED` with an `error_message` naming the fields that were dropped**,
    never `APPLIED`. The hash is echoed either way, so the Server does not re-offer in a loop.
    `TLSConnectionSettings` is refused on merit, not only for want of time: its fields are
    `ca_pem_contents`, `include_system_ca_certs_pool`, `insecure_skip_verify`, `min_version`,
    `max_version`, `cipher_suites`
    ([`opamp.proto:434-452`](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto#L434-L452)) — a
    Server that can command `insecure_skip_verify` can turn off the verification that authenticates
    *it*, and trust in this project is an operator's file (ADR-0007), not a Server's instruction.
    `ProxyConnectionSettings` has nothing on the Client to configure. Both are `[Development]`
    upstream.

12. **Three dependencies, none of them new C: `rcgen`, `time`, `x509-parser`.**

    ```toml
    rcgen = { version = "0.14", default-features = false, features = ["crypto", "ring", "pem", "x509-parser"] }
    time  = { version = "0.3", default-features = false, features = ["std"] }
    x509-parser = "0.18"
    ```

    `rcgen` is the one that matters: the Client produces the CSR with it, the Server signs it
    (`CertificateSigningRequestParams` → `signed_by`). Features are stated rather than inherited, as
    everywhere else in this workspace, because the backend is the point: `ring` is rcgen's default
    and is the provider ADR-0007 already installs, so **no new C and no cmake enter the build** —
    whereas `aws_lc_rs`, rcgen's alternative, would bring both.

    The other two are what rcgen leaves to the caller. It expresses a certificate's validity as
    `time::OffsetDateTime` and re-exports none of that crate, so "now plus ninety days" needs `time`
    itself rather than hand-rolled calendar arithmetic. And it parses X.509 only behind a private,
    test-only path, so reading back the validity of the certificate a Client holds — which is what
    decides when to renew — goes through `x509-parser`, the parser rcgen itself uses. Both are
    already in the tree as rcgen's own dependencies; declaring them adds manifest lines, not builds.

    Stated plainly: this workspace is **not** free of C. `ring` ships C and assembly and compiles
    them in its build script, so a C compiler is already required to build either binary. What
    ADR-0006 and ADR-0007 actually secured is the absence of *system* dependencies — no OpenSSL, no
    cmake, no `protoc` — and that is what this choice preserves.

    Nothing else is added: rustls client auth, `WebPkiClientVerifier`, reqwest's rustls identity, and
    `RustlsConfig::from_config` are all in crates the workspace already depends on.

13. **Mutual TLS is hop-by-hop; authentication travels end to end.** Where a Client in Gateway Mode
    stands between an Agent and the Server there are **two** mutual-TLS connections — Agent to
    Gateway, and Gateway to Server — each proving the peer at that hop. The Gateway holds a client
    identity of its own for the upstream leg, obtained through this same CSR flow, and it **forwards
    the Agent's authentication unchanged**, which is what ADR-0003 already requires of it and what
    the OpAMP Gateway Extension does in the same position.

    Two things follow, and they are the reason this clause is a decision rather than a note. The
    certificate the Server verifies behind a Gateway is the **Gateway's**, so clause 7 — a
    certificate proves membership, never identity — is not a convenience but a property of the
    topology. And the per-Agent proof behind a Gateway is the credential, which is why clause 6's
    "the credential can be dropped" holds only for a fleet whose Clients connect directly.

    The configuration surface a Gateway needs — its own server certificate downstream, its own
    `client_ca_file` for the Agents it accepts — is designed in ADR-0024 point 8.

### The trust boundary

14. **Admission is a fleet-wide trust boundary, and within it there is no authorization between
    Agents**: a report's `instance_uid` is self-asserted, and any admitted peer may report under any
    `instance_uid`. This is made explicit in operator-facing documentation
    ([`SECURITY.md`](../../SECURITY.md)) and in the Server's identity/admission module docs. Per-Agent
    authorization is **not** added by default, because the only mechanism that could provide it —
    binding a certificate to `instance_uid` — is rejected by clause 7 and is incompatible with
    Gateway Mode (ADR-0024) and with Server-initiated re-keying.

15. **Mutually distrusting Agents do not share an admitted fleet.** Operators who require isolation
    between them separate them by Server instance or network segment. This is the same shape as the
    OpAMP specification's own guidance that the transport, not the message, is the trust boundary.

What this does **not** decide: whether the Server may proxy a CSR to an external CA instead of
signing it. It is described under Consequences.

## Alternatives considered

- **Mutual TLS as the only authentication mechanism, with no header credential.** Authenticates the
  peer at the channel, but demands a client-certificate PKI on day one and does nothing for the
  low-friction start, and the Baseline's own rotation story treats authorization headers as the
  first-class credential. Clause 6 makes a certificate-only fleet reachable by configuration, but not
  as the *rule*: it forecloses Gateway Mode, where a terminating hop leaves the credential as the
  only per-Agent proof (clause 13).
- **Per-Agent credentials mapped to `instance_uid`** — turns authentication into identity
  management and brushes against authorization/tenancy, which the specification explicitly defers.
  One fleet-membership check is what goal 17 asks for. It is a much larger change to the identity
  model and the admission story.
- **Authenticating the whole listener (REST API + UI too)** — operator and Agent are different
  principals with different lifecycles. The Operator plane has its own listener (ADR-0032) and its
  own authentication (ADR-0032).
- **Hashed passwords (htpasswd/argon2) in `server.toml`** — the config file is operator-owned and
  permission-protected exactly like the TLS private key next to it (ADR-0007); hashing adds a
  tooling step without changing that trust boundary. Revisit if credentials ever leave the file.
- **Pluggable authenticators (OIDC, JWT validation, external IdP)** — YAGNI; static credentials
  satisfy "belongs to the fleet", and the hexagonal seam to add a validator later is not made
  harder by shipping the simple thing first.
- **Operator-provisioned certificates only, no CSR flow.** The smallest thing that closes the mutual
  TLS row: two file paths on each side and no issuance at all. Rejected as the *whole* answer — it
  leaves every host's certificate to be created, delivered, and renewed by whatever the operator
  builds, which is the "reaching a hundred machines" problem the specification exists to remove, and
  it leaves `AcceptsConnectionSettingsRequest` and the `certificate` field unimplemented. It
  survives as clause 8's first source, for fleets that already run a PKI.
- **The Server hands out certificate *and* private key, no CSR.** The protocol explicitly allows it —
  `TLSCertificate.private_key` is a required field of that message — and it is less code than
  issuing from a CSR. Rejected: every Agent's private key would then exist on the Server, travel the
  wire, and sit in whatever the Server persists, so one compromised Server yields the whole fleet's
  identities. The CSR flow exists precisely to avoid that, and the upstream specification treats it
  as the certificate story rather than as an exotic option.
- **Admission where any one configured proof suffices (the "or" rule).** It reads as the friendlier
  shape: a host with a credential but no certificate still gets in, so a fleet migrates without
  anyone provisioning anything. Rejected once the consequence is stated plainly — while `[auth]` is
  configured, a leaked token walks straight past the mutual TLS that was just installed, so adding a
  mechanism would not make the fleet safer, only busier. It also inverts the specification's own
  layering, which has certificates *added to* header authorization rather than standing in for it.
- **The credential as the bootstrap, instead of a bootstrap certificate.** The companion of the "or"
  rule: a certless host is admitted on its token for exactly long enough to submit a CSR. Rejected
  with it. It is the "or" rule narrowed to one message type, it needs a per-message exception in the
  admission path that exists for no other purpose, and the specification already answers the
  question with a bootstrap certificate whose distribution it deliberately leaves to the operator.
- **Require client certificates in the TLS handshake on the Agent plane.** One line instead of an
  acceptor wrapper. Rejected: the Agent plane also serves the package download, which the Client
  fetches without a certificate, so this would break every download (ADR-0032). A separate listener
  for downloads alone is rejected in ADR-0032.
- **Bind the certificate subject to the Agent's `instance_uid` and reject mismatches (true per-Agent
  authorization).** The strongest reading of "accepts only authenticated Agent identities". Rejected
  in clause 7: `AgentIdentification` re-keys an Agent at the Server's own initiative, and a
  certificate that a re-key invalidates turns a routine duplicate-uid resolution into a lost host.
  It would also break certificate renewal and Gateway Mode (one Gateway certificate legitimately
  carries many UIDs). It trades a medium-severity in-fleet poisoning risk for an outage of the
  Server's own making.
- **Trust-on-first-use pinning of `instance_uid` ↔ certificate.** Rejected — a legitimate certificate
  renewal (clause 10) or an Agent moving between Gateways changes the presented certificate, which
  TOFU would flag as impersonation. It would fire false positives on the system's own mechanisms.
- **Reject a report whose `sequence_num` regresses.** Rejected — an Agent restart legitimately resets
  `sequence_num` and sends a full report (the Baseline's own recovery), which a forger can imitate.
  It authorizes nothing while breaking legitimate restarts.
- **An optional, opt-in strict mode** (a config switch that binds certificate subject → `instance_uid`
  on the mTLS path and refuses mismatches, usable only where Gateway Mode is off and the Server does
  not re-key). Not chosen, but the least-incompatible way to offer real per-Agent isolation to
  operators who want it; recorded as possible future work rather than decided now, because it adds a
  security-relevant configuration surface and a mutual exclusion with Gateway Mode that deserve their
  own decision.
- **Honour `TLSConnectionSettings`.** It would close a `[Development]` field and one more part of
  the offer. Rejected in clause 11: most of what it can say weakens verification, and a Server able
  to command `insecure_skip_verify` on its Agents can disarm the check that proves it is the Server.
- **Acknowledge `APPLIED` and document the gap.** Zero code. Rejected: CONFORMANCE documents a
  *gap*, not a false report, and the whole reason the document exists (goal 12) is that a declared
  capability is a promise a peer may rely on.
- **Refuse an offer wholesale when it carries a field the Client cannot honour.** More consistent
  than partial application — `FAILED` would then mean "nothing happened". Rejected: a Server that
  adds a `proxy` field to an offer would then also stop the endpoint and credential rotation that
  the same offer carries, turning an unsupported extra into a fleet-wide freeze of the mechanism
  that fixes things.
- **A manual approval queue for CSRs** (the specification's "awaits for an approval"). Rejected for
  now: approval is an operator-facing workflow with a UI and a REST resource behind it, and this
  Server ships only a rudimentary UI by design. An unattended queue is an outage that looks like a
  policy. Admission is the approval, and a fleet that wants human approval can withhold
  `[client_ca]`.
- **A separate bootstrap CA, whose certificates may only submit a CSR.** Stricter than clause 9: a
  bootstrap certificate could then never be used as a working identity. Rejected as part of this
  decision — it needs a second trust anchor to configure and a per-message rule in the admission
  path, to constrain something that already opens nothing on its own. It is named as a follow-up if
  a deployment turns out to need it.
- **Client certificates end to end through a Gateway, by passing the connection through at the TCP
  level.** It is the only way an Agent's own certificate can reach the Server through a Gateway, and
  it would let mutual TLS identify the Agent rather than the hop. Rejected: a Gateway that cannot
  read OpAMP cannot fold *n* Agents onto *m* connections, which is the entire purpose ADR-0003 gives
  it — the result would solve reachability behind a firewall while giving up connection scaling.
  Clause 13 takes the hop-by-hop shape instead, which keeps both.
- **Proxy the CSR to an external CA** (the specification's other option, e.g. an ACME or Vault
  backend). Rejected as part of this decision: it is a second issuance path with its own failure
  modes and credentials, and nothing in the fleet needs it yet. Clause 9's `[client_ca]` is the seam
  it would be added behind.

## Sources / Prior art

- [OpAMP specification](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md) —
  connection settings `headers`: *"Typically used to set access tokens or other authorization
  headers"*, with the `Authorization: Basic …` example; auth methods MAY be used and `401` MUST be
  returned on failure (tracked in [`CONFORMANCE.md`](../CONFORMANCE.md)). Connection Settings
  Management and the client-certificate CSR flow — the numbered flow (regular TLS, keypair and CSR
  generated by the Agent, Server issues as a local CA or proxies to one), the
  `ServerErrorResponse`/`BadRequest` MUST when issuance fails, the restriction of the flow to the
  OpAMP connection, and the "verify by actually connecting" MUST. `instance_uid` is Agent-chosen and
  Server-assignable (`ServerToAgent.agent_identification`); the security model treats the
  connection, authenticated by the transport, as the trust boundary rather than authorizing
  individual messages. Baseline `v0.19.0` (see [`CONFORMANCE.md`](../CONFORMANCE.md)).
- The vendored Baseline schema itself:
  [`TLSCertificate`](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto#L478-L496),
  [`ConnectionSettingsRequest`](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto#L130-L157),
  [`TLSConnectionSettings`](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto#L434-L452),
  [`ProxyConnectionSettings`](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto#L455-L464).
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
- [rcgen](https://docs.rs/rcgen/latest/rcgen/struct.CertificateSigningRequestParams.html) — CSR
  generation and issuance from a CSR (`CertificateSigningRequestParams::from_pem` → `signed_by`);
  its [feature flags](https://lib.rs/crates/rcgen/features) confirm the default set
  (`crypto, pem, ring`), `x509-parser` as the one that reads a CSR, and `aws_lc_rs` as the opt-in
  alternative backend that clause 12 declines.
- [`ring`'s build requirements](https://docs.rs/crate/ring/latest) — the crate ships C, C++ and
  assembly and compiles them in its build script, which is the evidence behind clause 12: this
  workspace already needs a C compiler, and choosing rcgen's ring backend changes that in neither
  direction.
- [`reqwest::tls::Identity`](https://docs.rs/reqwest/latest/reqwest/tls/struct.Identity.html) — the
  client identity for the plain-HTTP transport. Note for the implementation: under the rustls
  feature the constructor is `from_pem`, taking key **and** certificate in one buffer;
  `from_pkcs8_pem`, which takes them separately, exists only for native-tls.
- [`axum_server::tls_rustls::RustlsConfig::from_config`](https://docs.rs/axum-server/latest/axum_server/tls_rustls/struct.RustlsConfig.html)
  — takes a prebuilt rustls `ServerConfig`, which is where the client verifier is attached; the
  [`axum-server-mtls`](https://lib.rs/crates/axum-server-mtls) crate is the pattern clause 5 follows
  for injecting the peer certificate into request extensions by wrapping `RustlsAcceptor`. Read as a
  design reference; the wrapper is small enough that this decision writes it rather than depending
  on it.
- [Kubernetes `certificates.k8s.io` CSR flow](https://kubernetes.io/docs/reference/access-authn-authz/certificate-signing-requests/)
  and [`client-go`'s certificate rotation](https://github.com/kubernetes/client-go/blob/master/util/certificate/csr/csr.go)
  — the same shape at fleet scale: locally generated key, submitted CSR, bootstrap identity distinct
  from the issued one, renewal before expiry rather than after failure. The two-thirds renewal
  window of clause 10 follows that lineage.
- Comparable systems: mutual-TLS fleets (e.g. a shared client CA in service meshes) likewise prove
  membership, not per-caller identity, unless a separate identity document (SPIFFE SVID and the like)
  is layered on — which is the "per-Agent credentials" alternative above.
- This project's own decisions: [ADR-0007](0007-dual-transport-and-tls.md) (rustls, no OpenSSL),
  [ADR-0014](0014-server-driven-connection-settings.md) (verify-by-connecting, persisted settings
  overriding `client.toml`, capability declared only while armed), ADR-0024 (a Gateway certificate
  carries many Agents), ADR-0004 (Baseline conformance), and the
  [Mutual TLS section of `CONFORMANCE.md`](../CONFORMANCE.md).

## Consequences

- Positive: goal 17 is complete — the connection is encrypted, the peer is proved, and the proof is
  something this project issues and rotates rather than something an operator distributes by hand.
  Every mainstream OpAMP client (opamp-go, `opampextension`) can present the required header, and
  the credential shape is the one the Baseline's connection settings rotate (ADR-0014). The Client
  does not report `APPLIED` for work it did not do.
- Positive: adding mutual TLS to a running fleet can only tighten it. Under clause 6 every mechanism
  an operator configures is enforced, so turning `client_ca_file` on never widens admission — the
  failure mode of a half-finished rollout is hosts that cannot connect, which is loud, rather than
  hosts that connect without the new proof, which is silent.
- Positive: the certificate machinery is exactly what `TelemetryConnectionSettings` needs, since that
  message carries the same `certificate`, `tls`, and `proxy` fields. Own-telemetry work inherits it
  instead of repeating it.
- Positive: the trust model is an explicit, reviewable decision rather than an implicit property.
  Operators can reason about it and design around it (one fleet = one trust domain; isolate by Server
  or network).
- Negative / trade-offs: credentials sit in plaintext in operator-owned config files (accepted —
  same trust boundary as the TLS key); Basic and Bearer over non-TLS transports are cleartext
  (mitigated by a Client warning, ultimately the operator's choice); a shared fleet credential
  identifies membership, not individual Agents.
- Negative / trade-offs: the Server becomes a CA. Its signing key is the fleet's trust anchor, and
  compromising it means being able to mint fleet members — which is why clause 9 keeps it in its own
  section and its own files rather than reusing the listener's key, and why the upstream schema's
  own comment warns against co-locating a CA key with a server certificate.
- Negative / trade-offs: **an expired certificate locks a host out, credential or not.** That is the
  direct cost of clause 6, and it is the sharpest edge of this decision: a Client switched off longer
  than its certificate's validity comes back unable to connect. Clause 10's renewal at two thirds is
  the mitigation for hosts that stay up; for the others the recovery is an operator's — temporarily
  unset `client_ca_file`, let the host re-enrol, put it back — and that procedure has to be written
  in the manual rather than discovered.
- Negative / trade-offs: a bootstrap certificate has to be distributed, which is exactly the
  per-host errand the specification exists to remove — mitigated only by it being *one* certificate
  for the fleet rather than one per host, and by it opening nothing on its own. Where it comes from
  on a fresh install (the interactive install of ADR-0020 is the obvious place) is a decision this
  one deliberately does not make.
- Negative / trade-offs: hop-by-hop mutual TLS (clause 13) means the Server can prove which
  *Gateway* a message came through, never which Agent produced it. That is the honest limit of
  certificates in a multiplexed topology, and it is why the credential stays load-bearing for
  gatewayed fleets rather than being a legacy mechanism on its way out.
- Negative / trade-offs: a malicious or compromised admitted peer can poison another Agent's
  Server-side record — most cleanly over plain HTTP, which offers nothing to distinguish pollers. We
  accept this *within* a fleet that shares admission; it is not a cross-fleet or unauthenticated
  exposure.
- Follow-ups: proxying a CSR to an external CA, if a deployment ever needs one, behind the
  `[client_ca]` seam. A separate bootstrap CA whose certificates may only enrol. Certificate
  revocation — a CRL or short validity — is not addressed here; short validity plus renewal is what
  this decision relies on, and a revocation story is its own decision if a fleet needs to eject a
  host faster than a certificate expires. An optional strict per-Agent-identity mode (excluding
  Gateway Mode and re-keying), and separately a per-Agent-credential identity model, are each
  possible future decisions, to be given their own ADR if and when pursued.
