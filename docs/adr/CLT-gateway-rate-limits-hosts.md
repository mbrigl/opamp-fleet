# CLT-gateway-rate-limits-hosts: A Gateway rate-limits each downstream host as the Server limits a host

- **Status:** 🟢 accepted
- **Date:** 2026-10-27
- **Deciders:** Markus Brigl
- **Applies to:** the downstream reports, enrolment requests and package downloads a Gateway serves in `crates/fleet-agent/src/gateway/mod.rs`, the buckets that count them in `crates/fleet-agent/src/gateway/`, the `[gateway.rate_limit]` section of `supervisor.toml` and its parsing in `crates/fleet-agent/src/config.rs`, and every document that tells an operator how a Gateway bounds what a host behind it sends

## Context

Behind a Gateway, every Agent reaches the Server under the Gateway's certificate. The Server counts
what a Gateway forwards in two buckets:

- one per Agent, keyed by the Gateway's Host and the message's `instance_uid`;
- the Gateway's aggregate, 500 messages a second with a burst of 10 000 by default
  ([CLT-modes-and-gateway §26](CLT-modes-and-gateway.md)).

A message that finds either bucket empty is answered `Unavailable` with `RetryInfo`
([SEC-tls13-bounded-input-chosen-planes §25](SEC-tls13-bounded-input-chosen-planes.md)).

Every host behind the Gateway shares the aggregate, and the Server cannot tell them apart. One host
may speak for up to `max_carried_agents` `instance_uid`s
([CLT-modes-and-gateway §15](CLT-modes-and-gateway.md)). Each of them has its own per-Agent bucket
at the Server, so together they can drain the aggregate. Every other host behind that Gateway is
then answered `Unavailable` until the aggregate refills. The same host can do the same with
downloads, which the Server counts in the aggregate alone, and an enrolling peer with its one
request. CLT-modes-and-gateway leaves a rate at the Gateway out of its scope.

The Gateway knows each downstream host. It binds every `instance_uid` to the host of the
certificate that reported it ([CLT-modes-and-gateway §15](CLT-modes-and-gateway.md)). It is
therefore the one place that can hold a host behind it to a budget of its own before the host's
messages reach the shared one.

The specification asks that the Fleet be secure by default (Q-1) and that a Gateway scale
connections, not identities (G-15).

## Decision

We will have a Gateway count every downstream report, enrolment request and package download in a
bucket of the host that sent it, sized as the Server sizes a host's bucket, and answer one past it
as the Server does, without forwarding it.

1. **Each downstream host has one bucket.**
   - The key is the host the downstream certificate names.
   - For a certificate that names no host, the key is the host derived from its issuer and serial
     ([CLT-modes-and-gateway §15](CLT-modes-and-gateway.md)).
   - The Gateway keys an enrolling peer on its one enrolment request by its address. It keys an
     IPv6 address by its /64. The Server keys it the same way
     ([CLT-modes-and-gateway §26](CLT-modes-and-gateway.md)). The reason: one bootstrap
     certificate may serve the whole fleet.
   - A new connection, a new `instance_uid` or a renewed certificate of the same host takes from
     the same bucket.

2. **What is counted.** The Gateway counts each of the following as the Server counts it
   ([SEC-tls13-bounded-input-chosen-planes §24](SEC-tls13-bounded-input-chosen-planes.md)):
   - every `AgentToServer` message on the downstream `/v1/opamp`, over a WebSocket and over plain
     HTTP;
   - a message there that cannot be decoded;
   - every request on the downstream package download route;
   - an enrolling peer's request.

   Each takes one token once the downstream handshake and the revocation verdict have admitted it.
   It takes the token before the Gateway does anything else with it. That means before the Gateway
   reads a CSR, binds an `instance_uid`, touches the package cache or forwards anything upstream.
   The Gateway does not count its own Supervisors or its upstream connection.

3. **The bucket is set in `[gateway.rate_limit]`.**
   - `messages_per_sec` (default `10`) is the number of tokens added per second, continuously.
   - `burst` (default `300`) is the bucket's capacity.
   - These defaults are the values the Server gives a host
     ([SEC-tls13-bounded-input-chosen-planes §22](SEC-tls13-bounded-input-chosen-planes.md)).
   - A new bucket starts full.
   - The Client refuses `0` for either key at startup, naming the key. No value turns the limit
     off.
   - The Client refuses any other key of the section, as in every section.

4. **A message past the limit gets what the Server would answer.**
   - The Gateway does not forward a report or an enrolment request that finds no whole token, and
     the message binds nothing.
   - The Gateway answers it with a `ServerToAgent` that carries the message's own `instance_uid`
     and an `error_response` of type `Unavailable`.
   - The `error_response` carries an `error_message` saying that too many messages arrive, and a
     `RetryInfo` of 30 seconds, the Server's own
     ([SEC-tls13-bounded-input-chosen-planes §25 and §27](SEC-tls13-bounded-input-chosen-planes.md)).
   - The Gateway answers a message that cannot be decoded the same way, without an `instance_uid`.
   - The reply names no capabilities, since the Gateway does not speak for the Server. A Client
     reads none from it.
   - The WebSocket stays open.
   - The Gateway answers a download past the limit with `429` and `Retry-After: 30`, and does not
     read the artifact
     ([SEC-tls13-bounded-input-chosen-planes §26](SEC-tls13-bounded-input-chosen-planes.md)).
   - The Gateway logs what it refuses once per host per minute, with the host and how many it
     refused.

5. **The buckets are bounded.**
   - They live in memory, and nothing persists them.
   - They hold at most 1 000 000 entries, the Gateway's total of bindings
     ([CLT-modes-and-gateway §15](CLT-modes-and-gateway.md)).
   - When the table is full, a new key evicts the bucket used least recently. An eviction only
     grants tokens and never withholds them
     ([SEC-tls13-bounded-input-chosen-planes §28](SEC-tls13-bounded-input-chosen-planes.md)).

**Out of scope:**

- a bucket per `instance_uid` at the Gateway, which the Server keeps;
- a rate the Server tells the Gateway;
- a limit on the downstream fetch of the package floors;
- a change to the Server's aggregate.

## Alternatives considered

- **A bucket per `instance_uid` at the Gateway too, as the Server keeps.** It repeats what the
  Server already does per Agent and leaves the host's sum unbounded. Not taken.
- **A bucket per certificate, by issuer and serial.** A renewal would hand the host a fresh, full
  bucket. Rejected in favour of the host, which the bindings already key on.
- **Values the Server sends the Gateway.** One setting for the fleet, but it needs a path from the
  Server to the Gateway that does not exist. Not taken.
- **A fixed rate with no keys.** A site whose hosts legitimately send more cannot raise it.
  Rejected.
- **Closing the connection, or `429`, past the limit.** Only a WebSocket Agent with a backoff
  would behave well, and the protocol's one retryable answer would go unused. Rejected.
- **Counting at the Server by a header the Gateway sets.** The Server would have to trust the
  Gateway's word about which host sent what. CLT-modes-and-gateway rejects that trust for the
  bindings. Rejected.

## Sources / Prior art

- This repository:
  - [SEC-tls13-bounded-input-chosen-planes §22, §24–§28](SEC-tls13-bounded-input-chosen-planes.md):
    the Server's buckets, their answers and their bound;
  - [CLT-modes-and-gateway §26](CLT-modes-and-gateway.md): their keys;
  - [CLT-modes-and-gateway §15](CLT-modes-and-gateway.md): the host a downstream certificate
    names, and the total of bindings;
  - [CLT-modes-and-gateway §20](CLT-modes-and-gateway.md) to
    [CLT-modes-and-gateway §25](CLT-modes-and-gateway.md): enrolment through a Gateway;
  - `crates/fleet-server/src/agent_rate.rs` and `docs/HARDENING.md`.
- The OpAMP specification, *Throttling*: a Server answers `UNAVAILABLE` with `RetryInfo`, and an
  Agent retries no sooner than it says. Recalled, not re-read for this record. The answer the
  Gateway gives is SEC-tls13-bounded-input-chosen-planes's, which was read.

## Consequences

- Positive: one host behind a Gateway cannot exhaust the aggregate the Server gives the Gateway,
  so the other hosts keep reaching the Server. A host behind a Gateway gets the budget a directly
  connected one gets.
- Negative / trade-offs: one more section of `supervisor.toml` and one more table in the Gateway's
  memory. A host whose Agents legitimately send more than its bucket allows needs it raised at
  each Gateway it reports through. A host that reports through two Gateways has a bucket at each.
- Follow-ups: values the Server tells its Gateways.

## Enforcement

These tests carry `Verifies: CLT-gateway-rate-limits-hosts`:

- [`crates/fleet-agent/src/gateway/mod.rs`](../../crates/fleet-agent/src/gateway/mod.rs):
  - `a_host_past_its_burst_is_answered_unavailable_and_another_is_forwarded` (clauses 2, 4). A
    host past its burst gets `Unavailable` with `RetryInfo`, over a WebSocket and over plain HTTP.
    Its report is not forwarded and binds nothing, and the WebSocket stays open. Another host
    behind the same Gateway is forwarded.
  - `every_connection_uid_and_renewed_certificate_of_a_host_takes_from_one_bucket` (clause 1).
  - `an_enrolling_peer_is_counted_by_its_address_before_its_request_is_read` (clauses 1, 2).
- [`crates/fleet-agent/src/gateway/cache.rs`](../../crates/fleet-agent/src/gateway/cache.rs):
  `a_download_past_the_hosts_limit_is_answered_429_and_reads_no_artifact` (clause 4).
- [`crates/fleet-agent/src/gateway/rate.rs`](../../crates/fleet-agent/src/gateway/rate.rs):
  - `a_hosts_bucket_holds_its_burst_and_refills_at_its_rate` (clause 3);
  - `an_enrolling_peer_is_keyed_by_its_address_an_ipv6_one_by_its_64` (clause 1);
  - `a_full_table_evicts_the_bucket_used_least_recently` (clause 5);
  - `refusals_are_logged_once_per_host_per_minute_with_their_count` (clause 4);
  - `a_refusal_names_its_own_agent_says_unavailable_and_when_to_retry` (clause 4).
- [`crates/fleet-core/src/rate.rs`](../../crates/fleet-core/src/rate.rs), the buckets the Gateway
  shares with the Server (clauses 1, 3, 5):
  - `a_bucket_starts_full_refills_continuously_and_holds_no_more_than_its_burst`;
  - `a_full_table_forgets_the_bucket_used_least_recently`;
  - `an_ipv6_peer_is_counted_by_its_64_and_a_mapped_one_as_ipv4`.
- [`crates/fleet-agent/src/config.rs`](../../crates/fleet-agent/src/config.rs):
  `the_gateway_rate_limit_defaults_and_refuses_zero_and_unknown_keys` (clause 3).
  `[gateway.rate_limit]` with `0` for either key, or with another key, fails startup naming it.
  The defaults are `10` and `300`.
