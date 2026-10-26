# Glossary — OpAMP Fleet

> The **vocabulary** of this project: the terms everyone, humans and agents alike, uses in code,
> comments, documentation, and ADRs. It defines words and decides nothing. Whoever resolves a term
> adds it here in the same change ([ADR-0001](adr/0001-agent-governance-model.md)).

One term per concept, only terms specific to this domain. Say what a term *is* in one or two
sentences, and list under _Avoid:_ the synonyms it displaces, where there are any. A term the
specification, an ADR, or the code uses without an entry here is a gap to close, not a convention to
invent.

## The protocol

- **OpAMP** — the Open Agent Management Protocol: the wire protocol between a Server and its
  Agents.
- **Protocol Baseline** — the pinned upstream `opamp-spec` version this project implements
  against, recorded in [`CONFORMANCE.md`](CONFORMANCE.md). Implementation targets the Baseline;
  moving to a newer upstream version is a deliberate change, not a silent one.
- **Capability** — a feature an Agent or the Server declares it supports (accepting remote
  configuration, reporting health, reporting effective configuration, …), carried as a bit in the
  protocol's capability bitmask. Neither side may assume an undeclared capability. Each capability
  carries an upstream **maturity** — stable, `[Beta]`, or `[Development]` — and is either
  **required** (the protocol mandates it) or **optional**.
- **Capability Set** — the capabilities one end actually declares. The Server's and the Client's
  sets are tracked per capability in [`CONFORMANCE.md`](CONFORMANCE.md).
- **Instance UID** — the identity of an Agent, generated as a **UUID v7** and stable across
  restarts by default. It is the **routing key**: it, and not the connection a message arrived on,
  determines which Agent a message belongs to. The Server may reassign it (via OpAMP's
  AgentIdentification), and the Client then adopts the new value for all further communication.
- **Config hash** — the identity of a configuration. An Agent reports the hash it last received;
  the Server compares it against the hash of the configuration that Agent should have, and sends a
  new configuration only when they differ. This comparison is the control loop.

## The two ends

- **Server** — this project's control plane. It tells Agents which configuration they should be
  running and records what they report back. There is exactly one Server per fleet. It is
  **API-first**: its primary interface is the OpenAPI REST API, and it ships only a rudimentary UI,
  so any external portal can integrate it. The Server runs on Linux only.
- **Client** — this project's client deployable: the process an operator installs on a machine to
  place it under management. One Client process runs in one or more **Client Modes** and may present
  itself to the Server as several **Agents**. It is the OpAMP Client in the protocol's sense.
- **Agent** — one `instance_uid` as the Server sees it: an independently identified, independently
  configured participant in the fleet. The Server does not distinguish an Agent backed by a
  Supervisor from one backed by a Collector's own OpAMP client, nor one reaching it directly from
  one arriving through a Gateway.
- **Host** — what a client certificate the Server issued identifies: one Client installation that
  connects to the Server directly, a Gateway included. It is named by the host id its certificate
  carries and described by the subject values — country, state, locality, organization,
  organizational unit, organization identifier — carried over from the bootstrap certificate it
  enrolled with. Every Agent reaching the Server over that certificate belongs to it, the Agents
  behind a Gateway to the Gateway's Host: the Server does not see the hosts behind a Gateway. Trust
  and revocation act on a Host; a Deployment aims at Agents.
- **REST API** — the Server's stable HTTP interface for reading fleet state and reading and
  changing configuration, described by an **OpenAPI** specification so clients and portals can be
  generated from it rather than hand-written. It is the integration contract any UI or portal builds
  on.
- **Fleet** — all Agents managed by the Server.
- **Client Agent** — the Agent a Client presents for itself, recognised by the `supervisor.kind.*`
  attributes it alone reports; its Labels hold for the Client's other Agents, the Supervisor Agents.
  _Avoid:_ own agent, main agent.
- **Bootstrap certificate** — the short-lived certificate a host presents to ask for enrolment;
  its subject values describe the Host it becomes.
  _Avoid:_ join token, enrolment token.
- **Enrolment** — the time-limited, operator-approved exchange in which a host holding a bootstrap
  certificate obtains its client certificate and becomes a Host.
  _Avoid:_ registration, onboarding, enrollment.
- **Enrolling peer** — a peer of a Gateway whose certificate chains to the bootstrap CA alone; the
  Gateway relays one plain-HTTP enrolment request of it and nothing else.
  _Avoid:_ bootstrap client, enrolling Agent.
- **Data directory** — the one directory, `data_dir`, under which the Server keeps all of its
  state, one subdirectory per domain.
  _Avoid:_ config directory, package directory, state directory (that is the Client's).

## Client Modes

- **Client Mode** — the role a Client process takes, selected at startup. There are exactly
  **two**, and they are independent: a Client may run **Supervisor Mode**, **Gateway Mode**, or both
  at once. Neither implies the other.
- **Supervisor Mode** — the Client runs **Supervisor** instances that manage local processes. This
  is the mode that closes the control loop for a machine's own agents. Every Supervisor also exposes
  a **Supervisor Endpoint** — that is part of what a Supervisor *is*, not a separate mode to
  enable.
- **Gateway Mode** — the Client accepts OpAMP connections from other Clients and forwards their
  messages upstream over a **Connection Pool**, so a large number of agents reaches the Server over
  a small number of connections. A Gateway forwards messages unchanged and holds **no authentication
  policy of its own**: it admits a connecting Client by a client certificate from the fleet's client
  CA in its handshake, as the Server would, and the one admission refusal it makes beyond that is
  the Server's — a certificate the Server has revoked. It forwards a Client's reports only for the
  Agents its host first reported, and passes on a package it holds for the Agents behind it only to
  the host whose Agent the Server offered it to through the Gateway. Where the operator gives it the
  bootstrap CA, it also relays a host's enrolment, which the Server decides as any other. Agents
  behind a Gateway remain distinct Agents.
- **Supervisor Endpoint** — the OpAMP endpoint a Supervisor exposes on the loopback interface so
  that a Managed Process carrying an OpAMP client of its own can report to it. It exists because
  such a client — notably the OpenTelemetry Collector's `opampextension` — is a **client only**
  and therefore needs something to connect *to*; the Supervisor is the natural counterpart, since it
  already holds the configuration to hand down and the upstream connection to relay onto. The
  Supervisor receives the process's agent description, health, and effective configuration there and
  relays them upstream, so a Collector reports through its own OpAMP client rather than being
  observed from the outside. It is **always present** while a Supervisor runs; for a Foreign Agent,
  which speaks no OpAMP, simply nothing ever connects to it. It is not addressable from outside the
  machine and never carries fleet traffic — that is the Server's role, and the Gateway's. Despite
  speaking the Server side of the protocol, it is not a **Server** in this vocabulary's sense: it
  manages no fleet, holds no configuration of its own, and serves exactly one Managed Process.

## Connections

- **Connection Pool** — the set of upstream connections a Client maintains to the Server, over
  which *n* Agents are carried by *m* connections (*n* ≥ *m* ≥ 1). Its size is a deployment
  choice, not a consequence of how many Agents exist.
- **Connection Multiplexing** — carrying more than one Agent over one connection, disambiguated by
  `instance_uid`. The protocol provides for this explicitly; both ends of this project support it.

## Supervision

- **Supervisor** — a unit inside a Client in Supervisor Mode that manages exactly one **Managed
  Process**: it applies the configuration it receives, reports health and effective configuration,
  and exposes a **Supervisor Endpoint** for a Managed Process able to use it. Each Supervisor is one
  **Agent** as the Server sees it, but does not necessarily own a connection of its own.
- **Collector Supervisor** — the OpAMP-native Supervisor plugin: it owns an OpenTelemetry
  **Collector**, writes the configuration it receives to disk, and restarts the Collector to apply
  it.
- **Custom Supervisor** — a Supervisor plugin that manages a **Foreign Agent** and translates that
  agent's lifecycle, configuration, and health into OpAMP toward the Server. It is how a non-OpAMP
  agent is brought into the fleet.
- **Managed Process** — the actual process a Supervisor manages: a Collector, or a Foreign Agent.
  What separates the two is **which Plugin has to exist for it**, not whether it speaks OpAMP: a
  Collector is served by the one Collector Supervisor that ships with the project, a Foreign Agent
  needs a Custom Supervisor written for its kind.
- **Collector** — an OpenTelemetry Collector process, managed by the **Collector Supervisor**. Its
  configuration format, lifecycle, and health are known to that Plugin, so one Plugin serves every
  Collector. Whether it additionally carries the `opampextension` changes only *how it reports*:
  with the extension it reports its own description, health, and effective configuration to the
  Supervisor Endpoint; without it, the Supervisor infers what it can from the outside. Either way
  the Collector never reaches the Server directly, and either way it is not a Foreign Agent.
- **Foreign Agent** — a Managed Process whose kind requires a **Custom Supervisor written for
  it**, because nothing in the project already knows its configuration format, lifecycle, or health.
  Bringing a new one under management means writing a Plugin (G-8); this is the sense in which it
  is *foreign*. It never speaks OpAMP — if it did it would be an Agent in its own right and need
  no Supervisor at all — but that is a consequence of being foreign, not the definition: a
  Collector without the `opampextension` speaks no OpAMP either and is still not a Foreign Agent,
  because the Collector Supervisor already covers it.
- **Plugin** — a Supervisor implementation compiled into the Client and plugged in behind the
  hexagonal ports. Supporting a new kind of managed process means adding a plugin, not changing the
  core.
- **Port** — a boundary the supervision domain defines and depends on: the Server-facing side
  (speaking OpAMP) and the Managed-Process side (a Managed Process's lifecycle, configuration, and
  health). The domain is written against ports, never against a concrete process type.
- **Adapter** — a concrete implementation of a Port: the OpAMP client is an adapter on the
  Server-facing side; each Plugin is an adapter on the Managed-Process side.
- **Process account** — the account the operator names at a Client's registration for every
  program its Supervisors start; never root or `LocalSystem`, and never the Client's own account. On
  Linux and macOS its primary group, the *process account's group*, is its own: the Client's account
  is its one member, and no other account's primary group it is. What the Client shares with the
  Managed Processes belongs to that group, and nothing else does; on Windows the same share is made
  by grants to the account's SID.
  _Avoid:_ agent user, run-as account (that is the Client's), process group (that is a Unix
process group, which a Managed Process may lead).
- **Second service** — the service `<PRODUCT_NAME>-agents`, registered beside the Client where a
  process account is named, which runs under that account and starts, signals and stops those
  programs on the Client's request, over a channel only the Client's account may use.
  _Avoid:_ helper, launcher, sidecar.
- **Tombstone** — the name, `.purge-<name>-<nonce>`, beneath the Supervisors' root that a removed
  Supervisor's directory is moved to before its purge deletes it, a name no Supervisor can carry;
  one the purge could not delete is removed again at the next start and with the next purge.
  _Avoid:_ trash directory, purge directory.

## Fleet operations

- **Remote configuration** — the configuration the Server distributes to an Agent. It is what the
  Server *wants* that Agent to run.
- **Effective configuration** — the configuration an Agent reports it is *actually* running. It
  may differ from the remote configuration (it may merge in local configuration, or have rejected
  the remote one).
- **Health** — an Agent's self-reported liveness and status.
- **Selector** — the rule by which the Server addresses a **subset** of the fleet: equality pairs
  over an Agent's effective description, which is what the Agent reports, the Labels an operator
  set, and its Host's subject values. It aims a Deployment, so a change reaches the matching Agents
  and leaves the rest running what they already run, and it chooses which blocks of a Configuration
  an Agent receives. One mechanism, not two.
- **Label** — a key and value an operator sets on an Agent, part of its effective description. The
  Labels set on a Client's own Agent hold for every Agent of that Client unless an Agent sets the
  key itself. A Label never overrides what an Agent reports or what its Host's certificate says.
- **Configuration** — a named piece of an agent's own configuration language, written for one
  Agent type, with the parameters it declares. It is content only: it reaches Agents through the
  Deployments that hold it.
- **Parameter** — a named value a Configuration leaves open. A Server-side parameter is filled in
  per Agent from what the Server knows — the Deployment, the Agent's effective description — and
  is validated against its declared type before a rollout releases it. A host-side parameter is
  filled in on the host and never reported back in clear; a secret is always one.
- **Package** — a versioned, downloadable software artifact an Agent installs, identified by the
  **Agent type it is built for and its version**; its display name is derived from the two. It holds
  one entry per platform: the artifact's content hash, its size and the operator's signature. Every
  artifact is uploaded to the Server, which offers only what it holds. An Agent verifies the content
  hash and the signature and installs nothing that fails either; the Server checks a signature
  against the operator's public key when it has one and reports a failed check, but the Agent's
  check decides. An Agent reports the status of each Package. This is how the Server updates an
  agent's software, not only its configuration.
- **Deployment** — a named release: Packages, at most one per Agent type, and Configurations,
  aimed at a subset of the Fleet by a Selector. It is the only thing that is rolled out; a rollout
  pins for each Agent the Package and the Configuration revisions it releases, so editing the
  Deployment afterwards changes nothing an Agent runs. An Agent belongs to **at most one**: two
  Deployments matching one Agent is a conflict, and that Agent is offered nothing new until it is
  resolved. An Agent in no Deployment is offered nothing.

- **Agent type** — the kind of agent an Agent runs, as the `service.name` it reports; a Package is
  built for one, a Configuration is written for one.
  _Avoid:_ service type, product.
- **Effective description** — what a Selector is matched against: its Host's subject values as
  `fleet.host.*`, an Agent's reported attributes, its Labels, and its Client Agent's Labels.
  _Avoid:_ attributes (alone), metadata.
- **Rollout** — the explicit act that releases a Deployment to Agents and pins what each receives.
  _Avoid:_ publish, push, deploy.
- **Entry** — one platform's artifact in a Package: operating system and architecture, content
  hash, size and signature.
  _Avoid:_ file, asset, build.
- **Blob** — the bytes of an Entry's artifact as the Server stores them, named by their SHA-256;
  identical bytes are one blob, whichever Entries name it.
  _Avoid:_ artifact file, binary.
- **High-water mark** — the highest Package version a Client has installed for one Agent type; a
  Supervisor of that type refuses any offer below it.
  _Avoid:_ floor (that is the operator's signed one), minimum version.
- **Floor** — the version the operator signed for one Agent type below which no Client takes a
  package of that type, its own update included; each Client keeps the highest floor it has seen.
  _Avoid:_ minimum version, revocation, high-water mark (that is what the host installed).

## The installed Client

- **Install lock** — on Windows, the mutex `<PRODUCT_NAME>-install\lock` in a private namespace
  only elevated Administrators and `LocalSystem` can create, held for the whole of a `service
  install`, `uninstall`, `start` or `stop` so that one runs at a time.
  _Avoid:_ install mutex, global mutex.
- **Withdrawal** — what an install takes from a former account, a process account dropped or
  replaced or a Client's account replaced: on Linux and macOS by giving what it owns, and what is in
  its group, to the Client; on Windows by removing every entry that allows it anything and giving
  what it owns to the Administrators.
  _Avoid:_ revocation (that is a certificate's), cleanup.
- **Record of what an install left to finish** (*the record*) — on Windows, the value `Pending` of
  `HKLM\SOFTWARE\<PRODUCT_NAME>\WithdrawalPending`: what an install left to finish, the former
  accounts with the roots to withdraw them from and the services to start again.
  _Avoid:_ pending file, state file.
