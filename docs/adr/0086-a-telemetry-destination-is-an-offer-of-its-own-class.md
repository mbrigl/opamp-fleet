# ADR-0086: A telemetry destination is an offer of its own class — applied without a connection to prove it

- **Status:** 🟡 proposed
- **Date:** 2026-08-20
- **Deciders:** Markus Brigl

Supersedes points **7** and **11** of [ADR-0036](0036-agents-report-their-own-telemetry.md).
Everything else ADR-0036 decides — the SDK, the signals, the Resource, the `https://`-or-loopback
rule, the unconditional capability declaration, and point 10's treatment of `certificate`, `tls` and
`proxy` — stands unchanged. Narrows [ADR-0014](0014-server-driven-connection-settings.md)'s
verify-by-connecting rule to the class of offer it was written for, without weakening it there.

## Context

ADR-0036 landed own telemetry. Its point 7 says the destinations are *"folded, persisted, and
applied **exactly as the `opamp` settings are**"*, citing ADR-0014; its point 11 says
`[telemetry_offer]` is compiled into *"the same hash-gated `ConnectionSettingsOffers`
`[connection_offer]` already produces"* and that `OffersConnectionSettings` stays *"what it is
today"* — that is, gated on `[connection_offer]`.

ADR-0014's flow, which point 7 points at, is: acknowledge `APPLYING`, **verify by actually
connecting**, persist, **reconnect**, acknowledge `APPLIED`.

**That flow cannot be run for a telemetry destination.** The thing to be proved is an OTLP receiver,
not the OpAMP endpoint; connecting to it proves nothing about the OpAMP connection, and there is no
connection to re-establish afterwards. Point 7 describes an operation that has no meaning for the
object it is applied to.

**The two ends of this project have already diverged along that seam.** In the same commit that
implemented ADR-0036 (`5ff00d1`), the Server grew a path the ADR does not describe: with a
`[telemetry_offer]` and no `[connection_offer]`, it composes an offer whose `opamp` field is absent
and sends it — `fleet.rs`, with the comment *"An Agent that accepts no OpAMP settings may still
report telemetry, so the two are gated separately: with only a telemetry destination to offer, that
is the whole offer."* `crates/server/tests/own_telemetry.rs` asserts exactly that shape. The Client
was written to the ADR: `AgentState::handle` acts on an offer only `if offers.opamp.is_some()`.

The result is a configuration in which the feature is inert and the protocol loops. A Server with
only `[telemetry_offer]` sends the offer; the Client ignores it entirely — no `APPLYING`, no
`connection_settings_status`, no exporters. The Server's hash gate compares a reported
`last_connection_settings_hash` that never arrives, so it re-offers on **every** exchange, forever.
Nothing logs an error on either side: each end is doing what it was built to do.

**The Baseline settles which end is wrong, and it is not the Server.** *Connection Settings
Management* opens by naming *"3 classes of destinations"* — the OpAMP Server, own telemetry, and
"other" — and states plainly: *"Depending on which connection settings are offered **the sequence of
operations is slightly different**."* It then routes each class to its own section. Three details
follow from that structure and none of them is ambiguous:

- The verification MUST is scoped to **one field**. It appears under
  `ConnectionSettingsOffers.opamp` — *"The Client MUST verify the offered connection settings by
  actually connecting before accepting the setting to ensure it does not lose access to the OpAMP
  Server due to invalid settings"* — and the justification is the scope: losing access to the
  *OpAMP Server*. No such requirement appears under `own_metrics`, `own_traces`, or `own_logs`.
- The telemetry sequence in *Own Telemetry Reporting* is: the offer arrives, the Agent exports. No
  verification step, no reconnect. What it does ask for is the acknowledgement: *"If the Agent has
  the ReportsConnectionSettingsStatus capability it SHOULD set the connection_settings_status
  accordingly when new settings are received."*
- The Baseline **expects the standalone offer**: *"The Server SHOULD populate the connection_settings
  field when it sends the first ServerToAgent message to the particular Agent (normally in response
  to the first status report from the Client), unless there is no OTLP backend that can be used."*
  A Server holding only a telemetry destination is asked to send it in its first reply, whether or
  not it has anything to say about the OpAMP connection.

So ADR-0036 point 7 is the deviation, and it entered as a compression: at the time, the only offer
that existed carried `opamp`, and "exactly as the `opamp` settings are" was a true description of
where the code ran rather than a decision about what the class *is*. Point 11 compounded it by
tying the capability bit to `[connection_offer]`, which leaves the Server exercising
`OffersConnectionSettings` without declaring it — the very thing its own `capabilities()` comment
promises never happens.

What has to be settled is not which side has the bug. It is whether this project has **one** class
of connection-settings offer with one lifecycle, or the Baseline's three with sequences of their
own.

## Decision

We will treat a telemetry destination as **an offer of its own class**: applied and acknowledged
without a connection to prove it and without restarting the OpAMP connection. The Baseline's three
classes become three classes here.

1. **An offer is actionable when it carries anything this Client can put in force** — OpAMP settings,
   or any of `own_metrics`, `own_traces`, `own_logs`. Not `opamp` alone. An offer carrying only
   `other_connections` is **not** actionable while `AcceptsOtherConnectionSettings` is undeclared:
   acknowledging what cannot be applied is the lie this whole path exists to prevent, and a
   conforming Server does not send it.

2. **Verification proves what it is able to prove.** The `opamp` half is verified by actually
   connecting, exactly as ADR-0014 requires and for exactly ADR-0014's reason — not losing access to
   the Server. A telemetry destination is not verified by connecting: reachability of an OTLP
   receiver is not this Client's to establish at offer time, and a receiver that is down is not an
   offer that is wrong. What *is* checked before it is put in force is what this Client can decide
   on its own — the `https://`-or-loopback refusal (ADR-0036 point 8) and the unhonoured fields
   (ADR-0036 point 10). Those checks are the telemetry class's admission test, and a failure is
   reported, not swallowed.

3. **One offer, one hash, one acknowledgement.** The Baseline hashes the whole message —
   *"Hash of all settings"* — so the Agent acknowledges the message, never one part of it. A single
   `connection_settings_status` is reported per offer, and its `error_message` names **everything**
   dropped or refused across both halves. If the `opamp` half fails verification, nothing from that
   offer is persisted or applied — the telemetry half included — and the offer reports `FAILED`.
   Half-applying an offer whose other half was rejected would leave the Server unable to tell what
   is running, which is what the single hash exists to prevent.

4. **A telemetry-only offer does not restart the connection.** It is applied in place, the
   acknowledgement rides the reports already owed, and the transport loop carries on. Only a
   verified `opamp` half causes the reconnect ADR-0014 describes.

5. **The Server declares `OffersConnectionSettings` whenever it can offer anything** — a
   `[connection_offer]`, a `[telemetry_offer]`, or a `[client_ca]` whose issued certificate travels
   as an ordinary offer (ADR-0035). This replaces point 11's "stays what it is today". Declaring a
   capability the Server exercises is the Baseline's rule, and the comment on `capabilities()` —
   *"an undeclared capability is never exercised, a declared one never hollow"* — becomes true again
   rather than aspirational.

6. **What is persisted says only what was offered.** The stored `connection-settings.pb` carries an
   `opamp` block only when the offer or the state it folds into had one. A file that claims the
   Server offered OpAMP settings it never offered is a lie in the one artefact an operator is told to
   inspect and delete.

7. **`other_connections` joins this class when it lands.** It is the Baseline's third class and it
   names destinations that are not the OpAMP endpoint, so the same reasoning applies: no
   verification by connecting, no reconnect, one acknowledgement. Implementing
   `AcceptsOtherConnectionSettings` therefore does not reopen this question — which is the point of
   deciding it here rather than case by case.

## Alternatives considered

- **Bring the Server back to ADR-0036 point 11 — always carry an `opamp` block.** The smaller diff,
  and it needs no new class. Rejected on three counts. It makes the Server synthesise a block it has
  nothing to put in, purely so the Client's guard passes. Each such offer then costs a
  verify-and-reconnect of the whole fleet for a change that never touched the OpAMP connection — a
  telemetry endpoint move would disconnect every Agent. And it contradicts the Baseline twice over:
  the *"3 classes"* structure, and the SHOULD that a Server send telemetry settings in its first
  reply *"unless there is no OTLP backend"* — with no mention of needing OpAMP settings to carry them.

- **Leave it: document the gap in `CONFORMANCE.md` and require `[connection_offer]` alongside
  `[telemetry_offer]`.** Honest, and it is what the Deviations table exists for. Rejected because
  the failure is silent on both ends and the workaround is a coupling with no reason behind it — an
  operator would have to configure credential rotation in order to get metrics. `CONFORMANCE.md`
  records deliberate departures; this would be recording an accident.

- **Give each class its own hash and acknowledge them separately.** Tempting: it would let the
  telemetry half apply when the OpAMP half fails. Rejected — the Baseline defines `hash` as *"Hash of
  all settings"* on the offer as a whole, and there is one `connection_settings_status` field to
  answer with. Splitting it would be this project inventing protocol semantics, which
  `SPECIFICATION.md` lists as a non-goal.

- **Verify a telemetry destination by connecting to it too, for symmetry.** Rejected on merit rather
  than on cost. An OTLP receiver that is momentarily down would turn a correct offer into a `FAILED`
  one, and the Server would re-offer settings that were never wrong. The OpAMP rule exists because a
  bad endpoint takes the host out of reach; a bad telemetry endpoint costs telemetry, which is what
  the refusal report is for.

- **Fold this into ADR-0036 as an amendment.** Not available: accepted ADRs are never edited
  (`AGENTS.md` §3.3), and points 7 and 11 are the two being replaced.

## Sources / Prior art

- **The Baseline `v0.20.0`, *Connection Settings Management*** — the *"3 classes of destinations"*
  framing and *"Depending on which connection settings are offered the sequence of operations is
  slightly different"* are the structural claim this ADR follows; the per-class capability gating
  listed there is what point 5 aligns the Server with.
- **The Baseline, `ConnectionSettingsOffers.opamp`** — the verification MUST and its stated
  justification (*"does not lose access to the OpAMP Server"*), which is what scopes it to one field.
- **The Baseline, *Own Telemetry Reporting*** — the sequence with no verification step, the SHOULD
  that the Server offers in its first message *"unless there is no OTLP backend that can be used"*,
  and the SHOULD that the Agent sets `connection_settings_status` *"when new settings are received"*.
- **`opampextension` (opentelemetry-collector-contrib)** — a widely deployed Client that implements
  `ReportsEffectiveConfig`, `ReportsHealth` and `ReportsAvailableComponents` and *not* the
  connection-settings capabilities, which is why interoperability here cannot be checked against it
  and had to be read out of the specification instead.
- **This project's own Server** — `fleet.rs::settings_offer` and
  `crates/server/tests/own_telemetry.rs`, which already implement and assert the standalone shape.
  The Server was right; it simply outran the ADR that authorised it.
- **ADR-0014's verification rule** and its reasoning, which this ADR narrows in scope and leaves
  untouched in force.

## Consequences

- **Positive: the feature works in the configuration it was designed for.** A Server holding only
  `[telemetry_offer]` reaches its Agents. The re-offer loop closes because there is finally an
  acknowledgement to close it with.

- **Positive: an operator gets metrics without configuring credential rotation.** The two settings
  become independent, which is what they always were on the wire.

- **Positive: a telemetry endpoint move no longer disconnects the fleet.** Under the alternative it
  would have — every change carried a synthetic `opamp` block through verify-and-reconnect.

- **Positive: `AcceptsOtherConnectionSettings` is unblocked rather than deferred.** Point 7 answers
  in advance the question that would otherwise have to be reopened when it lands.

- **Negative: two lifecycles now exist where there was one.** "Every offer is proved by connecting"
  was a rule with no exceptions and was easy to hold in the head; it now holds for one field of
  three. The mitigation is that the seam is the Baseline's own and is named in one predicate rather
  than spread through the transports — but it is a second case to reason about, and this ADR is
  where that cost is admitted.

- **Negative: a refused telemetry destination now fails the whole offer.** By point 3, a cleartext
  `own_logs` endpoint makes an offer `FAILED` that also carried a perfectly good credential
  rotation — which did apply, since verification succeeded, but is reported as part of a failed
  offer. The `error_message` names what was dropped, so the Server can tell; a Server that reads
  only the status enum sees a failure it may not deserve. Accepted as the price of one hash.

- **Negative: the Server declaring `OffersConnectionSettings` more often is visible to peers.** A
  Server with only `[client_ca]` now declares a capability whose standing offer is empty until a CSR
  arrives. That is what the bit means — it *can* offer — but it is a wider declaration than before,
  and a peer Client may probe it.

- **Follow-ups:** `AgentSummary` exposes `remote_config_status` but not `connection_settings_status`,
  so a stalled or failed rotation is invisible in the fleet view and in the REST API — which is
  precisely what an operator needs to see once offers can fail for a second reason. Worth its own
  change; not decided here. Whether the telemetry class should also carry the interim `APPLYING`
  state, given there is no lengthy verification to be in the middle of, is left as written — it is
  reported, because the Baseline's status enum has it and a Server may be watching for it.
