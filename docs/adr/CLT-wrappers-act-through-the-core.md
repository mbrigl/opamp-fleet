# CLT-wrappers-act-through-the-core: A Plugin is a state machine that acts only through the core, over a port of plain data

- **Status:** 🟡 proposed
- **Date:** 2026-11-26
- **Deciders:** Markus Brigl
- **Applies to:** crates/fleet-agent/src/supervisor/ (core, ports, the Plugin port with its inputs and host functions, the core that carries out a host function, process runner with its preflight, version probe and process-group stop, endpoint, placeholders, start, every kind and its manifest), the Client Agent in `build_engine`, crates/fleet-agent/src/config.rs (supervisor_dir, program resolution), the `[[supervisor]]` blocks of supervisor.toml apart from what CLT-server-manages-the-supervisor-set governs, the `[features]` table of crates/fleet-agent/Cargo.toml, the artifact documents under docs/artifacts/ each wrapped kind adds, the Plugin role in crates/fleet-core/tests/dependency_direction.rs
- **Supersedes:** [CLT-supervisor-mode-and-kinds](CLT-supervisor-mode-and-kinds.md)

## Context

This ADR and [CLT-server-manages-the-supervisor-set](CLT-server-manages-the-supervisor-set.md)
together supersede [CLT-supervisor-mode-and-kinds](CLT-supervisor-mode-and-kinds.md). An ADR is
superseded whole. The revision below touches the port, the kinds and their lifecycle; it does not
touch the Server-managed set, its two switches or the repeated offer. Those clauses move unchanged
in meaning into the other ADR, and this ADR restates the rest, revising what the port decides.
Every clause of the superseded ADR lives on in one of the two. The map, old clause to new, is in
the hand-off of this change.

**Why the port is revised.** The superseded ADR made a Plugin a factory that starts a tokio task
of its own (its clause 1). The task receives `SupervisorContext`: absolute paths, the Endpoint
token, the archive key, the process account's group, the event channel and the shutdown signal.
Each kind builds its own `Runner` from them. Nothing keeps a kind from reaching past the core:

- `collector` lists its configuration directory itself (`crate::storage::config_entries`).
- The `icinga2` kind on its feature branch reads, writes, copies, renames and removes files with
  `std::fs` in about 25 places. It starts its own subcommands with `tokio::process::Command`, asks
  `libc::getaddrinfo` for the host's name and runs `id` for its own account. It intercepts the
  command channel in tasks of its own.
- Because it starts programs past the `Runner`, `icinga2` cannot keep
  [CLT-accounts-service-runs-under §18](CLT-accounts-service-runs-under.md): every program a
  Supervisor starts is started by the second service. The kind refuses a registered process
  account instead. The missing boundary already costs a feature.
- `glpi` and `telegraf` on their branches need nothing beyond a process specification and a reload
  signal.

The human asked for the boundary to be drawn: a Plugin holds what is specific to its agent, and
every access to files, processes, the host and the network goes through the core. The same
boundary is to make a Plugin compiled to WebAssembly possible later, without building it now.

**What a port must be to carry WebAssembly later.** A WebAssembly component exchanges values its
interface declares in WIT: records, variants, lists, strings, integers. It has no host paths, no
file handles, no channels and no tracing spans. WASI 0.3, ratified in June 2026, adds `async func`,
`stream<T>` and `future<T>` to WIT, so an asynchronous host interface is possible too. A port
whose every value is plain data maps onto a WIT interface one to one. A port that hands a Plugin a
`PathBuf`, an `mpsc::Sender` or a `tracing::Span` does not.

Wasmtime implements WASI 0.3.0 with Component Model async enabled by default from version 46 on.
Wasmtime 43 to 45 implement a release candidate and cannot run components built against 0.3.0.

**Take-over.** A host may already run an agent installed by its package manager, such as an
`icinga2` with its configuration, certificates and system service. Clause 29 brings such an agent
in by repacking. What it leaves open is the installation already on the host. Its configuration
could be taken over into the Supervisor, and the existing service stopped, disabled or removed at
the end. That needs access outside the Supervisor's directory and rights the sandbox of this ADR
does not grant. The human asked for the architecture to provide for it, without building it now.

**Without a decision,** `icinga2` stays without a process account, every new wrapped kind brings
its own I/O, nothing the core enforces on paths holds for a kind, and a WebAssembly Plugin would
be a rebuild of the port rather than one more adapter.


Supervisors are the reason the client side exists
([CLT-modes-and-gateway](CLT-modes-and-gateway.md)): many Managed Processes per Client, each
appearing to the Server as its own Agent, multiplexed by `instance_uid` (G-6, G-14). A Managed
Process takes one of three integration paths ([`docs/GLOSSARY.md`](../GLOSSARY.md)): a Collector
carrying the `opampextension`, which reports its own description, health and effective
configuration to the Supervisor Endpoint (G-16); a Collector without it, observed from the
outside; and a Foreign Agent under a Custom Supervisor, whose lifecycle, configuration and health
a Plugin translates into OpAMP (G-7).

The specification asks for a hexagonal core: the supervision domain written against two Ports, the
Server-facing side speaking OpAMP and the Managed-Process side (lifecycle, configuration, health),
with Plugins as adapters, so that a new process type is a new Plugin (G-8).
[DEV-six-crates-tokio-axum](DEV-six-crates-tokio-axum.md) keeps such seams as modules until a need
makes them crates and requires a typo in the hand-edited TOML to fail loudly at startup. serde
cannot combine `#[serde(flatten)]` with `deny_unknown_fields` (serde-rs/serde#1547), so a block
whose kind-specific keys sit beside the common ones cannot be one strict struct.

The reference `opampsupervisor` runs a local OpAMP server on
`ws://127.0.0.1:{{port}}/v1/opamp` (WebSocket only), passes configuration to the Collector via
`--config`, restarts it on a configuration change, stops it with SIGTERM then a kill after a
timeout, and watchdog-restarts it with exponential backoff. It configures its agent generically,
an absolute `agent.executable` plus `args`, because it supervises a binary somebody else
installed. This Client is in the opposite position: it installs every program it supervises into a
directory it owns ([CLT-accounts-service-runs-under](CLT-accounts-service-runs-under.md) lays the
directory out), from an artifact this project packs (clause 29). It is the one end that can know an
agent's layout, and `opamp-fleetctl package fetch` already carries per-agent knowledge (service
name, release source, default Configuration names). Writing that knowledge into every host's TOML by
hand, one block per platform for an agent whose invocation differs by platform and one key per
layout path for an agent that has to be relocated, is a transcription that goes stale when an
artifact moves.

Agents differ in how they are installed, configured, reloaded and removed. Some re-read their
configuration on a signal and lose buffered state when restarted; systemd treats reload as
first-class beside restart (`ExecReload`, `reload-or-restart`). OpAMP itself only ever drives
configuration and packages, and its only process command is restart.

Agents also differ in how they run. A program that forks a worker can leave it behind when only
its own pid is killed, holding the agent's data and ports. A package built against a newer system
library than a host carries fails only when it starts, with the dynamic linker's message, and an
update that finds this out after the swap has already stopped what ran. Not every program prints
its version as Semantic Versioning.

A package update swaps files: the running program is renamed aside, the artifact is written beside
it and renamed into place ([PKG-signed-packages-and-delivery](PKG-signed-packages-and-delivery.md)).
All of that needs write permission on the **directory**. A program in a system directory such as
`/usr/local/bin` cannot be updated by a Client that does not run as root, and the failure would
appear at rollout time on every matched host rather than at startup on one. A Client that also
supervised programs installed by someone else would have two kinds of Managed Process, one it can
update, roll back and health-gate and one it merely runs, and every package, version and rollback
decision would carry both cases. Repacking vendor software into relocatable trees (clause 29)
brings such agents under fleet ownership instead.

`state_dir` is state: hardened hosts mount `/var/lib` `noexec` and size it for state, not for
several Collector binaries. An artifact staged under one filesystem and installed on another is
copied, a second full write of a program of several hundred megabytes.

A Foreign Agent is told where its configuration is through its own command line, and an absolute
path written there drifts silently when the Supervisor's directory moves or the Supervisor is
renamed: the process starts happily on a file nobody writes to.


**A program may answer only where and as its start runs it.** A kind may name the working directory
a Managed Process starts in (clause 6). A program that needs its tree root to run, as a bundled
interpreter that resolves its libraries from there does, answers a version probe or a preflight run
elsewhere as another program would, or not at all. A program that finds its tree's libraries only
through its environment answers a probe without that environment not at all. A preflight proves a
staged package, whose root is known only once it is staged, so a preflight that is to hand the
program a path into its own tree needs a name for that root.

**The `supervisor.` namespace is the Supervisor's own statement.** `[attributes]` refuses every
`supervisor.*` key ([CLT-what-an-agent-reports §10](CLT-what-an-agent-reports.md)), so the
kinds a Client Agent reports are the kinds it was compiled with (clause 24).


## Decision

We will make each Supervisor Plugin an asynchronous event handler that acts only through the
core's host functions, over a port of plain data. The host functions reach only the Supervisor's
own roots. The core keeps the whole lifecycle of the Managed Process. A WebAssembly component can
implement the port. A take-over of an existing installation is provided for, as host functions the
core grants.

1. **A Plugin is an event handler that acts through the host.**
   - A Plugin instance serves one Supervisor. It holds the state of its agent's integration.
   - The core calls the Plugin's handler with one input at a time. The handler is asynchronous and
     may await host functions.
   - The core must not call the handler again before the previous call has returned.
   - The Plugin must perform no I/O itself. It must not open a file, start a program, signal a
     process, resolve a host name or open a connection.
   - The Plugin must not spawn a task and must not read a clock. Time reaches it as an input.
   - The Plugin acts only through the host functions of clause 4. Each answers with its outcome or
     with the reason it was refused.
   - The core may cancel a running handler at shutdown and at an uninstall.
   - The port keeps three calls: the manifest (clause 6), the side-effect-free `check` of a block's
     settings, and the handler.

2. **The port carries plain data only.**
   - Every input, argument and result of a host function, manifest and setting is built from
     records, variants, lists, strings, byte strings, integers and booleans.
   - The port must carry no host path, file handle, channel, task handle, span or secret.
   - The core keeps the Endpoint token, the archive key, the process account's group and the
     tracing spans. It applies them while it carries out a host function.
   - The port must be expressible in WIT, with `async func` for the handler and the host
     functions, without a host resource.

3. **A Plugin names files by root and relative path.**
   - The roots are:
     - `config`: the Supervisor's `config/` directory, which the Plugin may read and must not
       write;
     - `program`: the live program or tree, which the Plugin may read and must not write;
     - `data`: a directory the Managed Process writes into (clause 6), shared with the process
       account's group;
     - `state`: the Plugin's own directory, which the Client alone reads and writes.
   - A relative path must name no absolute path, no `..` component and no empty component.
   - The core resolves every path inside its root through the no-follow adapter
     ([CLT-accounts-service-runs-under §15](CLT-accounts-service-runs-under.md)). A path that
     would leave its root fails the call.
   - The `state` root lies in the Supervisor's directory. It is the Client's alone, as everything
     there is that [CLT-accounts-service-runs-under §21](CLT-accounts-service-runs-under.md) does
     not name.
   - In a process specification an argument, an environment value or the working directory may
     be a root-relative path. The core expands it to an absolute path when it starts the program.

4. **The host functions are a closed vocabulary.**
   - The core offers a Plugin host functions:
     - to start, restart or reload the Managed Process on a process specification;
     - to run a program from its `program` root once and receive its exit status and output;
       the program runs under the account and through the path of the Managed Process;
     - to read, write, rename or remove a file within a root the function may reach;
     - to list or create a directory within such a root;
     - to set or cancel a timer, which arrives as an input;
     - to report health, a description or an effective configuration;
     - to answer the apply of a configuration or of a package, and to answer an uninstall.
   - Host facts reach a Plugin through a host function the core answers from its host-facts port.
     Examples are the fully qualified host name and the account a program runs under.
   - The core must refuse a call outside the root it names. It fails only that call and returns
     the reason to the Plugin.
   - A Plugin must reach the network, an agent's API included, only through a host function. The
     vocabulary carries no such function today.
   - The vocabulary grows only by an ADR that supersedes this one.

5. **The core keeps the lifecycle, and a Plugin steers it.**
   - The commands of the core are `ApplyConfig`, `ApplyPackage`, `Restart`, `Shutdown` and
     `Uninstall`. `Restart` is the Server's restart command: respawn on the current files, with no
     configuration acknowledgement.
   - The core translates each command into an input for the Plugin, and each host function into
     the work of the `Runner`.
   - The `Runner` implements spawn and watchdog, the bounded stop, restart as the configuration
     apply, swap-and-gate as the package install, and the stop as uninstall.
   - The events the core reports for a Supervisor carry description, pid, health, effective
     configuration and available components. They carry the outcomes of an apply, an install and
     an uninstall too.
   - A Plugin that only states a manifest and a process specification is complete. For every
     other input the core applies its defaults.

6. **A kind states what it knows in its manifest.** The manifest returns compile-time constants,
   resolved per platform:
   - the program's file name;
   - its `program_path` inside a tree, and its member in an archive;
   - the links a tree keeps;
   - the `service_name` it presents;
   - its timing corrections (clause 23);
   - whether a block may pin `endpoint_port`;
   - its reload signal, if its agent reloads on one;
   - whether its Managed Process leads its own process group (clause 32);
   - its version arguments and preflight, with their working directory and environment
     (clauses 33 and 34);
   - the directories its agent writes into, relative to the `data` root;
   - the block key naming its program, for `collector` and `command`.

   Beyond that:
   - **Working directory:** a Managed Process starts in the directory its program lives in,
     unless the process specification names another. That is `program/` for a single file and the
     tree root for a tree. No block sets it. Every path the core hands a process is absolute.
   - **Directories the agent writes into:** the core creates them, open to their owning account
     alone, before every spawn. A directory removed under a running fleet so returns on the next
     restart. They lie outside `program/`, which a package swap replaces whole. An installation
     that fails for want of a directory is this Client's failure, not the operator's.

7. **Reload is an apply strategy, not a command.**
   - `ApplyConfig` is the only configuration operation.
   - The core applies it by restart, the default, or by the reload signal the manifest names, with
     systemd's `reload-or-restart` semantics. A reload that cannot be delivered, or a process that
     dies on it, falls back to a restart on the new files.
   - The health gate of clause 19 applies to both paths.
   - `collector` and `command` restart. A wrapped kind reloads on a signal its ADR names, or
     restarts.
   - No block declares a reload. A signal written for a program the kind knows nothing about is
     the one value whose error is invisible.

8. **The core installs a package, and a Plugin only prepares and judges.**
   - `ApplyPackage` hands the core a verified artifact and expects a health-gated outcome with
     rollback on failure ([PKG-signed-packages-and-delivery](PKG-signed-packages-and-delivery.md)).
   - The core swaps the install target, one file or a tree, and runs the preflight of clause 33.
   - A Plugin receives the install as an input. Before the swap it may call what clause 4
     offers within its roots. After the swap the core's health gate decides.
   - A Plugin must not install by any other means than the core's swap.
   - Resolving the program and declaring `AcceptsPackages` stay in the core (clauses 27 and 28).
     A capability the Server acts on must derive from what the core reads, never from a kind's
     behaviour.

9. **Uninstall undoes what installing did, within the Supervisor's roots, and the core purges.**
   - A Supervisor removed from the set receives `Uninstall` before its directory is purged
     ([PKG-versions-high-water-mark-floor](PKG-versions-high-water-mark-floor.md)).
   - The core stops the Managed Process. The Plugin then receives the uninstall as an input. It
     may call what clause 4 offers within its roots, and answers the uninstall.
   - The generic uninstall is exactly the bounded graceful stop.
   - The Client logs an `Err` answer with what was left undone. It does not hold up the removal.
   - A Supervisor that is only changed is stopped, never uninstalled.

10. **The core persists a configuration, and the Plugin decides how it reaches the process.**
    Receiving, validating, persisting and status-reporting a remote configuration are OpAMP
    mechanics. They are the same for every kind and stay in the core. The core then hands the
    Plugin the written entries as an input: each entry's name, role and digest. How the files
    reach the running process is the Plugin's decision, carried out through its host functions.
    It decides which entry the process is pointed at, whether a validation runs, and restart or
    reload.

11. **A Plugin reads its program's version from the probe's output.** The core runs the version
    probe the manifest names and hands the Plugin its output as an input. The Plugin answers with
    the version, or with none. The default reading is strict Semantic Versioning. A kind whose
    program prints its version in another form reads it itself. Its ADR says how the form is
    read.

12. **The port is ready for a WebAssembly Plugin, and the build loads none.**
    - Today every Plugin is native Rust, compiled in and selected by `type` (clause 14).
    - A WebAssembly Plugin is one more implementation of the port. It is a component whose
      interface mirrors the port's types.
    - The core must not depend on whether a Plugin is native or WebAssembly.
    - Loading a WebAssembly Plugin is out of this ADR. It needs an ADR of its own that decides the
      runtime, the versioning of the interface, and how a component is signed, delivered and
      trusted.

13. **A take-over of an existing installation is provided for, as host functions the core grants.**
    - A take-over adopts an agent already installed on the host. It takes over the installation's
      configuration into the Supervisor. At its end it stops, disables or removes the existing
      service.
    - A take-over needs access outside the Supervisor's roots and rights the Managed Process does
      not hold. A Plugin must reach both only through host functions.
    - The core must grant such a call only where the operator consented on the host to the
      take-over by that Supervisor.
    - The vocabulary of clause 4 carries no take-over function today.
    - Building take-over needs an ADR of its own. It decides the host functions, the consent, the
      account that carries them out and how a failed take-over is undone.
    - The program the Supervisor runs still arrives by repacking (clause 29). A take-over moves
      configuration and retires the old installation. It does not supervise the old program.

14. **A compiled-in registry selected by `type`, each wrapped kind a Cargo feature.**
   - The kinds are `collector` (the Collector Supervisor), `command` (the Custom Supervisor for any
     Foreign Agent), and the wrapped kinds, each of which knows one third-party agent (clause 21).
   - A new process kind is one module and one registry line (`registry()` in `supervisor/mod.rs`).
     The core stays untouched.
   - Each wrapped kind is a feature of `fleet-agent` named after its `type`. The default features
     are all of them, so the released build carries every kind. A build that leaves one out is
     built with `--no-default-features --features <kinds>`. A new wrapped kind is a feature too, in
     the default set.
   - `collector` and `command` are always compiled in. They are the integration paths the
     specification names, and `command` runs any Foreign Agent no wrapper covers.
   - An unknown `type`, one this build does not carry, fails startup naming the known ones.
   - Plugins are not loaded dynamically.
   - A build's kinds are what its Client Agent reports (clause 24).
   - A build that leaves a kind out carries the same Agent type, and so the same package name, as
     the full build (clause 30). An operator may withdraw the consent on its hosts and update them
     through their own channel
     ([CLT-self-update-signed-package §2](CLT-self-update-signed-package.md)). A
     self-update into a build that lacks a kind a block on the host names fails its start. It is
     rolled back like any failed attempt
     ([CLT-self-update-signed-package](CLT-self-update-signed-package.md)).

15. **A block is parsed in two stages.**
   - The core takes the common keys: `type`, `name`, `service_name`
     ([CLT-what-an-agent-reports](CLT-what-an-agent-reports.md)), `endpoint_port`, `program_path`
     ([PKG-signed-packages-and-delivery](PKG-signed-packages-and-delivery.md)),
     `stop_timeout_secs`, `apply_grace_secs` and `retain_previous_secs`. It also takes the program
     key the kind names (`binary` for `collector`, `command` for `command`), which clause 27
     resolves.
   - Everything else is the kind's, parsed with `deny_unknown_fields`.
   - `name` follows the instance-name grammar of
     [CLT-installed-service-and-state](CLT-installed-service-and-state.md), because it is a
     directory name. It is unique across blocks.

16. **Each Supervisor is one Agent; the pool is one connection.**
   - Every Supervisor has its own persisted `instance_uid`, `sequence_num` and capability set.
   - All of them ride the Client's single upstream connection, disambiguated by `instance_uid`
     alone. That is the n-over-m model of [CLT-modes-and-gateway](CLT-modes-and-gateway.md) with
     m fixed at one.
   - The Client Agent ([CLT-self-update-signed-package](CLT-self-update-signed-package.md)) is
     always present beside them, with or without any `[[supervisor]]` block.
   - The Server needs no Supervisor-specific code.

17. **The Supervisor Endpoint is WebSocket-only on loopback.**
   - The Client binds it for every Supervisor at start to `127.0.0.1:<endpoint_port>`. `0`, the
     default, is an ephemeral port. A taken port thus fails the start rather than later.
   - It accepts connections with `tokio-tungstenite`, with no HTTP framework in the Client.
   - It declares `AcceptsStatus` and `AcceptsEffectiveConfig` and enforces the message size limit.
   - It folds **content, not identity**. A connecting process's description, health, effective
     configuration and available components become events of the owning Agent, whose
     `instance_uid` stays the Supervisor's.

18. **Process management follows the reference supervisor.**
   - The Runner spawns via `tokio::process`.
   - It watchdog-restarts an unexpectedly exited process with exponential backoff.
   - It stops gracefully. On Unix that is SIGTERM, a wait of up to the stop timeout, then a kill,
     signalling the process group where the kind leads one (clause 32). On Windows it is
     `Child::kill`.
   - On Client shutdown the Managed Processes stop first, then each Agent's `agent_disconnect` goes
     out, within the service managers' stop budgets
     ([CLT-installed-service-and-state](CLT-installed-service-and-state.md)).
   - The Collector Supervisor passes each written configuration entry as its own `--config`, never
     a supplementary entry
     ([SRV-configurations-compose-one-map](SRV-configurations-compose-one-map.md)). It appends its
     extra `args`, does not start before a configuration exists, and manipulates no YAML.

19. **Configuration status is health-gated.**
   - `RemoteConfigStatus` is `APPLYING` on receipt.
   - It is `APPLIED` only once the Managed Process has been (re)started on the new files and
     survived the apply grace.
   - It is `FAILED`, with the error, otherwise.

   A process that exits within the grace fails the apply and stays supervised.

20. **Derivable means derived, and decided elsewhere means not written here.**
    - A value this Client can compute from the kind, the platform and its own layout is not a block
      key. Neither is a value the fleet is the better place to set.
    - A block carrying such a key fails at startup with a message naming the key and what supplies
      the value. It is never a silent override.
    - This covers:
      - a wrapped kind's program key, `program_path`, `service_name`, `endpoint_port` and the three
        timing keys;
      - `command`'s `working_dir` and `reload_signal`;
      - `[supervisor.attributes]`, since a Server Label tags one Agent among several
        ([SRV-fleet-record-agents-hosts](SRV-fleet-record-agents-hosts.md)) while the Client-wide
        `[attributes]` describe the host.
    - A configuration a freshly self-updated version refuses is that update's failed attempt. A run
      resolves the update in flight even when the file does not load. The host thus returns to the
      version that can read the file
      ([CLT-self-update-signed-package](CLT-self-update-signed-package.md)).

21. **Each wrapped agent is decided in an ADR of its own.** The rule here is general; what a
    particular kind knows is not. An upstream that moves a path then touches one document: the
    kind's own ADR. Clause 22 decides `collector` and `command`.

22. **What stays in a block, stays for a stated reason.**
    - `collector` is one kind for both distributions and keeps `binary`. `otelcol` and
      `otelcol-contrib` share a lifecycle, a configuration mechanism and a supervision story, and
      which one a host runs is a decision. Its `service_name` falls back to the program's file name.
    - `collector` and `command` keep `args` and `env`. A feature gate, or a per-host value read as
      `${env:VAR}`, is a decision nothing can derive. The wrapped kinds build both whole and take
      neither. An operator who needs to change how a wrapped agent runs does it in that agent's
      configuration, which the fleet delivers.
    - `endpoint_port` is a key of `collector` only, the one kind whose Managed Process connects to
      the Endpoint. It cannot be derived: the port appears in the Collector configuration the
      fleet delivers, which this Client writes byte for byte and never rewrites. A non-zero value
      on another kind is refused. `0` is the default written out and passes. The default stays
      ephemeral so two Collector Supervisors on one host do not collide.
    - `command` keeps `args`, `env` and `version_args`. Without them most Foreign Agents never find
      their configuration and report no version, and each fails visibly when wrong.
    - A wrapped kind may keep a key where a decision exists that nobody else can make. The kind's
      own ADR names the key and the decision.

23. **Timing is a fleet policy, then a kind's correction, never a host's.**
    - `[supervisors]` holds `stop_timeout_secs` (default 10) and `apply_grace_secs` (default 3).
      `[updates]` holds `retain_previous_secs` (default one day,
      [PKG-signed-packages-and-delivery](PKG-signed-packages-and-delivery.md)).
    - A wrapped kind may correct any of the three where its agent demands it, such as an agent
      whose shutdown drains for up to a minute. Its blocks may state none of them.
    - Only `collector` and `command` blocks may override them, because there no kind exists to hold
      the value.

24. **A Client reports which kinds it carries.**
    - Its Client Agent reports one non-identifying attribute per compiled-in kind,
      `supervisor.kind.<kind> = "true"`. A Selector can then aim a Supervisor set at Clients that
      can run it.
    - There is one key per kind, because a Selector is equality over string values and the question
      is about one member of the set.
    - An operator cannot state one: `[attributes]` refuses every `supervisor.*` key at startup
      ([CLT-what-an-agent-reports §10](CLT-what-an-agent-reports.md)).
    - `AvailableComponents` is not used for this. The message is *Development* in the pinned
      Baseline ([API-protocol-pinned-interop-checked](API-protocol-pinned-interop-checked.md)), and
      Selectors do not match it.

25. **A kind binds itself to one artifact's shape.** The derived paths are those of the artifact
    `opamp-fleetctl package fetch` builds. A tree packed differently does not fit. The answer is to
    repack it with the tool, not to reopen a key.

26. **Every wrapped agent has an artifact document, pinned by two tests.**
    - The shape a kind runs and the tool packs is written once, in `docs/artifacts/<agent>.md`, for
      the wrapped kinds only.
    - `collector` needs none. Its artifact is installed as released upstream, found by the name in
      `binary`, and handed configurations it does not inspect. There is no second end to keep in
      step.
    - Each document has the same eight sections:
      1. source (repository, tag form, version reading);
      2. assets per platform and their naming;
      3. integrity (checksum form and location);
      4. treatment (as released, or which repack);
      5. shape in the delivered tree;
      6. what the Client derives from it, per platform, and why;
      7. the Configurations the tool uploads and what the agent reads them as;
      8. what can change upstream, with the test that goes red.
    - Each document is pinned by **one test per side**, `cfg`-gated per platform, and both tests
      name the document:
      - one against the tool's `Plan` (upstream file name, checksum source, action, output name);
      - one against the kind's constants (program name, `program_path`, derived directories,
        arguments).

27. **The program key is a bare file name.** `binary` (`collector`) and `command` (`command`) take
    a file name with no path separator and no `..`. It resolves to `program/<value>`, or to
    `program/tree/<program_path>` when the block names a tree. A wrapped kind names its own program
    (clause 6). Anything else fails at startup:
    - an **absolute path**, and on Windows a rooted path without a drive letter. The message names
      the block, the value and the way across: a program the fleet manages reaches the host as a
      package and is named here by its bare file name;
    - any other shape (`./x`, `a/b`, `../x`), with a message naming the rule.

    A bare name cannot escape the directory, so nothing has to be sanitised. It never searches
    `$PATH`.

28. **Every Supervisor accepts packages.**
    - Every program lives in a directory this Client owns, so every Supervisor's Agent declares
      `AcceptsPackages`. The capability is a constant, not a function of the configuration.
    - The Client prepares the `program/` directory at start, before the first package arrives.
    - A block carrying `accepts_packages` fails at startup, with a message saying the bare file
      name already makes the program updatable.
    - Startup logs, once per Supervisor, where its program is.

29. **A vendor agent is brought in by repacking.** An agent installed by the machine's package
    manager is not supervised here. It is repacked as a relocatable artifact and delivered as a
    package ([PKG-signed-packages-and-delivery](PKG-signed-packages-and-delivery.md)). The manual
    documents the fleet-delivered route for every agent it describes.

30. **The Client's own consent stays explicit.** `[packages] self_update` is the consent, and the
    one package it admits is the Client's own Agent type
    ([CLT-self-update-signed-package §2](CLT-self-update-signed-package.md)). Nothing about it is
    derived from a path. A package written over the Client takes the host out of reach, which is
    where implicit consent would be wrong.

31. **Placeholders name a Supervisor's own directories.**
    - In the `args` and `env` values of `collector` and `command` blocks, `${supervisor_dir}`
      expands to `<supervisor_dir>/<name>`. `${config_dir}` expands to
      `<supervisor_dir>/<name>/config`, where the received configuration's entries are written
      ([SRV-configurations-compose-one-map](SRV-configurations-compose-one-map.md)). So
      `args = ["-c", "${config_dir}/fluent-bit-conf"]` cannot drift.
    - They are expanded when the Supervisor starts. They are **never** expanded in the program key,
      whose written shape is what the operator reads.
    - An unrecognised `${…}` is passed through verbatim, neither refused nor emptied. A Foreign
      Agent's own configuration language may use the same syntax (Fluent Bit's does).

32. **A kind may lead its own process group, and is then stopped as a group.**
    - A kind whose agent leaves a worker behind when only its own process dies starts it in its own
      process group (`ProcessSpec::own_process_group`).
    - The Runner's stop then signals the group, so the escalation to `SIGKILL` cannot leave the
      worker behind.
    - Leading a group is opt-in per kind. Kinds with one process keep signalling the pid.
    - On Windows a program the second service starts runs in a job of its own. A stop ends the job
      with everything in it
      ([CLT-accounts-service-runs-under §19 and §20](CLT-accounts-service-runs-under.md)).
    - A program the Client starts itself on Windows has no group, and its stop ends the pid alone.

33. **A package is proved to run before it is installed.**
    - For every kind, the Runner runs the staged program once with a preflight its kind supplies.
      It does so after an artifact is unpacked to its staged name
      ([PKG-signed-packages-and-delivery](PKG-signed-packages-and-delivery.md), the staged name of
      an unpacked artifact), and before the swap.
    - A failure answers `PackageApplied{Err}`, carrying the dynamic linker's own message
      (*"version `GLIBC_2.39' not found"*, *"cannot open shared object file"*). Nothing is
      swapped, so a running Managed Process is never stopped for a package that cannot run.
    - A kind's preflight is its version arguments, with whatever environment its ADR adds.
      `command` uses its `version_args`, so a `command` block without them has no preflight.
    - The health gate and rollback of
      [PKG-signed-packages-and-delivery](PKG-signed-packages-and-delivery.md) remain the second
      line.

34. **The version probe and the preflight ask the program as its start runs it.**
    - `VersionProbe` and `Preflight` each take an optional working directory, inherited when none
      is named.
    - A kind that names a working directory for the start (clause 6) names it for both. The
      version is then asked of the program the Supervisor runs, and the preflight starts in the
      same place inside the staged tree.
    - The staged tree's root is known only once the package is staged. A preflight therefore
      writes `${staged}` for it. `${staged}` resolves to that root in the preflight's arguments, in
      the values of its environment and in its working directory.
    - A program handed paths into its own tree, or a tree carrying its own libraries, is thus proved
      against the staged tree rather than against what the running version leaves behind.
    - `VersionProbe` takes an environment too, empty by default. A kind whose start hands its
      program an environment the program needs to run at all hands the probe the same, pointing
      into the live tree. The library path of a tree carrying its own libraries is such an
      environment.
    - `collector` and `command` name no working directory and no environment for either.

**Out of scope:**
- loading a Plugin at run time, as a WebAssembly component or as a shared library (clause 12);
- any host function that reaches the network, and any host function of a take-over (clauses 4
  and 13);
- plain-HTTP polling on the Supervisor Endpoint;
- injecting the `opampextension` configuration into a Collector's configuration;
- connection pools larger than one (Gateway Mode,
  [CLT-modes-and-gateway](CLT-modes-and-gateway.md));
- reporting the kinds through `AvailableComponents`;
- a machine-readable artifact manifest shared by tool and kind;
- an opt-in reaping of orphaned directories;
- migrating a Supervisor tree when its root moves;
- a supervise-only mode for programs this Client does not install, which would need its own
  decision and capability model;
- how a multi-file tree is unpacked and swapped
  ([PKG-signed-packages-and-delivery](PKG-signed-packages-and-delivery.md));
- a group stop through a job object for a program the Client starts itself on Windows;
- everything [CLT-server-manages-the-supervisor-set](CLT-server-manages-the-supervisor-set.md)
  decides.

## Latitude

None.

## Alternatives considered

- **A synchronous state machine without I/O (sans-I/O).** The Plugin would answer each input with
  a list of requests, and the core would feed back each outcome as a new input, as `quinn-proto`
  does beneath `quinn`. Its effects are data, its tests are tables, and its guest needs no async
  runtime. It was proposed first, while async across the WebAssembly boundary looked immature.
  WASI 0.3 made asynchronous WIT functions part of the standard in June 2026, and Wasmtime runs
  them by default from version 46 on. The boundary is then as strong either way: a Plugin reaches
  only what the host offers. A kind with several steps in a row, as `icinga2` enrols, reads as
  code with a host it awaits. As a state machine every await becomes a state, and every future
  Plugin author pays for that. A fake host in memory tests the handler without stub programs.
- **A `Supervisor` trait with async lifecycle methods, as the superseded ADR rejected it.** It
  rejected async trait objects because they need `async-trait` or hand-rolled boxing, and because
  a core that awaits adapter methods couples itself to adapter timing. This ADR accepts the boxing.
  It bounds the timing by one handler call at a time per Plugin instance, by time arriving only as
  an input, and by the core's right to cancel a handler (clause 1).
- **Keep the factory and forbid direct I/O by a lint.** A lint can ban `std::fs` in a kind's
  module. It does not route a kind's subcommands through the second service, it confines no path,
  and the port still carries host resources a WebAssembly Plugin cannot take.
- **Capability handles instead of root-relative paths.** A Plugin would receive directory handles,
  as `cap-std` hands them out. A handle is a host resource, so a WebAssembly Plugin needs WIT
  resources and their lifetimes on both sides. A root and a relative path are data, and the core
  already confines paths through its no-follow adapter.
- **Agent knowledge in the core.** The core would implement `icinga2`'s enrolment itself. That
  moves what one agent needs into the core every Supervisor shares, against G-8.
- **An open host function to run any command.** It would make the closed vocabulary of clause 4
  meaningless. A program a Plugin runs comes from its own `program` root, through the same path as
  the Managed Process.
- **A WebAssembly runtime now.** It satisfies no present need. It adds a large dependency to the
  supply chain (Q-3) and a trust question for the components it loads. The port is shaped for it
  instead (clause 12).
- **A network function now.** No kind on `main` or on a feature branch needs one. A Plugin that
  needs an agent's API gets a host function, decided by the ADR that adds it (clause 4).
- **Take-over as a separate installer outside the Client.** It would act with rights the Client
  does not audit, and the fleet would not see it happen. Take-over through the core keeps one
  place that grants rights and reports (clause 13).
- **Dynamic plugin loading (shared libraries).** Rust has no stable ABI, so it means a C ABI
  boundary, unsafe code and version-skew handling for a third-party capability nobody needs; the
  registry keeps a new kind to one module.
- **A serde tagged enum instead of a registry and a two-stage parse.** It hard-codes every kind's
  settings into `config.rs` and cannot combine with `deny_unknown_fields`.
- **axum (or hyper) for the Supervisor Endpoint, or plain-HTTP support on it.** The endpoint serves
  one loopback WebSocket peer, which `tokio-tungstenite` already handles; the reference template is
  `ws://`, so HTTP polling would have no consumer.
- **One upstream connection per Supervisor.** The Server routes by `instance_uid` regardless, and
  n-over-1 is the model Gateway Mode generalises.
- **Injecting the extension configuration as `opampsupervisor` does.** It needs a YAML stack and
  templating in Rust; the operator pins `endpoint_port` on the Collector block and the delivered
  configuration names it.
- **A distinct `Reload` command beside `ApplyConfig`.** Nothing upstream asks for a bare reload, so
  it would have no sender; reload is how an apply is executed.
- **Operator-written lifecycle hooks (`reload_cmd`, `uninstall_cmd`, `reload_signal`) in TOML.**
  Kind knowledge belongs in the kind's module, written once and tested; a wrong hook silently
  misbehaves on a host.
- **Moving program resolution and package consent into the plugins.** A fleet-visible capability
  must derive from configuration the core reads.
- **Keep the derivable keys and fix the documentation, or keep them undocumented.** A template is a
  default that cannot be updated, and two sources of truth for a value the Client computes is how
  a host quietly differs from what the fleet believes.
- **Split `collector` into `otelcol` and `otelcol-contrib`.** It moves the distribution decision
  from `binary` into `type` and costs a second plugin for one lifecycle.
- **A machine-readable manifest per agent, read by tool and Client.** It needs a schema, a parser
  on each side and a policy for a manifest newer than the Client; two tests achieve the coupling.
- **Probing the unpacked tree at runtime, or a per-kind defaults file.** A wrong guess is silent on
  a host, and data nothing tests re-creates the transcription; a compiled-in constant fails a test
  when the artifact moves.
- **One list attribute for the kinds.** A Selector could match it only by spelling the whole list.
- **Every kind always compiled in.** It is the simpler build, but a host that runs one wrapped
  agent then carries the code, the attack surface and the reported capability of all of them.
- **A feature for `collector` and `command` too.** A build without either has no way left to run
  a Collector or an unwrapped agent; the two are the core's own paths, not a vendor's.
- **A self-update package derived from the build's kinds.** It names a package per combination of
  features, while the package name is the Client's Agent type in every build
  ([CLT-self-update-signed-package §2](CLT-self-update-signed-package.md)); the probation already
  rolls back a build that cannot read the host's blocks.
- **An `accepts_packages` flag beside a program path that may be absolute.** Two keys for one
  truth that can disagree, and the configuration could express what the filesystem forbids.
- **Probing writability at startup and deriving consent from it.** A fleet-visible capability would
  depend on a `chmod` nobody recorded and race a rollout when permissions change.
- **Supervising machine-installed programs without updating them.** Every package, version and
  rollback decision would carry a second case, the fleet's picture of a host would depend on which
  file a block came from, and an operator "fixing" a path to an absolute one would silently revoke
  a capability.
- **A separate `bin_dir`, or a `supervisor_dir` per block.** Two knobs for a separation nobody has
  asked for; one root per Client, laid out by
  [CLT-accounts-service-runs-under](CLT-accounts-service-runs-under.md), matches how the Client's
  own root is treated.
- **Keeping a bare name as a `$PATH` lookup and requiring `./` for the owned meaning.** A fourth
  case, noise in TOML, to keep behaviour fragile under a service manager's minimal `PATH`.
- **A `versions/` + `current` layout for Managed Processes.** A Managed Process is stopped before
  its swap and `.rollback` covers the fallback; only the running Client needs that layout.
- **Resolving relative arguments against the Supervisor's directory instead of placeholders, or
  exporting the paths as environment variables.** The same-looking rule would mean `program/` for
  one key and the Supervisor root for another, and nothing expands variables in `argv`. A general
  templating engine is far past the need.
- **Refusing unknown `${…}` placeholders.** It would break agents whose own syntax overlaps to
  catch a typo.
- **Run the version probe and the preflight in the Client's own working directory, with arguments
  as written and the probe with no environment.** A program that needs its tree root or its
  environment to run would then answer as another program would, or not at all, and a preflight
  could not hand the program a path into the staged tree. Both run as the start runs the program,
  and a preflight names the staged root `${staged}` (clause 34).
- **Leave an operator's `[attributes]` key under a kind's `supervisor.kind.*` key as written.** An
  operator could then claim for a Client a kind it was not compiled with, and a Selector would aim
  a Supervisor set at a Client that cannot run it. `[attributes]` refuses every `supervisor.*`
  key, so an operator cannot state one (clause 24).
- **Preflight by static inspection of required `GLIBC_` symbols.** A start attempt is the
  definition of "runs here", catches a missing library too, and yields the operator's message.

## Sources / Prior art

- [OpenTelemetry
  `opampsupervisor`](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/cmd/opampsupervisor),
  its
  [specification](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/specification/README.md)
  and its [embedded extension
  template](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/supervisor/templates/opampextension.yaml)
  (`ws://127.0.0.1:{{.SupervisorPort}}/v1/opamp`): local server, configuration handling, restart
  and stop behaviour; the generic `agent.executable` shape for a supervisor that does not install
  its agent; a per-supervisor `storage.directory` beside an absolute `agent.executable`; the
  remote-plus-local configuration merge; it manages only the agent it installs and has no
  Foreign-Agent path expansion (`opentelemetry-collector-contrib#36269`);
  `capabilities.accepts_remote_config` is a per-supervisor setting in its local file, `false`
  unless set, and without it the Collector runs on the local `agent.config_files` alone.
- [`opampextension`
  README](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/extension/opampextension):
  a client of both transports reporting effective configuration, health and components.
- [serde-rs/serde#1547](https://github.com/serde-rs/serde/issues/1547): `deny_unknown_fields`
  does not compose with `flatten`.
- [OpAMP specification](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md):
  `instance_uid` multiplexing; restart as the only process command; `AvailableComponents`.
- [Nomad task driver
  plugins](https://developer.hashicorp.com/nomad/docs/concepts/plugins/task-drivers):
  one driver interface over arbitrary process kinds, with stop and destroy as separate steps.
- [systemd service
  units](https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html):
  `ExecReload` and `reload-or-restart`.
- [Puppet package type and providers](https://www.puppet.com/docs/puppet/7/types/package.html): a
  uniform resource whose platform mechanics live in the provider; the manifest states intent.
- Elastic Agent / Fleet integrations (defaults that work unconfigured, paths resolved by the
  agent's own `paths` package) and Datadog Agent integrations (`conf.d/<name>.d/conf.yaml`,
  per-integration defaults): the tool that owns the installation knows the layout.
- [Elastic Agent version
  management](https://deepwiki.com/elastic/elastic-agent/6-version-management-and-upgrades):
  a versioned home with a symlink to the active executable; manages only binaries it installs.
- [Bindplane collector install and
  uninstall](https://docs.bindplane.com/deployment/virtual-machine/collector/install-and-uninstall-bindplane-collectors):
  managed vs detached collectors; removal deletes install directory and state as one act.
- [systemd.exec](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html):
  specifiers expand in arguments but not in the executable path; `StateDirectory=` exports
  `$STATE_DIRECTORY`.
- [WASI 0.3 launched](https://bytecodealliance.org/articles/WASI-0.3) and [WASI 0.3 on
  WASI.dev](https://wasi.dev/releases/wasi-p3): ratified June 2026; `async func`, `stream<T>`
  and `future<T>` in WIT; Wasmtime 46 the first release on the final 0.3.0, 43 to 45 on an
  incompatible release candidate.
- [Wasmtime and the Component
  Model](https://component-model.bytecodealliance.org/running-components/wasmtime.html) and the
  [Wasmtime v46.0.0 release
  notes](https://newreleases.io/project/github/bytecodealliance/wasmtime/release/v46.0.0): WASI
  0.3.0 and Component Model async enabled by default. The exact release date of 46 was not
  confirmed.
- [Writing I/O-free (sans-I/O) protocol
  implementations](https://sans-io.readthedocs.io/how-to-sans-io.html) and
  [`quinn-proto`](https://docs.rs/quinn-proto): the state-machine alternative, a deterministic
  core beneath an async layer, tested on simulated I/O.
- The code read for the revision: `ports.rs`, `collector.rs` and `command.rs` in
  `crates/fleet-agent/src/supervisor/` on `main`; `glpi.rs`, `icinga2.rs` and `telegraf.rs` on
  their feature branches, for every access to files, processes and the host they make.

## Consequences

- Positive: many Supervisors per Client, a Foreign Agent managed like a Collector, a new kind as
  one module, and an extension-carrying Collector visible through its own reporting, with no Server
  change, on the tested `instance_uid` routing.
- Positive: a wrapped agent's block is `type` and `name`, the same two lines on every platform; an
  artifact whose layout moves is one edit in the kind, reaching every host with the next Client
  version; the platform branches are compile-time constants a test can assert.
- Positive: reload-capable agents keep their state across configuration changes; retiring a
  Supervisor undoes what its installation did; a delivered agent never fails for a missing
  directory; a rollout can aim only at Clients carrying the kind.
- Positive: an upstream change surfaces as a red test on the side it touches, with a document
  saying what the other side owes.
- Positive: a build carries only the wrapped kinds its hosts run, and its Client Agent says which.
- Positive: an update writes only inside a directory the Client owns, with no root, no permissions
  on a system `bin` and no `.rollback` files beside system binaries, and hardening can grant write
  access to exactly one tree. `supervisor_dir` takes programs off a `noexec` or undersized `/var`.
- Positive: one kind of Managed Process. Every package feature has one case, and a capability can
  never depend on how a path was spelled; a locally written block and a delivered one describe the
  same thing.
- Positive: a Foreign Agent's configuration path follows a relocated root or a rename.
- Positive: the preflight protects every package: a Collector built against a newer libc is
  refused before the running one is stopped.
- Negative / trade-offs: losing the one upstream connection affects every Agent riding it, and a
  reconnect resends full state per Agent. An ephemeral Endpoint port cannot serve an
  extension-carrying Collector; the operator pins it.
- Negative / trade-offs: `APPLIED` means "restarted and survived the grace", not "validated"; a
  process that accepts a configuration and fails later surfaces as unhealthy. A reload leaves less
  evidence than a restart, so an adapter that cannot verify its reload says so in its outcome.
- Negative / trade-offs: supporting a new agent well costs a wrapper, a document and two tests;
  `command` keeps "not yet wrapped" usable, but such an agent applies by restart and runs in its
  program's directory.
- Negative / trade-offs: an Agent cannot be tagged per block before its first message; a Label
  covers it from the first exchange on. One attribute per kind reads worse than a list.
- Negative / trade-offs: every combination of features is a build that must keep compiling; the
  gate lints each wrapped kind alone and the build with none beside the default, on Linux. A build
  with fewer kinds cannot be kept apart by package name: its hosts withdraw the consent
  (`self_update = false`), or they take the full build at the next self-update.
- Negative / trade-offs: a kind is coupled to one artifact's shape; "bring your own tree" is a
  packaging question, not a configuration one. The documents' prose has no test behind it.
- Negative / trade-offs: a program installed by the machine cannot be supervised; adopting a host
  that already runs a vendor agent means a repack and a package rollout before the Agent appears.
- Negative / trade-offs: one copy of a program per Supervisor; an archive artifact is still written
  twice (unpacked), so the rename saving lands on bare-binary artifacts.
- Negative / trade-offs: an unknown placeholder passes through rather than failing, unlike an
  unknown key; an absolute path in an argument still works and can still drift.
- Negative / trade-offs: on Windows without a process account a kind whose agent forks a worker
  stops it by the pid alone; only the second service's job ends a whole tree there.
- Positive: a kind can no longer reach past the core. Every access to files, programs and the host
  passes the core's checks, and every program a Plugin runs goes through the second service where
  a process account is registered. `icinga2` can then run under a process account.
- Positive: a Plugin is tested against a host in memory, with no stub program and no file system.
- Positive: a WebAssembly Plugin is one more adapter behind an unchanged core.
- Positive: take-over has a place in the architecture and grants its rights in one place, the
  core.
- Negative / trade-offs: the port is a public interface in the making. Each new capability of a
  Plugin is a host function and costs an ADR that supersedes this one.
- Negative / trade-offs: the handler is a boxed future, so the trait needs the boxing the
  superseded ADR avoided. Timing between handler calls is the core's to keep honest.
- Negative / trade-offs: reading a large file through the port copies its bytes into the Plugin.
- Negative / trade-offs: the `icinga2` kind is rewritten against the port. `glpi` and `telegraf`
  change their wiring only. All three branches must follow this ADR before they land.
- Negative / trade-offs: the `state` directory is new in each Supervisor's directory. The tree in
  [CLT-accounts-service-runs-under §24](CLT-accounts-service-runs-under.md) does not show it, and
  [CLT-accounts-service-runs-under §21](CLT-accounts-service-runs-under.md) covers it only by its
  rule for what the table does not name.
- Follow-ups (by topic): the ADR that loads a WebAssembly Plugin, with runtime, interface
  versioning and the trust in a component; the ADR that builds take-over; a host function for an
  agent's API once a kind needs one; moving every citation of the superseded ADR to the clause
  that now carries it; the `state` directory in the layout of the next normalization;
  extension-configuration injection; plain-HTTP polling on the Endpoint; connection pools and
  their failure semantics; a verified-reload gate and making reload vs restart visible upstream;
  the kinds in `AvailableComponents` once it leaves *Development*; the next wrapped agent (Fluent
  Bit is the documented candidate); a machine-readable artifact manifest if wrapped agents
  multiply; pruning stale `.rollback` files on a schedule; a supervise-only mode as its own
  decision.

## Enforcement

- The boundary of the Plugin, in `crates/fleet-core/tests/dependency_direction.rs`:
  `a_plugin_names_only_the_port`. Each kind's module has the role `Plugin`. It may name the port
  module, `serde`, `toml` and the parts of `std` that do no I/O. It fails on `std::fs`,
  `std::process`, `std::net`, `tokio`, `libc`, `nix`, `crate::storage` and every other module of
  the crate (clauses 1 and 4).
- The port's data, in `crates/fleet-agent/src/supervisor/ports.rs`:
  `the_port_carries_no_host_resource`. It reads the port's types and fails on a path, a handle, a
  channel, a span or a secret type (clause 2).
- The host functions, in `crates/fleet-agent/tests/supervisor_process.rs`:
  `a_path_outside_its_root_fails_only_that_call`, `a_plugin_cannot_write_its_config_or_program`,
  `a_program_a_plugin_runs_goes_through_the_second_service`, and
  `the_handler_is_never_called_twice_at_once` (clauses 1, 3 and 4).
- Take-over and the network stay closed. The vocabulary of clause 4 has no function for either.
  `a_plugin_names_only_the_port` keeps a kind from reaching them itself (clauses 4 and 13).
- **Not mechanically decidable:** that the port stays expressible in WIT (clauses 2 and 12). No WIT
  file exists until the ADR that loads a WebAssembly Plugin. `the_port_carries_no_host_resource`
  decides the part a test can, and the review of a change to the port decides the rest.
- Registry, defaults and refusals, in `crates/fleet-agent/src/supervisor/mod.rs`:
  `every_wrapped_kinds_block_is_two_lines`, `an_unwrapped_kind_still_says_everything_itself`,
  `a_client_reports_the_kinds_it_was_compiled_with`,
  `the_fleets_timing_reaches_a_supervisor_that_says_nothing`,
  `a_build_carries_exactly_the_wrapped_kinds_its_features_name`,
  `a_kind_left_out_of_the_build_is_an_unknown_type`.
- `scripts/check-rust.sh` lints `fleet-agent` on Linux without any wrapped kind, and with each
  wrapped kind its `[features]` table names alone. It runs the tests without any wrapped kind. A
  smaller build thus stays compiling, and
  `a_build_carries_exactly_the_wrapped_kinds_its_features_name` decides something.
- `fleet-agent` carries no wrapped kind today. Its `[features]` table holds `default` alone, so the
  script lints no wrapped kind alone.
- Windows and macOS lint the default build only. Whoever builds a smaller build there checks it.
- A wrapped kind's own ADR names the tests that show, with that kind:
  - that its block needs neither a program nor a type;
  - that restating a derived value is refused (clause 20);
  - that its timing correction reaches its Supervisor (clause 23).
- Two-stage parse and retired keys: `supervisor_blocks_split_common_keys_from_plugin_settings`,
  `a_supervisor_block_needs_type_and_a_valid_name`, `duplicate_supervisor_names_are_rejected`,
  `attributes_describe_the_host_and_a_block_no_longer_tags_one_agent`
  (`crates/fleet-agent/src/config.rs`); `settings_parse_strictly` and
  `the_retired_keys_are_refused_by_name` in `command.rs`, `settings_parse_strictly` in
  `collector.rs`; `an_unreadable_configuration_resolves_the_update_in_flight`
  (`crates/fleet-agent/src/service/runtime.rs`).
- Agents over one connection and the Endpoint:
  `poll_reports_carries_every_agent_with_distinct_identities`,
  `a_reply_reaches_only_the_agent_its_uid_names` (`crates/fleet-agent/src/engine.rs`);
  `extension_reports_are_folded_into_process_events` (`supervisor/endpoint.rs`);
  `a_config_change_reaches_both_supervised_agents_over_one_connection`
  (`crates/fleet-agent/tests/e2e.rs`).
- Process management and the lifecycle vocabulary, in
  `crates/fleet-agent/tests/supervisor_process.rs`:
  `an_exiting_process_turns_unhealthy_and_is_restarted`,
  `a_process_surviving_the_apply_grace_is_acknowledged_applied`,
  `a_process_exiting_within_the_grace_fails_the_apply_and_stays_supervised`,
  `a_restart_command_cycles_the_process_without_a_config_ack`,
  `a_declared_reload_applies_without_a_restart`,
  `a_process_that_dies_on_the_reload_signal_is_restarted_instead`,
  `an_uninstall_stops_the_process_answers_and_exits`,
  `the_directories_an_agent_writes_into_are_made_before_it_runs`,
  `a_program_named_by_a_relative_path_still_starts_in_its_own_directory`; and
  `retiring_uninstalls_the_removed_and_only_stops_the_changed` (`engine.rs`),
  `a_collector_supervisor_passes_each_config_entry_as_a_config_flag`,
  `sigterm_stops_the_managed_process_and_the_client_cleanly`
  (`crates/fleet-agent/tests/supervisor.rs`).
- Artifact documents (clause 26): each wrapped kind's ADR names the two tests that pin its
  document, one in the kind's module and one in `crates/fleet-tools/src/fetch.rs`. Each names its
  document.
- Preflight (clause 33): `a_package_that_cannot_run_here_is_refused_without_stopping_what_runs`
  (`crates/fleet-agent/tests/supervisor_process.rs`) and
  `a_package_that_fails_the_configured_version_check_is_refused`
  (`crates/fleet-agent/tests/packages_e2e.rs`, through `command`'s `version_args`).
- Version probe (clause 11): `find_semver_extracts_the_first_strict_version_from_free_text`
  (`crates/fleet-agent/src/supervisor/process.rs`, the strict default);
  `the_probe_reports_a_version_description`, `a_failing_or_versionless_probe_stays_silent` and
  `a_swapped_binary_is_probed_again_for_its_version`
  (`crates/fleet-agent/tests/supervisor_process.rs`). A kind's own parser is tested with that kind.
- The probe and the preflight as the start runs them (clause 34):
  `the_version_probe_runs_through_the_second_service` (the probe's working directory and
  environment) and
  `the_preflight_runs_through_the_second_service` (`${staged}` resolved in the arguments, the
  environment and the working directory), in `crates/fleet-agent/src/supervisor/process.rs`.
- Directory and program, in `crates/fleet-agent/src/config.rs`:
  `the_supervisor_root_defaults_under_the_state_dir_and_is_relocatable` and
  `a_relative_state_dir_yields_absolute_directories` (the root of
  [CLT-accounts-service-runs-under §24](CLT-accounts-service-runs-under.md)),
  `a_bare_name_resolves_and_everything_else_is_refused`,
  `an_absolute_program_path_is_refused_and_names_the_way_across`,
  `the_retired_package_keys_are_refused`, `a_program_path_must_stay_inside_the_package`.
- Consent and start, in `crates/fleet-agent/src/supervisor/mod.rs`:
  `every_supervisor_declares_package_acceptance`,
  `installs_packages_reflects_declared_package_acceptance`,
  `a_tree_supervisor_prepares_its_root_and_leaves_the_tree_to_the_package`,
  `a_program_path_that_is_neither_fails_the_build`, `a_block_without_a_program_fails_the_build`,
  `an_orphaned_supervisor_directory_is_not_reaped_at_startup`
  ([CLT-accounts-service-runs-under §25](CLT-accounts-service-runs-under.md)).
- Placeholders: `the_placeholders_name_this_supervisors_own_directories` and
  `an_unknown_placeholder_is_passed_through_untouched`
  (`crates/fleet-agent/src/supervisor/ports.rs`),
  `a_command_supervisors_arguments_are_expanded_to_its_own_directories`
  (`crates/fleet-agent/tests/supervisor.rs`), `the_spec_carries_the_environment` (`collector.rs`).
