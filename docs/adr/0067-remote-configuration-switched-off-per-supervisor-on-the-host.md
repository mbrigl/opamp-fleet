# ADR-0067: Remote configuration can be switched off per Supervisor on the host

- **Status:** 🟢 accepted
- **Date:** 2026-10-06
- **Deciders:** Markus Brigl
- **Applies to:** the `remote_config_disabled` key of the `[supervisors]` section of supervisor.toml and its parsing in crates/fleet-agent/src/config.rs, the capability set and the handling of a received or stored remote configuration of a Supervisor's Agent in crates/fleet-agent/src/supervisor/agent.rs, the Supervisor start and the delivered-block check (`check_delivered_block`) in crates/fleet-agent/src/supervisor/mod.rs and the Supervisor-set apply in crates/fleet-agent/src/reconfigure.rs, and the stored `remote-config.pb` and entry files in crates/fleet-agent/src/storage.rs

## Context

A remote configuration is one of the channels by which the Server causes code to run on a host
([`HARDENING.md`](../HARDENING.md), *The channels that put code on the host*): the Server can
write any files into a Supervisor's own `config/` directory and restart its process, and what the
agent's configuration language allows — a Telegraf `inputs.exec`, an Icinga `CheckCommand`, a
Collector receiver bound to any port — it allows as the process's account. Package delivery on the
same channel stops at a signature the operator's key makes
([ADR-0042](0042-signed-package-delivery-from-allowed-sources.md)); a remote configuration has no
such stop. An operator who runs an agent whose configuration is too sensitive to take from a
compromised Server, or which another system already configures, has today no way to say so on the
host: every Supervisor's Agent declares `AcceptsRemoteConfig` as part of the constant
`AGENT_CAPABILITIES` in `crates/fleet-agent/src/supervisor/agent.rs`. The specification puts
security before convenience (Q-1), and the Baseline expects this switch: *"Remote configuration
capability can be disabled if necessary"* (OpAMP specification, *Configuration*).

What the Server may change on a host is bounded by what the host takes, not by what the Server
sends. The Server manages the set of `[[supervisor]]` blocks through the Client's own Agent, and
every other key of `supervisor.toml` — among them the `[supervisors]` section — is host-local and
out of its reach ([ADR-0051](0051-a-delivered-block-brings-nothing-past-the-signature.md) clause
7). A switch written inside a `[[supervisor]]` block would be one the Server can delete by
delivering the block again without it. The switch therefore lives in `[supervisors]`, keyed by
the Supervisor's `name`, which is unique across blocks and names its directory
([ADR-0015](0015-supervisor-mode-and-its-kinds.md) clause 3).

The Server already honours the capability: it offers a remote configuration only to an Agent
declaring `AcceptsRemoteConfig` ([ADR-0016](0016-configurations-and-the-rest-api.md) clause 6,
`offer` in `crates/fleet-server/src/fleet.rs`), as the Baseline requires (*"If the bit is not set
the Server MUST not offer a remote configuration to the Agent"*). A Server's declaration binds what
the Client reports, and an undeclared capability is not exercised on the Client's end either
([ADR-0060](0060-connection-settings-offered-without-a-credential-and-server-capabilities.md)
clauses 12 and 17); the same rule read the other way means an Agent that does not declare a
capability does not act on it.

What a Supervisor runs without a remote configuration follows from how each kind finds its
configuration. Every kind reads it from the Supervisor's own `config/` directory,
`<supervisor_dir>/<name>/config/` ([ADR-0051](0051-a-delivered-block-brings-nothing-past-the-signature.md)
clause 1), where `store_remote_config` writes one file per entry and records roles in
`.supplementary` ([ADR-0016](0016-configurations-and-the-rest-api.md) clause 9):

- `collector` passes every unroled file in `config/` as its own `--config`, then the block's
  `args`; with no such file it does not start and reports *awaiting configuration*
  (`collector_spec`, [ADR-0015](0015-supervisor-mode-and-its-kinds.md) clause 6).
- `command` always starts with the block's `args` and `env`; whatever configuration the Foreign
  Agent reads is the one those arguments name, `${config_dir}` included.
- `telegraf` starts with `--config <config_dir>/telegraf-conf`; with no such file Telegraf exits and
  the Runner reports it ([ADR-0028](0028-glpi-agent-and-telegraf.md)).
- `glpi` starts with `--conf-file=<config_dir>/glpi-agent-conf` ([ADR-0028](0028-glpi-agent-and-telegraf.md)).
- `icinga2` takes as its root the entry with `role = "main"` in `.supplementary`, else
  `icinga2-conf`; with neither it does not start and reports *awaiting Icinga's root
  configuration* ([ADR-0029](0029-icinga-2.md)). Its `ticket_file` and `trusted_cert_file` are
  the block's, wherever they point.

The stored `remote-config.pb` itself is read by no kind. It restores, at start, the
`RemoteConfigStatus` `APPLIED` with the stored hash and the configuration the Agent echoes as its
effective one (`AgentState::new`). What a kind runs on is the entry files beside it. Ignoring the
`.pb` alone would leave a listed Supervisor running the Server's last configuration from those
files.

A `[[supervisor]]` block configures its process too, not only the files in `config/`, and the
Server delivers the blocks. A `collector` or `command` block does it through `args` and `env`: a
Collector reads a whole configuration from `--config=yaml:…` or `--config=env:VAR`, and where the
operator set `[supervisors] delivered_args` or `delivered_env`, a block the Server delivers may
bring new ones ([ADR-0051](0051-a-delivered-block-brings-nothing-past-the-signature.md) clause
18). The other keys of a block do it as well. An `icinga2` block's `node_name` and `parent_host`
say which parent the agent enrols with and under what name, and its `trusted_cert_file` pins
that parent's certificate. ADR-0051 lets a delivered block point `trusted_cert_file` at a file
inside its own `config_dir`. For a listed Supervisor no remote configuration writes that file,
so it does not exist. `ensure_enrolled` in `crates/fleet-agent/src/supervisor/icinga2.rs` skips
a `trusted_cert_file` that is not a file and falls back to `pki save-cert`, trust on first use.
A delivered block that changes the parent and points the pin at a missing file therefore enrols
the operator's agent with a parent the Server chose, trusting whatever certificate it presents.
For a listed Supervisor any key a delivered block may change is a remote configuration under
another name.

## Decision

We will let the operator name Supervisors in `[supervisors] remote_config_disabled` of
`supervisor.toml`, whose Agents neither declare nor act on remote configuration and run only on
what the operator placed on the host, while the Client's own Agent and the Server stay as they are.

1. **The key.** `[supervisors] remote_config_disabled` is a list of Supervisor names and defaults
   to empty. It is read when the configuration is loaded, like every key of the file, and is never
   taken from the Server: the Supervisor-set apply keeps the running file's globals and replaces
   only the `[[supervisor]]` array ([ADR-0051](0051-a-delivered-block-brings-nothing-past-the-signature.md)
   clauses 7, 8 and 11). A Server that deletes a listed Supervisor's block and delivers it again
   under the same name gets a Supervisor that is still listed.

2. **A name that matches no block is a notice, not a refusal.** At startup each listed name that
   no `[[supervisor]]` block carries is logged once as a notice naming it, because the set may
   arrive later from the Server. A listed name equal to the Client's own `name` and to no block is
   logged with a notice that the Client's own Agent is not covered. A value that breaks the
   instance-name grammar ([ADR-0015](0015-supervisor-mode-and-its-kinds.md) clause 3) fails
   startup naming the key and the value, because no block can ever carry it.

3. **A listed Supervisor's Agent declares neither remote-configuration capability.** It is built
   without `AcceptsRemoteConfig` and without `ReportsRemoteConfig`. Every other capability stays as
   it is: `ReportsEffectiveConfig`, `AcceptsRestartCommand`, and `AcceptsPackages` with
   `ReportsPackageStatuses` where a verification key is configured. The Server then offers it no
   remote configuration ([ADR-0016](0016-configurations-and-the-rest-api.md) clause 6), and nothing
   on the Server changes.

4. **A remote configuration that arrives anyway is ignored.** It is not stored, no entry file is
   written, nothing is handed to the process adapter, and no `RemoteConfigStatus` is reported,
   since the Agent declares no `ReportsRemoteConfig`. The Baseline's rule for a part of a message
   the Agent does not support is that it *"SHOULD ignore it"*. The Client logs a warning naming
   the Supervisor and the offered hash once per hash it has seen since start, so a Server that
   resends the same offer on every exchange does not flood the log.

5. **A stored remote configuration from before is removed at start, and the operator's files
   stay.** When a listed Supervisor starts — at Client startup, or when a delivered set adds or
   changes it — and a `remote-config.pb` exists in its directory, the Client, before the kind is
   started:
   - deletes from `config/` each entry file the stored map names whose bytes are still the ones
     the map holds, and `.supplementary` if its bytes are still the ones the map would write;
   - leaves every other file in `config/` untouched, since the operator wrote or overwrote it;
   - deletes `remote-config.pb`;
   - logs once, naming the Supervisor, the stored hash, and every file it kept because its content
     had changed.

   A `remote-config.pb` that does not decode is deleted, `config/` is left as it is, and the
   warning says that `config/` may still hold files the Server wrote. No status is restored, so a
   listed Supervisor reports no `RemoteConfigStatus` after a restart and no
   `last_remote_config_hash`.

   Storing an offer (`store_remote_config`) removes the previous entry files, writes the new
   ones, and writes `remote-config.pb` last. A store cut short — a crash or a failed write —
   therefore never leaves a new `.pb` beside the previous offer's files, which the comparison
   above would keep as the operator's. What it can leave is the previous `.pb` beside some of
   the new offer's files. The previous map names none of those, so they stay in `config/`: a
   stop after the first new entry file and before the `.pb` is the one window in which the drop
   cannot tell the Server's files from the operator's.

6. **What a listed Supervisor runs is the operator's.** The kinds are unchanged. A listed
   Supervisor runs on the files the operator places in `<supervisor_dir>/<name>/config/`, under
   the names its kind reads (the context lists them), with roles in `.supplementary` in the format
   `storage.rs` writes. For `collector` and `command` the process also gets the block's `args` and
   `env`, which are the operator's as clause 7 keeps them. With remote configuration switched off,
   nothing else writes into that directory. A listed `collector` or `icinga2` with no such file
   waits and says so, and a listed `telegraf` with none exits and is reported, as each does today
   before its first remote configuration.

7. **A delivered block for a listed Supervisor repeats the running block whole.** In the
   Supervisor-set apply, the check of a delivered block (`check_delivered_block` in
   `crates/fleet-agent/src/supervisor/mod.rs`, called from `apply_inner` in
   `crates/fleet-agent/src/reconfigure.rs`) ignores `delivered_args` and `delivered_env` (the
   `SupervisorsConfig` fields of `crates/fleet-agent/src/config.rs`) for a block whose name is in
   `remote_config_disabled`, and holds such a block to this:
   - **A block of that name runs:** the delivered block equals it entirely, compared as the
     parsed block. That covers `type`, the core keys (`service_name`, `endpoint_port`,
     `stop_timeout_secs`, `apply_grace_secs`, `retain_previous_secs`, `program_path`) and every
     kind setting, among them `args`, `version_args` and `env` and an `icinga2` block's
     `node_name`, `parent_host`, `ticket_file` and `trusted_cert_file`. A key the running block
     lacks may not appear, and one it has may not change or go. Absent and empty differ.
   - **No block of that name runs, because the block is added:** the delivered block carries
     nothing beyond what naming its program requires. That is `type` and `name`, plus the kind's
     program key where the kind does not name its own program: `binary` for `collector` and
     `command` for `command`. A `telegraf`, `glpi` or `icinga2` block carries `type` and `name`
     alone, since each of those kinds installs and names its own program and refuses the key
     ([ADR-0015](0015-supervisor-mode-and-its-kinds.md) clause 13). An added listed `icinga2`
     is thus a standalone node until the operator writes its parent into the host's
     `supervisor.toml`, and from then on the running block is the one a delivered block must
     equal.

   A block that breaks this fails the whole offer before anything stops, as ADR-0051 clauses 8
   and 18 refuse a block: the set is reported `FAILED` with a reason naming the block and the
   key, nothing is stopped, nothing is written, and the running set stays in force. Without
   this, the Server could configure a listed Supervisor through its block instead of its
   `config/`. It could put a configuration on a Collector's command line or into its
   environment (`--config=yaml:…`, `--config=env:VAR`) where `delivered_args` or
   `delivered_env` allow it. It could also, through the `icinga2` keys the context describes,
   enrol the operator's agent with a parent of the Server's choosing on trust on first use.
   Equality closes that route for every key, including keys a kind adds later. A list of the
   keys that matter would have to be kept current with every kind. The Server can still remove a
   listed block or deliver it unchanged. Delivering it unchanged is how a Server that manages the
   set keeps it.

8. **Switching it back on hands `config/` to the Server again.** When a name leaves the list, the
   next start declares both capabilities, reports no hash, and is offered whatever is released to
   that Agent ([ADR-0027](0027-rollout-and-what-reaches-an-agent.md)). The first stored offer
   replaces every file in `config/` ([ADR-0016](0016-configurations-and-the-rest-api.md) clause 9),
   the operator's included.

**Out of scope:** the Client's own Agent and the Supervisor set it receives. A host that wants the
Server unable to change, remove or add Supervisors needs a decision of its own, and until then a
Server can remove a listed Supervisor and deliver the same agent under another name, which is not
listed. Also out of scope: the restart command and package delivery to a listed Supervisor, which
stay as they are; what a listed Supervisor reports as its effective configuration when its process
reports none (today the empty map, as for any Supervisor before its first remote configuration);
and any change on the Server, the REST API or the bundled UI.

## Alternatives considered

- **A key inside the `[[supervisor]]` block** (`accepts_remote_config = false`). The Server
  replaces the blocks ([ADR-0051](0051-a-delivered-block-brings-nothing-past-the-signature.md)
  clause 7), so it could switch the setting back on by delivering the block without the key. A
  delivered-block check that refuses dropping the key would make the operator's choice depend on a
  rule in the apply path, when it can depend on a section the apply path never writes.
- **A Client-wide switch for every Supervisor.** It is too coarse: a host that wants one agent's
  configuration kept local loses fleet configuration for every other agent it runs.
- **Including the Client's own Agent now.** Withholding its remote configuration freezes the
  Supervisor set, which is the opposite of what ADR-0051 clause 13 settles for a host that is
  offered one. It is a different decision with different consequences, and is left as a follow-up.
- **Keeping `AcceptsRemoteConfig` and answering every offer `FAILED`.** The Server would keep
  offering, the fleet view would show a refusal on every rollout, and the Agent would declare a
  capability it does not exercise.
- **Ignoring only `remote-config.pb` and leaving `config/` as it is.** The kinds read the entry
  files, not the `.pb`, so the Server's last configuration would go on running under a switch that
  says it does not.
- **Wiping the whole `config/` directory at start.** It would delete the files an operator placed
  there before the restart that put the switch in force.
- **Moving the Server's stored files aside instead of deleting them.** They can carry secret
  material (ADR-0016 clause 9), and nothing needs them back: switching back on reports no hash, so
  the Server offers again what is released.
- **A separate host path for a listed Supervisor's local configuration.** It would be a second
  place each kind reads from, and a block key the Server could rewrite. `config/` is already where
  every kind looks.
- **Letting `delivered_args` and `delivered_env` stand for a listed Supervisor.** They were written
  to let the fleet steer a Supervisor's process. For a listed one the same keys would let the Server
  deliver a configuration on the command line or in the environment, and the switch would not
  mean what it says.
- **Holding only `args`, `version_args` and `env` to the running block.** It closes the command
  line and the environment and leaves every other key to ADR-0051's bounds. An `icinga2` block's
  `parent_host`, `node_name` and `trusted_cert_file` then stay deliverable, and with the pin
  pointed at a file no remote configuration writes, the agent enrols on trust on first use with a
  parent the Server chose. Any such list has to be kept current with every kind, and equality
  needs no list.
- **Letting an added listed block carry kind settings within ADR-0051's bounds.** The same keys
  would reach the agent through the block that adds it, before the operator wrote anything. An
  added block carries what names its program, and the operator configures the rest on the host.
- **Refusing startup when a listed name matches no block.** A Server that delivers that block later
  is the normal case, and the refusal would stop the whole Client for it.

## Sources / Prior art

- [OpAMP specification v0.20.0](https://github.com/open-telemetry/opamp-spec/blob/v0.20.0/specification.md)
  (the pinned Baseline): *Configuration* (*"Remote configuration capability can be disabled if
  necessary"*; the Client MUST set `AcceptsRemoteConfig` if the Agent can accept a remote
  configuration, and without it the Server MUST NOT offer one; the effective configuration may
  merge local configuration); *AgentToServer.capabilities* (an Agent MAY update its capabilities,
  and SHOULD ignore a part of a message for a capability it does not support); the
  `AgentCapabilities` enum (`AcceptsRemoteConfig`, `ReportsRemoteConfig`,
  `ReportsEffectiveConfig`); *Interoperability of Partial Implementations*.
- [OpAMP Supervisor specification](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/specification/README.md)
  — `capabilities.accepts_remote_config` is a per-supervisor setting in its local file, `false`
  unless set, and without it the Collector runs on the local `agent.config_files` alone.
- The code read for the context: `AGENT_CAPABILITIES`, `AgentState::new`, `apply` and
  `config_applied` in `crates/fleet-agent/src/supervisor/agent.rs`; `start_supervisor` in
  `crates/fleet-agent/src/supervisor/mod.rs`; each kind's configuration source in
  `collector.rs`, `command.rs`, `telegraf.rs`, `glpi.rs` and `icinga2.rs`; `SupervisorsConfig` in
  `crates/fleet-agent/src/config.rs`; `apply_inner` in `crates/fleet-agent/src/reconfigure.rs`;
  `store_remote_config` and `config_entries` in `crates/fleet-agent/src/storage.rs`; `offer` in
  `crates/fleet-server/src/fleet.rs`.

## Consequences

- Positive: an operator can close the configuration channel for one agent with one line the
  Server cannot change. A compromised Server can then no longer write that agent's configuration
  or run what its configuration language allows (Q-1). Signed package updates still reach it.
- Positive: the Server needs no change and the fleet view shows why: the Agent's capability set,
  which the Server already lists, lacks `AcceptsRemoteConfig`, and the Server offers it nothing
  however a rollout is aimed.
- Positive: switching off takes the Server's last configuration out of force at the next start
  without destroying what the operator put in its place.
- Negative / trade-offs: a listed Supervisor is configured by hand on its host. A rollout aimed at
  it releases nothing it receives, and G-1's loop does not close for that Agent by design.
- Negative / trade-offs: the switch binds a name, not an agent. While the Server still manages the
  set it can remove a listed Supervisor and add the same agent under a name that is not listed. That
  Agent is new, installed afresh from a signed package and visible as such in the fleet, but its
  configuration is the Server's.
- Negative / trade-offs: a change to the list takes effect at the next start of the Client, like
  every other key of the file.
- Negative / trade-offs: the per-Agent capability set is no longer the same for every Supervisor.
  [`CONFORMANCE.md`](../CONFORMANCE.md) rows for `AcceptsRemoteConfig` and `ReportsRemoteConfig`
  must say they can be withdrawn per Supervisor (G-12).
- Negative / trade-offs: a Server that manages the set must deliver a listed block exactly as it
  runs. A change the operator makes to it on the host has to reach the Server's copy before the
  next delivered set, or that set fails.
- Follow-ups (by topic): letting a host stop the Server from changing its Supervisor set; reporting
  a listed Supervisor's local files as its effective configuration when its process reports none;
  whether an `icinga2` Agent should refuse to enrol on trust on first use when its block names a
  `trusted_cert_file` that does not exist, rather than fall back to `pki save-cert` (ADR-0029),
  which is not decided here; storing an offer so that a stop part-way leaves no file the drop of
  clause 5 cannot attribute.

## Enforcement

Planned tests, each citing this ADR:

- `crates/fleet-agent/src/config.rs`:
  `remote_config_disabled_defaults_to_empty_and_lists_supervisor_names` (planned, clause 1),
  `a_remote_config_disabled_name_outside_the_instance_name_grammar_fails_startup` (planned,
  clause 2).
- `crates/fleet-agent/src/supervisor/agent.rs`:
  `a_supervisor_with_remote_config_disabled_declares_neither_remote_config_capability` (planned,
  clause 3: the report's `capabilities` lack both bits and keep `ReportsEffectiveConfig`,
  `AcceptsRestartCommand` and the package bits),
  `a_remote_config_offered_anyway_is_neither_stored_nor_applied_nor_reported` (planned, clause 4:
  no `remote-config.pb`, no entry file, no pending apply, no `remote_config_status` in the next
  report), `an_ignored_remote_config_is_logged_once_per_hash` (planned, clause 4).
- `crates/fleet-agent/src/supervisor/mod.rs`:
  `a_listed_name_without_a_block_is_a_notice_not_a_refusal` (planned, clause 2),
  `a_listed_supervisor_drops_the_stored_remote_config_and_keeps_the_operators_files` (planned,
  clause 5: unchanged entries and `.supplementary` deleted, an overwritten entry and an
  operator's extra file kept, `remote-config.pb` gone, before the kind starts),
  `an_undecodable_stored_remote_config_is_deleted_and_config_is_left_alone` (planned, clause 5),
  `a_listed_supervisor_reports_no_remote_config_status_after_a_restart` (planned, clause 5),
  `the_clients_own_agent_keeps_accepting_its_supervisor_set_when_its_name_is_listed` (planned,
  clause 2 and out of scope).
- `crates/fleet-agent/src/reconfigure.rs`:
  `a_delivered_set_cannot_switch_remote_config_back_on_for_a_listed_name` (planned, clause 1: a set
  that removes and re-adds the listed block leaves the started Agent without
  `AcceptsRemoteConfig`, and the written file keeps `[supervisors]` byte for byte),
  `a_delivered_block_for_a_listed_supervisor_must_equal_the_running_block_whole` (planned,
  clause 7: with `delivered_args = true` and `delivered_env = ["*"]`, a delivered block for a
  listed name that changes `args`, `version_args`, an `env` entry or a core key, adds a key or
  drops one fails the offer naming the block and the key; the running block delivered back
  passes),
  `a_listed_icinga2_keeps_the_parent_and_the_pin_the_operator_wrote` (planned, clause 7: a
  delivered `icinga2` block for a listed name that changes `parent_host` or `node_name`, or
  points `trusted_cert_file` at a missing file in `${config_dir}`, fails the offer naming the
  key),
  `an_added_listed_supervisor_carries_only_what_names_its_program` (planned, clause 7: with no
  running block, `type`, `name` and `binary` for a `collector`, `command` for a `command`, and
  nothing else for an `icinga2` pass; any further key, an empty `args` or an `icinga2`
  `parent_host` among them, fails the offer naming it).
- `crates/fleet-agent/src/storage.rs`:
  `a_store_cut_short_leaves_no_previous_entry_file_beside_a_new_pb` (planned, clause 5: a
  store cut short while it writes the new entry files leaves the previous `remote-config.pb`
  beside no previous entry file, so nothing of the previous offer stays unattributed).
- `crates/fleet-agent/tests/supervisor.rs`:
  `a_listed_collector_runs_on_the_entries_the_operator_placed` (planned, clause 6).
- `crates/fleet-agent/tests/e2e.rs`:
  `a_server_offers_no_configuration_to_a_listed_supervisor` (planned, clauses 3 and 8: a released
  Configuration reaches the unlisted Supervisor and not the listed one over the same connection).
