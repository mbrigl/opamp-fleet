# Hardening the Client–Server Link

**This document is a backlog, not a decision.** Everything below is a *candidate* measure for
hardening the connection between the Client and the Server: written down so it can be picked up
deliberately later instead of being rediscovered, and ordered so the cheap wins are not buried under
the expensive ones. Nothing here is binding. Each measure states what it would cost and what still
has to be established, and the ones that constrain the architecture say so and name the ADR they
would have to supersede — by topic, since the number does not exist yet.

Every measure is stated here in full, so this document can be read on its own. One of them (H3) is
*also* a conformance matter, because it closes a Baseline MUST that is currently unmet; that fact —
and only that fact — is recorded where conformance is tracked, in
[`CONFORMANCE.md`](CONFORMANCE.md#mutual-tls-and-the-two-fields-still-refused). Nothing else below
is a conformance question: the Baseline requires none of it.

## Scope

The link this document is about is **Client ↔ Server**. In this project's vocabulary a Supervisor
lives *inside* the Client ([`crates/client/src/supervisor/`](../crates/client/src/supervisor/)) and
does not speak to the Server itself — the Client carries every Supervisor's Agent over its one
connection (ADR-0003, ADR-0011). Where other OpAMP material says "Supervisor ↔ Server", this is the
link it means.

One adjacent surface is in scope because it terminates on the same host and carries the same
protocol: the **Supervisor Endpoint**, the loopback WebSocket each Supervisor serves for a Managed
Process's `opampextension` ([`endpoint.rs`](../crates/client/src/supervisor/endpoint.rs)). It is
treated separately at the end.

Out of scope, and deliberately so: **authorization and multi-tenancy**, which the specification
names as non-goals. The boundary is worth stating precisely because one measure below (H5) runs
close to it — *which Agent is speaking* is authentication and belongs here; *what that Agent is
allowed to do* is authorization and does not.

## What already holds

Stated first so the list below is not read as a list of absences. On this link the project already
has:

- TLS on both transports, on both ends, with a private CA supported on the Client (ADR-0007).
- **Cumulative** admission on `/v1/opamp`: every configured proof must succeed — a credential when
  `[auth]` is set, a client certificate when a client CA is, both when both are (ADR-0035,
  `transport::Admission`). Not "either one", which is what keeps switching mutual TLS on from ever
  admitting more than before.
- Constant-time comparison of the presented `Authorization` value, so a comparison leaks nothing
  about how far it matched — on both planes, from one primitive (`credentials.rs`).
- Optional Basic authentication over the **whole** Operator plane, the UI included (`[rest.auth]`,
  ADR-0067), on a listener that is loopback until an operator publishes it (ADR-0066).
- Client certificates the Server issues itself through the Baseline's CSR flow, with the Agent
  keeping its private key — and with the request's `basicConstraints`, `keyUsage`,
  `extendedKeyUsage`, and SANs **overwritten** rather than carried over, so a CSR cannot ask for the
  powers of a CA ([`ca.rs`](../crates/server/src/ca.rs)).
- Message size limits enforced in both directions on both transports, and at the Supervisor
  Endpoint.
- **Connection setup bounded on both of the Server's planes** (ADR-0073): a peer has 30 seconds to
  send its request line and headers, and 10 seconds to complete the TLS handshake, before it is hung
  up on — enforced below every other limit in this list, because it applies before a request exists
  and therefore before Admission ever runs.
- Package content hashed always, and Ed25519-verified when a key is configured; archive members
  validated before anything is written.
- `TLSConnectionSettings` and `ProxyConnectionSettings` refused on merit, so a Server cannot command
  a Client to weaken its own verification
  ([`CONFORMANCE.md`](CONFORMANCE.md#mutual-tls-and-the-two-fields-still-refused)).

### Where each bound applies today

The list above is per mechanism; this is the same state per **surface**, since a rule that holds on
one listener and not on its neighbour is the failure mode worth seeing at a glance. ✅ in force,
⚠️ partial, ❌ absent.

- **Agent plane** — `0.0.0.0:4320`, public by default (ADR-0066).
  - ✅ TLS handshake ≤ 10 s · ✅ headers ≤ 30 s (HTTP/1) · ✅ message size, in both directions ·
    ✅ gzip bounded *after* decompression · ✅ Admission, cumulative
  - ❌ concurrent connections: uncapped (**H16**) · ❌ HTTP/2 has no header bound (**H17**) ·
    ❌ attempt rate: unthrottled (**H10**)
- **Operator plane** — `127.0.0.1:4321` until an operator publishes it (ADR-0066).
  - ✅ TLS handshake ≤ 10 s · ✅ headers ≤ 30 s · ✅ optional Basic over the whole plane (ADR-0067) ·
    ✅ Fetch-Metadata CSRF guard on the body-less `POST` routes
  - ⚠️ the package upload is unbounded in **time** and, by decision, in size (ADR-0008) — the one
    route where that is intended · ❌ H16, H17, H10 as above
- **Client → Server**, outbound (`transport/http.rs`, `transport/ws.rs`).
  - ✅ request timeout 30 s on the polling transport · ✅ redirects refused outright ·
    ✅ reconnect backoff · ✅ message size in both directions
- **Gateway endpoint** — the Client serving OpAMP downstream (ADR-0037).
  - ✅ message size, gzip after decompression, per-hop exchange timeout, `max_carried_agents`
  - ❌ **no header-read bound**: it runs on `axum::serve` and `axum_server` without a timer, which is
    exactly the state the Server was in before ADR-0073 (**H18**)
- **Supervisor Endpoint** — loopback, one Managed Process (`supervisor/endpoint.rs`).
  - ✅ message size in both directions
  - ❌ **no handshake bound**, and connections are served one at a time by design: a local process
    that connects and never completes the WebSocket upgrade holds the endpoint against the Agent it
    exists for (**H18**)

What follows is therefore not "make it secure" but two narrower things: **close the windows during
which a withdrawn credential still works**, and **shrink the surface that sits beside the protocol**.

**Status of each measure below:** 🔴 not taken · 🟡 partly in force · ⚪ cannot be judged until the
check under [Unverified claims](#unverified-claims) is run · 🟢 in force. Nothing is 🟢 here by
construction: a measure that reaches it moves up into [What already holds](#what-already-holds), the
way the connection-setup bound did when ADR-0073 took it. The ✅/⚠️/❌ marks in that section are the
same three states seen per *surface* rather than per measure.

## Stage 1 — Separate rotation from revocation

The theme of this stage: today a credential can be *replaced*, but not *withdrawn*. Until that
changes, every other identity measure is advisory.

🔴 **H1 — End an established WebSocket session when the credential behind it changes.**
`[auth]` is checked on every plain-HTTP POST but exactly once per WebSocket connection, at the
upgrade (ADR-0013) — which is all the transport offers, since the header rides the upgrade request
and nothing after it. A credential rotated through a connection-settings offer (ADR-0014), or one
struck from `server.toml` outright, therefore governs only the *next* connection: every session
already up keeps running on the credential it was admitted with, for as long as it stays up. On
plain HTTP the same rotation takes hold at the next poll. This is the credential half of what
[`CONFORMANCE.md`](CONFORMANCE.md#mutual-tls-and-the-two-fields-still-refused) says about
certificate revocation — the Server cannot eject a peer sooner than that peer chooses to reconnect —
and it is the reason rotation must not be mistaken for revocation.
**To work out:** whether a maximum session age is the answer or a re-authentication the Server
pushes; what either costs in reconnect churn across a fleet, given every drop is a reconnect storm's
worth of handshakes; and how it behaves in Gateway Mode (ADR-0037), where one upstream connection
carries many Agents and closing it disconnects all of them at once. Needs an ADR for that last
reason, and H10 belongs in the same decision — the churn this creates is the churn that one bounds.

🔴 **H2 — A Server-side revocation list, checked in `Admission`.**
Keyed by certificate serial number and by credential. This deliberately avoids CRL and OCSP: the
Server is the only party that verifies anything on this link, so revocation can be Server state
rather than infrastructure. On its own it only takes effect at the next connection — H1 is what
makes it immediate, which is why the two are one piece of work and not two.

🔴 **H3 — Verify a CSR's `instance_uid` against its sender.**
The Baseline: *"When the Server receives a CSR containing the instance_uid in CSR fields the Server
MUST verify that the instance_uid field in AgentToServer message matches the instance_uid in the CSR
fields"*, justified as what *"prevents Agents impersonating other Agents"*. This Server performs no
such check — `ClientCa::sign` treats the subject as descriptive and drops the request's SANs. Note
what this is *not*: it is not the question H4 opens. Refusing to bind the **issued** certificate to
an `instance_uid` is right, for the reason ADR-0035 gives; the MUST is about rejecting a mismatched
**request**, and nothing about re-keying argues against that. The MUST is conditional and nothing
triggers it today — this project's Client puts its configured name in the CSR's common name and no
`instance_uid` anywhere — but the Baseline invites a peer implementation to include one, and
interoperability with such Clients is a stated target (ADR-0040).
**To work out:** where an `instance_uid` may legitimately appear in a CSR, given the Baseline
prescribes no field for it (*"one of the CSR fields (or part of the field)"*); how to recognise one
without reading an ordinary descriptive subject as a claim; and whether a mismatch is answered
`BadRequest` or stripped in silence. The smallest item of this stage and the only one needing no
ADR — it changes no interface, adds no state, and refuses something already refusable as
`BadRequest`.

## Stage 2 — Sharpen identity

🔴 **H4 — Decide what a client certificate proves: fleet membership, or a specific Agent.**
Today it proves membership only, and that is a recorded decision with a real reason: binding the
issued certificate to an `instance_uid` would mean a re-key through `AgentIdentification` kills a
certificate the Server itself issued (ADR-0035, ADR-0047). Hardening this means *resolving* that
conflict rather than working around it. Three approaches are worth weighing, and none is obviously
right:

- a **stable enrolment identity** carried in the certificate, distinct from the re-keyable
  `instance_uid`, with the Server holding the mapping;
- **stop re-keying** while certificates are in force, making `instance_uid` stable by construction
  and accepting what that costs in duplicate-identity handling;
- **bind only on direct connections**, leaving a gatewayed fleet on membership proof, since the
  certificate the Server sees there is the Gateway's anyway.

This is the most expensive measure in the document and the one with the widest blast radius. It
would need an ADR superseding ADR-0035 on this specific point, and that ADR is where the
authorization boundary named under [Scope](#scope) has to be drawn explicitly — otherwise it moves
unnoticed.

🔴 **H5 — Make enrolment an approval, not a side effect of admission.**
Today the endpoint's own admission *is* the approval: whatever satisfies `/v1/opamp` gets a
certificate signed. That is a defensible reading of the Baseline, which leaves the approval policy
open — but it means one leaked fleet credential yields certificates, not just access. Harder: a
single-use bootstrap credential per host, consumed by the first issuance, plus an explicit approval
queue surfaced in the UI. Note the interaction with H4: an approval queue is only meaningful if what
is approved is an identity rather than a membership.

🔴 **H6 — Shorten certificate validity once renewal is proven.**
`validity_days` defaults to 90. Short validity is what stands in for revocation today, so it is
currently set far too coarse for that job. This measure is cheap but must follow H1/H2, not precede
them: shortening validity without a revocation path just moves the failure from "cannot eject a
host" to "eject the whole fleet by accident".

## Stage 3 — Stop storing secrets in the clear

🔴 **H7 — Store admission credentials hashed, and referenced rather than inline.**
`server.toml` holds Bearer tokens and Basic passwords verbatim, so they reach backups, diffs, and
config management. Since ADR-0067 this is **two** sections — `[auth]` for the fleet and
`[rest.auth]` for the operators — and they have to change together, or the file ends up carrying two
credential formats. Two different answers are needed for the two schemes, and conflating them would
be a mistake: Basic passwords want a password hash (Argon2/bcrypt), while running a KDF per Bearer
request is itself a denial-of-service vector — a plain SHA-256 over a high-entropy token is the
right shape there. Either way the constant-time comparison already in place has to survive the
change. Secondly, allowing a credential to be named by file or environment reference keeps it out of
the configuration file altogether.

⚪ **H8 — Confirm the file mode of issued key material.** *(verify first — see
[Unverified claims](#unverified-claims))*
The Client writes its configuration with mode `0600`
([`reconfigure.rs`](../crates/client/src/reconfigure.rs)). Whether the private key obtained through
the CSR flow and the cache of rotated credentials get the same treatment in the state directory has
not been established. If they do, this item disappears; if they do not, it is the cheapest fix in
the document.

## Stage 4 — Shrink the surface and bound the abuse

🟡 **H9 — Require the client certificate in the TLS handshake on the Agent plane.** *(half taken —
ADR-0066)*
The listener split this measure asked for is **done**: the REST API and the UI have their own
listener (ADR-0066, superseding ADR-0005 on that point), and the OpAMP endpoint no longer shares a
port with a browser. What has *not* changed is the verifier: client authentication is still
*optional* at the TLS layer and required on the route
([`tls.rs`](../crates/server/src/tls.rs)) — and the reason is now a different one. The Agent plane
also serves the **package download**, which a Client fetches presenting no certificate (the artifact
is protected by its hash and signature, ADR-0015), so requiring one in the handshake today would
break every rollout.
**To work out:** whether the Client's downloader should present its certificate when the artifact
host is its own Server — and what that means for a `download_url` pointing at a mirror, where
sending it would be wrong — or whether the download plane gets a listener of its own. Only then can
the handshake require the certificate, so that an unauthorized peer dies before it reaches any
handler and the route check becomes a second line rather than the only one. Needs an ADR for
whichever shape wins.

🔴 **H10 — Throttle, as the Baseline's SHOULD asks.**
`ServerErrorResponse` with `ServerErrorResponseType_Unavailable` and `retry_info` is not emitted;
nothing in the Server rate-limits anything today, and `max_agents` bounds the fleet but not the
attempt rate. This bounds enrolment flooding and credential guessing — and it bounds the reconnect
storm H1 can itself cause, which is why it is part of that decision rather than a separate one.

🔴 **H11 — Pin the TLS version floor.**
Neither end sets `min_version`, so both inherit the rustls default (TLS 1.2 and 1.3, safe suites
only — the default is not a weak position). TLS 1.3 only is a small change and worth taking once
every peer in a deployment can do it; the reason to write it down rather than just do it is that it
is a compatibility decision, not a code decision.

🔴 **H16 — Cap concurrent connections per plane.**
ADR-0073 made each connection cheap and short-lived while it is still unproven, but not *few*:
nothing bounds how many a peer may hold open at once, and `max_agents` bounds the fleet, not the
sockets. The cap belongs at the accept loop, where refusing costs one `accept` and a close — and it
has to be a number an operator can raise, since a legitimate fleet reconnecting after a Server
restart arrives all at once. Related to H10 and separate from it: H10 bounds the *rate* of attempts
with the Baseline's own answer (`retry_info`), this bounds the *number* held simultaneously, which
no protocol message expresses.

🔴 **H17 — Bound HTTP/2 the way HTTP/1 is now bounded.**
The TLS listeners offer `h2` by ALPN, and hyper's header-read timeout is HTTP/1 only — HTTP/2 has no
equivalent, because there is no header phase to time. Its analogues are `max_concurrent_streams`, the
header-list size, and keep-alive pings that evict a peer which stops answering. None is set today, so
an h2 peer is bounded by message size and by nothing else. Cheap to take, but it is a set of numbers
that wants measuring against a real fleet rather than guessing — and it is the reason ADR-0073 says
"HTTP/1" and not "the transport".

🔴 **H18 — Give the Client's own listeners the bound the Server's have.**
Two surfaces on the Client speak the server side of this protocol and were untouched by ADR-0073:
the **Gateway** endpoint (ADR-0037), which runs on `axum::serve` and `axum_server` with no timer
installed and is therefore in exactly the state the Server was in; and the **Supervisor Endpoint**,
which wraps `accept_async_with_config` in no timeout at all and serves connections one at a time, so
a half-finished handshake does not merely cost memory — it holds the endpoint against the Managed
Process it exists for. The Gateway half is the same three lines as ADR-0073 applied to a different
binary. The Supervisor Endpoint half is a `tokio::time::timeout` around the upgrade, and is the
cheapest item in this document.

## Stage 5 — The channels that put code on the host

Remote configuration and package delivery are the paths by which the Server causes code to run on an
Agent's host. They deserve at least as much attention as the transport, and arguably more.

🟡 **H12 — Make package signature verification mandatory instead of conditional.**
Today the content hash is always checked and the Ed25519 signature is checked *when a key is
configured* — which means an unconfigured Client fails open. The hardened form is fail-closed: no
signing key, no package applied. The cost is operational, not technical: every fleet must then
manage a key before it can distribute anything.

🔴 **H13 — Allow-list the sources of referenced packages.**
A referenced package (ADR-0018) is fetched from an operator-supplied URL. The hash and TLS
verification already apply, so this is not an open hole — but the set of hosts a Client will fetch
from is currently unbounded, and bounding it is cheap.

⚪ **H14 — Establish how far remote configuration is already constrained.** *(verify first)*
The Baseline asks that the Server restrict what configuration can be set remotely and what the Agent
accepts. ADR-0021 (path-implied package consent) and ADR-0057 (a Server-pushed Supervisor block
names only what the Client already owns) plainly cover part of this ground. **How much** they cover
has not been established, and that has to come first — building a new restriction on top of an
unexamined one would be the wrong order.

## Stage 6 — Make it provable after the fact

🔴 **H15 — An audit record for admission, issuance, rotation, revocation, and package application.**
Every measure above changes what the Server permits; none of them is demonstrable afterwards without
a record of what was permitted and to whom. This is last in order but not in importance — it is what
turns an incident into an investigation.

## Unverified claims

Two claims in this document rest on reading the code, not on running it, and are marked in place:

- **H8** — whether the CSR-obtained private key and the rotated-credential cache are written with a
  restrictive file mode.
- **H14** — how much of the Baseline's "restrict what the Agent can accept" ADR-0021 and ADR-0057
  already cover.

Both are cheap to settle and both change what the measure above them is worth, so they belong before
the planning rather than inside it.

## Suggested order

**H18 first — it is the smallest item here and it closes a gap the Server no longer has.** ADR-0073
bounded the Server's two planes; leaving the Client's two listeners unbounded means the fleet's
weakest surface is now the one running on the hosts, and the fix is already written next door.

**Then H1 + H2 + H10 as one decision, then H3, H9, H12.** That is the largest gain in what the Server
can actually enforce, for the smallest architectural commitment — and of those, H3, H11, H12, H13,
H17, and H18 need no ADR at all.

**H4 last of the identity work, not first.** A sharper identity is only worth what the revocation
path behind it is worth: binding certificates to Agents while still being unable to withdraw one
buys precision without control. Stage 1 is the prerequisite, not the warm-up.

## Verifying a measure is in force

A hardening measure is the kind of change that looks done as soon as code exists, because the thing
it prevents was already not happening in any test. So each one below states the observable that
proves it — and the rule for all of them is the project's own ([`AGENTS.md`](../AGENTS.md) §5): **the check
must fail before the change and pass after**. A test that passes today verifies nothing about a
measure that has not been taken.

| # | Verified when |
|---|---|
| H1 | A session established over WebSocket is **closed** after the credential behind it is rotated or removed, within the configured bound; reconnecting with the old credential is answered `401`. Today the session survives indefinitely, which is what the check must first show. |
| H2 | A revoked certificate serial and a revoked credential are both refused on a *new* connection on **both** transports, and — together with H1 — an established connection using either is dropped rather than left running. |
| H3 | A CSR carrying an `instance_uid` that differs from the sender's is answered `ServerErrorResponse` of type `BadRequest`; one carrying a matching `instance_uid` is signed; one carrying none is signed exactly as today. The third case is the regression guard: this measure must not break the enrolment this project's own Client performs. |
| H4 | *Cannot be fixed before the ADR* — the shape decides the check. Two conditions hold whichever way it goes, and are the floor: a certificate issued for one Agent does not authenticate a connection claiming another, and a re-key through `AgentIdentification` does not invalidate a certificate still in force. |
| H5 | *Cannot be fixed before the ADR.* Floor: a bootstrap credential that has produced one certificate is refused the second time, and enrolment without an approval produces no certificate. |
| H6 | Renewal is observed to complete **before** expiry in a fleet left running longer than one validity period. Not a unit test — this one needs a soak, and shortening validity without that evidence is the failure mode the measure is meant to avoid. |
| H7 | No credential appears in `server.toml` in a form that authenticates on its own; a correct credential still authenticates; a wrong one is still rejected in constant time. The last clause matters: the point of the change is not to lose the property already held. |
| H8 | The CSR-obtained private key and the rotated-credential cache carry mode `0600` on Unix and the equivalent ACL on Windows. The Windows half is `cfg(windows)` code and therefore invisible to a local `cargo test` — it needs cross-compilation to typecheck, and CI to run. |
| H9 | The listener split is in force and covered: the REST API answers on the Operator plane and `404`s on the Agent plane, and the artifact download does the reverse (ADR-0066, `auth.rs` and `packages.rs` integration tests). What remains unverified is the handshake half: a peer presenting no client certificate must fail in the **TLS handshake** on the Agent plane — an error at the transport, not a `401` from a handler — while a browser reaching the Operator plane with none is served. The distinction between those two failures is the rest of the measure. |
| H10 | Connection or enrolment attempts past the configured rate are answered `ServerErrorResponse` of type `Unavailable` carrying `retry_info`, and the Client backs off accordingly (the Client half is already implemented, so this verifies the pair). |
| H11 | A peer offering only TLS 1.2 fails the handshake against both ends. |
| H12 | A package offered to a Client with no signing key configured is **not applied**, and is reported `InstallFailed` with a reason. Today it is applied, hash-verified only. |
| H13 | A referenced package whose host is not allow-listed is refused **before** any byte is fetched — the assertion is on the absence of the request, not on the outcome of the download. |
| H14 | First a written statement of what a remote configuration can and cannot cause on a host, derived from ADR-0021 and ADR-0057. Only then, one check per "cannot". Writing checks before that statement would test the implementation against itself. |
| H15 | Each of admission, issuance, rotation, revocation, and package application emits exactly one audit record naming the Agent and the outcome — including the **refusals**, which is the half that is easy to omit and the half an investigation needs. |
| H16 | Connections past the configured cap are refused while the ones already established keep working, and the cap is reached by opening sockets that send nothing — the same peer ADR-0073 hangs up on, in quantity. |
| H17 | An HTTP/2 peer that opens streams past `max_concurrent_streams` is refused, and one that stops answering keep-alive pings is dropped. Neither happens today, which is what the check must first show. |
| H18 | On the Gateway: a downstream connection that never finishes its headers is closed, exactly as [`connection_setup.rs`](../crates/server/tests/connection_setup.rs) shows for the Server. On the Supervisor Endpoint: a local connection that never completes the WebSocket upgrade is dropped, **and a second connection is served afterwards** — the second clause is the measure, since the first would pass on a listener that simply died. |

Two further points hold across the table. **H3 and H9 belong in the interoperability suite**, not only
in this project's own tests: both concern what the Server does with a peer it did not write, and
opamp-go is the peer that already stands in for that (ADR-0040). And **the two unverified claims**
under [Unverified claims](#unverified-claims) are checks to run *before* planning, not after
implementing — they decide whether H8 and H14 are work at all.

## The local endpoint

The Supervisor Endpoint binds `127.0.0.1` and authenticates nothing: any local process can take the
place of the Managed Process and report health, description, and effective configuration in its
name. On a single-purpose host that is proportionate — anything able to open that socket can usually
also write the files the Supervisor reads. On a shared or multi-user host it is not, and the fleet's
view of that Agent becomes forgeable from the inside.

This needs a decision either way: a local authentication mechanism, or a written statement that
single-purpose hosts are the assumed deployment and that shared hosts are an accepted risk. The one
outcome to avoid is leaving it unstated, since the assumption is currently implicit in the code and
nowhere else.
