# SEC-connection-settings-never-plaintext: Connection settings never lead to plaintext, and a Server's capabilities bind what the Client sends

- **Status:** 🟢 accepted
- **Date:** 2026-10-19
- **Deciders:** Markus Brigl
- **Applies to:** the `[connection_offer]` section of `server.toml` and its startup check in `crates/fleet-server/src/config.rs`, the offer composition in `crates/fleet-server/src/fleet.rs` (`compose_settings_offer`) and `crates/fleet-server/src/fleet/session.rs` (`settings_offer`), the capability declaration in `crates/fleet-server/src/fleet.rs`, the Client's offer handling and its verification of an offered endpoint in `crates/fleet-agent/src/connection.rs`, `crates/fleet-agent/src/transport/mod.rs` and `crates/fleet-agent/src/engine.rs`, its persisted `connection-settings.pb` and how it is loaded, every gate on a Server capability in `crates/fleet-agent/src/supervisor/agent.rs`, and every document that tells an operator where an offer may move a fleet

## Context

An endpoint, a heartbeat interval or a client certificate that only an edit of every host's
configuration file could change would bring back the fleet-wide chore this project exists to
remove. The Baseline's answer is connection settings management:

- `ConnectionSettingsOffers` carries an `OpAMPConnectionSettings` (endpoint, headers *"typically
  used to set access tokens"*, heartbeat interval, certificate, `tls`, `proxy`), the three
  own-telemetry destinations, and `other_connections`, under one `hash` described as *"Hash of all
  settings"*.
- `ConnectionSettingsStatus` reports the last hash with `APPLYING`, `APPLIED` or `FAILED`, and
  *"if the hashes are different the Server MUST include the connection_settings field in the
  response"*.
- The Baseline names *"3 classes of destinations"*: the OpAMP Server, own telemetry, and "other".
  It says *"Depending on which connection settings are offered the sequence of operations is
  slightly different."*
- Its verification MUST stands under `ConnectionSettingsOffers.opamp`, and its scope justifies it:
  *"The Client MUST verify the offered connection settings by actually connecting before
  accepting the setting to ensure it does not lose access to the OpAMP Server"*.
- The own-telemetry sequence has no verification step and no reconnect. It asks the Server to send
  destinations in its first reply *"unless there is no OTLP backend that can be used"*, whether or
  not it has OpAMP settings to offer. It asks the Agent to set `connection_settings_status` when it
  receives new settings.
- For an Agent with `ReportsHeartbeat` the Server MAY set `heartbeat_interval_seconds`, the Agent
  MUST use it, and a plain-HTTP Client MUST use it as its polling interval.
  `ConnectionSettingsStatus`, `ReportsConnectionSettingsStatus` and `heartbeat_interval_seconds`
  are of Development maturity, the same deliberate risk acceptance as `ReportsHeartbeat`.

The Agent plane admits by client certificate alone
([SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md)). There is no fleet
credential for the Server to offer or for the Client to send. An offered credential, or an offered
header on the OpAMP connection, has nothing on the Server that reads it.

The specification puts security before convenience (Strategy *Security before convenience*, Q-1
*Secure by default*). An offered endpoint is a connection that leaves the host, so it is TLS 1.3. A
Client without a CA file of its own trusts the public roots
([`HARDENING.md`](../HARDENING.md), *What already holds*). A Server could then move the fleet to any
host a public CA certified.

An offered endpoint on the loopback stays within the wording of Q-1, which forbids sending a
configuration or a package unencrypted beyond the loopback. It leaves the Server's identity
unproven, though: a `ws://` or `http://` endpoint has no TLS to verify.

- Whoever listens on that port of the Client's host speaks to the Client as its Server, and any
  local account can open such a port.
- That listener can send Supervisor sets and configurations whose language runs commands: an entry
  that runs a program, or whatever the configuration of a `command` kind's program makes it run.
  These run with the rights of the account the Client's service runs under
  ([CLT-accounts-service-runs-under](CLT-accounts-service-runs-under.md)).
- One such offer suffices, from a Server that is compromised or misconfigured. The real Server
  cannot undo the move, because the Client never connects to it again and the persisted endpoint
  outranks `supervisor.toml`.

Plaintext on the loopback exists for development and tests. There an operator writes the endpoint
into `supervisor.toml`, and the Client connects to a Server on the same host. That needs no offer.

Capability negotiation is symmetrical: *"after the Agent learns about the capabilities of the
Server the Agent MUST stop using the capabilities that the Server does not support."* With
`opamp-go` as the behavioural oracle
([API-protocol-pinned-interop-checked](API-protocol-pinned-interop-checked.md)), a third-party
Server may implement only the required bits, and this Client has to notice. Applied literally,
field by field, the rule deadlocks: a Server that sends an offer without declaring
`OffersConnectionSettings` receives no acknowledgement and re-offers forever. One plausible gate,
`remote_config_status` behind `OffersRemoteConfig`, would do harm. What needs deciding is the rule
that produces the gates, including when not to gate.

## Decision

We will offer connection settings from `server.toml` as one hash-gated message in the Baseline's
three classes that never moves a Client to a `ws://` or `http://` endpoint, have the Client verify
by connecting only the OpAMP half, apply the rest in place, persist what it applied and acknowledge
the whole message once, and make the Client's use of Server capabilities a stated rule: optimistic
until the Server speaks, outranked by what the Server actually sends, and pessimistic only where a
message would be an error.

1. **The Server's standing offer is `[connection_offer]`, and it carries no credential.**
   - The section names an optional `heartbeat_interval_secs`, an optional `endpoint` (for example
     for a Server move), or both. An empty section fails startup.
   - The `endpoint` must be `wss://` or `https://`. The Server refuses a `ws://` or `http://`
     endpoint at startup, on the loopback too, and never only warns about it. The message names
     `[connection_offer] endpoint` and says that an offer never moves a fleet to plaintext.
   - The Server refuses a credential key at startup with a message naming the key. The credential
     keys are `bearer_token_file`, `username`, `password_file`, and the inline `bearer_token` and
     `password`. The Agent plane admits by client certificate alone
     ([SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md)), so nothing
     would read an offered credential.
   - The OpAMP settings the Server offers carry no `headers`.

2. **One message, one SHA-256 hash, offered on mismatch.**
   - The Server composes the OpAMP settings and the own-telemetry destinations into one
     `ConnectionSettingsOffers` and hashes the whole message.
   - It includes the message whenever an Agent's reported `last_connection_settings_hash` differs.
   - The OpAMP half goes only to Agents that declare `AcceptsOpAMPConnectionSettings`. Each
     telemetry destination goes only to an Agent that declares the matching signal
     ([CLT-what-an-agent-reports](CLT-what-an-agent-reports.md)).
   - With only telemetry to offer, the telemetry is the whole offer, with no `opamp` block.
   - A certificate issued in answer to a CSR
     ([SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md)) travels as the
     standing OpAMP settings plus that certificate, under a hash of its own.

3. **The Server declares `OffersConnectionSettings` whenever it can offer anything.** It can offer
   when it has a `[connection_offer]`, a `[telemetry_offer]`, or a `[client_ca]` whose issued
   certificate travels as an offer. The Server never exercises an undeclared capability and never
   declares a hollow one.

4. **An offer is actionable when it carries anything this Client can put in force.** That is OpAMP
   settings, or any of `own_metrics`, `own_traces` and `own_logs`. An offer carrying only
   `other_connections` is not actionable while `AcceptsOtherConnectionSettings` is undeclared.
   Acknowledging what the Client cannot apply is a false report, and a conforming Server does not
   send such an offer. Once `other_connections` is implemented, it joins the telemetry class of
   clause 6: no verification by connecting, no reconnect, one acknowledgement.

5. **The Client verifies the OpAMP half by actually connecting, then persists it, then reconnects
   with it.**
   - On receipt the Client reports `APPLYING` and checks the candidate's endpoint.
   - A candidate whose endpoint differs from the one in force is a move. The Client refuses a move
     to a `ws://` or `http://` endpoint without connecting, wherever its host is.
   - The Client refuses a move to another `wss://` or `https://` endpoint the same way unless
     `[tls] ca_file` is set. With it set, the Client verifies the candidate against that CA alone.
     Under the public roots an offer can move the fleet to any host a public CA ever certified.
     That move would outlive the operator's file (clause 9).
   - A refused move is reported `FAILED`, with an `error_message` naming the endpoint and the
     reason.
   - Otherwise the Client connects with the candidate over TLS 1.3. The candidate is the offered
     fields, falling back to those in force, an offered certificate included and offered `headers`
     never (clause 8).
   - On that connection the Client presents the client certificate in force, unless the offer
     carries one, and sends no `Authorization`.
   - Only a candidate that keeps a loopback endpoint already in force connects in plaintext.
   - A WebSocket candidate must complete its handshake. A plain-HTTP candidate must complete a
     real exchange.
   - Only then does the Client persist the settings and store an issued certificate. It then drops
     the connection, and the runtime reconnects with the new settings, possibly on the other
     transport.
   - A refused or failed verification keeps everything in force as it was and reports `FAILED`
     with the error.

6. **The Client verifies no telemetry destination by connecting, and a telemetry destination does
   not restart the connection.**
   - Reachability of an OTLP receiver is not this Client's to establish at offer time. A receiver
     that is down does not make an offer wrong.
   - Before it puts a destination in force, the Client checks what it can decide alone: the
     cleartext rule and the unhonoured fields of
     [CLT-what-an-agent-reports](CLT-what-an-agent-reports.md). It reports a refusal and never
     swallows it.
   - The Client applies a telemetry-only offer in place. Its acknowledgement rides the reports
     already owed, and the transport loop carries on.
   - The headers of a telemetry destination belong to an OTLP receiver, not to admission. They stay
     as CLT-what-an-agent-reports has them, offered only on member connections
     ([SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md)). Clause 8 does
     not reach them.

7. **One offer, one acknowledgement.**
   - A single `connection_settings_status` answers each offer.
   - Its `error_message` names everything dropped or refused across both halves.
   - If the OpAMP half fails verification, the Client persists and applies nothing from that
     offer, the telemetry half included, and reports the offer `FAILED`. Half-applying an offer
     whose other half was rejected would leave the Server unable to tell what is running.

8. **The Client does not honour `tls`, `proxy` and `headers`, and the acknowledgement says so.**
   - The Client applies an offer for every field it honours: endpoint, heartbeat, certificate.
   - It then reports the offer `FAILED`, with an `error_message` naming the dropped fields. It
     names offered `headers` by their keys and never by their values.
   - It echoes the hash either way, so the Server does not re-offer in a loop.
   - The Client refuses `TLSConnectionSettings` on merit. A Server able to command
     `insecure_skip_verify` can switch off the check that proves it is the Server. Trust is an
     operator's file
     ([SEC-tls13-bounded-input-chosen-planes §5](SEC-tls13-bounded-input-chosen-planes.md)).
   - `ProxyConnectionSettings` has nothing on this Client to configure. Both fields are of
     Development maturity upstream.
   - The Client refuses offered `headers` on the OpAMP connection on merit too. The Agent plane
     reads no `Authorization` and admits by client certificate alone
     ([SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md)). An applied
     header would be a value the Server plants on every connection of the fleet, persisted and sent
     upstream with no reader on this Server. It would also follow the fleet to whatever endpoint a
     later offer moves it to.
   - [`CONFORMANCE.md`](../CONFORMANCE.md) records the three refusals.

9. **The Client persists what is in force as `connection-settings.pb`, and the file outranks
   `supervisor.toml`.**
   - The file in `state_dir` is the Baseline's own `ConnectionSettingsOffers`. It holds the merged
     settings in force and the hash that reports them `APPLIED`. The Client writes it with no access
     for any other account: mode `0600` on Unix.
   - OpAMP fields fold: an offer that omits a field leaves the one in force.
   - The file carries an `opamp` block only when an offer, or the state it folds into, had one.
   - How the telemetry destinations fold is
     [CLT-what-an-agent-reports](CLT-what-an-agent-reports.md)'s.
   - At startup the persisted settings override the `endpoint` and the heartbeat and poll intervals
     of `supervisor.toml`. The persisted hash restores the status to report.
   - A persisted `ws://` or `http://` endpoint that differs from the endpoint of `supervisor.toml`
     is the exception. The Client does not apply it, uses the endpoint of `supervisor.toml`
     instead, and logs a warning naming both.
   - The persisted OpAMP settings carry no `headers`. The Client drops on load, and never sends, an
     `Authorization` header that a file holds from an earlier rotation.
   - `supervisor.toml` stays what the operator wrote. Deleting the file reverts to it.

10. **An offered heartbeat replaces the configured interval.** A non-zero
    `heartbeat_interval_seconds` becomes the heartbeat period on WebSocket and the polling interval
    on plain HTTP. The Server judges staleness by the heartbeat it offered
    ([SRV-fleet-record-agents-hosts](SRV-fleet-record-agents-hosts.md)).

11. **Settings are connection-scoped; offers are per Agent.** One connection may carry n Agents
    ([CLT-modes-and-gateway](CLT-modes-and-gateway.md)).
    - The Server offers to each of them.
    - The Client keeps one pending offer and verifies and switches once per hash.
    - The Client reports status for every Agent it carries.

12. **The Client is optimistic until the Server has spoken, and bound once it has.**
    - Until the Client has seen a `ServerToAgent` with a non-zero `capabilities` field, every report
      rides.
    - From then on the Server's declaration governs.
    - A later `capabilities` of zero is the Baseline's "MAY be omitted", not a retraction. The last
      non-zero declaration stands.

13. **A received offer outranks the bitmask for the report that answers it.** A received
    `connection_settings` latches the connection-settings status on for the life of the Agent. This
    holds whatever `OffersConnectionSettings` says, and even when this Client cannot act on the
    offer. The alternative is a Server that offers, never learns, and therefore never stops.

14. **The Client gates a report where the Server's bit governs the report itself.**
    - It withholds effective configuration from a Server that declared capabilities without
      `AcceptsEffectiveConfig`.
    - It withholds `package_statuses` from one without `AcceptsPackagesStatus`.
    - It withholds `connection_settings_status` from one without `OffersConnectionSettings`,
      subject to clause 13.
    - It does not queue a withheld report. The dirty flag clears as usual, and the state returns in
      the full snapshot after any reconnect or `ReportFullState`. A bit declared late therefore
      never receives a stale status.

15. **The Client gates sending pessimistically only where the message would be an error.** The
    Client withholds a CSR until the Server has declared `AcceptsConnectionSettingsRequest`. A
    Server that does not sign answers a CSR `BadRequest`. This is the one exception to clause 12.
    Its test is narrow: the Server would reject the message, not merely leave it unused.

16. **The Client never gates `remote_config_status`.**
    - `OffersRemoteConfig` says what the Server sends.
    - What licenses an inbound status is `AcceptsStatus`, which every Server MUST set. There is no
      `AcceptsRemoteConfigStatus`.
    - The hash is the only input to the Server's re-offer decision. Gating it would put the fleet in
      a re-offer loop the moment a Server stopped declaring the bit.
    - Against a Server that never offers a configuration the status is already absent, so the gate
      would do nothing where it was meant to help.
    - The reasons stand here, at the code, and in [`CONFORMANCE.md`](../CONFORMANCE.md).

17. **The Client does not exercise an undeclared capability either.** A new capability must be
    gated under clause 14 or 15. A decision not to gate one must be recorded in `CONFORMANCE.md`
    with its reason, as clause 16 is.

**Out of scope:**

- How the Agent plane admits, and how it treats a Client of an older version that still sends
  `Authorization`
  ([SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md)).
- Plaintext that an operator writes into `supervisor.toml` themselves. It stays allowed on a
  loopback IP literal
  ([SEC-tls13-bounded-input-chosen-planes](SEC-tls13-bounded-input-chosen-planes.md)).
- The audit entries an offer leaves.
- `AcceptsOtherConnectionSettings`.
- What own telemetry is, which destinations an offer sets, and how they and their headers fold
  ([CLT-what-an-agent-reports](CLT-what-an-agent-reports.md)).
- `connection_settings_status` in the fleet view and the REST API.
- An audit of the Server's own use of Agent capabilities under the same rule.

## Alternatives considered

- **An optional offered credential.** The Agent plane reads none
  ([SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md)). An offered
  credential would be a secret the Server keeps and sends and the fleet persists, for no check
  anywhere.
- **Honouring offered `headers` other than `Authorization`.** No header has a reader on this
  Server. A generic pass-through would let a Server, or whoever can make one send an offer, attach
  any value to every upstream connection of the fleet and keep it there across restarts.
- **Dropping offered `headers` silently and reporting `APPLIED`.** A false report.
  `CONFORMANCE.md` records gaps, not false reports, and a Server rotating a token through headers
  would believe it in force.
- **Refusing an offer wholesale when it carries `headers`.** A Server that offers a credential, of
  an older version or a third party, would freeze the endpoint, heartbeat and certificate the same
  offer carries.
- **Accepting any scheme the Server offers.** The Client would follow a Server, or whoever can make
  one send an offer, onto a plaintext endpoint off the host and hand it the fleet's reports in the
  clear. Q-1 forbids that whatever the configuration, so both ends refuse it.
- **Allowing an offered `ws://` or `http://` endpoint on a loopback IP literal.** It stays within
  the wording of Q-1, but no TLS proves that the listener is the Server, and any local account can
  open the port. One offer would hand the Client to that process, and the persisted endpoint would
  keep it there. Rejected for clauses 1, 5 and 9.
- **Allowing a move to plaintext only from plaintext.** A Client already on a loopback `ws://`
  endpoint could still be moved to another loopback port. That keeps a development convenience,
  but the Client is still moved to whoever listens on the new port. Development needs no offer to
  change its endpoint, so this buys little. Rejected.
- **Requiring `[tls] ca_file` for a move to plaintext too.** A plaintext connection has no
  certificate to check against the CA, so the condition would decide nothing. Rejected.
- **Keeping the move to plaintext and warning.** A warning in the Client's log does not stop a
  takeover that has already happened. Rejected.
- **Leaving a persisted plaintext endpoint in force.** A host whose file holds one would keep
  connecting to whoever listens there. Falling back to `supervisor.toml` reaches it at the next
  start. Rejected.
- **Offering blind, without `ReportsConnectionSettingsStatus`.** The Server could never stop
  re-offering, and a rejection would be invisible.
- **Hot reload of the standing offer instead of a Server restart.** The Server has no hot reload; a
  restart is how a change to `server.toml` takes effect.
- **Every offer always carries an `opamp` block.** The Server would synthesise a block it has
  nothing to put in, and every telemetry change would verify and reconnect the whole fleet. It
  contradicts the Baseline's three classes and its first-reply SHOULD.
- **Requiring `[connection_offer]` beside `[telemetry_offer]`.** An operator would have to
  configure an endpoint or heartbeat offer to get metrics, a coupling with no reason.
- **A hash and an acknowledgement per class.** The Baseline defines one hash over all settings and
  one status field. Splitting them would invent protocol semantics.
- **Verifying a telemetry destination by connecting, for symmetry.** A receiver that is down for a
  moment would turn a correct offer `FAILED`, and the Server would re-offer settings that were
  never wrong.
- **Refusing an offer wholesale when it carries `tls` or `proxy`.** An unsupported extra would
  freeze the endpoint move and the certificate the same offer carries.
- **Honouring `TLSConnectionSettings`.** Most of what it can say weakens verification.
- **Keeping `APPLIED` for a partly honoured offer and documenting it.** `CONFORMANCE.md` records
  gaps, not false reports; a declared capability is a promise a peer relies on.
- **Gating every report on its corresponding bit, no exceptions.** It deadlocks the
  acknowledgement against Servers that offer without declaring, and breaks `remote_config_status`.
- **Gating nothing, relying on the Server to ignore what it does not want.** Defensible on the
  wire, but the Baseline states a MUST, and third-party Servers are a target.
- **A mechanical table from bit to field.** The mapping is not one-to-one in either direction, so
  the table would carry exceptions. It is worth revisiting if the gated fields multiply.

## Sources / Prior art

- Connection settings in the Baseline:
  [OpAMP specification — Connection Settings
  Management](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md#connection-settings-management)
  (Baseline `v0.20.0`): the three classes and their sequences, the verification MUST under
  `ConnectionSettingsOffers.opamp` (the Agent verifies offered settings before it uses them), the
  own-telemetry sequence and its first-reply SHOULD, the hash gate MUST, the heartbeat obligations,
  and `OpAMPConnectionSettings.headers` as the field *"typically used to set access tokens"*;
  *Interoperability of Partial Implementations* for the symmetrical capability MUST;
  `ServerToAgent.capabilities` (*"MAY be omitted in subsequent ServerToAgent messages"*); and the
  `ServerCapabilities` comments that tell `OffersRemoteConfig` from `AcceptsStatus`.
- The oracle's client:
  [`opamp-go` client callbacks](https://pkg.go.dev/github.com/open-telemetry/opamp-go/client/types):
  `OnOpampConnectionSettings` hands the offer to the Agent, which accepts after its own
  verification; the library then reconnects.
- Persisted settings elsewhere:
  [OpAMP Supervisor
  specification](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/specification/README.md):
  persists Server-offered connection settings across restarts.
- A terse peer:
  [`opampextension`](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/extension/opampextension):
  declares a small subset of capabilities and none of the connection-settings ones. A peer's
  declaration therefore cannot be assumed generous, and this behaviour was read from the
  specification rather than checked against that peer.
- The code: `verify` in `crates/fleet-agent/src/connection.rs` applies the `ca_file` condition to
  a move over TLS, `check_url` in `crates/opamp/src/endpoint.rs` admits `ws://` and `http://` on a
  loopback IP literal, and `apply` in `crates/fleet-agent/src/connection.rs` lets a persisted
  endpoint outrank `supervisor.toml`.
- [`HARDENING.md`](../HARDENING.md), *What already holds*: an offered move to another TLS endpoint
  needs the Client's own CA file.

## Consequences

- Positive: the Server can retune every Agent's heartbeat and polling cadence and move the fleet
  to a new endpoint without touching a host, and an issued client certificate reaches its Agent
  the same way. No secret sits in `[connection_offer]`, in a file beside it, or in a Client's state
  directory.
- Positive: a single offer cannot hand a Client over to a local process. A host whose persisted
  endpoint is plaintext and differs from that of `supervisor.toml` returns to its configured
  endpoint at its next start.
- Positive: a Server with only `[telemetry_offer]` reaches its Agents, and a move of a telemetry
  endpoint never disconnects the fleet.
- Positive: a conforming but terse Server is safe to talk to. The next capability has a stated
  rule and worked examples of each branch.
- Negative / trade-offs: two lifecycles where "every offer is proved by connecting" would be one.
  The seam is the Baseline's own and is named in one predicate.
- Negative / trade-offs: a development setup cannot move a Client between loopback ports by offer;
  it edits `supervisor.toml`. A `server.toml` whose `[connection_offer] endpoint` is `ws://` or
  `http://` does not start.
- Negative / trade-offs: a refused telemetry destination fails an offer whose OpAMP half applied
  fine. The `error_message` says what was dropped, but a Server reading only the enum sees a
  failure. Accepted as the price of one hash.
- Negative / trade-offs: a Server that offers a credential in `headers`, of an older version or a
  third party, receives `FAILED` for every such offer, though the endpoint, heartbeat and
  certificate it carried are in force. The `error_message` names the headers, and the echoed hash
  stops the re-offer.
- Negative / trade-offs: the persisted settings are a file in the state directory that an operator
  must know about. Several surfaces are of Development maturity upstream.
- Negative / trade-offs: by clause 13, a Server that sent one offer receives connection-settings
  status for that Agent's life. A withheld report is lost until the next full snapshot. Clause 16
  departs from a literal field-by-field reading, and every review has to be argued out of it.
- Follow-ups: `connection_settings_status` in the fleet view and the REST API, so a stalled
  endpoint move is visible; the Server's use of Agent capabilities under the same rule.

## Enforcement

- [`crates/fleet-server/src/config.rs`](../../crates/fleet-server/src/config.rs):
  `a_connection_offer_needs_at_least_one_field`,
  `a_connection_offer_rejects_a_bad_endpoint_scheme`,
  `a_connection_offer_refuses_a_plaintext_endpoint_off_loopback_naming_the_setting`,
  `a_connection_offer_refuses_a_plaintext_endpoint_on_localhost` (clause 1).
- The offer on the Server:
  [`crates/fleet-server/tests/connection_settings.rs`](../../crates/fleet-server/tests/connection_settings.rs):
  `no_offer_without_the_capability_or_without_a_configured_section`,
  `the_reported_hash_gates_reoffering` (clause 2).
- [`crates/fleet-server/tests/own_telemetry.rs`](../../crates/fleet-server/tests/own_telemetry.rs):
  `a_telemetry_only_server_declares_that_it_offers_connection_settings` (clauses 2, 3).
- [`crates/fleet-agent/src/connection.rs`](../../crates/fleet-agent/src/connection.rs):
  `verify_refuses_a_plaintext_endpoint_off_loopback_without_connecting`,
  `verify_refuses_a_plaintext_endpoint_on_a_host_name`,
  `verify_presents_the_client_certificate_in_force`, `an_offered_move_needs_the_clients_own_ca`
  (clause 5); `offered_tls_and_proxy_are_neither_stored_nor_claimed` (clause 8);
  `merge_leaves_opamp_absent_when_neither_side_has_one`,
  `merge_of_a_telemetry_only_offer_carries_the_opamp_settings_in_force_forward`,
  `merge_keeps_unchanged_fields_from_the_previous_settings`, `load_store_round_trips`,
  `stored_settings_and_their_directory_are_owner_only`,
  `apply_overrides_client_toml_where_the_server_spoke`,
  `apply_leaves_untouched_what_the_offer_omits` (clauses 9, 10). The fold and apply tests carry
  endpoint, heartbeat and certificate in their fixtures.
- The offer end to end:
  [`crates/fleet-agent/tests/connection_settings_e2e.rs`](../../crates/fleet-agent/tests/connection_settings_e2e.rs):
  `an_offer_is_verified_persisted_and_reported_applied`;
  [`crates/fleet-agent/tests/gateway_and_supervisor_e2e.rs`](../../crates/fleet-agent/tests/gateway_and_supervisor_e2e.rs):
  `a_verified_offer_restarts_the_gateway_and_leaves_the_supervisors_running` (clauses 5, 11).
- [`crates/fleet-agent/src/supervisor/agent.rs`](../../crates/fleet-agent/src/supervisor/agent.rs):
  `an_offer_carries_settings_when_it_names_anything_this_client_applies` (clause 4);
  `a_connection_offer_is_acknowledged_applying_and_handed_to_the_transport`,
  `a_failed_offer_still_reports_the_hash_so_the_server_stops_reoffering`,
  `an_offer_is_applied_and_acknowledged` (clauses 5, 7, 8);
  `package_statuses_ride_until_the_server_has_spoken`,
  `package_statuses_stop_once_the_server_says_it_accepts_none`,
  `effective_config_respects_the_servers_capability_set` (clauses 12, 14);
  `a_connection_settings_status_is_reported_to_a_server_that_offered_without_declaring_the_bit`,
  `a_restored_connection_settings_status_is_withheld_from_a_server_that_never_offers` (clauses 13,
  14); `a_remote_config_status_rides_to_a_server_that_offers_no_remote_config` (clause 16).
- [`crates/fleet-server/tests/mutual_tls.rs`](../../crates/fleet-server/tests/mutual_tls.rs):
  `a_csr_to_a_server_that_signs_nothing_is_a_bad_request`, the error clause 15 keeps a Client from
  provoking.
- [`crates/fleet-server/src/fleet.rs`](../../crates/fleet-server/src/fleet.rs):
  `an_offered_heartbeat_interval_sets_the_budget` (clause 10).
- TLS 1.3 is the provider's: `the_provider_offers_tls_1_3_suites_alone` in
  [`crates/opamp/src/tls.rs`](../../crates/opamp/src/tls.rs) (clause 5).

The refusals of a credential, of offered headers and of plaintext on the loopback:

- [`crates/fleet-server/src/config.rs`](../../crates/fleet-server/src/config.rs):
  `a_connection_offer_refuses_a_credential_key_naming_it`, for each of `bearer_token_file`,
  `username`, `password_file`, `bearer_token` and `password` (clause 1);
  `a_connection_offer_refuses_a_plaintext_endpoint_on_a_loopback_ip_literal`: the endpoint
  `ws://127.0.0.1:4320/v1/opamp` in `[connection_offer]` fails startup with a message naming the
  key (clause 1).
- The offer's content:
  [`crates/fleet-server/tests/connection_settings.rs`](../../crates/fleet-server/tests/connection_settings.rs):
  `the_offer_reaches_a_capable_agent_and_carries_no_headers` (clauses 1, 2, 3).
- [`crates/fleet-agent/src/connection.rs`](../../crates/fleet-agent/src/connection.rs):
  - `offered_headers_are_neither_stored_nor_claimed`, which also checks that the `error_message`
    names the header keys and no value (clause 8);
  - `verify_sends_no_authorization_even_when_one_is_offered` (clauses 5, 8);
  - `a_persisted_authorization_header_is_dropped_on_load` (clause 9);
  - `verify_refuses_a_move_to_plaintext_on_the_loopback`: a move to `ws://` or `http://` on
    `127.0.0.1` is refused with an error naming the endpoint, and the endpoint is never dialled
    (clause 5);
  - `apply_ignores_a_persisted_plaintext_endpoint_the_file_does_not_name`: a persisted `ws://`
    loopback endpoint that differs from the endpoint of `supervisor.toml` is not applied at load,
    one the file names is, and a persisted `wss://` endpoint still is (clause 9).

**Not mechanically decidable:** the `OfferOutcome::Applied` path in
[`transport/mod.rs`](../../crates/fleet-agent/src/transport/mod.rs) and review hold clause 6's "no
reconnect" for a telemetry-only offer. Clause 17 governs capabilities not yet added, which only the
review of each new gate can hold.
