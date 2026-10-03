# ADR-0033: An Agent's side of OpAMP is one reusable client — a protocol state machine apart from what the Client does with it, and the connection driven over a session

- **Status:** ⚪ superseded by [ADR-0036](0036-the-whole-opamp-communication-layer-in-the-opamp-crate.md)
- **Date:** 2026-10-03
- **Deciders:** Markus Brigl
- **Applies to:** `crates/opamp/src/client/` (`mod.rs`, `protocol.rs`, `ws.rs`, `http.rs`, `backoff.rs`), `AgentState` in `crates/fleet-agent/src/supervisor/agent.rs`, and the Client's `Session` and its flows after a reply in `crates/fleet-agent/src/transport/`

## Context

`AgentState` (`crates/fleet-agent/src/supervisor/agent.rs`) is a state machine without I/O —
`next_report()` builds the next `AgentToServer`, `handle()` reacts to a `ServerToAgent` — but it is
two things in one type:

- **what OpAMP defines**: the `instance_uid` and a Server-assigned replacement, the `sequence_num`,
  both Capability Sets and what they license
  ([ADR-0018](0018-connection-settings-and-server-capabilities.md)), full versus compressed reports
  and the flags that demand a full one, `ServerErrorResponse` with `RetryInfo`, commands,
  `agent_disconnect`, and the remote-configuration and connection-settings statuses;
- **what this Client decides**: the host description and configured attributes
  ([ADR-0024](0024-what-an-agent-reports-about-itself.md)), the single package an Agent takes and
  what its package status says ([ADR-0019](0019-package-delivery-on-the-agent.md),
  [ADR-0021](0021-the-client-updates-itself.md)), persistence of identity, configuration and the
  package record, the Managed Process's fold, the CSR enrolment
  ([ADR-0017](0017-admission-and-authentication.md)).

And both transports ran the Client's own flows after every reply — certificate, connection-settings
offer, packages, the self-update restart, the Supervisor set — written twice, in different orders.

Most of the specification's MUSTs on the client side live in the connection loops, not in the state
machine: the limit in both directions, backoff with jitter, the 1009 close, how a throttled Agent
waits (`RetryInfo`, `429`/`503` with `Retry-After`, `413` not retried), the goodbye. A reusable
client that offered only the state machine would leave every user to write exactly that again.

This Client carries *n* Agents over one connection
([ADR-0009](0009-client-modes-and-the-gateway.md)); `opamp-go`'s client carries one. A driver built
for *n* serves the single-Agent case as *n* = 1; the reverse does not hold.

The client lives in the `opamp` crate behind its `client` feature
([ADR-0031](0031-one-opamp-crate-a-publishable-wire-layer-with-client-and-server-features.md)); the
Server-facing side of a Supervisor stays a Port ([ADR-0015](0015-supervisor-mode-and-its-kinds.md)).
How the Client builds its TLS and its credential stays
[ADR-0012](0012-transports-tls-and-the-servers-two-planes.md)'s and
[ADR-0017](0017-admission-and-authentication.md)'s.

## Decision

We will separate the protocol from what the Client does with it, and implement an Agent's
connection **once**, in `opamp::client`, beside the state machine, as two drivers generic over a
**session** the application implements.

1. **The protocol state machine**, `opamp::client::protocol`, owns the first list and depends on
   `opamp` and the standard library alone. `next_report(&impl ReportContent)` decides *which* fields
   a report carries; `ReportContent` supplies *what* they hold. `receive(&ServerToAgent) ->
   Received` settles capabilities, flags, errors, identity and commands, and hands the rest over as
   data.
2. **`AgentState` keeps this Client's decisions** and owns one protocol state machine; its interface
   towards the Engine does not change.
3. **`opamp::client::ws::run` and `opamp::client::http::run`** own the connection: connecting with
   backoff and jitter, the heartbeat or the poll interval, framing, the limit in both directions,
   the 1009 close, throttling, and the goodbye.
4. **The application implements `Session`** — the side of one connection carrying any number of
   Agents: `connected`, `routine`, `owed`, `on_reply`, `after_reply`, `changed`, `exchange_failed`,
   `stop`, `goodbyes`. `AfterReply` says `Continue`, `Reconnect` or `End`; `StopSignal` ends a run;
   `ReportSink` is how a long-running job reports through the driver.
5. **The application owns the connection's material**: it hands a driver the endpoint, the headers,
   a TLS connector or an HTTP client it built, the limit and the intervals. Building those from
   `supervisor.toml` — the private CA, the client certificate, the credential, the cleartext
   warnings — stays in the Client, whose `Engine` is the session.
6. **The Client's flows after a reply are one step, `after_reply`, in one order**: certificate,
   connection settings, packages, **the self-update restart before the Supervisor set** — the
   `Installing` report is the last thing a version says (ADR-0021), so nothing is applied after it.
7. **`Backoff` is public**: the Client spreads its process restarts with it too.
8. **Behaviour-preserving**, the suite is the proof, apart from the order in clause 6 on plain HTTP:
   a pending Supervisor set is re-offered to, and applied by, the new version.

**Out of scope:** a single-Agent callback driver; the connection-settings verification and the
Gateway's upstream pool, which keep their own connect.

## Alternatives considered

- **A callback client per Agent, in the style of `opamp-go`** — not chosen as the core or the
  driver: a state machine without I/O serves this Client's Engine, which carries *n* Agents over one
  connection, and a callback driver can be written on top of `Session` for a user who wants it.
- **Split only the transports** — rejected: it leaves the larger knot in `AgentState`.
- **Only the state machine, the loops left to each user** — rejected: the loops are where most
  client-side MUSTs live.
- **Let the client build TLS and credentials from settings** — rejected: that is policy this Client
  decides (ADR-0012, ADR-0017).

## Sources / Prior art

- [`opamp-go` client](https://github.com/open-telemetry/opamp-go/tree/main/client) — the protocol
  and the agent's decisions separated along the same line; one client per Agent; `wsclient.go` and
  `httpclient.go` over `client/internal` (senders and receivers per transport, the client state,
  the processing of what was received, the package syncer); backoff from `cenkalti/backoff`. Read
  2026-10-01.
- The sans-IO pattern: [sans-io.readthedocs.io](https://sans-io.readthedocs.io/);
  [`quinn`](https://docs.rs/quinn) over [`quinn-proto`](https://docs.rs/quinn-proto) — a tokio
  driver in front of a state machine without I/O; [`rustls`](https://docs.rs/rustls) under
  [`tokio-rustls`](https://docs.rs/tokio-rustls).

## Consequences

- Positive: the OpAMP half can be read, tested and changed on its own; the client-side MUSTs live in
  one place beside the state machine they belong to, and this Client and any other Rust agent share
  them. The flows after a reply exist once, in one order.
- Negative / trade-offs: `ReportContent` and `Received` are an interface where a method used to read
  a field; it earns that only while the state machine stays free of the rest of the Client.
- Negative / trade-offs: `Session` is the widest trait in the project, and the code on every host
  runs through it; it holds what both loops call and nothing the Client merely happens to need.
- Follow-ups: a single-Agent callback driver if a user asks; whether the connection-settings
  verification and the Gateway's upstream pool use the drivers' connect instead of their own.

## Enforcement

- `crates/opamp/src/client/protocol.rs`: `the_first_report_is_full_and_the_next_carries_only_identity`,
  `report_full_state_forces_a_full_report_and_owes_it_now`, `a_command_is_acted_on_alone`,
  `unavailable_yields_the_retry_hint_or_half_a_minute`,
  `a_reassigned_identity_is_adopted_and_handed_over`,
  `the_servers_capabilities_bind_what_a_report_carries`,
  `a_remote_config_status_rides_whether_or_not_remote_config_is_offered`,
  `an_offer_arms_the_connection_settings_status_whatever_the_bitmask_says`,
  `components_go_out_as_a_hash_until_the_server_asks_for_the_map`,
  `a_certificate_request_goes_out_once_and_only_to_a_server_that_declared_signing`,
  `the_goodbye_carries_agent_disconnect_and_the_declared_set` (clause 1).
- `crates/opamp/src/client/ws.rs` `an_oversized_message_from_the_server_closes_with_1009`;
  `crates/opamp/src/client/http.rs` `an_oversized_report_is_never_sent`,
  `the_instance_uid_rides_as_a_header`, `a_throttling_response_is_honoured_for_the_interval_it_names`,
  `throttling_without_a_hint_waits_the_recommended_minimum`,
  `an_oversized_report_is_refused_rather_than_treated_as_a_lost_exchange`,
  `an_oversized_response_is_discarded` (clause 3).
- `crates/opamp/src/client/backoff.rs` `backoff_doubles_and_caps_within_its_jittered_bounds`,
  `two_backoffs_do_not_produce_the_same_ladder` (clauses 3, 7).
- The Client's transport and end-to-end suites run unchanged over its `Session` (clauses 2, 4, 5,
  8).

**Not mechanically decidable:** that the state machine depends on nothing of the Client (clause 1)
is held by the crate boundary rather than a test. The order of clause 6 is one function,
`after_reply`, read in review; no test yet drives a self-update and a Supervisor set through one
reply.
