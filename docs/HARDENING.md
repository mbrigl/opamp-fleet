# Hardening the Client–Server Link

**This document is a backlog, not a decision.** Everything below is a *candidate* measure for
hardening the connection between the Client and the Server: written down so it can be picked up
deliberately later instead of being rediscovered, and ordered so the cheap wins are not buried under
the expensive ones. Nothing here is binding. Each measure states what it would cost and what still
has to be established, and the ones that constrain the architecture say so and name the ADR they
would have to supersede — by topic, since the number does not exist yet.

Every measure is stated here in full, so this document can be read on its own. None below is a
conformance question: the Baseline requires none of it.

## Scope

The link this document is about is **Client ↔ Server**. In this project's vocabulary a Supervisor
lives *inside* the Client ([`crates/fleet-agent/src/supervisor/`](../crates/fleet-agent/src/supervisor/)) and
does not speak to the Server itself — the Client carries every Supervisor's Agent over its one
connection (ADR-0009, ADR-0015). Where other OpAMP material says "Supervisor ↔ Server", this is the
link it means.

One adjacent surface is in scope because it terminates on the same host and carries the same
protocol: the **Supervisor Endpoint**, the loopback WebSocket each Supervisor serves for a Managed
Process's `opampextension` ([`endpoint.rs`](../crates/fleet-agent/src/supervisor/endpoint.rs)). It is
treated separately at the end.

Out of scope, and deliberately so: **authorization and multi-tenancy**, which the specification
names as non-goals. The boundary is worth stating precisely because measures below (H4) run
close to it — *which Agent is speaking* is authentication and belongs here; *what that Agent is
allowed to do* is authorization and does not.

## What already holds

Stated first so the list below is not read as a list of absences. On this link the project already
has:

- TLS on both transports, on both ends, with a private CA supported on the Client (ADR-0012).
- **Cumulative** admission on `/v1/opamp`: every configured proof must succeed — a credential when
  `[auth]` is set, a client certificate when a client CA is, both when both are (ADR-0017,
  `transport::Admission`). Not "either one", which is what keeps switching mutual TLS on from ever
  admitting more than before.
- Constant-time comparison of the presented `Authorization` value, so a comparison leaks nothing
  about how far it matched — on both planes, from one primitive (`credentials.rs`).
- Optional Basic authentication over the **whole** Operator plane, the UI included (`[rest.auth]`,
  ADR-0017), on a listener that is loopback until an operator publishes it (ADR-0012).
- Client certificates the Server issues itself through the Baseline's CSR flow, with the Agent
  keeping its private key — and with the request's `basicConstraints`, `keyUsage`,
  `extendedKeyUsage`, and SANs **overwritten** rather than carried over, so a CSR cannot ask for the
  powers of a CA ([`ca.rs`](../crates/fleet-server/src/ca.rs)).
- Message size limits enforced in both directions on both transports, and at the Supervisor
  Endpoint.
- **TLS 1.3 alone, on every connection of both ends** (ADR-0036, ADR-0038): the process-wide
  provider carries the three TLS 1.3 suites and no other, every configuration pins TLS 1.3, and a
  peer offering only TLS 1.2 fails the handshake. Plaintext is accepted on `127.0.0.1` and `::1`
  alone and refused at startup anywhere else.
- **Both proofs, in the handshake, and enrolment by approval** (ADR-0039, ADR-0040): every
  Agent presents the fleet credential and a client certificate the Agent plane and every Gateway
  ask for in the TLS handshake. A fresh host enrols with a bootstrap certificate from a CA of its
  own, only inside an operator-opened window, and only once an operator approves its request.
  Repeated admission failures from one address are answered `429` before the credential is
  compared, on both planes.
- **Nothing installed unsigned, and downloads only from allowed sources** (ADR-0042, ADR-0045):
  a Client without the verification key takes no package, the Server offers no entry its
  Deployment has not signed, and every download hop is held to `https://` and the Server's origin
  or `[packages] allowed_sources` before a request is sent. The Client's certificate goes to its
  Server alone, an offered header only to an allowed source.
- **A connection cap per plane and bounded HTTP/2** (ADR-0038): `max_connections` closes a
  connection past the cap on accept, before any handshake, while the held ones keep working; HTTP/2
  allows 100 concurrent streams per connection and drops a peer that leaves a ping unanswered.
- **Connection setup bounded on both of the Server's planes** (ADR-0012): a peer has 30 seconds to
  send its request line and headers, and 10 seconds to complete the TLS handshake, before it is hung
  up on — enforced below every other limit in this list, because it applies before a request exists
  and therefore before Admission ever runs.
- Package content hashed always, and Ed25519-verified when a key is configured; archive members
  validated before anything is written.
- **Revocation that ends sessions** (ADR-0049): the Server keeps a persisted
  list of revoked certificates, by issuing CA and serial and extended along every renewal it signed,
  and of revoked credentials, by hash. Admission refuses them on both transports and on the
  download, a revocation closes exactly the WebSocket sessions it concerns with `1008`, and every
  session ends when the certificate that admitted it expires.
- **A CSR's claim to an `instance_uid` is checked** (ADR-0050): a CSR naming any
  `instance_uid` but its sender's is answered `BadRequest` before it is signed or queued — the
  Baseline's conditional MUST.
- **Key material and credentials readable by their owner alone** (ADR-0039 clause 8, ADR-0046
  clause 18): on Unix the private key, the stored connection settings and `supervisor.toml` are
  written `0600` in `0700` directories; on Windows a system-scope install cuts the data root off
  from the read right every local user inherits under `%ProgramData%` and leaves it to LocalSystem,
  the Administrators and the service account.
- **A delivered Supervisor block reaches no further than the package signature** (ADR-0051): no
  environment or arguments beyond what the running block has or the operator allowed, never a
  loader variable or `PATH`, and no file outside its own `config/` directory.
- **No credential in `server.toml` authenticates on its own** (ADR-0039, ADR-0041): Bearer tokens
  as SHA-256, Basic passwords as Argon2id hashes, verified in constant time; the credential the
  Server offers for rotation lives in an owner-only file of its own.
- **An audit record of every security decision** (ADR-0052): admissions and refusals, enrolment,
  issuance, revocation, rotation, operator acts and package outcomes, one hash-chained line each;
  no admission without its record.
- `TLSConnectionSettings` and `ProxyConnectionSettings` refused on merit, so a Server cannot command
  a Client to weaken its own verification
  ([`CONFORMANCE.md`](CONFORMANCE.md#mutual-tls-and-the-two-fields-still-refused)).

### Where each bound applies today

The list above is per mechanism; this is the same state per **surface**, since a rule that holds on
one listener and not on its neighbour is the failure mode worth seeing at a glance. ✅ in force,
⚠️ partial, ❌ absent.

- **Agent plane** — `127.0.0.1:4320` until an operator publishes it (ADR-0038).
  - ✅ TLS handshake ≤ 10 s · ✅ headers ≤ 30 s (HTTP/1) · ✅ message size, in both directions ·
    ✅ gzip bounded *after* decompression · ✅ Admission, cumulative
  - ✅ connections capped (`max_connections`) · ✅ HTTP/2 streams and pings bounded
  - ✅ failed admissions throttled per address
- **Operator plane** — `127.0.0.1:4321` until an operator publishes it (ADR-0038).
  - ✅ TLS handshake ≤ 10 s · ✅ headers ≤ 30 s · ✅ optional Basic over the whole plane (ADR-0017) ·
    ✅ Fetch-Metadata CSRF guard on the body-less `POST` routes
  - ⚠️ the package upload is unbounded in **time** and, by decision, in size (ADR-0011) — the one
    route where that is intended · ✅ capped, HTTP/2-bounded and throttled as above
- **Client → Server**, outbound (`opamp::client::connection`, ADR-0036).
  - ✅ request timeout 30 s on the polling transport · ✅ redirects refused outright ·
    ✅ reconnect backoff · ✅ message size in both directions
- **Gateway endpoint** — the Client serving OpAMP downstream (ADR-0009).
  - ✅ TLS handshake ≤ 10 s · ✅ headers ≤ 30 s (HTTP/1), from the listener every OpAMP endpoint
    is served on (ADR-0036) · ✅ message size, gzip after decompression, per-hop exchange timeout,
    `max_carried_agents`
  - ✅ a downstream peer that never finishes its headers after the handshake is hung up on
    ([`gateway_tls.rs`](../crates/fleet-agent/tests/gateway_tls.rs))
- **Supervisor Endpoint** — loopback, one Managed Process (`supervisor/endpoint.rs`).
  - ✅ headers ≤ 30 s (HTTP/1), from the same listener · ✅ connections served concurrently ·
    ✅ message size in both directions
  - ✅ a half-finished upgrade is dropped while other connections are served, and the endpoint
    serves the next one afterwards
    ([`endpoint.rs`](../crates/fleet-agent/src/supervisor/endpoint.rs))

What follows is therefore not "make it secure" but two narrower things: **close the windows during
which a withdrawn credential still works**, and **shrink the surface that sits beside the protocol**.

**Status of each measure below:** 🔴 not taken · 🟡 partly in force · 🟢 in force. Nothing is 🟢 here by
construction: a measure that reaches it moves up into [What already holds](#what-already-holds), the
way the connection-setup bound did when ADR-0012 took it. The ✅/⚠️/❌ marks in that section are the
same three states seen per *surface* rather than per measure.

## Stage 1 — Separate rotation from revocation

Revocation and the CSR check are in force and listed under
[What already holds](#what-already-holds). What remains of revocation is the Gateway: the Server sees the Gateway's certificate, so a downstream
certificate is revoked only once that Agent connects directly. Handing the list to Gateways would
make the Gateway take an admission decision, and is a measure of its own when a fleet needs it.

## Stage 2 — Sharpen identity

🔴 **H4 — Decide what a client certificate proves: fleet membership, or a specific Agent.**
Today it proves membership only, and that is a recorded decision with a real reason: binding the
issued certificate to an `instance_uid` would mean a re-key through `AgentIdentification` kills a
certificate the Server itself issued (ADR-0017). Hardening this means *resolving* that
conflict rather than working around it. Three approaches are worth weighing, and none is obviously
right:

- a **stable enrolment identity** carried in the certificate, distinct from the re-keyable
  `instance_uid`, with the Server holding the mapping;
- **stop re-keying** while certificates are in force, making `instance_uid` stable by construction
  and accepting what that costs in duplicate-identity handling;
- **bind only on direct connections**, leaving a gatewayed fleet on membership proof, since the
  certificate the Server sees there is the Gateway's anyway.

This is the most expensive measure in the document and the one with the widest blast radius. It
would need an ADR superseding ADR-0017 on this specific point, and that ADR is where the
authorization boundary named under [Scope](#scope) has to be drawn explicitly — otherwise it moves
unnoticed.

🔴 **H6 — Shorten certificate validity once renewal is proven.**
`validity_days` defaults to 90. Revocation ends a session at once (ADR-0049), so a short
validity no longer has to stand in for it; what remains is the reach of a certificate stolen
without anyone noticing. Shortening it is cheap, but only once renewal is shown to complete in a
fleet left running for longer than one validity period: otherwise it moves the failure to
"eject the whole fleet by accident".

## Stage 4 — Shrink the surface and bound the abuse

🔴 **H20 — Bound certificate issuance per Agent, not per chain.**
The Server signs every CSR an admitted member sends, and bounds the register per renewal chain
(ADR-0049). Behind a Gateway every downstream renewal descends from the Gateway's certificate, so
one downstream Agent looping CSRs fills that chain and every renewal behind the Gateway is refused
until what it obtained expires. A rate bound alone only slows this; telling the abusing Agent apart
needs the per-Agent identity H4 decides. **To work out:** whether a renewal may be refused while
the requesting key's certificate is still young, and how that holds behind a Gateway, where the
Server sees no downstream certificate at all.

## Stage 5 — The channels that put code on the host

Remote configuration and package delivery are the paths by which the Server causes code to run on an
Agent's host. They deserve at least as much attention as the transport, and arguably more.

**What a remote configuration can and cannot cause on a host.** The Server reaches a Client host
through seven channels. It **can** write any files into a Supervisor's own `config/` directory and
restart its process; add, change, purge and restart Supervisors of the compiled-in kinds, each
running a program from its own `program/` directory; install any package signed with the
operator's key into a Supervisor, and a newer signed Client build into the Client itself; move the
fleet to another TLS endpoint its trust accepts, rotate the credential, install a certificate for
the key the Client generated, and set its telemetry destinations; restart a Managed Process; and
re-key an Agent's `instance_uid`. What an Agent's own configuration language allows — a Telegraf
`inputs.exec`, an Icinga `CheckCommand` — it allows as the process's account; that is the product.

It **cannot**: write outside a Supervisor's `config/` (entry names are sanitized); name a program
outside a Supervisor's `program/`, by absolute, rooted or escaping path, or a wrapped kind's program
at all; start a kind that is not compiled in; change any key of `supervisor.toml` but the
`[[supervisor]]` array — not the endpoint, `state_dir`, `[auth]`, `[tls]`, the verification key,
`allowed_sources` or `[self_update]`; apply a set with one bad block; purge outside the Supervisors'
root; install anything unsigned, from a source not allowed, or with an archive member that climbs
out; downgrade the Client or install a program that is not the Client as the Client; switch to
plaintext beyond the loopback; weaken TLS verification or set a proxy; hand the Client a private
key. Each of these is enforced in the code, and most by a test.

Two things a reader would expect to be refused are not, and each is a measure below.

🔴 **H23 — A Supervisor's package is bound to its bytes, not to its Agent type or version.**
Any artifact signed with the operator's key installs as any Supervisor's program, at any version
label, older ones included; only the Client's self-update refuses a downgrade. A compromised Server
can roll a Managed Process back to a signed build with a known flaw. **To work out:** whether the
signature should cover the Agent type and version, as the Deployment already pairs them
(ADR-0045), and a Client-side refusal of a downgrade.

🔴 **H24 — An offered endpoint is trusted by the public roots when no `ca_file` is set.**
A connection offer may move the fleet to any host whose certificate a public CA issued, and the
Client keeps the move in `connection-settings.pb`, which outranks the operator's file. A
compromised Server can re-home the fleet for good. **To work out:** requiring `ca_file` for an
offered endpoint, or pinning the offered endpoint to the trust the current one was reached with.

Checks still missing for things that are enforced: an unknown Supervisor `type`; a delivered set
naming top-level keys beyond `endpoint` and `state_dir`; a refused set leaving the running
Supervisors untouched; the OpAMP half of an offer's `tls` and `proxy` not being honoured; two
top-level packages in one offer; a delivered program name and Supervisor name that traverse.

## Suggested order

**H23 and H24 next.** Each narrows what a compromised Server can do on a host beyond what the
signature already stops.

**H4 last of the identity work, not first.** A sharper identity is only worth what the revocation
path behind it is worth: binding certificates to Agents while still being unable to withdraw one
buys precision without control. Revocation is the prerequisite, not the warm-up, and it is in force.

## Verifying a measure is in force

A hardening measure is the kind of change that looks done as soon as code exists, because the thing
it prevents was already not happening in any test. So each one below states the observable that
proves it — and the rule for all of them is the project's own ([`AGENTS.md` §5](../AGENTS.md#5-quality-bar--definition-of-done)): **the check
must fail before the change and pass after**. A test that passes today verifies nothing about a
measure that has not been taken.

| # | Verified when |
|---|---|
| H4 | *Cannot be fixed before the ADR* — the shape decides the check. Two conditions hold whichever way it goes, and are the floor: a certificate issued for one Agent does not authenticate a connection claiming another, and a re-key through `AgentIdentification` does not invalidate a certificate still in force. |
| H6 | Renewal is observed to complete **before** expiry in a fleet left running longer than one validity period. Not a unit test — this one needs a soak, and shortening validity without that evidence is the failure mode the measure is meant to avoid. |
| H23 | A signed artifact offered to a Supervisor of another Agent type, or at a version below the one installed, is refused before anything is swapped. |
| H24 | An offered endpoint is not adopted unless the Client's own `ca_file` verifies it. |

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
