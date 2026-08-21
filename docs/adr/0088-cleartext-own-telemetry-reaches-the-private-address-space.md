# ADR-0088: Cleartext own telemetry reaches the private address space — by address, never by name

- **Status:** 🟢 accepted
- **Date:** 2026-08-20
- **Deciders:** Markus Brigl

Supersedes point **8** of [ADR-0036](0036-agents-report-their-own-telemetry.md). Everything else
ADR-0036 decides stands, and so does the shape of the refusal itself: a destination this Client will
not send to is still *reported*, never warned about and never downgraded — which is
[ADR-0086](0086-a-telemetry-destination-is-an-offer-of-its-own-class.md) point 2's admission test,
left exactly as it is. Only the line between admitted and refused moves.

## Context

ADR-0036 point 8 reads *"`https://` or loopback, nothing else"*, and gives the reason: the OTLP
Resource carries the Agent's identifying attributes and the log records carry whatever the Client
logs, so the Baseline's *"MAY refuse to send the telemetry if the URL begins with `http://`"* is
taken — *"one step firmer"* than the credential warning of
[ADR-0013](0013-opamp-endpoint-authentication.md), because this is a continuous stream rather than a
single request.

The reasoning is sound and this ADR does not touch it. What is wrong is the line it was drawn at.

**Loopback was chosen because it was the only line with a definition.** It is not a judgement about
networks — it is the statement *nothing leaves the machine*, which needs no assumption about the
deployment to be true. The shape it was written against is the one this repository ships:
[`.devcontainer/`](../../.devcontainer/) publishes the Collector's OTLP/HTTP port to
the host precisely so `http://localhost:4318` means the same thing inside the workspace container as
outside it, and `config/server.toml`'s first worked example says so in as many words.

**What it makes impossible is the ordinary small-fleet shape.** One Collector on a host of its own,
Agents on the same LAN, one hop between them: the configuration a fleet reaches the moment it stops
being one machine, and long before it becomes something with an ingress and a certificate lifecycle.
Under point 8 an operator in that position has exactly two options — put a Collector on every host,
or terminate TLS in front of the one they have. The second means a certificate, a name to put it on,
and a renewal, all so that a stream can cross a network segment the operator already owns and where
nothing on the path is anyone else's.

**The risk point 8 names has an address range attached to it.** *"Someone between the Agent and the
receiver can read the identifying attributes and the logs"* is a statement about who is on the wire.
Loopback answers it with *nobody*; the public internet answers it with *anyone*. The private
address space — [RFC 1918](https://www.rfc-editor.org/rfc/rfc1918)'s `10/8`, `172.16/12` and
`192.168/16`, and [RFC 4193](https://www.rfc-editor.org/rfc/rfc4193)'s `fc00::/7` — answers it with
*whoever the operator has put on their own network*, and those ranges are not routable across the
public internet, so the answer does not quietly change when a route does. That is a weaker guarantee
than loopback's and a categorically stronger one than a public address's, and it is the same line
this project already draws elsewhere: `crates/server/src/api.rs` deliberately does *not* block the
RFC 1918 ranges when it validates a listener.

**A second thing surfaced while reading the check.** It compared the host against four literals, and
one of them could never match: a URL brackets an IPv6 literal, so `http://[::1]:4318/v1/logs` split
on `:` yields `[`, and the loopback address point 8 explicitly admits was refused as cleartext. The
predicate has to parse an address rather than compare strings, which is what makes range membership
answerable at all.

What has to be settled is whether the admission test asks *does this leave the machine* or *does
this leave the operator's network* — and, once it is the second, how the answer is established.

## Decision

We will admit a cleartext `http://` telemetry destination **inside the private address space**, and
refuse it everywhere else.

1. **The admitted set is loopback plus the private ranges.** `127.0.0.0/8` and `::1`; RFC 1918's
   `10.0.0.0/8`, `172.16.0.0/12` and `192.168.0.0/16`; RFC 4193's unique-local `fc00::/7`. An
   `https://` destination is unaffected and always was — this rule governs cleartext only.

2. **The judgement is made on an address, never on a name.** `localhost` stays admitted, because it
   *is* loopback by definition ([RFC 6761](https://www.rfc-editor.org/rfc/rfc6761)) rather than by
   resolution. No other name is resolved to decide this. An admission test whose answer a re-resolve
   can flip is not one an operator can reason about, and DNS is the part of the path an attacker
   would move; a Collector reached by name over cleartext is therefore refused, and the answer is to
   name it by address or to put TLS in front of it.

3. **Membership is decided by parsing, not by prefix.** `192.168.0.1.example.com` is a host name that
   begins with a private address and is a public destination; `172.32.0.5` is one character from
   `172.16.0.0/12` and outside it. Both are refused because the host is parsed as an IP address
   first and tested for range membership second. This is also what fixes the bracketed IPv6 literal
   that point 8 admitted and the old check refused.

4. **Ranges that are private but not the operator's are not admitted.** Link-local
   (`169.254.0.0/16`, `fe80::/10`) is autoconfiguration rather than a network anyone deployed, and on
   a cloud host `169.254.169.254` is the instance metadata service — not a place to stream logs at.
   Carrier-grade NAT (`100.64.0.0/10`) is shared with other subscribers of the same provider, which
   is the *someone else's wire* this refusal exists for. Neither is admitted, and neither is a
   judgement call left to the code: they are named here so that a later reading of "private" does not
   quietly widen.

5. **The refusal keeps its voice.** A destination outside the admitted set is refused and reported
   back with the reason, exactly as ADR-0036 point 8 has it — not warned about, not downgraded, not
   dropped in silence. The message now names what would be accepted, because *"use https://"* is
   unhelpful advice to an operator whose Collector is one hop away.

6. **This is the Agent's rule and it stays there.** The Server validates that a `[telemetry_offer]`
   endpoint is a full OTLP/HTTP URL with a path and nothing further; whether a destination is
   reachable in cleartext is decided where the packets originate. A Server may therefore offer a
   private address to a fleet, and each Agent answers for itself.

## Alternatives considered

- **Leave point 8 as it is.** The status quo, and it needs no decision at all. Rejected because the
  cost lands on the deployment this project is actually for: a fleet of some tens of hosts with one
  Collector among them, told to obtain and renew a certificate for a stream that never leaves its own
  network. A rule at that price is one operators work around — by putting the Collector on every
  host, or by not reporting own telemetry at all — and neither workaround is better for the thing
  point 8 protects.

- **Admit `192.168.0.0/16` alone**, which is the range the request that prompted this ADR named.
  Rejected: `10.0.0.0/8` and `172.16.0.0/12` are the same class of network under the same RFC, and
  which of the three a site uses is an addressing-plan accident. A rule that admits one and refuses
  the others is one an operator cannot predict, and the first person to hit it would read it as a
  bug rather than a decision.

- **Make the admitted set configurable on the Client** — a list of trusted networks in
  `supervisor.toml`. Rejected on ADR-0036 point 7: nothing about own telemetry is configured on the
  Client, because the whole content of the capability is *"report to the destination you name"*.
  Splitting the decision across a Server-named destination and a per-host allowlist would give a
  fleet two places to look when telemetry does not arrive, and the second one is on the host nobody
  is logged into.

- **Resolve host names and admit those that resolve into the private space.** The friendly option:
  an operator's `collector.lan` would just work. Rejected because it makes the admission test depend
  on what DNS answered at the instant the offer arrived. The offer is persisted and re-applied across
  restarts, the name can be re-pointed afterwards without any offer changing, and the check that
  said yes would never be consulted again. A test that cannot be re-run to the same answer is not a
  security boundary.

- **Drop the refusal and emit a warning instead**, as ADR-0013 does for credentials. Rejected for
  the reason point 8 gave when it chose otherwise, which this ADR agrees with: a warning is
  proportionate to a credential on one request and not to a continuous stream of identifying
  attributes and log records. Moving the line is not the same as removing it.

## Sources / Prior art

- **The Baseline `v0.20.0`, `TelemetryConnectionSettings`** — *"The Agent MAY refuse to send the
  telemetry if the URL begins with `http://`"*. A MAY, which is what leaves this project free to
  decide where the line falls; the decision is this project's, not the protocol's.
- **[RFC 1918](https://www.rfc-editor.org/rfc/rfc1918)** (private IPv4 address space) and
  **[RFC 4193](https://www.rfc-editor.org/rfc/rfc4193)** (unique-local IPv6) — the definitions point 1
  takes verbatim, and the reason the set is exactly three IPv4 ranges and one IPv6 range.
- **[RFC 6761](https://www.rfc-editor.org/rfc/rfc6761) §6.3** — `localhost` resolves to loopback by
  specification, which is what lets point 2 keep it as the single admitted name without resolving it.
- **[RFC 3927](https://www.rfc-editor.org/rfc/rfc3927)** (IPv4 link-local) and
  **[RFC 6598](https://www.rfc-editor.org/rfc/rfc6598)** (carrier-grade NAT) — the two ranges point 4
  names and excludes.
- **Rust's standard library** — `Ipv4Addr::is_private` is the RFC 1918 trio exactly, and
  `Ipv4Addr::is_loopback` is `127.0.0.0/8`; `Ipv6Addr::is_unique_local` is still unstable, so
  `fc00::/7` is spelled out at the code with a note saying why.
- **This project's own `crates/server/src/api.rs`** — `is_internal`, which decides where a
  client-supplied artifact URL may steer the Server. It blocks link-local (*"where `169.254.169.254`
  lives"*) and the CGNAT range, and deliberately does *not* block loopback or the RFC 1918 /
  unique-local ranges, *"an operator's mirror legitimately lives on an internal network"*. Points 1
  and 4 draw the same line for the same reasons, arrived at independently and reconciled here on
  purpose: two admission tests in one codebase disagreeing about what "private" means is how a gap
  gets found by somebody else.
- **ADR-0036 point 8**, whose reasoning this ADR keeps and whose boundary it moves, and **ADR-0086
  point 2**, which made that check the telemetry class's admission test.

## Consequences

- **Positive: the shape between "one machine" and "an ingress with a certificate" is now
  configurable.** A Collector on a LAN address is offered to the fleet as a plain `http://` URL, and
  the Agents report to it. That is the configuration most fleets of this size are actually in.

- **Positive: the bracketed IPv6 loopback literal is admitted at last.** `http://[::1]:4318/v1/logs`
  was refused by the old check although point 8 named it as allowed. Nobody had reported it, which
  says something about how much IPv6 loopback gets used, but it was wrong.

- **Positive: the development stack is untouched.** Loopback is still loopback, the published ports
  in `.devcontainer/` still exist for the reason they always did, and no existing
  configuration changes meaning.

- **Negative: the test is on the address's *class*, not on reachability or on which network the Agent
  is actually on.** `http://192.168.10.5:4318` is admitted by every Agent in the fleet, including one
  on a different site where that address belongs to a different machine — and that Agent will send
  its logs there, in cleartext, to whatever answers. Loopback had no such ambiguity: it was always
  the same host. The mitigation is that the destination is one fleet-wide operator decision rather
  than something an Agent discovers, but the ambiguity is real and this is where it is admitted.

- **Negative: a private network is not a trusted network, and this rule assumes it is.** A flat
  office LAN with a guest VLAN bridged onto it satisfies every clause here. What is being decided is
  that the operator's network is the operator's problem, which is a reasonable division of
  responsibility and is nevertheless weaker than what point 8 guaranteed.

- **Negative: cleartext by name stays refused, and names are what operators use.** An operator whose
  Collector is `collector.lan` on DHCP has to pin an address or terminate TLS. Point 2 accepts that
  friction deliberately, but it is friction, and it will be reported as a bug at least once.

- **Follow-ups:** whether the Server should refuse to *compile* a `[telemetry_offer]` it can tell no
  Agent will accept — a public `http://` address, today accepted by the Server and refused by every
  Agent it reaches — is a separate decision about where configuration errors are caught, and is not
  taken here.
