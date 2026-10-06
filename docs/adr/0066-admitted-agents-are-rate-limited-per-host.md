# ADR-0066: Admitted Agents are rate-limited per host, and per Agent within a Gateway's own limit

- **Status:** 🟢 accepted
- **Date:** 2026-10-06
- **Deciders:** Markus Brigl
- **Applies to:** the message limit in a new `crates/fleet-server/src/agent_rate.rs`, `on_message` and `on_unreadable` of the Server's handler, the enrolment refusals in `Fleet::enrol` and the download guard `admit_download` in `crates/fleet-server/src/transport.rs`, every `Unavailable` reply in `crates/fleet-server/src/fleet.rs`, the `[agent_rate_limit]` section of `server.toml` and its parsing and startup warning in `crates/fleet-server/src/config.rs`, and the `agent_rate.throttled` audit event

## Context

Admission on the Agent plane ends at the handshake
([ADR-0059](0059-admission-by-a-client-certificate-alone.md)). From there on, nothing bounds how
many messages an admitted peer sends. Existing bounds cover other things:

- the connection cap
  ([ADR-0054](0054-tls-1-3-plaintext-on-the-loopback-alone-and-bounded-planes-bodies-and-messages.md)
  clause 16) bounds sockets;
- the message size limit (clause 3) bounds each message;
- the pace floor (clause 14) bounds how slowly a message may arrive;
- the throttle of ADR-0059 clause 24 counts failed admissions per peer address, and a member never
  fails.

None of them bounds how often. One WebSocket session can send a full report as fast as the Server
decodes it. Each report takes the fleet lock, may write the Agent's record to disk
([ADR-0026](0026-the-fleet-record.md)), may compose an offer, and may ask for a certificate to be
signed. A plain-HTTP poller can do the same with one request per message. The package download
route is the Agent plane's other member route, and it is about to cost more: under
[ADR-0068](0068-a-host-fetches-only-the-packages-offered-to-its-own-agents.md), each request scans
the fleet under its lock to find an Agent the artifact is offered to.

The specification holds that no vulnerability can be ruled out (Strategy *Security before
convenience*). A host taken over, or a Client with a bug that loops, is still a member, and
without a bound it can take the Server's time from every other Agent. Q-2 asks that untrusted
input be bounded, and an admitted peer's message is still input from the network.

Forces that shape the answer:

- **The host is the unit admission knows.** A certificate names the host it was issued to as
  `urn:opamp-fleet:host:<id>` (ADR-0059 clause 7). `instance_uid` is self-asserted (clause 14), so
  a limit keyed on it alone binds nothing: a peer chooses a new uid for each message. A certificate
  an operator provisioned outside the CSR flow names no host until it is first renewed. It is
  still one certificate, told apart by its issuer and serial (`CertId` in
  [`revocation.rs`](../../crates/fleet-server/src/revocation.rs)).
- **A Gateway carries other hosts' Agents.** Behind a marked Gateway every Agent arrives under the
  Gateway's certificate (ADR-0059 clause 13,
  [ADR-0064](0064-client-modes-and-a-gateway-that-admits-by-certificate-and-refuses-what-the-server-revoked.md)
  clause 11). A per-host limit there would put a whole site under one host's budget. A per-uid
  limit alone bounds nothing, because a downstream peer can cycle uids. The Gateway's
  `max_carried_agents` does not stop that either: it bounds one downstream connection, and a
  plain-HTTP downstream exchange is a connection of its own
  ([`gateway/mod.rs`](../../crates/fleet-agent/src/gateway/mod.rs) creates a `Downstream` per
  exchange).
- **The protocol has one retryable refusal.** `ServerErrorResponseType` has three types. Only
  `UNAVAILABLE` tells the Agent to retry, and `RetryInfo.retry_after_nanoseconds` says when.
  - On a WebSocket, the Baseline's *Throttling* section says a Server that cannot process an
    `AgentToServer` message *"SHOULD respond with an ServerToAgent message"* whose `error_response`
    type is `UNAVAILABLE`, and that *"The Client SHOULD disconnect, wait, then reconnect again"*.
  - On plain HTTP the Server *"MAY return HTTP 503 … or HTTP 429"* with `Retry-After`, a header
    that *"SHOULD be used only for the client's attempts to reconnect"*.
  - *"The minimum recommended retry interval is 30 seconds."*
  - This Server already answers `Unavailable` with `retry_info` of 30 s (`RETRY_AFTER` in
    [`fleet.rs`](../../crates/fleet-server/src/fleet.rs)) in four places: at the Agent-record
    ceiling, when the enrolment queue is full, when no enrolment window is open (ADR-0059
    clause 21), and when the audit record holds back a CSR
    ([ADR-0063](0063-an-append-only-audit-record-chained-by-hash.md) clause 6).
- **The Client already honours `Unavailable` on both transports.**
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
  too (ADR-0064 clause 13, [`pool.rs`](../../crates/fleet-agent/src/gateway/pool.rs)).
  `unavailable()` in `fleet.rs` sets no `instance_uid`, so none of the four `Unavailable` replies
  above reaches its Agent today, and the Client never waits as told.
- **A host speaks for at most 256 Agents** (ADR-0059 clause 7), and after a reconnect the Client
  sends one full report for each of them. At the Baseline's 30 s heartbeat or poll, 256 Agents
  send about 8.5 messages a second.
- **Refusals already have a bounded record.** ADR-0063 clause 5 writes at most ten refusals of one
  event per peer address and second, and counts the rest. Clause 6 counts every decision whose
  entry could not be queued in the next entry's `unrecorded_before`.

## Decision

We will limit what an admitted peer sends on the Agent plane, its `AgentToServer` messages on
`/v1/opamp` over both transports and its requests on the package download route, with a token
bucket per host, per Agent within an aggregate bucket for a host marked as a Gateway. A message
past the limit goes unprocessed and is answered `ServerErrorResponse` `Unavailable` with a 30 s
`retry_info`, and a download past it is answered `429` with `Retry-After: 30`.

1. **`[agent_rate_limit]` in `server.toml`.** The section is optional, and its defaults are in
   force when it is absent. Any other key is refused, as in every section. No value switches the
   limit off.

   | Key | Default | Rule |
   |---|---|---|
   | `messages_per_sec` | `10` | Tokens added to a host's or an Agent's bucket per second, continuously. `0` is refused at startup with a message naming the key. |
   | `burst` | `300` | That bucket's capacity, enough for one full report from each of the 256 Agents a host may speak for (ADR-0059 clause 7) after a reconnect. A new bucket starts full. `0` is refused at startup with a message naming the key. |
   | `gateway_messages_per_sec` | `500` | Tokens added per second to the aggregate bucket of a host marked as a Gateway. `0` is refused at startup with a message naming the key. |
   | `gateway_burst` | `10000` | The aggregate bucket's capacity: one full report from each Agent at the default `max_carried_agents` (ADR-0064 clause 4). `0` is refused at startup with a message naming the key. |

2. **The Server warns when the limit is below what the fleet's heartbeat needs.**
   `messages_per_sec` times the offered `[connection_offer] heartbeat_interval_secs` is the number
   of Agents one host can report for at that interval. Without an offer, the Baseline's 30 s is
   used. When that product is below 256, the Server logs a warning at startup naming both keys.
   It is a warning, not a refusal: a limit set too low costs availability, not security. At the
   defaults the product is 300.

3. **What is counted.** Each of the following costs one token, on every connection that passed
   admission:
   - every message the endpoint hands to the Server's handler on `/v1/opamp`, on both transports.
     That covers a member's connection and an enrolment connection, a plain-HTTP request (one
     message each), and every message on a WebSocket session;
   - a message that cannot be decoded (`on_unreadable`);
   - every request on the package download route that admission let through.

   On `/v1/opamp` the check runs first in `on_message`: before enrolment, before a CSR is read, and
   before `process_presented` takes the fleet lock. On the download route it runs in
   `admit_download` once the certificate is admitted, before the route's handler.

   Not counted: a WebSocket upgrade that sends nothing, which the connection cap bounds, and the
   Gateways' revocation list.

4. **Which bucket a message or a download is counted in.** The key comes from the certificate
   admission proved (`Proofs` in [`transport.rs`](../../crates/fleet-server/src/transport.rs)):
   - **A certificate that names a host:** that host.
   - **A member's certificate that names no host** (one an operator provisioned that has not been
     renewed yet): its issuer and serial.
   - **A bootstrap certificate on an enrolment connection:** the peer address, an IPv6 one by its
     /64 (`throttle::peer_key`). A bootstrap certificate may be one for the whole fleet (ADR-0059
     clause 10), so its issuer and serial would put every enrolling host in one bucket. The
     enrolment window, the bound on the queue and the operator's approval already bound what
     enrolment costs (ADR-0059 clauses 20, 21).
   - **A host marked as a Gateway** (`Revocations::is_gateway`), in two buckets, and a message must
     take a token from both:
     - the bucket of the Agent the message names, keyed by the host and the message's
       `instance_uid` and sized by `messages_per_sec` and `burst`;
     - the Gateway's aggregate, keyed by the host and sized by `gateway_messages_per_sec` and
       `gateway_burst`.

     A message whose `instance_uid` is not 16 bytes, an undecodable message, and a download
     request through a Gateway name no Agent, so they are counted in the aggregate alone. The mark
     is read per message, so marking or unmarking a Gateway takes effect at the next message.

   A connection with no certificate exists only behind `Admission::open`, which no Server
   configuration serves (ADR-0059 clause 6). It is counted by its peer address.

5. **What a message past the limit gets.** A message that finds no whole token in a bucket it must
   pass is not processed. No record is created or updated, `last_seen_ms` does not move, no CSR is
   read, and nothing is offered. Its reply is a `ServerToAgent` with:
   - the message's own `instance_uid` (clause 7);
   - the Server's capabilities;
   - `error_response` of type `Unavailable`, with an `error_message` saying that too many messages
     arrive;
   - `retry_info.retry_after_nanoseconds` of 30 s, the `RETRY_AFTER` every other `Unavailable` of
     this Server carries and the Baseline's minimum recommended retry interval.

   On plain HTTP the reply is the protobuf body of a `200`, never a `429` or a `503`. The Baseline
   gives `Retry-After` to reconnect attempts, and this keeps the OpAMP answer apart from the
   failed-admission throttle of ADR-0059 clause 24.

   The Server does not close the connection. A WebSocket Client that honours the Baseline closes it
   itself and comes back after 30 s; messages that arrive before it does are counted and answered
   the same way. A throttled message leaves `sequence_num` where it was, so the next message the
   Server processes from that Agent is answered with `ReportFullState`.

6. **What a download past the limit gets.** The download route is plain HTTP and not OpAMP, so it
   has no `ServerErrorResponse`. A request past the limit is answered `429` with `Retry-After: 30`,
   and the artifact is not read. This `429` differs from the one ADR-0059 clause 24 sends:

   | | ADR-0059 clause 24 | This clause |
   |---|---|---|
   | When it is sent | Before the certificate is judged | After the certificate is admitted |
   | Who gets it | A peer address in back-off for failed admissions | A member over its rate |
   | `Retry-After` | The seconds of back-off left | 30 |
   | Feeds the failure count | — | No, it is no failure |
   | Recorded as | `download.throttled` | `agent_rate.throttled` with `route` `download` (clause 9) |

   The Client's downloader treats either one as a failed download today.

7. **Every `Unavailable` reply carries the `instance_uid` of the message it answers.** The Client
   and a Gateway route replies by that field alone and drop a reply without it. The throttled
   reply of clause 5 therefore names the Agent that sent the message, and only that Agent. The
   implementation gives the existing `Unavailable` replies the same, as a bug fix within ADR-0059
   and ADR-0063. Those replies are:
   - the Agent-record ceiling in `fleet.rs`;
   - a CSR held back for its audit record;
   - the full enrolment queue and the closed enrolment window in `Fleet::enrol`.

   An undecodable message has no `instance_uid`, so its throttled reply carries none, and no Agent
   receives it.

8. **The tables are bounded.** The buckets live in memory and nothing persists them. They are kept
   in two tables:
   - Buckets keyed by host, by issuer and serial, or by an enrolling peer's address, and the
     Gateways' aggregates, hold at most `revocation::MAX_ISSUED` (100 000) entries, the
     certificate register's own capacity.
   - Buckets keyed by a Gateway's host and an `instance_uid` hold at most `max_agents` entries
     (default 100 000), the fleet's own ceiling.

   When a table is full, a new key evicts the bucket that was used least recently. Eviction can
   only grant tokens, never withhold them, because a new bucket starts full. Its worst case is a
   peer that refills its own bucket early, and filling a table to that end takes as many keys in
   use as the table holds.

9. **The audit records every throttled message and download as a refusal.** Each one is passed to
   the audit record (ADR-0063) as event `agent_rate.throttled` with outcome `throttled`. The entry
   carries:
   - the peer address;
   - `host`, or `serial` and the CA role where the certificate names no host;
   - `instance_uid`, hex, where the message carries a valid one;
   - `bucket`: `agent`, `host` or `gateway`, naming the bucket that was empty;
   - `route`: `opamp` with the `transport`, or `download`.

   The entries are aggregated under ADR-0063 clause 5, by event and peer address. A host that
   floods costs at most ten entries and one `agent_rate.throttled.aggregated` count per address and
   second. The refusal never waits for its entry. A refusal that finds the audit channel full is
   counted in the next entry's `unrecorded_before` (ADR-0063 clause 6). A message or download the
   limit lets through is recorded by nothing new.

**Out of scope:** the Operator plane, guarded by ADR-0059 clauses 2 and 24. The Gateway's
downstream endpoint, which applies no limit of its own and forwards the Server's `Unavailable`
unchanged. Limits per message kind, or limits that weigh a message by its cost. The rate of
WebSocket upgrades and TLS handshakes. Whether the Client's downloader honours `Retry-After`.

## Alternatives considered

- **A bucket per peer address.** Hosts behind one NAT, or every Agent behind a Gateway, would share
  one budget, and one busy neighbour would throttle the rest. The certificate names the host
  exactly, and admission has already proved it.
- **A bucket per `instance_uid` for every peer.** The uid is self-asserted (ADR-0059 clause 14), so
  a peer that wants more messages names a fresh uid for each one, and the limit binds nothing.
- **Behind a Gateway, per-uid buckets alone.** A downstream peer that cycles uids gets a fresh,
  full bucket each time. `max_carried_agents` does not bound that, because each plain-HTTP
  exchange is a downstream connection of its own. The aggregate is what bounds the Gateway.
- **Behind a Gateway, one bucket for the host alone.** A whole site would share one host's budget,
  and one looping Agent behind the Gateway would throttle every other Agent it carries. The per-uid
  bucket inside the aggregate keeps that to the Agent that loops.
- **A bucket per connection.** A peer opens another connection, up to the cap. A poller has no
  connection that outlives one exchange, so there would be nothing to count it on.
- **`429` or `503` with `Retry-After` on `/v1/opamp` over plain HTTP.** The Baseline gives these to
  refusing a connection, and `Retry-After` to reconnect attempts. One protocol-level answer on both
  transports keeps an over-rate member apart from the failed-admission throttle. The download route
  has no protocol-level answer, so `429` is its answer (clause 6).
- **Leaving the download route uncounted.** The connection cap and the pace floor bound how many
  downloads are open and how slowly they run, but not how often a member asks. Once each request
  scans the fleet under its lock, a member asking in a loop holds that lock from every Agent.
- **Closing the WebSocket on the first message over the limit.** The Baseline leaves the
  disconnect to the Client, and closing gives the Agent no `retry_info`, only a reconnect at its
  backoff.
- **`retry_info` as the time until the next token.** That is at most 100 ms at the defaults, and a
  WebSocket Client reconnects at that pace. Every reconnect costs a TLS handshake and an admission
  entry, and it brings a full report from every Agent at once, which can run into the limit again.
  30 s is the Baseline's recommended minimum and what every other `Unavailable` of this Server
  says.
- **A small `burst`, such as 50.** A host speaking for more Agents than `burst` could never get the
  full reports it sends after a reconnect through.
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

- [OpAMP specification v0.20.0](https://github.com/open-telemetry/opamp-spec/blob/v0.20.0/specification.md):
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
- The code this decision builds on:
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
- [`CONFORMANCE.md`](../CONFORMANCE.md), the *Retrying, throttling, bad request* row: Server-side
  throttling has not yet been taken up.

## Consequences

**Positive:**

- One host, one Agent behind a Gateway, or one Gateway as a whole can no longer take the Server's
  time from the rest of the fleet, on `/v1/opamp` or on the download route. What a looping or
  compromised member costs is bounded by numbers in `server.toml` (Q-2, Strategy *Security before
  convenience*).
- The answer is the protocol's own on both transports, and the Client of this project honours it
  once the reply names its Agent. A throttled message loses nothing, because `ReportFullState`
  recovers the Agent's state at its next processed message.
- The limit follows the identity admission proved. Hosts behind one NAT do not share a budget, and
  a site behind a Gateway is not throttled as one host.
- The `Unavailable` replies this Server already sends reach their Agent, directly and behind a
  Gateway, so the Client waits as it is told.
- The Server emits throttling itself, so the *Retrying, throttling, bad request* row and the
  Deviations table of `CONFORMANCE.md` change with the implementation.

**Negative / trade-offs:**

- A throttled WebSocket Client disconnects and returns after 30 s, as the Baseline asks, so
  throttling costs a host up to 30 s of reports from every Agent on that connection. On its return
  a `burst` of 300 lets one full report from each Agent through.
- The defaults leave little headroom: 256 Agents at a 30 s heartbeat send about 8.5 messages a
  second against 10. A shorter offered heartbeat on a host near 256 Agents needs a higher
  `messages_per_sec`, and the startup warning says so.
- A downstream peer behind a Gateway that names a neighbour's `instance_uid` drains that Agent's
  bucket, and the neighbour is throttled. This is no worse than what ADR-0064 already accepts
  behind a Gateway, where such a peer takes over the neighbour's route. A peer that cycles uids is
  bounded by the Gateway's aggregate, which every Agent behind that Gateway shares.
- An over-limit download fails that download attempt on the Client, which does not honour
  `Retry-After` there yet.
- A throttled `agent_disconnect` is not processed. Over a WebSocket the record is marked
  disconnected when the socket closes. Over plain HTTP it stays connected until it goes stale.
- The Baseline describes `UNAVAILABLE` as an overloaded Server, and here it answers one host or
  Gateway over its rate while the Server has time to spare. The Agent sees no difference, and the
  protocol has no other retryable answer.
- An aggregate audit entry names the peer address, not the host, because ADR-0063 aggregates by
  address. Behind a NAT or a Gateway only the first ten entries of each second name the host or
  the Agent.

**Follow-ups:** limits weighted by message kind; the Client's downloader honouring `Retry-After`.

## Enforcement

Planned tests, each marked `Verifies: ADR-0066`:

- `crates/fleet-server/src/agent_rate.rs` (planned):
  - `a_bucket_starts_full_and_refills_at_the_configured_rate` (clause 1);
  - `a_host_is_one_bucket_whatever_its_agents_report`,
    `a_member_certificate_without_a_host_is_counted_by_issuer_and_serial`,
    `an_enrolment_connection_is_counted_by_its_peer_address` and
    `marking_a_gateway_takes_effect_at_the_next_message` (clause 4);
  - `behind_a_gateway_a_message_passes_its_agents_bucket_and_the_aggregate`: a peer cycling fresh
    uids is stopped by the aggregate, and one looping uid is stopped by its own bucket while
    another uid passes (clause 4);
  - `a_message_naming_no_agent_behind_a_gateway_counts_in_the_aggregate_alone` (clause 4);
  - `a_full_table_evicts_the_bucket_used_least_recently` and `both_tables_are_bounded`
    (clause 8).
- `crates/fleet-server/src/config.rs` (planned):
  - `the_agent_rate_limit_defaults_and_refuses_zero`: the defaults are 10, 300, 500 and 10 000, a
    `0` in any key is refused naming it, and an unknown key is refused (clause 1);
  - `a_limit_below_the_heartbeat_for_256_agents_warns_naming_both_keys`: `messages_per_sec = 5`
    with `heartbeat_interval_secs = 30` warns, naming both keys, and starts; the defaults do not
    warn (clause 2).
- `crates/fleet-server/tests/ws_transport.rs` (planned):
  `a_session_past_its_burst_is_answered_unavailable_with_retry_info`. A member sends `burst + 1`
  messages, and the last reply carries `Unavailable`, a `retry_info` of exactly 30 s and the
  message's `instance_uid`. The connection stays open, the Agent's record is unchanged, and once
  the bucket refills the next message is processed with `ReportFullState` set (clauses 3, 5, 7).
- `crates/fleet-server/tests/http_transport.rs` (planned):
  `a_poller_past_its_burst_is_answered_unavailable_in_the_body`: a `200` carrying the same
  `ServerToAgent`, never a `429` or a `503` (clause 5).
- `crates/fleet-server/tests/packages.rs` (planned):
  `a_download_past_the_limit_is_answered_429_after_thirty_seconds`. A member over its rate gets
  `429` with `Retry-After: 30`, its next download is admitted once the bucket refills, and the
  refusal counts no failure toward the throttle of ADR-0059 clause 24 (clauses 3, 6).
- `crates/fleet-server/tests/mutual_tls.rs` (planned):
  - `two_certificates_of_one_host_share_a_bucket_and_two_hosts_do_not` and
    `a_bootstrap_certificate_shared_by_two_addresses_is_two_buckets` (clause 4);
  - `a_throttled_message_leaves_an_aggregated_refusal_naming_the_host`: beyond ten a second, the
    entries are counted in `agent_rate.throttled.aggregated` (clause 9);
  - `every_unavailable_reply_names_the_agent_it_answers`: each of these carries the message's
    `instance_uid` (clause 7):
    - the throttled reply;
    - the Agent-record ceiling;
    - a CSR held back for its audit record;
    - the full enrolment queue;
    - the closed enrolment window.
- `crates/fleet-server/src/audit_log.rs` (planned):
  `a_throttle_refusal_that_finds_the_channel_full_is_counted_unrecorded` (clause 9).
- `crates/fleet-agent/tests/gateway_e2e.rs` (planned):
  `a_throttled_agent_behind_a_gateway_hears_unavailable_and_its_neighbour_does_not`. Two Agents
  ride one folded connection to a marked Gateway, and one floods. It alone receives the
  `Unavailable`, routed by its `instance_uid`, and the other's reports keep being processed
  (clauses 4, 5, 7).
