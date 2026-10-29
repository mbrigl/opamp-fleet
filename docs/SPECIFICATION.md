# Specification — OpAMP Fleet

> The **specification** of this project: the problem it solves, where it is going, and what has to
> be true for it to have succeeded, written in the vocabulary of [`GLOSSARY.md`](GLOSSARY.md). It
> is the **constitution**: every Architecture Decision Record in [`adr/`](adr/) derives from it and
> must not contradict it ([ADR-0001](adr/0001-agent-governance-model.md)).

## Problem

Operators of a telemetry fleet run agents on many machines, each configured by a file next to it. To
change what a hundred agents do, they reach a hundred machines, by hand or through a
configuration-management tool, and nobody can say with certainty what each agent runs right now, in
configuration or in software. Configuration drifts, rollouts are ad hoc, and a bad configuration
shows as missing telemetry rather than as a report.

[OpAMP](https://opentelemetry.io/docs/specs/opamp/), the Open Agent Management Protocol, closes
this loop: an agent accepts configuration over the protocol and reports back what it applied and
how it is doing. Three gaps keep it from managing a real fleet:

- **No Server to run.** The protocol needs a Server on the other end. The reference
  implementations provide libraries and examples to build one, not a Server an operator
  deploys to manage a fleet.
- **Heterogeneous fleets.** Beside OpenTelemetry Collectors, a fleet runs agents that do not
  speak OpAMP. A tool that sees only OpAMP-native agents manages a slice of the fleet, never all
  of it.
- **Partial implementations.** Implementations cover a convenient subset of the protocol and
  leave the rest undocumented, so an operator cannot tell what a pairing of Server and Client
  will do.

No alternative available today closes all three, which is why the work is worth doing now.
**OpAMP Fleet** closes them: a Server, a Client that manages OpAMP-native and foreign processes
alike behind the same protocol, and a written record of how much of the protocol each end
implements.

## Mission

Give operators one place — reachable by any UI through a REST API — to decide what every agent
in a heterogeneous fleet is running, and to see what each one is actually running, whether or not
the agent itself speaks OpAMP — on an implementation of OpAMP that is as complete as the protocol
allows and demonstrably in step with its upstream specification.

## Vision

An operator changes a configuration in one place and knows, within seconds, which agents took it and
which did not, and why — across OpAMP-native Collectors and foreign, non-OpAMP agents alike. A
change can address the whole fleet or a chosen subset of it, so a configuration can be rolled out to
part of the fleet before all of it. And what the Server distributes is not only configuration but
the agents' software itself: it can update an agent's binary in place, verified before it is applied
and rolled back on failure. Agents that were never built for OpAMP are brought into the same control
loop by purpose-built Custom Supervisor Plugins, so the whole fleet's real state is observable
rather than assumed, and a rejected configuration or a failed update is a reported event, not a
silent outage.

One Client binary covers the shapes a real deployment needs. It **supervises** local processes — and
because every Supervisor also listens on loopback for OpAMP, a Collector carrying its own OpAMP
client reports *to its own Supervisor* rather than being watched from the outside. Independently of
that, the same binary can stand at a network boundary as a **gateway**, folding many clients'
connections onto a small pool of upstream ones; a machine may do both at once. Whichever shape it
takes, the Server sees only Agents distinguished by their identity, never by the connection that
carried them — so a fleet can grow past the point where one connection per agent is affordable.

The Server exposes a stable, OpenAPI-described REST API and carries only a rudimentary user
interface of its own, so any external portal can integrate it easily and render the fleet however it
likes. The Client is one process that can supervise many Managed Processes at once, installs as a
native operating-system service that updates itself in place, and runs on Linux, macOS, and Windows;
growing to a new kind of Managed Process means writing a new Plugin, not changing the core. Both
ends carry a written record of which parts of the protocol they implement, kept in step with the
upstream specification rather than drifting quietly behind it. The project grows from the smallest
thing that closes the loop and widens only as the protocol and its agents actually allow.

## Strategy

- **Implement the protocol in full, and keep it in sync.** The wire contract is the OpAMP
  specification, implemented faithfully and as completely as the protocol allows on both
  ends. The version implemented against is pinned and recorded as the **Protocol Baseline**; which
  capabilities each end implements — and whether upstream considers each one stable, `[Beta]`, or
  `[Development]` — is written down in [`CONFORMANCE.md`](CONFORMANCE.md) and kept current with
  the code. Drift from upstream is detected, not discovered.
- **Own both ends in Rust.** The Server and the Client are one Rust stack, so the whole control
  loop is code this project controls. Where a reference implementation exists — `opamp-go`,
  the OpenTelemetry Collector's `opampextension`, and the `opampsupervisor` — it stays the
  behavioural oracle the Rust code is checked against, rather than being replaced by it.
- **API-first Server, portal-friendly.** The Server's public contract is an **OpenAPI-described
  REST API**, so any portal can generate a client and drive the fleet through it. The Server
  bundles only a rudimentary UI for basic operation; a richer UI lives wherever the operator
  wants it — a standalone app or an existing portal — and is expected to be built outside this
  project.
- **One Client, two modes that compose.** The client side is a single deployable with two **Client
  Modes**: **Supervisor Mode** manages local processes, **Gateway Mode** multiplexes other clients'
  connections upstream. They are independent — a Client may run either, or both at once. Serving
  a Collector that carries its own OpAMP client is not a third mode but an intrinsic part of
  Supervisor Mode: a Supervisor always exposes a **Supervisor Endpoint** on loopback, whether or
  not anything connects to it. One binary covers every shape, so an operator learns and deploys
  one thing.
- **Hexagonal core, plugins at the edge.** The supervision domain sits at the centre; **Ports**
  abstract the Server-facing side and the managed-process side; **Plugins** are the adapters that
  implement concrete process types. A Collector Supervisor and a Custom Supervisor are two plugins
  behind the same ports, and a mode is a composition of ports rather than a fork of the core.
- **Route by `instance_uid`, not by connection.** The protocol states that multiple Agents may
  be multiplexed onto one connection and distinguished by `instance_uid`. Both ends honour that:
  the Server never assumes one connection means one Agent, and the Client maps *n* Agents onto *m*
  connections (*n* ≥ *m* ≥ 1). A connection is transport, not identity.
- **Ship the Client as a self-updating OS service across all platforms; the Server on Linux.** The
  Client installs as a native operating-system service and can replace its own binary in place, and
  it is built for Linux, macOS, and Windows so one client shape manages a heterogeneous fleet. The
  Server targets Linux only.
- **Bring agents the project was never built for into OpAMP.** A Custom Supervisor plugin owns
  a foreign agent — one whose configuration format, lifecycle, and health nothing here already
  knows — and translates all three into OpAMP toward the Server, so heterogeneous agents share
  one control loop and appear in the fleet like any other Agent.
- **Security before convenience.** A fleet manager puts configuration and software onto
  every host it reaches, so a flaw in it is a flaw on all of them at once. Where security and
  convenience conflict, security wins, and the operator meets a refusal that says what to fix,
  not a warning. Every connection that leaves the host is TLS 1.3; plaintext is accepted on the
  loopback alone. An Agent proves fleet membership with a client certificate in the TLS handshake,
  issued to its host, and it obtains that certificate in a separate enrolment that is time-limited
  and approved. This is done with the protocol's own means — client certificates, the CSR flow,
  and the `ConnectionSettings` offers that let the Server renew a Client's certificate — never
  through a private side channel. Software is installed only when it is signed with a key the
  operator holds and fetched from the fleet itself — the Server, or a Gateway relaying it. No
  vulnerability can be ruled out, so the project shrinks the chance of one and the damage it can
  do: every input from the network is bounded before it is parsed, the parsers are fuzzed, and the
  dependencies are checked before they are merged.
- **Close the loop before widening it.** A working control loop — configure, apply, report back
  — for one managed process comes first. Targeting a subset of the fleet and updating an agent's
  software are core goals, built on top of that loop once it holds, not before it.
- **Distribute software, not only configuration.** The Server can deliver and apply agent binaries,
  not just their configuration — verified before applying and rolled back on failure — but only
  as far as the managed processes actually support it, never pretending to a capability an agent
  lacks.
- **Configure the whole fleet or a part of it.** A configuration can be directed at the whole fleet
  or at a selected subset, so a change can be rolled out gradually; an Agent outside a target is
  left running what it already runs.
- **One configuration, many hosts.** A Configuration is written once and serves every host it
  reaches: what differs between them is a parameter, filled in from what the Server knows about an
  Agent and its Host, or from what only the host knows. Secrets stay on the host.

## Goals / Success Criteria

1. **G-1** — **The loop closes.** When a configuration changes on the Server, the Server shall
   send it to every connected Agent it is meant for without that Agent asking, and the Agent
   shall report it applied.
2. **G-2** — **The Server knows the fleet's state.** The Server shall report for every connected
   Agent its identity, its health, the configuration it holds, and whether it accepted or
   rejected it, with the error of a rejection.
3. **G-3** — **No redundant reconfiguration.** When an Agent reports the config hash of the
   configuration it should run, the Server shall not send that configuration again.
4. **G-4** — **A rejected configuration is visible.** When an Agent refuses a configuration, the
   system shall surface the reason rather than fail silently.
5. **G-5** — **Any UI can drive the fleet.** The Server shall expose fleet state and
   configuration through an OpenAPI-described REST API as a stable contract, so that an external
   UI or portal reads the fleet and changes what it runs through that API alone. The Server
   bundles only a rudimentary UI of its own.
6. **G-6** — **One Client runs many Supervisors.** When one Client process runs several
   Supervisors, each shall appear to the Server as an independent Agent.
7. **G-7** — **An agent the project was never built for is managed like any other.** When a
   Foreign Agent runs under a Custom Supervisor written for its kind, it shall be configured,
   report health, and report back through the same control loop, and appear in the fleet as a
   Collector does.
8. **G-8** — **A new process type is a new Plugin.** Supporting another kind of Managed Process
   shall take a Plugin written against the existing Ports, without a change to the supervision
   core.
9. **G-9** — **A configuration can target a subset of the fleet.** When a Deployment's Selector
   matches a subset of the fleet, the matching Agents shall apply what it releases and every
   other Agent shall keep running what it runs, so a change rolls out gradually.
10. **G-10** — **The Server updates an agent's software, not only its configuration.** When a
    rollout releases a Package, for a Collector or for the Client itself, the Agent shall verify
    it before applying it, report the outcome, and roll back on failure.
11. **G-11** — **The Client runs and updates itself as an OS service, on every platform.** The
    Client shall install and run as a native service on Linux, macOS, and Windows, and when it
    takes a Package of its own, it shall replace its binary in place, survive the service
    restart, and roll back on failure.
12. **G-12** — **Protocol coverage is on the record.** [`CONFORMANCE.md`](CONFORMANCE.md) shall
    state, for every capability of both ends, whether it is implemented, its upstream maturity,
    and whether it is required or optional, and match what the code does.
13. **G-13** — **The protocol stays in step with upstream.** The Protocol Baseline shall be
    visible in the repository, and when upstream releases a newer specification, a check shall
    report the divergence rather than leave it to chance.
14. **G-14** — **n Agents over m connections.** Whether Supervisors share one connection or
    spread across several, as configured, the Server shall tell them apart by `instance_uid`
    alone and behave the same.
15. **G-15** — **A Gateway scales connections, not identities.** When Clients reach the Server
    through a Client in Gateway Mode, each shall appear as its own manageable Agent over a small
    Connection Pool, and the Gateway shall decide no admission of its own: it admits a Client by
    a certificate from the fleet's client CA, as the Server does, refuses what the Server has
    revoked, admits no one while it cannot learn what that is, forwards a Client's reports only
    for its own host's Agents, and passes on a package the Server offered through it only to the
    host it was offered to. A host that only a Gateway reaches shall enrol through it.
16. **G-16** — **A Collector reports through its own OpAMP client.** When a Collector carries the
    `opampextension`, it shall report its `service.*` description, its health, and its effective
    configuration to its Supervisor's Supervisor Endpoint, which relays them upstream. What the
    host is stays the Supervisor's and the operator's to state.
17. **G-17** — **The connection is secured and the Agent is identified.** Client-to-Server
    traffic shall be TLS-protected with mutual TLS on both ends, and the Server shall accept only
    authenticated Agent identities.
18. **G-18** — **One Configuration serves many hosts.** When a rollout releases a Configuration,
    the Server shall fill in its Server-side parameters per Agent, validate each value against
    its declared type, stop the rollout for an Agent whose value does not fit and say why, and
    choose the blocks an Agent receives by their Selectors. Host-side parameters, secrets among
    them, are filled in on the host and reach the Server only masked.
19. **G-19** — **The Server's state can live outside its host.** The Server shall keep its
    Packages' artifacts under its data directory or in an S3-compatible object store, and its
    records under its data directory or in PostgreSQL, as the operator names. An Agent receives
    the same verified bytes either way, through the Server, and the fleet never reaches a store
    itself.
20. **G-20** — **A fleet of up to about 100 000 Agents is served on when a Server instance is
    lost.** Two or more Server instances shall serve one fleet of up to about 100 000 Agents from
    one PostgreSQL database and one S3-compatible object store. When one instance stops without
    warning, every Agent it carried shall be connected to another instance within its lease time and
    two heartbeat intervals (90 seconds at the defaults), served from what the shared stores hold. A
    revocation committed on any instance shall end the sessions it concerns on every instance within
    one lease renewal interval (10 seconds at the defaults). A test shows both for a simulated fleet
    of 10 000 Agents across three instances. A second test shows that the database carries the
    presence writes and the notifications of 100 000 simulated Agents at the default heartbeat of 30
    seconds, about 3 300 reports a second, across three instances, with no presence batch written
    later than its interval after it was due and no notification left unread. The database and the
    object store are what the instances share, and making them redundant is the operator's: G-20
    holds while they are.

## Quality Goals

1. **Q-1** — **Secure by default.** Whatever its configuration, neither end sends a credential, a
   configuration or a package unencrypted beyond the loopback, admits an Agent onto the Server or a
   Gateway without a client certificate, or installs a package without a valid signature. A
   configuration that would do any of these is refused at startup, naming the setting.
2. **Q-2** — **Untrusted input is bounded and fuzzed.** Every parser that reads bytes from the
   network or from a downloaded artifact enforces its size limit before it allocates, and has a fuzz
   target that runs in CI.
3. **Q-3** — **The supply chain is checked before it is merged.** No change merges while a
   dependency carries a known vulnerability, an unapproved licence, or a source outside the
   registry, or while the TLS stack accepts a protocol version below 1.3.
4. **Q-4** — **A host receives only what is released to it.** The Server, and a Gateway from its
   package cache, shall never serve a host a configuration or an artifact that is not released to an
   Agent that host speaks for.
5. **Q-5** — **A remote configuration stays inside its Supervisor.** No remote configuration shall
   write outside a Supervisor's configuration directory, name a program outside its program
   directory, start a kind the build does not carry, or change any key of the Client's own
   configuration but its list of Supervisors.
6. **Q-6** — **No downgrade.** No Client shall install a Package below the highest version it has
   installed for that Agent type, or below the highest floor the operator signed for it.

## Non-Goals

- **Being a telemetry backend.** OpAMP Fleet manages agents; it does not receive, store, or query
  the telemetry they produce.
- **Replacing an agent's configuration language.** The Server distributes each Managed Process's own
  configuration; it does not invent an abstraction over the Collector's or a Foreign Agent's format.
  Parameters and blocks (G-18) are a text layer over that language — substitution and selection
  — not a model of it.
- **Forking or extending the protocol.** This project implements OpAMP as upstream defines it. Where
  it falls short of the Baseline the gap is recorded in [`CONFORMANCE.md`](CONFORMANCE.md) as a
  deviation; it is never resolved by inventing protocol semantics of this project's own.
- **Shipping a production UI.** The Server provides the OpenAPI REST API and only a rudimentary UI
  for basic operation; a production-grade user interface is external and out of scope for this
  project to build.
- **Authorization and multi-tenancy.** The Server authenticates *that* a peer belongs to the fleet
  (G-17), but does not distinguish *which* operator may do *what*, nor separate one operator's fleet
  from another's. Roles, permissions, and tenancy are real needs deferred rather than half-built. On
  the Agent plane one bound holds: a host receives from the Server, directly or through a Gateway,
  only the configurations and packages released to an Agent it speaks for (Q-4).

## Constraints

- **Platforms.** The Server runs on Linux only, shipped as a container image. The Client runs on
  Linux, macOS, and Windows, on the architectures each release names.
- **Protocol.** The wire contract is the OpAMP specification at a released version, the Protocol
  Baseline. A version upstream has not released is never implemented against.
- **Systems to work with.** The OpenTelemetry Collector and its `opampextension`, `opamp-go` as the
  behavioural oracle, the OpenTelemetry semantic conventions for what an Agent reports, and, where
  the operator names them, an S3-compatible object store and PostgreSQL, which the Server connects
  to and does not run.
- **Hosting.** The code, its CI, its releases, and the Server's image live on GitHub.
- **Team.** One maintainer decides; coding agents do most of the work under the rules of
  [`AGENTS.md`](../AGENTS.md).

## Threats & Forbidden Actions

- **Assets:** every host the fleet reaches, since the Server causes code to run on each; the
  operator's signing key; the fleet's client CA and bootstrap CA; each host's private key; the
  host-side parameters and secrets of a Configuration; the Server's records and its audit record.
- **Actors and trust boundaries:**
  - *The network* between a Client and the Server or a Gateway: untrusted. TLS 1.3 and a client
    certificate cross it; plaintext stays on the loopback.
  - *An admitted host*: inside the fleet's one trust domain, but bounded by its host. It may lie
    about what it reports and about its `instance_uid`, and it may be compromised.
  - *A Gateway*: an admitted host that speaks for many. It decides no admission of its own.
  - *The Server*: trusted to manage the fleet, not trusted with what a host must verify itself. A
    compromised Server must not get a host to run unsigned code or an older signed build.
  - *A Managed Process and its account*: where a process account is registered, below the
    Client's account on its host; it must not gain the Client's key, program, or configuration.
    Without one, it runs as the Client's account.
  - *The operator*: trusted. The project authenticates fleet membership, not who may do what.
  - *The supply chain*: dependencies and CI actions, checked before they merge.
- The system shall never:
  - send a credential, a configuration, or a package unencrypted beyond the loopback (Q-1);
  - admit a peer to the Server or a Gateway without a client certificate (Q-1);
  - install a package without a valid signature (Q-1), or below what the host has already run
    (Q-6);
  - serve a host what is not released to an Agent it speaks for (Q-4);
  - let a remote configuration act outside the Supervisor it is for (Q-5);
  - hand a host a private key: a host's key is made on the host;
  - let the fleet reach the Server's object store or database itself (G-19).

## Assumptions

- **A fleet is in the order of a thousand hosts** and their Agents per Server.
- **One fleet is one trust domain,** operated by one organization: operators who must not see each
  other's hosts run separate Servers.
- **The protocol keeps being released upstream** and `opamp-go` stays maintained, so the Baseline
  can move and the oracle stays available.
- **The TLS and crypto dependencies stay maintained** (rustls with ring, tokio, axum), since the
  security posture rests on them.
