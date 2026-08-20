# ADR-0087: A Server's capabilities bind what the Client reports — optimistic until it speaks, and an offer outranks its bitmask

- **Status:** 🟡 proposed
- **Date:** 2026-08-20
- **Deciders:** Markus Brigl

## Context

The Baseline makes capability negotiation symmetrical and mandatory. *Interoperability of Partial
Implementations*: *"After the Server learns about the capabilities of the particular Agent the
Server MUST stop using the capabilities that the Agent does not support. Similarly, after the Agent
learns about the capabilities of the Server the Agent MUST stop using the capabilities that the
Server does not support."*

This project has taken the first half seriously and the second half only in places. The Server gates
what it offers on what each Agent declares — configuration only to `AcceptsRemoteConfig`, telemetry
destinations per signal, restart only to `AcceptsRestartCommand`. The Client caches
`ServerCapabilities` on every reply, but only two of the seven bits change any behaviour:
`AcceptsEffectiveConfig` decides whether effective configuration is reported, and
`AcceptsConnectionSettingsRequest` decides whether a CSR is sent. Everything else is stored and
ignored — `package_statuses` rides to a Server that never declared `AcceptsPackagesStatus`, and
`connection_settings_status` to one that never declared `OffersConnectionSettings`.

Against this project's own Server that costs nothing today, because it declares what it exercises.
The target that makes it matter is [ADR-0040](0040-interoperability-against-opamp-go.md):
`opamp-go` as the behavioural oracle, and third-party Servers generally. A Server is entitled to
implement the two required bits and nothing else, and this Client is obliged to notice.

**The two existing gates already disagree with each other, and neither disagreement is written
down.** `server_accepts_effective_config` starts optimistic — report until told otherwise —
because a Server that has not yet replied has said nothing, and silence is not a denial.
`server_signs_certificates` starts pessimistic — never send a CSR until the bit is seen — because a
CSR to a Server that does not sign them earns a `BadRequest`. Both are right, for different
reasons, and the reasons live only in their doc comments. Adding a third gate means picking a
default a third time, and the next person to add one has nothing to consult.

**A naive reading of the rule deadlocks two live features.** The obvious gate for
`connection_settings_status` is `OffersConnectionSettings`. But a Server can send an offer without
declaring that bit — this project's own does, for a `[telemetry_offer]`-only or `[client_ca]`-only
configuration, and a third-party Server may for reasons of its own. If the Client then withholds the
acknowledgement, the Server's hash gate never closes and it re-offers forever. The rule meant to
improve interoperability would, applied literally, produce an infinite loop out of two conforming
implementations. Something has to outrank the bitmask, and it has to be written down before it is
coded, not discovered in a bug report.

**And one plausible gate would be actively harmful.** `remote_config_status` looks like it belongs
behind `OffersRemoteConfig`. It does not, and the reasons are specific enough that a future reader
will re-derive the wrong answer unless they are recorded. This is the case that makes the decision
worth an ADR rather than three `if`s: what needs deciding is not each gate but the rule that
produces them, including the rule for when *not* to gate.

## Decision

We will make the Client's use of Server capabilities a stated rule rather than a per-field
judgement.

1. **Optimistic until the Server has spoken; binding once it has.** Until a `ServerToAgent` carrying
   a non-zero `capabilities` field has been seen, every report rides. From then on the Server's
   declaration governs. This is the Baseline's own timing — *"After the Agent learns about the
   capabilities of the Server"* — and it is what makes the first status report, which necessarily
   precedes any declaration, legal. A `capabilities` of zero is *"MAY be omitted in subsequent
   ServerToAgent messages"*, not a retraction, so the last non-zero declaration stands.

2. **A received message outranks the bitmask for the report that answers it.** If the Server has
   sent an offer, this Client reports on that offer regardless of the corresponding bit. An
   exercised capability is a stronger statement about what a Server accepts than its bitmask is, and
   the alternative is a loop: a Server that offers and then learns nothing can never stop offering.
   Concretely, a received `connection_settings` latches the connection-settings status on for the
   life of the Agent, even when the offer was one this Client could not act on.

3. **Reporting is gated where the Server's bit governs the report itself.** `package_statuses` is
   withheld from a Server that has declared capabilities without `AcceptsPackagesStatus`;
   `connection_settings_status` from one without `OffersConnectionSettings`, subject to point 2. A
   withheld report is not queued for later — the dirty flag clears as usual, and the report returns
   through the full snapshot that follows any reconnect or `ReportFullState`. Holding the flag
   would deliver a stale status the moment the bit appeared.

4. **Sending is gated pessimistically only where the message would be an error.** A CSR to a Server
   that does not sign certificates is answered with `BadRequest`, so it is withheld until the bit is
   seen — the existing `server_signs_certificates` behaviour, now with a stated reason. This is the
   deliberate exception to point 1, and the test for it is narrow: the message would be *rejected*,
   not merely unused.

5. **`remote_config_status` is never gated, and that is a decision rather than an omission.** Three
   reasons, recorded here and at the code:
   - **The bit does not say what the gate would assume.** `OffersRemoteConfig` is *"The Server can
     offer remote configuration to the Agent"* — a statement about what the Server sends. What
     licenses an inbound status report is `AcceptsStatus`, which every Server MUST set. There is no
     `AcceptsRemoteConfigStatus`, and substituting an outbound-offer bit for one is this project
     inventing a reading the protocol does not have.
   - **Where it would bite, it bites hard.** `last_remote_config_hash` is the only input to the
     Server's re-offer decision and to `in_sync`. A Server that stops declaring the bit — a restart
     with the section removed, an intermediary that rewrites capabilities — would silence the hash
     and put the fleet into a permanent re-offer loop.
   - **Where it would not bite, it does nothing.** The status is unset until a configuration has
     actually been offered, so against a Server that never offers one the field is already absent.
     The gate would be a no-op in exactly the case it was meant to cover.

6. **A capability that is undeclared is not exercised, on this end too.** Point 5 is the exception
   that proves the rule, not a licence: where a Server bit genuinely governs what the Client sends,
   the Client honours it. New capabilities are added under points 3 and 4, and a decision not to
   gate is recorded in `CONFORMANCE.md` with its reason, the way point 5 is.

## Alternatives considered

- **Leave it as three `if`s with doc comments.** Where the code is heading anyway, and it is honest
  about being local. Rejected because the two existing gates already default in opposite directions
  with the reasoning buried in comments, and the third gate cannot be written correctly without the
  rule in point 2 — which is not a coding detail but a choice about which signal wins when a peer
  contradicts itself.

- **Gate everything on the corresponding bit, no exceptions.** The literal reading. Rejected: it
  deadlocks the connection-settings acknowledgement against Servers that offer without declaring,
  and it breaks `remote_config_status` for the reasons in point 5. A rule that produces an infinite
  loop out of two conforming peers is a misreading of a rule whose stated purpose is
  interoperability.

- **Gate nothing and rely on the Server ignoring what it does not want.** Defensible on the wire —
  an unwanted sub-message costs a few bytes and a Server must tolerate it. Rejected because the
  Baseline states a MUST, ADR-0040 makes third-party Servers a target rather than a hypothetical,
  and `CONFORMANCE.md` currently claims this row implemented. The cheapest honest options were to
  implement it or to write it down as a deviation; this project's stated commitment is the former.

- **Derive the gates from the capability enum mechanically — a table from bit to field.** Attractive
  and it would prevent the next omission. Rejected for now because the mapping is not one-to-one in
  either direction: `AcceptsStatus` licenses several fields, `OffersConnectionSettings` governs a
  report about something the *Server* sends, and point 5 is a deliberate non-mapping. A table would
  have to carry exceptions, and a table of exceptions is the `if`s again with more machinery. Worth
  revisiting if the count of gated fields grows.

- **Put this in ADR-0004 or in `CONFORMANCE.md` rather than in an ADR.** Rejected. ADR-0004 decides
  how conformance is *tracked*; this decides behaviour. `CONFORMANCE.md` records what is implemented,
  and a rule that governs how every future capability is added is not a matrix row.

## Sources / Prior art

- **The Baseline `v0.20.0`, *Interoperability of Partial Implementations*** — the symmetrical MUST,
  and its timing (*"After the Agent learns about the capabilities of the Server"*), which is what
  makes point 1's optimistic start conformant rather than a shortcut.
- **The Baseline, `ServerToAgent.capabilities`** — *"This field MUST be set in the first
  ServerToAgent sent by the Server and MAY be omitted in subsequent ServerToAgent messages by setting
  it to UnspecifiedServerCapability value"*, which is why zero is read as silence and not as a
  retraction.
- **The Baseline, `ServerCapabilities`** — the enum comments that distinguish `OffersRemoteConfig`
  ("can offer") from `AcceptsStatus` ("MUST be set, since all Server MUST be able to accept status
  reports"), which is the textual basis for point 5's first reason.
- **This project's two existing gates** — `server_accepts_effective_config` and
  `server_signs_certificates` — which already embody points 1 and 4 and are generalised rather than
  changed by this ADR.
- **ADR-0040** — interoperability against `opamp-go` as the behavioural oracle, which is what turns
  this from housekeeping into a requirement.
- **`opampextension`** — an OpAMP client that declares a small subset and is documented as doing so;
  the mirror image of the problem this ADR addresses, and the reason a peer's declaration cannot be
  assumed generous.

## Consequences

- **Positive: a stated default.** The next capability added has a rule to follow and two worked
  examples of each branch, instead of a coin toss between the two existing gates.

- **Positive: the deadlock is designed out rather than patched.** Point 2 is what makes a
  conforming-but-terse Server safe to talk to, and it is written before the code that needs it.

- **Positive: the `CONFORMANCE.md` row stops overstating.** "Capability negotiation — implemented"
  currently rests on two of seven bits.

- **Negative: point 2 weakens the gate it qualifies.** A Server that sends one offer receives
  connection-settings status for the rest of that Agent's life, even if it later stops declaring the
  bit. That is the intended trade — a loop is worse than an unwanted sub-message — but it means the
  gate is not the clean function of the bitmask that point 3 suggests in isolation.

- **Negative: a withheld status is genuinely lost until the next full snapshot.** Point 3 chooses a
  stale-free report over a prompt one. A Server that declares `AcceptsPackagesStatus` late learns
  the package state at the next reconnect or `ReportFullState` rather than immediately.

- **Negative: point 5 is a documented non-conformance to a literal reading.** A reviewer applying
  the MUST field by field will find `remote_config_status` going to a Server without
  `OffersRemoteConfig` and have to be argued out of it. That is why the reasons are recorded in
  three places — here, at the code, and in the matrix — and it is a cost that recurs at every review.

- **Follow-ups:** whether the Server's own use of Agent capabilities is complete under the same rule
  has not been audited here — this ADR looks at one direction. The mechanical bit-to-field table is
  left open for the point at which the number of gated fields makes the exceptions cheaper than the
  repetition.
