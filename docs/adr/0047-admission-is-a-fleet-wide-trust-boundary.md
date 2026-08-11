# ADR-0047: Admission is a fleet-wide trust boundary — `instance_uid` is self-asserted, and there is no authorization between Agents

- **Status:** 🟢 accepted
- **Date:** 2026-08-11
- **Deciders:** Markus Brigl

## Context

A security review of the Server raised: **within an admitted fleet, `instance_uid` is self-asserted
and not authorized between Agents.** Any peer that passes admission — the [ADR-0013](0013-opamp-endpoint-authentication.md)
credential and/or the [ADR-0035](0035-mutual-tls-and-the-server-issued-client-certificate.md) client
certificate — may send an `AgentToServer` under *any* `instance_uid`. The Server's `process`
(`crates/server/src/fleet.rs`) takes the reported `instance_uid` at face value and updates that
record's `sequence_num`, `health`, `effective_config`, and `remote_config_status`. Because
`remote_config_status` is the hash that gates whether a Configuration is (re-)offered, a peer polling
as a victim's `instance_uid` can suppress or force config delivery to that Agent, or poison the fleet
view. Over plain HTTP the Server is explicitly unable to tell two pollers apart (the code says so);
the WebSocket path rekeys a *duplicate* `instance_uid` seen on a second connection, but that is
collision handling, not authorization.

The forces that make this hard to "fix" are already settled by accepted ADRs, not open questions:

- **The Baseline makes `instance_uid` self-asserted.** An Agent chooses its own UID (UUID v7
  recommended), and the Server may *assign* a new one via `AgentIdentification`. Identity is a
  routing key, not a secret ([ADR-0004](0004-protocol-baseline-and-conformance-tracking.md)).
- **A certificate cannot serve as per-Agent identity.** [ADR-0035](0035-mutual-tls-and-the-server-issued-client-certificate.md)
  decided this directly: the Server re-keys an Agent at any time with `AgentIdentification`, which
  would invalidate a certificate whose validity depended on its subject matching the current
  `instance_uid`; and a Client in Gateway Mode terminates TLS, so the certificate the Server sees
  belongs to the Gateway, not to the Agents behind it. `ca.rs` therefore signs the CSR subject
  verbatim and binds it to nothing.
- **A Gateway folds many Agents onto few connections.** [ADR-0037](0037-gateway-mode.md) has one
  Gateway certificate carry the reports of every downstream Agent, so "one connection, one Agent
  identity" is false by design.
- **Admission is fleet-wide.** The [ADR-0013](0013-opamp-endpoint-authentication.md) credential
  set and the [ADR-0035](0035-mutual-tls-and-the-server-issued-client-certificate.md) client CA both
  prove *fleet membership*. There is no per-Agent secret anywhere in the model to authorize against.

So the property is not a defect layered on top of the design — it is the direct consequence of the
identity model the accepted ADRs chose. What is missing is that this trust boundary is implicit: it
lives in scattered code comments, not in a decision an operator can read.

## Decision

We will record that **admission is a fleet-wide trust boundary**, and that **within it there is no
authorization between Agents**: a report's `instance_uid` is self-asserted, and any admitted peer may
report under any `instance_uid`. We will make this explicit in operator-facing documentation
([`SECURITY.md`](../../SECURITY.md)) and in the Server's identity/admission module docs, and we will
**not** add per-Agent authorization by default, because the only mechanism that could provide it —
binding a certificate to `instance_uid` — was already rejected by ADR-0035 and is incompatible with
Gateway Mode (ADR-0037) and with Server-initiated re-keying.

Operators who require isolation between mutually distrusting Agents must not place them in one
admitted fleet: separate them by Server instance or network segment. This is the same shape as the
OpAMP specification's own guidance that the transport, not the message, is the trust boundary.

## Alternatives considered

- **Bind the certificate subject to `instance_uid` and reject mismatches (true per-Agent
  authorization).** Rejected — ADR-0035 already rejected certificate-bound identity, and enforcing it
  here would break Server re-keying (`AgentIdentification`), certificate renewal, and Gateway Mode
  (one Gateway certificate legitimately carries many UIDs). It trades a medium-severity in-fleet
  poisoning risk for an outage of the Server's own making.
- **Trust-on-first-use pinning of `instance_uid` ↔ certificate.** Rejected — a legitimate certificate
  renewal (ADR-0035) or an Agent moving between Gateways changes the presented certificate, which
  TOFU would flag as impersonation. It would fire false positives on the system's own mechanisms.
- **Reject a report whose `sequence_num` regresses.** Rejected — an Agent restart legitimately resets
  `sequence_num` and sends a full report (the Baseline's own recovery), which a forger can imitate.
  It authorizes nothing while breaking legitimate restarts.
- **An optional, opt-in strict mode** (a config switch that binds certificate subject → `instance_uid`
  on the mTLS path and refuses mismatches, usable only where Gateway Mode is off and the Server does
  not re-key). Not chosen for this decision, but the least-incompatible way to offer real per-Agent
  isolation to operators who want it; recorded as possible future work rather than decided now,
  because it adds a security-relevant configuration surface and a mutual exclusion with Gateway Mode
  that deserve their own decision.
- **Per-Agent credentials instead of a fleet credential.** Rejected here — a much larger change to
  the identity model and admission story; out of scope of closing this specific gap.

## Sources / Prior art

- OpAMP specification: `instance_uid` is Agent-chosen and Server-assignable (`ServerToAgent.agent_identification`);
  its security model treats the connection, authenticated by the transport, as the trust boundary
  rather than authorizing individual messages.
- This project's own settled decisions on the same forces: ADR-0035 (why a certificate is not bound
  to `instance_uid`), ADR-0037 (a Gateway certificate carries many Agents), ADR-0013 (admission is a
  fleet-wide operator/agent credential boundary), ADR-0004 (Baseline conformance).
- Comparable systems: mutual-TLS fleets (e.g. a shared client CA in service meshes) likewise prove
  membership, not per-caller identity, unless a separate identity document (SPIFFE SVID and the like)
  is layered on — which is the "per-Agent credential" alternative above.

## Consequences

- **Positive:** the trust model becomes an explicit, reviewable decision rather than an implicit
  property. Operators can reason about it and design around it (one fleet = one trust domain; isolate
  by Server or network). The security review finding is closed as *documented and accepted* rather
  than left ambiguous.
- **Negative / trade-offs:** a malicious or compromised admitted peer can poison another Agent's
  Server-side record — most cleanly over plain HTTP, which offers nothing to distinguish pollers. We
  accept this *within* a fleet that shares admission; it is not a cross-fleet or unauthenticated
  exposure.
- **Follow-ups:** an optional strict per-Agent-identity mode (excluding Gateway Mode and re-keying),
  and separately a per-Agent-credential identity model, are each possible future decisions — described
  here by topic, to be given their own ADR if and when pursued. The documentation this ADR decides on
  (SECURITY.md and the Server's admission/identity module docs) is the implementation that follows its
  acceptance.
