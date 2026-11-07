# ADR-0015: A WebSocket on which nothing arrives for one heartbeat interval is ended by the end that opened it

- **Status:** 🟢 accepted
- **Date:** 2026-11-07
- **Deciders:** Markus Brigl
- **Applies to:** how long a WebSocket the Agent's end opens may stay silent: the driver in `crates/opamp/src/client/ws.rs` (`run`, `run_from` and `serve`), the Gateway's upstream pool in `crates/fleet-agent/src/gateway/pool.rs`, every other WebSocket an Agent, a Client or a Gateway opens to a Server, and the reading of a WebSocket by the endpoint in `crates/opamp/src/server.rs` (`serve_socket`) the Server, the Supervisor Endpoint and the Gateway's endpoint share

## Context

An Agent learns that its Server is gone when its connection closes. A Server killed by its kernel
resets its connections, and its Agents reconnect as
[ADR-0012](0012-tls-1-3-two-planes-and-bounded-input.md) clause 4 says. A Server whose host loses
power, whose process is frozen, or which a partition cuts off sends nothing and closes nothing. The
Agent's heartbeats go into its socket's send buffer and are retransmitted until the kernel gives
up: at Linux's default `tcp_retries2` of 15, about fifteen minutes. A frozen process is worse:
its kernel keeps acknowledging what arrives until the receive buffer is full, so TCP sees nothing
wrong at all. G-20 asks that every Agent of a lost instance be connected to another within its
lease time and two heartbeat intervals, 90 seconds at the defaults.

The WebSocket driver of `crates/opamp` sends a routine report per Agent every heartbeat interval
and waits for nothing. The Gateway's upstream pool forwards what its Agents send and waits for
nothing either. Neither sets a TCP keepalive or a TCP user timeout. Opening a connection has no
bound of its own: a TCP connection a frozen process's kernel accepted, or a balancer still sends
to, waits for a TLS answer that never comes. Plain HTTP is already bounded: every exchange has
`HTTP_TIMEOUT`, 30 seconds.

A WebSocket ping is answered by every conforming peer: RFC 6455 says an endpoint that receives a
Ping must send a Pong. It needs nothing of OpAMP, so it holds against any Server the `opamp`
crate talks to, with heartbeats or without, and on a Gateway's pool connection, which carries other
Agents' reports and has no heartbeat of its own. The Server's rate limit counts messages handed to
its handler (ADR-0012 clause 24), and its pace floor judges data frames (clause 18); a ping is
neither.

The heartbeat interval is the operator's measure of how often an Agent is heard from, and G-20 is
stated in it. A bound that follows it keeps the failover inside G-20 whatever the operator sets,
and costs one ping per connection per interval at most, since anything that arrives counts.

The `opamp` crate is versioned by the Baseline ([ADR-0008](0008-five-crates-on-tokio-and-axum.md)
clause 2): a new field on its public settings would break its interface and wait for the next
Baseline. The heartbeat is already a field of the driver's settings.

## Decision

We will end a WebSocket that an Agent, a Client or a Gateway opened to a Server once nothing has
arrived on it for one heartbeat interval, after sending a WebSocket ping when nothing has arrived
for half of one, and reconnect as after a lost connection, and have the end that listens answer a
ping while it handles a message.

1. **Anything that arrives counts.** A message, a pong, a ping or a close from the Server is a
   sign of life; the bounds run from the last one. A ping goes out only when nothing has arrived
   for half an interval, and at most once until something arrives. Its pong is given the rest of
   the interval from the moment the ping went out, so an end that was busy with its own work,
   reading nothing meanwhile, does not take its own delay for the Server's silence.
2. **The bound is the heartbeat interval.** Without a heartbeat (`heartbeat_interval_secs = 0`, or
   no heartbeat in the driver's settings) it is 30 seconds, the default interval. No key sets it
   apart from the heartbeat.
3. **Opening is bounded too, address by address.** Resolving the endpoint's name gets one
   interval. Each address the name resolves to is then tried in turn, in the resolver's order, and
   the TCP connection, the TLS handshake and the upgrade to it together get one interval; the TLS
   server name and the `Host` header stay the name. An address not reached within its interval, or
   whose TCP connection, TLS handshake or I/O fails, gives way to the next. An HTTP answer to the
   upgrade ends the attempt as it is, and the first address that opens a WebSocket ends it too.
   This holds wherever a WebSocket is opened: by the driver, by the Gateway's pool, and by the
   probe of an offered endpoint. An attempt that reaches no address fails with the last address's
   failure and is retried with the next backoff step.
4. **An ended connection is a lost one.** The driver logs that nothing arrived, with the bound,
   and reconnects after the delay ADR-0012 clause 4 draws over `[0, 30 s)`. The Gateway's pool
   marks the connection gone, and its Agents are placed anew at their next report (ADR-0016
   clause 9).
5. **Nothing at the TCP level.** No TCP keepalive and no TCP user timeout are set.
6. **No existing interface changes.** No public function or setting of `crates/opamp` changes;
   the bounds come from the heartbeat already in the settings. `ws::Silence` is public, so that
   the Gateway's pool derives the same bounds, and so is `ws::run_from`, `run` with every
   connection opened from a local address the caller names
   ([ADR-0025](0025-several-server-instances-share-one-postgresql-database-and-two-versions-run-side-by-side.md)
   clause 20). Both are additions, not breaks under ADR-0008 clause 2.
   `Connection::connect_websocket` keeps its signature and fails once the bounds of clause 3 have
   passed.
7. **The end that listens answers a ping while it handles a message.** The endpoint of
   `crates/opamp`, which the Server, the Supervisor Endpoint and the Gateway's endpoint share,
   reads a WebSocket on while it handles one of its messages, so a stall of the handling, on the
   disk, the database's pool or a lock, ends no connection for silence. The messages read
   meanwhile wait and are handed to the handler one at a time, in the order they arrived, and
   their replies go out in that order. Once eight wait, or as many bytes as the message limit,
   the connection is read no further until one is handled. A reply to a message handled after
   the peer closed cannot go out and is handed back to the handler as unsent.

**Out of scope:** how the Server notices an Agent that stopped answering; plain HTTP, which
`HTTP_TIMEOUT` already bounds; ending a silent connection on the Supervisor Endpoint and the
Gateway's endpoint, where the Agent's end is the one that listens and answers pings as clause 7
says.

## Alternatives considered

- **An answer awaited for each heartbeat.** Our Server answers every message, but whether every
  OpAMP Server answers every message on a WebSocket was not established, and the `opamp` crate
  talks to any Server. Without heartbeats there would be no check at all, and a Gateway's pool
  connection has no heartbeat of its own to await an answer to.
- **Fixed values.** Ten seconds to the ping and twenty to the end notices a frozen instance
  sooner, but also ends the connections of an instance that is merely slow, and sends a fleet that
  is already under load back as a reconnect wave. Fifteen and thirty match the defaults but drift
  from G-20 when the operator changes the interval. Following the heartbeat gives the same values
  at the defaults and keeps the failover inside G-20 at any interval.
- **A key of its own in the Client's configuration.** A knob is added when a deployment needs a
  value other than the heartbeat's, not before, as ADR-0012 clause 18 does for its bounds.
- **TCP keepalive, or `TCP_USER_TIMEOUT`.** Neither notices a frozen process, whose kernel keeps
  acknowledging. Keepalive's interval needs `socket2`, a dependency this would add for nothing the
  ping does not do; the user timeout is Linux's alone; and the connect path of
  `tokio-tungstenite` does not hand out the socket before it is connected.
- **One interval for the whole opening.** A name whose first address is not reached never
  connects at a short interval: on Windows a refused connection to `::1` takes about two seconds,
  so at a heartbeat of one second every attempt ends before `127.0.0.1` is tried.
- **The addresses raced, as Happy Eyeballs does (RFC 8305).** A dead first address costs less
  than an interval, but each attempt opens several connections at once and needs a staggering of
  its own. Trying the addresses in turn is simpler and bounded too.
- **A new field on the driver's settings.** It would break the `opamp` crate's interface
  (ADR-0008 clause 2) and gives nothing the heartbeat does not.

## Sources / Prior art

- RFC 6455 *The WebSocket Protocol*,
  [section 5.5.2 Ping](https://www.rfc-editor.org/rfc/rfc6455#section-5.5.2)
  and section 5.5.3 Pong: a Pong is sent in response to a Ping unless a Close was already
  received.
- `tungstenite` 0.30 `WebSocket::read` documentation (in the registry source this repository
  builds): a received Ping queues a Pong, written and flushed by the next `read`, `write` or
  `flush`; a server that keeps reading therefore answers without code of its own.
- Linux `tcp(7)`: `tcp_retries2` (default 15) bounds retransmission of unacknowledged data, about
  924 seconds; `TCP_USER_TIMEOUT` (RFC 5482) shortens it per socket, on Linux.
- RFC 8305 *Happy Eyeballs Version 2: Better Connectivity Using Concurrency*: the addresses of a
  name tried with staggered parallel attempts, the alternative to trying them in turn.
- gRPC keepalive (`GRPC_ARG_KEEPALIVE_TIME_MS`, `GRPC_ARG_KEEPALIVE_TIMEOUT_MS`): a ping after a
  quiet time and the connection closed when no answer comes within a timeout, the same shape.

## Consequences

- Positive: an Agent of a frozen, powerless or cut-off instance reconnects within one heartbeat
  interval and its drawn delay, 60 seconds at most at the defaults, inside G-20's 90.
- Positive: a Server, a balancer or a Gateway's upstream that accepts a connection and never
  answers does not hold the Agent forever.
- Negative / trade-offs: an instance that reads nothing for a whole interval, overloaded rather
  than lost, loses its Agents to the others; its connections come back spread by the drawn delay.
  A stall of the Server's handling is not such a case (clause 7), but a peer that sends eight
  messages or the message limit ahead of its answers is read no further, and answered no ping,
  until one is handled.
- Negative / trade-offs: a message counts only once it has arrived whole. A `ServerToAgent` that
  takes longer than an interval to arrive, near the message limit on a slow link, ends the
  connection before it is complete, and is sent again after the reconnect.
- Negative / trade-offs: an upstream that freezes holds a Gateway's forward of a downstream
  Agent's report for up to an interval. The Gateway reads that Agent's connection on meanwhile and
  answers its pings, as the Server does, so the Agents behind it keep their connections to it
  unless they send more than the eight messages the Gateway lets wait.
- Negative / trade-offs: a heartbeat interval set very short makes the bound as short; one second
  means a connection that is silent for a second ends.
- Negative / trade-offs: opening gets one interval per address and no more. At a heartbeat of one
  second, a link whose round trip is above about 300 milliseconds cannot finish the TCP
  connection, the TLS handshake and the upgrade in time, and no connection comes up. Each address
  not reached delays the connection by an interval, and an attempt that reaches none takes one
  interval for the resolution and one per address before it fails.
- Negative / trade-offs: a connection that is quiet in both directions carries a ping and a pong
  every half interval, a few bytes each.
- Follow-ups: whether a send blocked on a full buffer needs a bound of its own, when a measurement
  shows one; and whether the Gateway's pool needs a bound on handing a reply to a downstream Agent
  (`deliver`), which waits for as long as that Agent reads slowly and holds the reader meanwhile.

## Enforcement

- [`crates/opamp/src/client/ws.rs`](../../crates/opamp/src/client/ws.rs):
  `a_connection_on_which_nothing_arrives_for_a_heartbeat_interval_is_ended` (clauses 1, 4),
  `a_server_that_answers_only_pings_keeps_the_connection` (clauses 1, 2),
  `the_bounds_follow_the_heartbeat_and_are_thirty_seconds_without_one` (clause 2),
  `a_session_busy_for_longer_than_an_interval_keeps_a_live_connection` (clause 1),
  `the_pong_is_given_the_rest_of_the_interval_from_its_ping` (clause 1),
  `an_upgrade_that_is_never_answered_fails_within_the_bound`,
  `an_address_not_reached_within_its_interval_gives_way_to_the_next`,
  `an_http_answer_on_the_first_address_ends_the_attempt` (clause 3).
- [`crates/opamp/src/client/connection.rs`](../../crates/opamp/src/client/connection.rs):
  `a_probe_of_an_endpoint_that_never_answers_fails_within_the_bound` (clause 3).
- [`crates/fleet-agent/src/gateway/mod.rs`](../../crates/fleet-agent/src/gateway/mod.rs):
  `an_upstream_connection_on_which_nothing_arrives_is_ended_and_its_agents_placed_anew`
  (clauses 1, 4), `an_upstream_that_answers_pings_keeps_its_connection` (clause 1),
  `an_upstream_that_never_answers_the_upgrade_fails_the_forward_within_the_bound` (clause 3),
  `an_upstream_reader_held_up_by_a_slow_downstream_keeps_a_live_connection` (clause 1),
  `an_ended_upstream_connection_is_closed_towards_the_server` (clause 4).
- [`crates/fleet-server/tests/ws_transport.rs`](../../crates/fleet-server/tests/ws_transport.rs):
  `the_server_answers_a_ping_with_a_pong` (the Server's side of clause 1).
- [`crates/opamp/src/server.rs`](../../crates/opamp/src/server.rs):
  `a_ping_is_answered_while_a_message_is_handled`,
  `the_messages_of_one_connection_are_handled_one_at_a_time_and_answered_in_order`,
  `a_connection_with_eight_messages_waiting_is_read_no_further_until_one_is_handled`,
  `a_connection_with_the_limit_in_bytes_waiting_is_read_no_further_until_one_is_handled`,
  `messages_before_a_peers_close_are_handled_and_their_replies_reported_unsent` (clause 7).
- [`crates/fleet-server/tests/g20.rs`](../../crates/fleet-server/tests/g20.rs):
  `g20_a_frozen_instance_s_agents_are_carried_by_the_others_within_the_bounds`, the failover with
  the instance stopped by `SIGSTOP` rather than killed, under G-20's bounds.
- Clause 5 is **Not mechanically decidable:** the absence of a socket option is not observable
  from a test without asserting on the implementation; review checks that no keepalive or user
  timeout is set.
