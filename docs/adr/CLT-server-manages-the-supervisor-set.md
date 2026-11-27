# CLT-server-manages-the-supervisor-set: The Server manages a Client's Supervisor set, the host may keep it or one Supervisor's configuration, and a repeated offer is applied once

- **Status:** 🟢 accepted
- **Date:** 2026-11-26
- **Deciders:** Markus Brigl
- **Applies to:** crates/fleet-agent/src/reconfigure.rs (the Supervisor-set apply), crates/fleet-agent/src/config.rs (the `server_manages_set` and `remote_config_disabled` keys), the capability set and the handling of a received or stored remote configuration of the Client Agent and of a Supervisor's Agent in crates/fleet-agent/src/supervisor/agent.rs and agent/remote_config.rs, the delivered-block check (`check_delivered_block`) in crates/fleet-agent/src/supervisor/mod.rs and what each kind states for it, the stored `remote-config.pb` and entry files in crates/fleet-agent/src/storage.rs, the `[supervisors]` section of supervisor.toml (`delivered_env`, `delivered_args`, `server_manages_set` and `remote_config_disabled`)

## Context

This ADR restates, unchanged in meaning, the clauses of
[CLT-supervisor-mode-and-kinds](CLT-supervisor-mode-and-kinds.md) that govern the Server-managed
Supervisor set, the two switches `server_manages_set` and `remote_config_disabled`, and the repeated
offer of a configuration. [CLT-wrappers-act-through-the-core](CLT-wrappers-act-through-the-core.md)
supersedes it, restates and revises the rest, and leaves these clauses to this ADR. The split
exists because an ADR is superseded whole, and the revision of the port does not touch these
clauses. A status line names one successor, so this ADR carries no `Supersedes` field.

What the fleet needs to manage on a Client is which Supervisors it runs. Everything else in
`supervisor.toml` ([CLT-installed-service-and-state](CLT-installed-service-and-state.md)), the
Server endpoint, the credential, the state directory and the instance name, is host-local trust and
wiring; a Server that could rewrite it could cut a Client off with one bad delivery. The
specification allows an Agent's effective configuration to merge in local configuration, and bounds
a remote configuration to its Supervisor and the Client's list of Supervisors (Q-5). The program a
Supervisor spawns is itself a trust anchor: a Server, or someone who has compromised it without the
package-signing key, that could name any program on the host would have fleet-wide code execution
that bypasses signature verification entirely.

Keeping a delivered block to a program from its own directory, a program that arrived as a signed
package, prevents that. It is not enough on its own: a block's `env` and `args` would pass to that
program unchecked, with placeholders expanded in both
([CLT-wrappers-act-through-the-core §31](CLT-wrappers-act-through-the-core.md)). A Server that
delivers a shared object as a configuration entry and then a block with `LD_PRELOAD` pointing at
it has the loader
run that object as the account the Managed Process runs under
([CLT-accounts-service-runs-under](CLT-accounts-service-runs-under.md)), and any signed program that
loads a plugin or a script named in its arguments reaches as far. A kind setting that names a file
the Supervisor reads reaches as far as the host's files: a kind that reads such a file and sends its
content to a peer the block names sends whatever file a delivered block points it at, to whatever
peer that block names. [`HARDENING.md`](../HARDENING.md) lists the bound on both under *What
already holds*. The specification puts security before convenience; what a compromised Server can
make a Supervisor block do on a host must stop at the signature. What an Agent's own configuration
language allows its process to do stays the product's.

Who manages a host's Supervisor set is one question, answered for the fleet and, by a switch, for
one host (clauses 13 to 19). Which Supervisors take their configuration from the Server is a
question of the same host-local section, answered per Supervisor (clauses 20 to 27).

A remote configuration is one of the channels by which the Server causes code to run on a host
([`HARDENING.md`](../HARDENING.md), *The channels that put code on the host*): the Server can
write any files into a Supervisor's own `config/` directory and restart its process, and what the
agent's configuration language allows, a command a configuration names, a Collector receiver
bound to any port, it allows as the process's account. Package delivery on the
same channel stops at a signature the operator's key makes
([PKG-signed-packages-and-delivery](PKG-signed-packages-and-delivery.md)); a remote configuration
has no such stop. An operator may run an agent whose configuration is too sensitive to take from a
compromised Server, or which another system already configures. A switch per Supervisor binds a
name, not an agent, while the Server manages the set: the Server can remove a listed Supervisor and
deliver the same agent under another name, which is not listed. Through the set the Server adds,
changes, restarts and purges Supervisors on the host. The Protocol Baseline allows remote
configuration to be switched off: *"Remote configuration capability can be disabled if necessary"*
(OpAMP specification, *Configuration*).

What the Server may change on a host is bounded by what the host takes, not by what the Server
sends. The Server manages the set of `[[supervisor]]` blocks through the Client Agent, and every
other key of `supervisor.toml`, the `[supervisors]` section among them, is host-local and out of
its reach (clause 1). A switch written inside a `[[supervisor]]` block would be one the Server
can delete by delivering the block again without it. The switches therefore live in
`[supervisors]`, the one per Supervisor keyed by the Supervisor's `name`, which is unique across
blocks and names its directory
([CLT-wrappers-act-through-the-core §15](CLT-wrappers-act-through-the-core.md)).

The Client Agent is built by `AgentState::new` in `crates/fleet-agent/src/supervisor/agent.rs` and
declares the constant `AGENT_CAPABILITIES`: `ReportsStatus`, `AcceptsRemoteConfig`,
`ReportsEffectiveConfig`, `ReportsRemoteConfig`, `ReportsHealth`,
`AcceptsOpAmpConnectionSettings`, `ReportsConnectionSettingsStatus`, `ReportsOwnMetrics`,
`ReportsOwnTraces` and `ReportsOwnLogs`. `build_engine` in
`crates/fleet-agent/src/supervisor/mod.rs` adds `ReportsHeartbeat` when heartbeats are enabled,
and `AcceptsPackages` with `ReportsPackageStatuses` when `[packages] self_update` consents and a
verification key is configured
([CLT-self-update-signed-package](CLT-self-update-signed-package.md)). Its effective configuration
is the redacted text of `supervisor.toml`. Only `AcceptsRemoteConfig` and `ReportsRemoteConfig`
concern the set. `AgentState` can be restored without both bits, and then ignores an offer that
arrives anyway and logs it once per hash in a bounded set (`ignored_configs`).

The Client Agent stores an applied set too. `config_applied` writes it with `store_remote_config`
once the Supervisor-set apply succeeded: `remote-config.pb` and one copy of each entry under
`config/`, both in `state_dir`. Nothing runs on those files, since the set they carry is already
in `supervisor.toml` (clause 5). Restoring them at start reports `RemoteConfigStatus` `APPLIED`
with the stored hash, which tells the Server the Client runs that set, and the Server offers a set
only while the reported hash differs
([SRV-configurations-compose-one-map](SRV-configurations-compose-one-map.md), the offer of a remote
configuration).

The Server offers a remote configuration only to an Agent declaring `AcceptsRemoteConfig`
([SRV-configurations-compose-one-map](SRV-configurations-compose-one-map.md), `offer` in
`crates/fleet-server/src/fleet/rollout.rs`), as the Baseline requires, and a Client's
capabilities bind what it reports and acts on
([SEC-connection-settings-never-plaintext](SEC-connection-settings-never-plaintext.md),
the Client's capabilities); read the other way, an Agent that does not declare a capability does
not act on it. A Client whose Client Agent, or a Supervisor's Agent, does not declare the bit is
therefore offered no remote configuration, with no change on the Server.

What a Supervisor runs without a remote configuration follows from how each kind finds its
configuration. Every kind reads it from the Supervisor's own `config/` directory,
`<supervisor_dir>/<name>/config/`, where `store_remote_config` writes one file per entry and
records roles in `.supplementary`
([SRV-configurations-compose-one-map](SRV-configurations-compose-one-map.md), how a received
configuration is stored):

- `collector` passes every unroled file in `config/` as its own `--config`, then the block's
  `args`; with no such file it does not start and reports *awaiting configuration*
  (`collector_spec`,
  [CLT-wrappers-act-through-the-core §18](CLT-wrappers-act-through-the-core.md)).
- `command` always starts with the block's `args` and `env`; whatever configuration the Foreign
  Agent reads is the one those arguments name, `${config_dir}` included.
- a wrapped kind points its agent at the entry its own ADR names, or at a root it finds by a role
  its ADR defines. With no such entry it either does not start and reports what it awaits, or
  starts the agent and the Runner reports its exit, as its ADR decides. A setting of a wrapped kind
  that names a file the Supervisor reads is the block's, wherever it points.

A Supervisor's stored `remote-config.pb` itself is read by no kind. It restores, at start, the
`RemoteConfigStatus` `APPLIED` with the stored hash and the configuration the Agent echoes as its
effective one (`AgentState::new`). What a kind runs on is the entry files beside it. Ignoring the
`.pb` alone would leave a listed Supervisor running the Server's last configuration from those
files.

A `[[supervisor]]` block configures its process too, not only the files in `config/`, and the
Server delivers the blocks. A `collector` or `command` block does it through `args` and `env`: a
Collector reads a whole configuration from `--config=yaml:…` or `--config=env:VAR`, and where the
operator set `[supervisors] delivered_args` or `delivered_env`, a block the Server delivers may
bring new ones (clause 10). The other keys of a block do it as well. A setting a wrapped kind keeps
is a decision about how its agent runs
([CLT-wrappers-act-through-the-core §22](CLT-wrappers-act-through-the-core.md)): which peer it
talks to, under what name, which certificate it trusts. Clause 11 lets a delivered block point a
file setting at a file inside its own `config_dir`. For a listed Supervisor no remote configuration
writes that file, so it does not exist, and a kind that falls back to a weaker default when a named
file is missing then runs the operator's agent on what the Server chose. For a listed Supervisor
any key a delivered block may change is a remote configuration under another name.

**One offer of a configuration reaches a Client twice in the normal case.** After a rollout, a
heartbeat that left the Client before the Server's offer arrived still names the old hash, and the
Server answers it with the offer again. A second apply would wait behind the first and restart the
Managed Process once more after the first had reported `APPLIED`: on a test host a flaky
end-to-end test, on a production host a needless restart. A package offer is treated the same way:
a repeat of the hash installed or in flight is ignored
([PKG-signed-packages-and-delivery §49](PKG-signed-packages-and-delivery.md)).

## Decision

We will let the Server replace the set of `[[supervisor]]` blocks, and nothing else in the file,
unless the host sets `[supervisors] server_manages_set = false`, while a delivered block may bring
no environment, arguments or host paths that reach past what its package's signature covers, and
let the operator name Supervisors in `[supervisors] remote_config_disabled`, whose Agents neither
declare nor act on remote configuration and run only on what the operator placed on the host, and
take an offered configuration whose hash an Agent is applying or has applied as a report, not a
second apply.

1. **The Server manages the set through the Client Agent, and only the blocks are read.**
    - A remote configuration offered to the Client Agent is a Configuration stated for the Agent
      type `supervisor`
      ([SRV-configurations-compose-one-map](SRV-configurations-compose-one-map.md)).
      The Client parses it entry by entry as TOML.
    - The union of the entries' `[[supervisor]]` blocks, in entry-name order, is the offered set.
      The Client ignores every other top-level key.
    - The boundary is enforced by what the Client takes, not by policing what the Server sends.
    - The offer fails on a duplicate `name` within or across entries, on an entry that is not TOML,
      and on a block the startup parser would refuse.

2. **An offered set is validated as startup would read it, before anything stops.**
    - The merge is the running file's globals with the offered blocks.
    - Each block passes the same checks startup applies:
      - kind known;
      - program key present and well-shaped;
      - `service_name`, `endpoint_port` and timing as this decision allows them;
      - retired keys refused;
      - the kind's strict settings parsed through the side-effect-free `Plugin::check`.
    - A set that fails is reported `FAILED` with the reason. Nothing is stopped, nothing is
      written, and the running set stays in force.

3. **A delivered block may name only a Client-owned program.** The apply path checks that every
    offered block's program resolves inside the Supervisor's own `program/`.
    [CLT-wrappers-act-through-the-core §27](CLT-wrappers-act-through-the-core.md) already
    refuses every other shape for every principal, so this check cannot fire today. It stays as
    defence in depth against a future shape, in the apply path that knows the block came from the
    Server.

4. **The apply is a diff keyed by Supervisor `name`, in a fixed order.** Comparing running and
    offered blocks yields removed, changed (any key differs), added and unchanged. Then:
    1. the removed and changed Supervisors stop, a removed one uninstalled
       ([CLT-wrappers-act-through-the-core §9](CLT-wrappers-act-through-the-core.md)) and a
       changed one only stopped, and their Agents send `agent_disconnect`;
    2. the merged document is written to `supervisor.toml`;
    3. the removed Supervisors' directories are purged (the purge of a removed Supervisor,
       [PKG-versions-high-water-mark-floor](PKG-versions-high-water-mark-floor.md));
    4. the changed and added Supervisors start from the file just written, an added one
       introducing itself as a new Agent on the running connection.

    Unchanged Supervisors keep running untouched. A failed write restarts the stopped Supervisors
    from the still-standing old file. A crash before the write restarts into the old file, and one
    after it into the new one. Both converge, because startup builds exactly what the file says.

5. **`supervisor.toml` remains the single truth.**
    - There is no overlay and no second file. After an apply the file is the configuration. It is
      the same file the operator reads and the Client Agent reports as its effective configuration,
      refreshed in the same step. An offline restart runs the delivered Supervisors because they
      are in it.
    - Only the `[[supervisor]]` array is replaced, by editing the document with `toml_edit`. The
      operator's comments, ordering and formatting survive byte for byte, and the offered blocks
      keep their text.
    - The write goes through a sibling temporary file renamed into place. On Unix it carries the
      existing file's mode. A file the write creates is `0600`, because it holds the credential.

6. **The status is honest.** The Client Agent reports:
    - `APPLYING` on receipt;
    - `APPLIED` once the file is written and the starts have succeeded;
    - `FAILED` when parsing, validation, the write or a start fails.

    A started Supervisor whose process later crashes is a health fact of that Supervisor's Agent,
    not a failed configuration.

7. **No offer, no change.** A Client to which no Supervisor set is offered runs its locally
    written blocks. The first applied offer replaces the local set. From then on, while
    `[supervisors] server_manages_set` is true, the Server's set is authoritative for that Client's
    Supervisors (clauses 13 to 19). The Client compares the offer against the file, not against the
    last offer.

8. **The purge never leaves the Supervisor root.** The purge unlinks a symlink where a
    Supervisor's directory belongs, and never follows it. It leaves alone, with a warning, a
    directory whose resolved path lies outside the root.

9. **A directory the purge cannot delete fails nothing.** The Supervisor is already stopped and
    the file written, so the set the Server asked for is running. A purge error, such as a file held
    open on Windows or a permission, is a warning naming the path. The purge renamed the directory
    to a tombstone before deleting it. The next start and the next purge remove the tombstone again
    ([CLT-accounts-service-runs-under §25](CLT-accounts-service-runs-under.md)).

10. **A delivered block brings no environment and no arguments the operator did not allow.**
    - In the apply path, a delivered block's `env` entries, `args` and `version_args` must equal
      those of the running block of the same name, or be allowed by the operator. The operator
      allows them in `supervisor.toml`'s `[supervisors]` section, whose keys the Server cannot
      change (clause 1):
      - `delivered_env` is a list of variable names, each exact or ending in `*` as a prefix
        (`["OTEL_*", "GOMAXPROCS"]`). It defaults to none.
      - `delivered_args = true` lets delivered blocks state arguments. It defaults to `false`.
    - Names are compared without regard to case, as Windows compares them.
    - Whatever the list says, the Client refuses a delivered `env` that adds or changes a variable
      steering which code a program loads: `PATH`, every `LD_*` and `DYLD_*`, `GCONV_PATH`,
      `GLIBC_TUNABLES`, `OPENSSL_CONF`, `OPENSSL_ENGINES`, `DOTNET_STARTUP_HOOKS`,
      `COR_PROFILER*`, `CORECLR_PROFILER*`, `NODE_OPTIONS`, `JAVA_TOOL_OPTIONS`, `_JAVA_OPTIONS`,
      `JDK_JAVA_OPTIONS`, `PYTHONPATH`, `PYTHONSTARTUP`, `PERL5LIB`, `PERL5OPT`, `RUBYOPT`,
      `BASH_ENV` and `ENV`.
    - It also refuses a new or changed value that contains `${config_dir}` or `${supervisor_dir}`.
      The Server delivers files there that no one signed, and an allowed name such as
      `OTEL_JAVAAGENT_EXTENSIONS` would load one.
    - The list is a floor, not a guarantee. `delivered_env = ["*"]` leaves only it between a
      delivered block and every other variable a program reads.
    - A block that breaks this fails the whole offer before anything stops (clause 2). The reason
      names the block and the variable or key.
    - A block the operator writes is unaffected: the rule is about what the Server may add.

11. **A delivered block names no host path outside its own configuration directory.**
    - A kind states which of its settings name a file the Supervisor reads, and confines them in a
      delivered block (`Plugin::check_delivered`). By default a kind names none.
    - In a delivered block each such setting must be `${config_dir}/` followed by a relative path
      with no `..`, `.` or root component. The one exception: it equals the running block's value,
      and the settings that say where the file's content goes are unchanged.
    - The kind's own ADR names those settings. It also names what else a delivered block of the
      kind must state so that the Server cannot choose whom the agent trusts. For an
      operator-written block that ADR stands as it is.

12. **What is checked is what is written.** The Client renders the rewritten `supervisor.toml`
    before anything stops, with every delivered table and its sub-tables placed in the order of the
    set. It refuses the offer unless the rendered file reads back as exactly the set that passed
    clauses 2, 10 and 11.

13. **The key `server_manages_set`.**
    - `[supervisors] server_manages_set` is a boolean and defaults to `true`. `true` is the
      behaviour clauses 1 to 12 describe, with the purge of a removed Supervisor
      ([PKG-versions-high-water-mark-floor](PKG-versions-high-water-mark-floor.md)) and the
      orphaned directories of [CLT-accounts-service-runs-under](CLT-accounts-service-runs-under.md).
    - The Client reads it when the configuration is loaded, like every key of the file. It never
      takes it from the Server, since the Supervisor-set apply replaces only the `[[supervisor]]`
      array (clauses 1 and 5).
    - A change takes effect at the next start of the Client.

14. **With `server_manages_set` false, the Client Agent declares neither remote-configuration
    capability.**
    - It is built without `AcceptsRemoteConfig` and without `ReportsRemoteConfig`.
    - Every other capability stays as the context lists it:
      - `ReportsStatus`;
      - `ReportsEffectiveConfig`, with `supervisor.toml` as its effective configuration;
      - `ReportsHealth`;
      - the connection-settings bits and the own-telemetry bits;
      - `ReportsHeartbeat` where enabled;
      - `AcceptsPackages` with `ReportsPackageStatuses`, where `[packages] self_update` consents
        and a verification key is configured.
    - Self-update is a separate consent
      ([CLT-wrappers-act-through-the-core §30](CLT-wrappers-act-through-the-core.md)), and the key
      does not touch it.
    - The Server then offers the Client no set
      ([SRV-configurations-compose-one-map](SRV-configurations-compose-one-map.md), the offer of a
      remote configuration).

15. **A set that arrives anyway is ignored.**
    - The Client does not store it, does not apply it, and reports no `RemoteConfigStatus`. No
      Supervisor stops or starts, and `supervisor.toml` is not written. The Baseline's rule for a
      part of a message the Agent does not support is that it *"SHOULD ignore it"*.
    - The Client logs a warning naming the offered hash, once per hash it has seen since start. It
      remembers at most as many hashes as it does for a Supervisor under clause 23 before it starts
      over.
    - The Supervisor-set apply itself refuses to run while the key is `false`, and logs the hash it
      was handed. No path that hands it a set gets past the key.

16. **A stored set from before is not restored, and is removed at start.** When the key is `false`
    and `remote-config.pb` exists in `state_dir`, the Client, before it connects:
    - does not restore it, so it reports no `RemoteConfigStatus` and no
      `last_remote_config_hash`;
    - deletes `remote-config.pb` first, so nothing that fails after it can leave the stored hash
      behind;
    - then deletes, as far as it can, each entry copy in `<state_dir>/config/` whose bytes are
      still the ones the stored map holds, as clause 24 compares for a Supervisor; it leaves every
      other file there;
    - deletes a `remote-config.pb` that does not decode, and leaves `config/` as it is;
    - logs once, naming the stored hash and every copy it kept or failed to remove.

    Nothing runs on these files, and the set they carry is already in `supervisor.toml`. A file
    that cannot be read or deleted is therefore a warning naming the error, and startup continues.
    Kept, `remote-config.pb` would be reported `APPLIED` again once the key returns to `true`. The
    Server would then not offer its set while that hash is unchanged, though the file may by then
    hold the operator's blocks. The warning for `remote-config.pb` itself says so.

17. **The blocks in the file stay, and are the operator's.**
    - Setting the key rewrites nothing. Every `[[supervisor]]` block stays as it is, including
      blocks an earlier applied set wrote, and the Client runs them as written.
    - From then on only the operator changes them. An operator who switches the key off reviews the
      blocks first, since some of them may be the Server's.
    - A Supervisor's own remote configuration is not affected. Each Supervisor's Agent keeps taking
      its configuration unless it is named in `remote_config_disabled`.
    - `delivered_args` and `delivered_env` keep their meaning, and have nothing to act on while no
      set is delivered.

18. **Together with `remote_config_disabled` it closes what that list leaves open.**
    - With the key `false` the Server can neither remove a listed Supervisor nor add the same agent
      under a name that is not listed.
    - The Server can still install a signed package into a Supervisor, restart a Managed Process,
      offer connection settings, and configure every Supervisor not named in
      `remote_config_disabled`.
    - A command a configuration can run runs as the account its Managed Process runs under. Such a
      command is a check or exec entry an agent's configuration names, or the configuration a
      `command` kind's program reads.
      - Where a process account is registered, that is the process account, which cannot write
        `supervisor.toml` ([CLT-accounts-service-runs-under](CLT-accounts-service-runs-under.md)).
      - Where none is, it is the Client's account. What the key holds against the Server then holds
        only while every Supervisor whose configuration language can run commands is named in
        `remote_config_disabled`. Otherwise the Server can configure such a Supervisor to rewrite
        `supervisor.toml`, and the key flips at the next start.
    - A signed Client build can also still replace the Client's own program where
      `[packages] self_update` consents
      ([CLT-self-update-signed-package](CLT-self-update-signed-package.md)), and that
      program reads the key.

19. **Switching `server_manages_set` back on hands the set to the Server again.** When the key
    returns to `true`, the next start declares both capabilities. It reports no hash, unless that
    earlier start warned that it failed to remove `remote-config.pb` itself. It is offered whatever
    set is released to the Client Agent
    ([SRV-deployments-and-pinned-rollouts](SRV-deployments-and-pinned-rollouts.md)). The first
    applied offer replaces the `[[supervisor]]` array, the operator's blocks included. It purges
    each Supervisor it removes (clause 7 and the purge of a removed Supervisor,
    [PKG-versions-high-water-mark-floor](PKG-versions-high-water-mark-floor.md)).

20. **The key `remote_config_disabled`.**
    - `[supervisors] remote_config_disabled` is a list of Supervisor names and defaults to empty.
    - The Client reads it when the configuration is loaded, like every key of the file. It never
      takes it from the Server: the Supervisor-set apply keeps the running file's globals and
      replaces only the `[[supervisor]]` array (clauses 1, 2 and 5).
    - A Server that deletes a listed Supervisor's block and delivers it again under the same name
      gets a Supervisor that is still listed.

21. **A name that matches no block is a notice, not a refusal.**
    - At startup the Client logs once, as a notice, each listed name that no `[[supervisor]]` block
      carries, because the set may arrive later from the Server.
    - It logs a listed name equal to the Client's own `name` and to no block with a notice that the
      Client Agent is not covered. Clauses 13 to 19 cover the Client Agent.
    - A value that breaks the instance-name grammar
      ([CLT-wrappers-act-through-the-core §15](CLT-wrappers-act-through-the-core.md)) fails startup
      naming the key and the value, because no block can ever carry it.

22. **A listed Supervisor's Agent declares neither remote-configuration capability.**
    - It is built without `AcceptsRemoteConfig` and without `ReportsRemoteConfig`.
    - Every other capability stays as it is: `ReportsEffectiveConfig`, `AcceptsRestartCommand`, and
      `AcceptsPackages` with `ReportsPackageStatuses` where a verification key is configured.
    - The Server then offers it no remote configuration
      ([SRV-configurations-compose-one-map](SRV-configurations-compose-one-map.md), the offer of a
      remote configuration), and nothing on the Server changes.

23. **A remote configuration that arrives anyway is ignored.**
    - The Client does not store it, writes no entry file, and hands nothing to the process adapter.
    - It reports no `RemoteConfigStatus`, since the Agent declares no `ReportsRemoteConfig`. The
      Baseline's rule for a part of a message the Agent does not support is that it *"SHOULD
      ignore it"*.
    - The Client logs a warning naming the Supervisor and the offered hash, once per hash it has
      seen since start. A Server that resends the same offer on every exchange thus does not flood
      the log.

24. **A stored remote configuration from before is removed at start, and the operator's files
    stay.** A listed Supervisor starts at Client startup, or when a delivered set adds or changes
    it. When a `remote-config.pb` then exists in its directory, the Client, before the kind is
    started:
    - deletes from `config/` each entry file the stored map names whose bytes are still the ones
      the map holds, and `.supplementary` if its bytes are still the ones the map would write;
    - leaves every other file in `config/` untouched, since the operator wrote or overwrote it;
    - deletes `remote-config.pb`;
    - logs once, naming the Supervisor, the stored hash, and every file it kept because its content
      had changed.

    The Client deletes a `remote-config.pb` that does not decode and leaves `config/` as it is. The
    warning then says that `config/` may still hold files the Server wrote. No status is restored,
    so a listed Supervisor reports no `RemoteConfigStatus` and no `last_remote_config_hash` after a
    restart.

    Storing an offer (`store_remote_config`) removes the previous entry files, writes the new ones,
    and writes `remote-config.pb` last.
    - A store cut short, by a crash or a failed write, therefore never leaves a new `.pb` beside the
      previous offer's files, which the comparison above would keep as the operator's.
    - What it can leave is the previous `.pb` beside some of the new offer's files. The previous map
      names none of those, so they stay in `config/`.
    - A stop after the first new entry file and before the `.pb` is thus the one window in which
      the drop cannot tell the Server's files from the operator's.

25. **What a listed Supervisor runs is the operator's.**
    - The kinds are unchanged. A listed Supervisor runs on the files the operator places in
      `<supervisor_dir>/<name>/config/`, under the names its kind reads (the context lists them).
      Roles go in `.supplementary`, in the format `storage.rs` writes.
    - For `collector` and `command` the process also gets the block's `args` and `env`. Those are
      the operator's, as clause 26 keeps them.
    - With remote configuration switched off, nothing else writes into that directory.
    - A listed Supervisor with no such file does what its kind does before its first remote
      configuration. A `collector` waits and says so. A wrapped kind waits, or lets its agent exit
      and be reported, as its ADR decides.

26. **A delivered block for a listed Supervisor repeats the running block whole.** In the
    Supervisor-set apply, the check of a delivered block (`check_delivered_block` in
    `crates/fleet-agent/src/supervisor/mod.rs`, called from `apply_inner` in
    `crates/fleet-agent/src/reconfigure.rs`) treats a block whose name is in
    `remote_config_disabled` apart. It ignores `delivered_args` and `delivered_env` (the
    `SupervisorsConfig` fields of `crates/fleet-agent/src/config.rs`) for that block, and holds the
    block to this:
    - **A block of that name runs:** the delivered block equals it entirely, compared as the parsed
      block.
      - That covers `type`, the core keys (`service_name`, `endpoint_port`, `stop_timeout_secs`,
        `apply_grace_secs`, `retain_previous_secs`, `program_path`) and every kind setting. Among
        them are `args`, `version_args` and `env`, and every setting a wrapped kind keeps.
      - A key the running block lacks must not appear. A key it has must not change or go.
      - Absent and empty differ.
    - **No block of that name runs, because the block is added:** the delivered block carries
      nothing beyond what naming its program requires.
      - That is `type` and `name`, plus the kind's program key where the kind does not name its own
        program: `binary` for `collector` and `command` for `command`.
      - A wrapped kind's block carries `type` and `name` alone, since a wrapped kind installs and
        names its own program and refuses the key
        ([CLT-wrappers-act-through-the-core §20](CLT-wrappers-act-through-the-core.md)).
      - A setting the kind keeps is then the operator's to write into the host's `supervisor.toml`.
        From then on the running block is the one a delivered block must equal.

    A block that breaks this fails the whole offer before anything stops, as clauses 2 and 10
    refuse a block. The set is reported `FAILED` with a reason naming the block and the key.
    Nothing is stopped, nothing is written, and the running set stays in force.

    Without this rule, the Server would configure a listed Supervisor through its block instead of
    its `config/`:
    - it would put a configuration on a Collector's command line or into its environment
      (`--config=yaml:…`, `--config=env:VAR`), where `delivered_args` or `delivered_env` allow it;
    - through the settings a wrapped kind keeps, it would change which peer the operator's agent
      talks to;
    - with a file setting pointed at a file no remote configuration writes, it would change what
      the agent trusts (the context describes how).

    Equality closes that route for every key, including keys a kind adds later. A list of the keys
    that matter would have to be kept current with every kind. The Server can still remove a listed
    block or deliver it unchanged. Delivering it unchanged is how a Server that manages the set
    keeps it.

27. **Switching it back on hands `config/` to the Server again.** When a name leaves the list, the
    next start declares both capabilities and reports no hash. It is offered whatever is released
    to that Agent ([SRV-deployments-and-pinned-rollouts](SRV-deployments-and-pinned-rollouts.md)).
    The first stored offer replaces every file in `config/`, the operator's included
    ([SRV-configurations-compose-one-map](SRV-configurations-compose-one-map.md), how a received
    configuration is stored).

28. **An offer of the hash applying or applied is a report, not an apply.**
    - A remote configuration whose `config_hash` is the hash of the status the Agent holds,
      `APPLYING` or `APPLIED`, is not stored. It is not handed to the process adapter or to the
      Supervisor-set apply, and it restarts nothing.
    - The Agent reports the status it holds again in its next message.
    - This holds for a Supervisor's Agent and for the Client Agent alike.
    - A Server answers a heartbeat that left the Client before the offer arrived with the offer
      again. One offer reaching a Client twice is thus the normal case, not a fault.
    - The Agent takes an offer of another hash as before, after the one applying.
    - The Agent applies a hash whose apply `FAILED` again when it is offered again. A host repaired
      after the failure thus takes the offer once more.
    - A deliberate restart stays the `Restart` command.
    - [PKG-signed-packages-and-delivery §49](PKG-signed-packages-and-delivery.md) treats a package
      hash already installed or in flight the same way.

**Out of scope:**
- reconciling a locally edited `[[supervisor]]` set against the last applied offer at startup;
- an opt-in reaping of orphaned directories;
- keeping some blocks from the Server while it manages the others;
- how the fleet view or the REST API shows that a host keeps its set, beyond the capability set
  the Server already lists;
- any change on the Server, the REST API or the Server's rudimentary UI for either switch (a
  production UI is a Non-Goal of the [specification](../SPECIFICATION.md), *Shipping a production
  UI*);
- the restart command and package delivery to a listed Supervisor, which stay as they are;
- what a listed Supervisor reports as its effective configuration when its process reports none
  (the empty map, as for any Supervisor before its first remote configuration).

## Alternatives considered

- **Apply every remote configuration received.** A heartbeat that crosses the Server's offer
  brings the same offer a second time, and its apply restarts the Managed Process once more after
  the first has reported `APPLIED`. An offer of the hash applying or applied is a report (clause
  28).
- **The Server holds back the offer while it is in flight.** A heartbeat that crosses an offer is
  a race of the Baseline that any Server can produce, and the Server cannot know whether the offer
  arrived. Only the Client knows what it is applying, so it decides (clause 28).
- **A repeated offer of a hash that failed is a report too.** A host repaired after the failure
  would then need a new hash to try again, which only a changed Configuration gives. The repeat of
  a failed hash is applied again; what a Server re-offers after a failure is the Server's
  (clause 28).
- **Compare the content of an offer, not its hash.** The hash is what the Server sends for the
  offer and what the status names; two offers of one hash are one configuration (clause 28).
- **A denylist of dangerous variables alone.** `LD_PRELOAD` is one of many; `PYTHONPATH`,
  `NODE_OPTIONS`, `JAVA_TOOL_OPTIONS`, `BASH_ENV` and every program's own plugin variable reach as
  far, and the list never ends. An allow-list the operator writes ends it; the loader variables are
  refused on top because no Managed Process needs them from the Server.
- **No delivered `env` or `args` at all.** Safe, but a fleet that steers its Collectors with
  `OTEL_*` variables or a Fluent Bit's `-c ${config_dir}/…` from the Server would lose that; the
  operator's allow-list keeps it where it is wanted.
- **Clearing the environment of every Managed Process.** The Client's own environment carries what
  the host's agents expect (proxies, locale); clearing it breaks operator-written blocks to protect
  against delivered ones.
- **Letting the Server deliver absolute program paths, or allowlisting some per host.** The first
  hands a Server arbitrary code execution outside package signing; the second is host-local policy
  for a capability the fleet path does not need. Warning and applying anyway is not a control.
- **The Server delivering the whole `supervisor.toml`.** Endpoint, credential and state directory
  are the host's trust anchors; one bad delivery could leave the Client unreachable.
- **A separate overlay file for the delivered set** (the `opampsupervisor` model). Two files would
  answer "what does this Client run", and an offline restart would depend on state the operator
  never sees.
- **Restarting the Client, or every Supervisor, to apply.** It cycles every healthy Supervisor to
  change one.
- **Failing an offer that carries a non-Supervisor key.** One stray global key would block the
  Supervisors that came with it; the fleet view shows the file that actually runs.
- **Re-serialising the file with the ordinary `toml` writer.** It destroys the operator's comments
  and layout after the first apply.
- **Keeping a removed Supervisor's data, keeping only its identity, a retention window, or renaming
  it aside.** A re-added Supervisor would resurrect a disconnected Agent's identity and stale
  configuration; a split directory is unreasoned; retention exists to roll back a living
  Supervisor, which a removed one is not; a renamed copy is a slower leak. The purge
  [PKG-versions-high-water-mark-floor](PKG-versions-high-water-mark-floor.md) decides removes the
  directory whole.
- **Reaping orphaned directories at startup.** Startup cannot tell a leftover from a deliberate
  hand edit, and the destructive reading deletes an identity that was not meant to go; the
  orphaned directories are reported as
  [CLT-accounts-service-runs-under](CLT-accounts-service-runs-under.md) decides.
- **Default `false` for `server_manages_set`.** Every existing Client would stop following the
  Server's set at upgrade, including hosts that took one, and G-1's loop would no longer close for
  Supervisor sets until each host's file says so. The switch is an operator's narrowing of what
  the Server may do on one host; the default keeps the behaviour clauses 1 to 12 describe.
- **A reserved name in `remote_config_disabled`** (the Client's own `name`, or a fixed name such
  as `supervisor`). The list holds Supervisor names under the instance-name grammar, so any
  reserved literal is a name a block can carry, and the Client's own `name` is the operator's to
  change. One key would mean two things: a Supervisor's configuration and the membership of the
  set. Clause 21 already answers the Client's own name in the list with a notice that it is not
  covered.
- **A key per `[[supervisor]]` block** (`managed_by_server = false`). The Server replaces the
  blocks (clause 1) and could drop the key by delivering the block without it. The decision is
  also about the set: adding a block is the change to stop, and no existing block can say that.
- **Keeping `AcceptsRemoteConfig` and answering every set, or every offer to a listed Supervisor,
  `FAILED`.** The Server would keep offering, the fleet view would show a refusal on every rollout,
  and the Agent would declare a capability it does not exercise.
- **Restoring the stored set, or leaving it on disk unrestored.** Restored, the Server sees a set
  reported `APPLIED` that the host no longer follows. Left on disk, switching back on restores its
  hash, and the Server does not offer an unchanged set to a file the operator may have changed
  since.
- **Failing startup when the stored set cannot be removed.** Clause 24 fails closed because a
  Supervisor runs on its files; nothing runs on these, and a refusal would take the host off the
  fleet for an inert file.
- **A top-level key outside `[supervisors]`.** `[supervisors]` already holds what the host allows
  the Server for its Supervisors (`delivered_env`, `delivered_args`, `remote_config_disabled`), and
  the Server never writes it.
- **A separate ADR for the switch.** It would make clause 7 untrue for some hosts without
  changing it; who manages a host's set is one decision.
- **A key inside the `[[supervisor]]` block to switch off its remote configuration**
  (`accepts_remote_config = false`). The Server replaces the blocks (clause 1), so it could switch
  the setting back on by delivering the block without the key. A delivered-block check that
  refuses dropping the key would make the operator's choice depend on a rule in the apply path,
  when it can depend on a section the apply path never writes.
- **A Client-wide switch of remote configuration for every Supervisor.** It is too coarse: a host
  that wants one agent's configuration kept local loses fleet configuration for every other agent
  it runs.
- **Ignoring only a listed Supervisor's `remote-config.pb` and leaving `config/` as it is.** The
  kinds read the entry files, not the `.pb`, so the Server's last configuration would go on running
  under a switch that says it does not.
- **Wiping the whole `config/` directory at start.** It would delete the files an operator placed
  there before the restart that put the switch in force.
- **Moving the Server's stored files aside instead of deleting them.** They can carry secret
  material ([SRV-configurations-compose-one-map](SRV-configurations-compose-one-map.md), how a
  received configuration is stored), and nothing needs them back: switching back on reports no hash,
  so the Server offers again what is released.
- **A separate host path for a listed Supervisor's local configuration.** It would be a second
  place each kind reads from, and a block key the Server could rewrite. `config/` is already where
  every kind looks.
- **Letting `delivered_args` and `delivered_env` stand for a listed Supervisor.** They exist to let
  the fleet steer a Supervisor's process. For a listed one the same keys would let the Server
  deliver a configuration on the command line or in the environment, and the switch would not mean
  what it says.
- **Holding only `args`, `version_args` and `env` of a listed Supervisor's block to the running
  block.** It closes the command line and the environment and leaves every other key to the bounds
  of clauses 10 and 11. The settings a wrapped kind keeps then stay deliverable, among them which
  peer its agent talks to, and a file setting pointed at a file no remote configuration writes
  leaves the kind on whatever weaker default it falls back to, with a peer the Server chose. Any
  such list has to be kept current with every kind, and equality needs no list.
- **Letting an added listed block carry kind settings within the bounds of clauses 10 and 11.** The
  same keys would reach the agent through the block that adds it, before the operator wrote
  anything. An added block carries what names its program, and the operator configures the rest on
  the host.
- **Refusing startup when a listed name matches no block.** A Server that delivers that block later
  is the normal case, and the refusal would stop the whole Client for it.
- **Leave a directory the purge cannot delete as an orphan.** The orphan stays beneath the
  Supervisors' root with nothing to remove it. The purge renames the directory to a tombstone first,
  which the next start and the next purge remove again
  ([CLT-accounts-service-runs-under §25](CLT-accounts-service-runs-under.md); clause 9).
- **Run every command a configuration can run as the Client's account.** Where a process account is
  registered, that puts a command a Server can deliver above the Managed Process it belongs to. It
  runs as its Managed Process's account instead
  ([CLT-accounts-service-runs-under](CLT-accounts-service-runs-under.md); clause 18).

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
- [OpAMP specification
  v0.19.0](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)
  and [v0.20.0](https://github.com/open-telemetry/opamp-spec/blob/v0.20.0/specification.md):
  `EffectiveConfig` may merge local configuration; `RemoteConfigStatuses`; `agent_disconnect` as an
  Agent's last word; `AcceptsPackages` as a per-Agent capability. For the switches, v0.20.0
  *Configuration* (*"Remote configuration capability can be disabled if necessary"*; *"If the bit
  is not set the Server MUST not offer a remote configuration to the Agent"*) and
  *AgentToServer.capabilities* (an Agent that does not support a capability *"SHOULD ignore"* the
  part of a message that belongs to it). Further in v0.20.0: the Client MUST set
  `AcceptsRemoteConfig` if the Agent can accept a remote configuration; an Agent MAY update its
  capabilities; the `AgentCapabilities` enum (`AcceptsRemoteConfig`, `ReportsRemoteConfig`,
  `ReportsEffectiveConfig`); *Interoperability of Partial Implementations*.
- [`toml_edit`](https://docs.rs/toml_edit): format- and comment-preserving TOML editing, what
  `cargo add` edits manifests with.
- [Debian FAQ: remove vs purge](https://www.debian.org/doc/manuals/debian-faq/uptodate.en.html):
  purge semantics, chosen because no operator is present after a Server-driven removal.
- [`HARDENING.md`](../HARDENING.md): the bound on a delivered block's environment, arguments and
  file settings under *What already holds*, and *The channels that put code on the host*.
- The code read for the switches: `AGENT_CAPABILITIES`, `AgentState::new`, `restore`, `handle`,
  `apply` and `config_applied` in `crates/fleet-agent/src/supervisor/agent.rs`; `build_engine`,
  `start_supervisor` and `remote_config_disabled_notices` in
  `crates/fleet-agent/src/supervisor/mod.rs`; `SupervisorsConfig` in
  `crates/fleet-agent/src/config.rs`; `store_remote_config`, `drop_remote_config` and
  `config_entries` in `crates/fleet-agent/src/storage.rs`; the self-Agent dispatch in
  `crates/fleet-agent/src/engine.rs`; `apply_inner` in `crates/fleet-agent/src/reconfigure.rs`;
  each kind's configuration source in `collector.rs` and `command.rs`; and the
  `AcceptsRemoteConfig` gate, `offer`, in `crates/fleet-server/src/fleet/rollout.rs`.

## Consequences

- Positive: Supervisors are fleet-manageable end to end with the same rollout and Selector scoping
  as every Configuration; unchanged Supervisors ride through an apply; a delivered set can only run
  programs that arrive as verified packages.
- Positive: a removal is complete, with no program-sized leftovers and no stale identity.
- Positive: an operator can stop the Server from adding, changing or purging Supervisors on a host
  with one line the Server does not write (Q-1); with `remote_config_disabled`, a listed agent can
  no longer reach the Server's configuration under another name, which closes on a host that sets
  both what the list alone leaves open.
- Positive: either switch needs no change on the Server, and the fleet view shows it through the
  capability set, which lacks `AcceptsRemoteConfig`; the Server offers such an Agent nothing however
  a rollout is aimed.
- Positive: an operator can close the configuration channel for one agent with one line the Server
  cannot change. A compromised Server can then no longer write that agent's configuration or run
  what its configuration language allows (Q-1). Signed package updates still reach it.
- Positive: switching off takes the Server's last configuration out of force at the next start
  without destroying what the operator put in its place.
- Negative / trade-offs: after the first applied offer, while the Server manages the set, a local
  edit to the blocks drifts silently until the next rollout overwrites it.
- Negative / trade-offs: removal is destructive and final; a Supervisor removed by a mis-scoped
  rollout loses its identity and its program. A crash between write and purge leaves an orphan
  only a human removes.
- Negative / trade-offs: `server_manages_set = false` holds against the Server only while every
  Supervisor whose configuration language can run commands as the Client's account is in
  `remote_config_disabled`, and a signed Client build can still replace the program that reads it
  where `[packages] self_update` consents (clause 18).
- Negative / trade-offs: a host that keeps its set is managed by hand. A Supervisor set released to
  it reaches nothing, and G-1's loop does not close for the Client Agent there by design.
- Negative / trade-offs: the blocks an earlier set wrote stay in force without notice when the
  switch goes off; the operator has to review them, and the manual says so. Switching back on
  replaces the operator's blocks with the first applied set.
- Negative / trade-offs: a listed Supervisor is configured by hand on its host. A rollout aimed at
  it releases nothing it receives, and G-1's loop does not close for that Agent by design.
- Negative / trade-offs: `remote_config_disabled` binds a name, not an agent. While the Server
  still manages the set it can remove a listed Supervisor and add the same agent under a name that
  is not listed. That Agent is new, installed afresh from a signed package and visible as such in
  the fleet, but its configuration is the Server's; `server_manages_set = false` closes that.
- Negative / trade-offs: a change to either key takes effect at the next start of the Client, like
  every other key of the file.
- Negative / trade-offs: the per-Agent capability set is not the same on every host, for the
  Client Agent or for every Supervisor. The [`CONFORMANCE.md`](../CONFORMANCE.md) rows for
  `AcceptsRemoteConfig` and `ReportsRemoteConfig` must say they can be withdrawn, for the Client
  Agent and per Supervisor (G-12).
- Negative / trade-offs: a Server that manages the set must deliver a listed block exactly as it
  runs. A change the operator makes to it on the host has to reach the Server's copy before the
  next delivered set, or that set fails.
- Follow-ups (by topic): startup reconciliation of a locally edited set; an editor for Supervisor
  sets in the Server's rudimentary UI; an opt-in reap of orphaned directories; a purge option on
  `service uninstall`; keeping chosen blocks from the Server while it manages the rest; showing in
  the fleet view that a host keeps its set; reporting a listed Supervisor's local files as its
  effective configuration when its process reports none; storing an offer so that a stop part-way
  leaves no file the drop of clause 24 cannot attribute.

## Enforcement

- Delivered environment, arguments and rendering, in
  [`crates/fleet-agent/src/reconfigure.rs`](../../crates/fleet-agent/src/reconfigure.rs):
  `a_delivered_block_may_not_add_environment_the_operator_did_not_allow`,
  `a_loader_variable_is_refused_whatever_the_operator_allowed`,
  `a_delivered_block_keeps_the_environment_it_already_runs_with`,
  `delivered_arguments_need_the_operators_consent`,
  `a_delivered_value_may_not_point_into_its_own_directories` (clause 10),
  `delivered_tables_from_two_entries_keep_their_sub_tables` (clause 12). The confinement of clause
  11 is tested with the kind that states a file setting, by the tests its ADR names.
- The set apply, in `crates/fleet-agent/src/reconfigure.rs`: `foreign_top_level_keys_are_ignored`,
  `duplicate_supervisor_names_fail_the_offer`, `a_malformed_block_names_its_entry`,
  `blocks_are_collected_across_entries_in_name_order`,
  `a_server_delivered_block_may_not_name_an_absolute_program`,
  `a_server_delivered_block_naming_a_bare_program_is_accepted`,
  `a_delivered_collector_binary_must_be_owned_too`,
  `the_write_replaces_blocks_and_keeps_the_operators_file`,
  `the_rewrite_keeps_the_files_restrictive_mode`, `a_freshly_created_file_is_owner_only`,
  `the_plan_is_a_diff_by_name_and_unchanged_blocks_ride_through`,
  `the_purge_deletes_exactly_the_removed_supervisors_directory` (the purge of
  [PKG-versions-high-water-mark-floor §13](PKG-versions-high-water-mark-floor.md)),
  `the_purge_does_not_follow_a_symlink_out_of_the_supervisors_root`,
  `an_empty_offer_removes_every_block`; `retiring_uninstalls_the_removed_and_only_stops_the_changed`
  (`crates/fleet-agent/src/engine.rs`); and end to end
  `a_config_change_reaches_both_supervised_agents_over_one_connection`
  (`crates/fleet-agent/tests/e2e.rs`), which adds, keeps and removes a Supervisor through a
  delivered set and checks the purge.
- The switch `server_manages_set` (clauses 13 to 19):
  `server_manages_set_defaults_to_true_and_reads_false` (`crates/fleet-agent/src/config.rs`,
  clause 13);
  `the_own_agent_of_a_host_that_keeps_its_set_declares_neither_remote_config_capability`,
  `a_set_offered_anyway_to_a_host_that_keeps_it_is_neither_stored_nor_applied_nor_reported` and
  `a_set_ignored_by_a_host_that_keeps_it_is_logged_once_per_hash`
  (`crates/fleet-agent/src/supervisor/agent.rs`, clauses 14 and 15);
  `the_supervisor_set_apply_refuses_to_run_on_a_host_that_keeps_its_set`
  (`crates/fleet-agent/src/transport/mod.rs`, clause 15);
  `a_host_that_keeps_its_set_drops_the_stored_set_and_reports_no_status`,
  `a_stored_set_that_cannot_be_removed_does_not_stop_startup`,
  `a_copy_that_cannot_be_removed_still_leaves_no_hash_to_report`,
  `an_undecodable_stored_set_is_deleted_and_config_is_left_alone` and
  `the_removed_stored_set_is_logged_naming_its_hash`
  (`crates/fleet-agent/src/supervisor/mod.rs`, clauses 14, 16 and 19); and end to end
  `a_server_offers_no_supervisor_set_to_a_host_that_keeps_it` (`crates/fleet-agent/tests/e2e.rs`,
  clauses 14, 17 and 19).
- The switch per Supervisor (clauses 20 to 27):
  - `crates/fleet-agent/src/config.rs`:
    `remote_config_disabled_defaults_to_empty_and_lists_supervisor_names` (clause 20),
    `a_remote_config_disabled_name_outside_the_instance_name_grammar_fails_startup` (clause 21).
  - `crates/fleet-agent/src/supervisor/agent.rs`:
    `a_supervisor_with_remote_config_disabled_declares_neither_remote_config_capability` (clause
    22: the report's `capabilities` lack both bits and keep `ReportsEffectiveConfig`,
    `AcceptsRestartCommand` and the package bits),
    `a_remote_config_offered_anyway_is_neither_stored_nor_applied_nor_reported` (clause 23: no
    `remote-config.pb`, no entry file, no pending apply, no `remote_config_status` in the next
    report), `an_ignored_remote_config_is_logged_once_per_hash` (clause 23).
  - `crates/fleet-agent/src/supervisor/mod.rs`:
    `a_listed_name_without_a_block_is_a_notice_not_a_refusal` (clause 21),
    `a_listed_supervisor_drops_the_stored_remote_config_and_keeps_the_operators_files` (clause 24:
    unchanged entries and `.supplementary` deleted, an overwritten entry and an operator's extra
    file kept, `remote-config.pb` gone, before the kind starts),
    `an_undecodable_stored_remote_config_is_deleted_and_config_is_left_alone` (clause 24),
    `a_listed_supervisor_reports_no_remote_config_status_after_a_restart` (clause 24),
    `the_clients_own_agent_keeps_accepting_its_supervisor_set_when_its_name_is_listed` (clause 21).
  - `crates/fleet-agent/src/reconfigure.rs`:
    `a_delivered_set_cannot_switch_remote_config_back_on_for_a_listed_name` (clause 20: a set that
    removes and re-adds the listed block leaves the started Agent without `AcceptsRemoteConfig`,
    and the written file keeps `[supervisors]` byte for byte),
    `a_delivered_block_for_a_listed_supervisor_must_equal_the_running_block_whole` (clause 26:
    with `delivered_args = true` and `delivered_env = ["*"]`, a delivered block for a listed name
    that changes `args`, `version_args`, an `env` entry or a core key, adds a key or drops one
    fails the offer naming the block and the key; the running block delivered back passes),
    `an_added_listed_supervisor_carries_only_what_names_its_program` (clause 26: with no running
    block, `type`, `name` and `binary` for a `collector`, and `command` for a `command`, pass; any
    further key, an empty `args` or `env` among them, fails the offer naming it).
  - `crates/fleet-agent/src/storage.rs`:
    `a_store_cut_short_leaves_no_previous_entry_file_beside_a_new_pb` (clause 24: a store cut
    short while it writes the new entry files leaves the previous `remote-config.pb` beside no
    previous entry file, so nothing of the previous offer stays unattributed).
  - `crates/fleet-agent/tests/supervisor.rs`:
    `a_listed_collector_runs_on_the_entries_the_operator_placed` (clause 25).
  - `crates/fleet-agent/tests/e2e.rs`:
    `a_server_offers_no_configuration_to_a_listed_supervisor` (clauses 22 and 27: a released
    Configuration reaches the unlisted Supervisor and not the listed one over the same
    connection).
- Clause 28, in `crates/fleet-agent/src/supervisor/agent.rs`:
  `a_repeated_offer_of_the_hash_applying_is_reported_not_applied`,
  `a_repeated_offer_of_the_hash_applied_restarts_nothing`,
  `the_client_agent_applies_a_repeated_offer_of_its_set_once`,
  `an_offer_of_another_hash_is_applied_after_the_one_applying`,
  `a_repeated_offer_of_a_hash_that_failed_is_applied_again`; and in
  `crates/fleet-agent/tests/e2e.rs`
  `a_config_change_reaches_both_supervised_agents_over_one_connection`, whose Server's offer is
  delayed past a heartbeat.
