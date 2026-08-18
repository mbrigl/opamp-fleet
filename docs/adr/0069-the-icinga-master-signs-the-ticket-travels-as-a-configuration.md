# ADR-0069: The Icinga master stays the CA — the ticket travels as a Configuration, and the Supervisor enrols once

- **Status:** 🟢 accepted
- **Date:** 2026-08-17
- **Deciders:** Markus Brigl

## Context

An Icinga 2 Agent is useless without a certificate signed by its Icinga master: the `ApiListener`
needs one to connect, and the master will not talk to a node it cannot verify. A fleet-delivered
Icinga 2 ([ADR-0068](0068-icinga-2-is-supervised-by-a-kind-of-its-own.md)) therefore has to answer a
question GLPI never posed — **who obtains that certificate, and with what secret.**

The forces are unusually clear:

- **This project already has a CA, and it is the wrong one.** [ADR-0035](0035-mutual-tls-and-the-server-issued-client-certificate.md)
  lets the fleet Server sign client certificates for the OpAMP link. Reusing it for Icinga would make
  the fleet Server an Icinga CA — a second trust root inside somebody else's monitoring topology,
  which nothing in the specification asks for and which no Icinga master would accept anyway.
- **The Icinga flow is ticket-based and non-interactive.** The master computes a ticket as an HMAC of
  the node's common name under its own `TicketSalt`; the node generates a key locally, sends a CSR,
  and receives a signed certificate. The private key never leaves the host.
- **The one-shot bootstrap must not touch `ConfigDir`.** `icinga2 node setup` does the whole dance in
  one command — and writes `zones.conf`, `api.conf` and `constants.conf` into `ConfigDir`, the single
  constant that is not reliably relocatable. The lower-level `pki` subcommands (`new-cert`,
  `save-cert`, `request`, `verify`) take **every path as an argument**; a spike confirmed they touch
  no system directory at all and write the private key `0600` themselves.
- **The Server already has a way to deliver a per-host secret.** [ADR-0016](0016-configuration-content-role.md)'s
  `supplementary` role writes a Configuration entry as a plain file the Supervisor does not pass to
  its process, and [ADR-0012](0012-selector-targeted-configurations-and-openapi-rest-api.md)'s
  Selectors already aim a Configuration at exactly one Agent. A ticket is precisely that: a per-host
  string that must land as a file and go nowhere else.
- **A Configuration apply empties the entry directory.** Every apply deletes the entry files before
  writing the new set, so anything needed *after* the enrolment must be copied out of it.

## Decision

We will keep **the Icinga master as the only CA**, and have the Supervisor enrol **once, on the
host**, with the ticket and the parent's certificate delivered as ordinary Configurations.

Bound by this decision:

- **The fleet Server signs nothing and holds no `TicketSalt`.** It transports two artefacts that are
  not private keys: the **ticket** (an HMAC over the common name, useless for any other node) and the
  **parent's certificate** (public by nature). No private key ever reaches the Server, and the OpAMP
  PKI of ADR-0035 and the Icinga PKI never touch.
- **The ticket is a Configuration named for the Supervisor's use, with `role = "supplementary"` and a
  Selector matching one Agent.** It lands as a file the Supervisor reads and nothing else consumes.
  A block may also name a ticket inline for a hand-written host; the manual discourages it.
- **The parent's certificate is pinned, not trusted on sight.** It arrives the same way, and is
  **copied out of the entry directory** at enrolment, because the next apply would delete it.
  `icinga2 pki save-cert` — trust on first use — remains as a fallback, taken only when no pinned
  certificate was delivered, and logged with the fingerprint it accepted.
- **Enrolment is three `pki` calls, never `node setup`:** `pki new-cert` (key and CSR, explicit
  paths), then `pki request` against the parent with the ticket, the pinned certificate, and the
  target paths. Nothing is written outside the Supervisor's own directory.
- **The certificate on disk is the state; the marker is a hint.** Enrolment runs when there is no
  usable certificate — verified with `pki verify`, not by looking at a flag — or when it is inside
  its renewal window, or when the common name or the parent changed. A marker file records what was
  enrolled, so the ordinary start costs no subprocess.
- **Without a ticket, enrolment is still correct.** The CSR reaches the master's signing queue and
  waits for `icinga2 ca sign` there. The Agent stays unhealthy until it is signed, saying so.
- **An unreachable master is a wait, not a failure.** Health reports what is missing, the attempt
  backs off and repeats, and the daemon is not started — no crash loop, and the Client's own startup
  never blocks on somebody else's master.
- **Renewal is the daemon's own**, with the Supervisor as a start-time safety net only: a certificate
  past its renewal window is re-requested before the daemon starts, never while it runs.
- **Revocation stays on the master.** Removing the Supervisor removes its key material with the
  directory (ADR-0059); the master's `icinga2 ca remove` is the operator's, and the Supervisor says
  so when it is retired rather than pretending it cleaned up.

## Alternatives considered

- **Let the fleet Server issue Icinga certificates** by extending ADR-0035's CA. Rejected: it makes
  the fleet Server a trust root in the monitoring topology, requires the Icinga master to trust it,
  and doubles the blast radius of a Server compromise for a certificate the master could sign itself.
- **Deliver the finished certificate and private key as Configurations.** Simplest to implement and
  the worst outcome: a private key generated centrally, travelling through the Server, stored in its
  Configuration store, and readable to whoever reads the fleet. Rejected outright — the Icinga flow
  exists precisely so the key stays put.
- **`icinga2 node setup`.** One command instead of three, and it writes into `ConfigDir`, which
  ADR-0068's spike showed to be the one path that cannot be relocated. Rejected on that alone; the
  `pki` subcommands do the same work with explicit paths.
- **Trust on first use as the default.** Convenient at scale, and it hands the first attacker in the
  path a permanent foothold. Kept only as a logged fallback for the case where nothing was pinned.
- **A ticket in `client.toml` on every host.** Works, and puts a per-host secret in the file the
  fleet also rewrites (ADR-0056). Rejected as the default: the Selector-targeted Configuration is
  the mechanism this project already has for exactly this.
- **Enrolment in `Plugin::start`** rather than in the adapter task. Rejected: it would block the
  Client's startup on the reachability of an Icinga master.

## Sources / Prior art

- [Icinga 2 — Distributed Monitoring](https://icinga.com/docs/icinga-2/latest/doc/06-distributed-monitoring/):
  the ticket as *"a client ticket … generated on the master"*, CSR auto-signing, and the on-demand
  signing queue.
- [Icinga 2 — CLI commands](https://icinga.com/docs/icinga-2/latest/doc/11-cli-commands/): `pki
  new-cert`, `save-cert`, `request`, `verify`, `ticket`, `ca sign`.
- Spike against Icinga 2.14.6-1 (2026-08-17): `pki new-cert` and `pki verify` with explicit paths
  touch no system directory, write the key `0600`, and require the same `-D RunAsUser=`/`-D
  RunAsGroup=` as every other invocation.
- [ADR-0035](0035-mutual-tls-and-the-server-issued-client-certificate.md) — the other PKI in this
  project, and the one this decision deliberately does not reuse.
- [ADR-0016](0016-configuration-content-role.md) and [ADR-0012](0012-selector-targeted-configurations-and-openapi-rest-api.md)
  — the delivery mechanism for a per-host file that is not the process's configuration.

## Consequences

- Positive: a host enrols itself with no operator on it, and the fleet never becomes a certificate
  authority for a system it does not own.
- Positive: the private key is generated where it is used and never travels; the worst a compromised
  fleet Server leaks is a ticket bound to one common name.
- Negative / trade-offs: the fleet Server must be given per-host Configurations for tickets, which is
  a Configuration per enrolling host until it is signed. Selectors make that mechanical, and the
  ticket may be withdrawn afterwards.
- Negative / trade-offs: a host waiting for on-demand signing looks like an unhealthy Agent for as
  long as nobody signs. Correct, and it belongs in the manual's troubleshooting table so it is not
  reported as a defect.
- Negative / trade-offs: the Supervisor shells out to `icinga2 pki` and depends on its exit codes and
  output. Bounded by a timeout, and the alternative — reimplementing the CSR protocol — is worse.
- Follow-ups: renewal is left to the daemon and only netted at start; if that turns out not to hold
  in practice, an explicit renewal schedule is its own decision.
