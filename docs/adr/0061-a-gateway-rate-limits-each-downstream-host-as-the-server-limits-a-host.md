# ADR-0061: A Gateway rate-limits each downstream host as the Server limits a host

- **Status:** 🟡 proposed
- **Date:** 2026-10-25
- **Deciders:** Markus Brigl
- **Applies to:** the downstream reports, enrolment requests and package downloads a Gateway serves in `crates/fleet-agent/src/gateway/mod.rs`, the buckets that count them in `crates/fleet-agent/src/gateway/`, the `[gateway.rate_limit]` section of `supervisor.toml` and its parsing in `crates/fleet-agent/src/config.rs`, and every document that tells an operator how a Gateway bounds what a host behind it sends

## Context

Behind a Gateway every Agent reaches the Server under the Gateway's certificate. The Server
therefore counts what a Gateway forwards in two buckets: one per Agent, keyed by the Gateway's host
and the message's `instance_uid`, and the Gateway's aggregate, 500 messages a second with a burst of
10 000 by default
([ADR-0013](0013-tls-1-3-plaintext-on-the-loopback-alone-and-bounded-planes-bodies-and-messages.md)
clause 22). A message that finds either bucket empty is answered `Unavailable` with `RetryInfo`
(ADR-0013 clause 23).

The aggregate is shared by every host behind the Gateway, and the Server cannot tell them apart. A
single host may speak for up to `max_carried_agents` `instance_uid`s
([ADR-0058](0058-a-gateways-bindings-outlast-its-restart-and-end-with-their-hosts-certificate.md)
clause 2). Each of them has its own per-Agent bucket at the Server, so together they can drain the
aggregate. From then on every other host behind that Gateway is answered `Unavailable` until the
aggregate refills. The same host can do the same with downloads, which the Server counts in the
aggregate alone, and an enrolling peer with its one request. ADR-0058 names a rate per downstream
member certificate at the Gateway as out of scope and a follow-up. `docs/HARDENING.md` lists it
among the next steps.

The Gateway knows each downstream host. It binds every `instance_uid` to the host of the
certificate that reported it (ADR-0058 clause 2). So it is the one place where a host behind it can
be held to a budget of its own before its messages reach the shared one.

The specification's quality goal Q-1 asks that the Fleet be secure by default, and G-15 that a
Gateway scale connections, not identities.

## Decision

We will have a Gateway count every downstream report, enrolment request and package download in a
bucket of the host that sent it, sized as the Server sizes a host's bucket, and answer one past it
as the Server does, without forwarding it.

1. **Each downstream host has one bucket.** The key is the host the downstream certificate names,
   or for a certificate that names none, the host derived from its issuer and serial (ADR-0058
   clause 2). A peer with a bootstrap certificate on its one enrolment request is keyed by its
   address, an IPv6 one by its /64, as the Server keys it (ADR-0013 clause 22), since a bootstrap
   certificate may be one for the whole fleet. A new connection, a new `instance_uid` or a renewed
   certificate of the same host takes from the same bucket.

2. **What is counted.** Every `AgentToServer` message on the downstream `/v1/opamp`, over a
   WebSocket and over plain HTTP, every request on the downstream package download route, and an
   enrolling peer's request. Each takes one token before the Gateway does anything else with it:
   before it reads a CSR, binds an `instance_uid`, touches the package cache or forwards anything
   upstream. The Gateway's own Supervisors and its upstream connection are not counted.

3. **The bucket is set in `[gateway.rate_limit]`.** `messages_per_sec` (default `10`) is the
   tokens added per second, continuously, and `burst` (default `300`) the bucket's capacity, the
   values the Server gives a host (ADR-0013 clause 19). A new bucket starts full. `0` for either
   key is refused at startup, naming the key. No value turns the limit off. Any other key of the
   section is refused, as in every section.

4. **What a message past the limit gets is what the Server would answer.** A report or an
   enrolment request that finds no whole token is not forwarded and binds nothing. It is answered
   with a `ServerToAgent` that carries the message's own `instance_uid`, `error_response` of type
   `Unavailable` with an `error_message` saying that too many messages arrive, and `RetryInfo` for
   when the next token is due (ADR-0013 clauses 23 and 25). The WebSocket stays open. A download
   past the limit is answered `429` with `Retry-After: 30`, and the artifact is not read (ADR-0013
   clause 24). What is refused is logged once per host per minute, with the host and how many were
   refused.

5. **The buckets are bounded.** They live in memory, and nothing persists them. They hold at most
   1 000 000 entries, the Gateway's total of bindings (ADR-0058 clause 2). When the table is full,
   a new key evicts the bucket used least recently, which only grants tokens, never withholds them
   (ADR-0013 clause 26).

**Out of scope:** a bucket per `instance_uid` at the Gateway, which the Server keeps; a rate the
Server tells the Gateway; a limit on the downstream fetch of the package floors; changing the
Server's aggregate.

## Alternatives considered

- **A bucket per `instance_uid` at the Gateway too, as the Server keeps.** It doubles what the
  Server already does per Agent and leaves the host's sum unbounded. Not taken.
- **A bucket per certificate, by issuer and serial.** A renewal would hand the host a fresh, full
  bucket. Rejected in favour of the host, which the bindings already key on.
- **Values the Server sends the Gateway.** One setting for the fleet, but it needs a path from the
  Server to the Gateway that does not exist. Not taken now.
- **A fixed rate with no keys.** A site with hosts that legitimately send more could not raise it.
  Rejected.
- **Closing the connection, or `429`, past the limit.** Only a WebSocket Agent with a backoff
  would behave well, and the protocol's one retryable answer would go unused. Rejected.
- **Counting at the Server by a header the Gateway sets.** The Server would have to trust the
  Gateway's word about which host sent what, which ADR-0039 rejected for the bindings. Rejected.

## Sources / Prior art

- This repository: [ADR-0013](0013-tls-1-3-plaintext-on-the-loopback-alone-and-bounded-planes-bodies-and-messages.md)
  clauses 19 and 21 to 26 (the Server's buckets, their keys, their answers and their bound),
  [ADR-0058](0058-a-gateways-bindings-outlast-its-restart-and-end-with-their-hosts-certificate.md)
  clause 2 (the host a downstream certificate names, and the total of bindings),
  [ADR-0039](0039-through-a-gateway-a-csr-proves-the-certificate-it-rests-on.md) (enrolment
  through a Gateway), `crates/fleet-server/src/agent_rate.rs`, `docs/HARDENING.md`.
- The OpAMP specification, *Throttling*: a Server answers `UNAVAILABLE` with `RetryInfo`, and an
  Agent retries no sooner than it says. Recalled, not re-read for this record; the answer the
  Gateway gives is ADR-0013's, which was.

## Consequences

- Positive: one host behind a Gateway can no longer exhaust the aggregate the Server gives the
  Gateway, so the other hosts keep reaching the Server. A host behind a Gateway gets the budget a
  directly connected one gets.
- Negative / trade-offs: one more section of `supervisor.toml` and one more table in the Gateway's
  memory. A host whose Agents legitimately send more than its bucket allows must have it raised at
  each Gateway it reports through. A host that reports through two Gateways has a bucket at each.
- Follow-ups: values the Server tells its Gateways.

## Enforcement

Tests that will carry `Verifies: ADR-0061` in `crates/fleet-agent/src/gateway/` and
`crates/fleet-agent/src/config.rs`:

- A host past its burst is answered `Unavailable` with `RetryInfo` over a WebSocket and over plain
  HTTP, its report is not forwarded and binds nothing, and the WebSocket stays open; another host
  behind the same Gateway is forwarded meanwhile.
- New `instance_uid`s, new connections and a renewed certificate of one host take from the one
  bucket.
- A download past the limit is answered `429` with `Retry-After: 30` and the artifact is not read,
  and an enrolling peer is keyed by its address, an IPv6 one by its /64.
- `[gateway.rate_limit]` with `0` for either key, or with another key, fails startup naming it, and
  the defaults are `10` and `300`.
- A full table evicts the bucket used least recently.
