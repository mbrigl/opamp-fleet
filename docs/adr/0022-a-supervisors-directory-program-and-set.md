# ADR-0022: Each Supervisor owns one directory and runs only a program installed there, and the Server manages the set of Supervisors

- **Status:** 🟢 accepted
- **Date:** 2026-08-19
- **Deciders:** Markus Brigl
- **Applies to:** crates/fleet-agent/src/config.rs (supervisor_dir, program resolution), crates/fleet-agent/src/supervisor/ (start, placeholders), crates/fleet-agent/src/reconfigure.rs (the Supervisor-set apply), the `[[supervisor]]` blocks of supervisor.toml

## Context

A package update swaps files: the running program is renamed aside, the artifact is written beside
it and renamed into place ([ADR-0019](0019-package-delivery-on-the-agent.md)). All of that needs
write permission on the **directory**. A program in a system directory such as `/usr/local/bin`
cannot be updated by a Client that does not run as root, and the failure would appear at rollout
time on every matched host rather than at startup on one. A Client that also supervised programs
installed by someone else would have two kinds of Managed Process — one it can update, roll back
and health-gate, one it merely runs — and every package, version and rollback decision would carry
both cases. Repacking vendor software into relocatable trees ([ADR-0028](0028-glpi-agent-and-telegraf.md),
[ADR-0029](0029-icinga-2.md)) brings such agents under fleet ownership instead.

`state_dir` is state: hardened hosts mount `/var/lib` `noexec` and size it for state, not for
several Collector binaries. An artifact staged under one filesystem and installed on another is
copied, a second full write of a program of several hundred megabytes.

A Foreign Agent is told where its configuration is through its own command line, and an absolute
path written there drifts silently when the Supervisor's directory moves or the Supervisor is
renamed: the process starts happily on a file nobody writes to.

What the fleet needs to manage on a Client is which Supervisors it runs. Everything else in
`supervisor.toml` ([ADR-0023](0023-releases-installers-and-the-name-supervisor.md)) — the Server
endpoint, the credential, the state directory, the instance name — is host-local trust and wiring;
a Server that could rewrite it could cut a Client off with one bad push. The specification allows
an Agent's effective configuration to "merge in local configuration". The program a Supervisor
spawns is itself a trust anchor: a Server — or someone who has compromised it without the
package-signing key — that could name any program on the host would have fleet-wide code execution
that bypasses signature verification entirely.

## Decision

We will give every Supervisor one directory it owns under a root the operator can place, accept
only programs this Client installs into that directory, point Foreign Agents at it by placeholder,
and let the Server replace the set of `[[supervisor]]` blocks — and nothing else in the file —
purging a removed Supervisor's directory with it.

1. **One directory per Supervisor, under a relocatable root.** The top-level key `supervisor_dir`
   defaults to `<state_dir>/supervisors` and is made absolute at load. Under it each Supervisor
   owns `<supervisor_dir>/<name>/`, holding everything it needs:

   ```
   <supervisor_dir>/<name>/
     instance-uid            # its Agent identity
     remote-config.pb        # the last received configuration
     installed-package.json  # what is installed
     config/                 # the written configuration entries
     program/<file>          # the Managed Process — or program/tree/<program_path> for a tree
     packages/               # this Supervisor's download staging
   ```

   One knob moves state and program together. Staging beside `program/` makes a raw artifact's
   install a rename on one filesystem. The directory is `program/`, not `bin/`, because it holds a
   whole tree for a multi-file package ([ADR-0019](0019-package-delivery-on-the-agent.md)). Moving
   `supervisor_dir` on a running host leaves the old tree behind and migrates nothing; each
   Supervisor then registers as a new Agent.

2. **The program key is a bare file name.** `binary` (`collector`) and `command` (`command`) take
   a file name with no path separator and no `..`, resolving to `program/<value>` — or to
   `program/tree/<program_path>` when the block names a tree. A wrapped kind names its own program
   ([ADR-0015](0015-supervisor-mode-and-its-kinds.md)). Anything else fails at startup:
   - an **absolute path** — and on Windows a rooted path without a drive letter — with a message
     naming the block, the value and the way across: a program the fleet manages reaches the host
     as a package and is named here by its bare file name;
   - any other shape (`./x`, `a/b`, `../x`) with a message naming the rule.

   A bare name cannot escape the directory, so nothing has to be sanitised, and it never searches
   `$PATH`.

3. **Every Supervisor accepts packages.** Since every program lives in a directory this Client
   owns, every Supervisor's Agent declares `AcceptsPackages`; the capability is a constant, not a
   function of the configuration. The `program/` directory is prepared at start, before the first
   package arrives. A block carrying `accepts_packages` fails at startup with a message saying the
   bare file name already makes the program updatable. Startup logs, once per Supervisor, where its
   program is.

4. **A vendor agent is brought in by repacking.** An agent installed by the machine's package
   manager is not supervised here; it is repacked as a relocatable artifact and delivered as a
   package ([ADR-0020](0020-the-package-store.md), [ADR-0028](0028-glpi-agent-and-telegraf.md),
   [ADR-0029](0029-icinga-2.md)). The manual documents the fleet-delivered route for every agent it
   describes.

5. **The Client's own consent stays explicit.** `[self_update]` names the package it takes
   ([ADR-0021](0021-the-client-updates-itself.md)); nothing about it is derived from a path. A
   package written over the Client takes the host out of reach, which is where implicit consent
   would be wrong.

6. **Placeholders name a Supervisor's own directories.** In the `args` and `env` values of
   `collector` and `command` blocks, `${supervisor_dir}` expands to `<supervisor_dir>/<name>` and
   `${config_dir}` to `<supervisor_dir>/<name>/config`, where the received configuration's entries
   are written ([ADR-0016](0016-configurations-and-the-rest-api.md)) — so
   `args = ["-c", "${config_dir}/fluent-bit-conf"]` cannot drift. They are expanded when the
   Supervisor starts, and **never** in the program key, whose written shape is what the operator
   reads. An unrecognised `${…}` is passed through verbatim — neither refused nor emptied — because
   a Foreign Agent's own configuration language may use the same syntax (Fluent Bit's does).

7. **The Server manages the set through the Client's own Agent, and only the blocks are read.** A
   remote configuration offered to the Client's own Agent — a Configuration stated for the Agent
   type `supervisor` ([ADR-0016](0016-configurations-and-the-rest-api.md)) — is parsed entry by
   entry as TOML. The union of the entries' `[[supervisor]]` blocks, in entry-name order, is the
   offered set; every other top-level key is ignored. The boundary is enforced by what the Client
   takes, not by policing what the Server sends. A duplicate `name` within or across entries, an
   entry that is not TOML, or a block the startup parser would refuse fails the offer.

8. **An offered set is validated as startup would read it, before anything stops.** The merge is
   the running file's globals with the offered blocks. Each block passes the same checks startup
   applies — kind known, program key present and well-shaped, `service_name`, `endpoint_port` and
   timing as [ADR-0015](0015-supervisor-mode-and-its-kinds.md) allows them, retired keys refused,
   the kind's strict settings parse through the side-effect-free `Plugin::check`. A set that fails
   is reported `FAILED` with the reason; nothing is stopped, nothing is written, the running set
   stays in force.

9. **A delivered block may name only a Client-owned program.** The apply path checks every offered
   block's program resolves inside the Supervisor's own `program/`. Clause 2 already refuses every
   other shape for every principal, so this check cannot fire today; it stays as defence in depth
   against a future shape, in the apply path that knows the block came from the Server.

10. **The apply is a diff keyed by Supervisor `name`, in a fixed order.** Comparing running and
    offered blocks yields removed, changed (any key differs), added and unchanged. Then:
    1. the removed and changed Supervisors stop — a removed one is uninstalled
       ([ADR-0015](0015-supervisor-mode-and-its-kinds.md)), a changed one only stopped — and their
       Agents send `agent_disconnect`;
    2. the merged document is written to `supervisor.toml`;
    3. the removed Supervisors' directories are purged (clause 14);
    4. the changed and added Supervisors start from the file just written, an added one introducing
       itself as a new Agent on the running connection.

    Unchanged Supervisors keep running untouched. A failed write restarts the stopped Supervisors
    from the still-standing old file. A crash before the write restarts into the old file, one
    after it into the new one; both converge, because startup builds exactly what the file says.

11. **`supervisor.toml` remains the single truth.** No overlay and no second file: after an apply
    the file is the configuration, the same file the operator reads and the Client's own Agent
    reports as its effective configuration (refreshed in the same step), and an offline restart
    runs the delivered Supervisors because they are in it. Only the `[[supervisor]]` array is
    replaced, by editing the document with `toml_edit`, so the operator's comments, ordering and
    formatting survive byte for byte and the offered blocks keep their text. The write goes through
    a sibling temporary file renamed into place; on Unix it carries the existing file's mode, and a
    file created by the write is `0600`, because it holds the credential.

12. **The status is honest.** The Client's own Agent reports `APPLYING` on receipt, `APPLIED` once
    the file is written and the starts have succeeded, and `FAILED` when parsing, validation, the
    write or a start fails. A started Supervisor whose process later crashes is a health fact of
    that Supervisor's Agent, not a failed configuration.

13. **No offer, no change.** A Client to which no Supervisor set is offered runs its locally written
    blocks. The first applied offer replaces the local set, and from then on the Server's set is
    authoritative for that Client's Supervisors; the offer is compared against the file, not
    against the last offer.

14. **A removed Supervisor is purged.** Removal is keyed by name: only a name absent from the new
    set is removed, while a changed block keeps its directory, identity and installed package. The
    purge runs only after the write succeeded, because a failed write restarts the old set from
    whole directories. It deletes `<supervisor_dir>/<name>/` recursively — program, staging,
    configuration and `instance-uid` — so a Supervisor later re-added under the same name is a new
    Agent, installed afresh.

15. **The purge never leaves the Supervisor root.** A symlink where a Supervisor's directory should
    be is unlinked, never followed, and a directory whose resolved path lies outside the root is
    left alone with a warning.

16. **A directory the purge cannot delete fails nothing.** The Supervisor is already stopped and
    the file written, so the set the Server asked for is running; a purge error (a file held open
    on Windows, permissions) is a warning naming the path, and the directory becomes an orphan.

17. **An orphaned directory is reported at startup, never reaped.** A directory under the root
    that no block names — a purge cut short, a block removed by hand while the Client was down, a
    moved root — is logged as a warning with its path and left in place. Startup cannot tell a
    leftover from a block temporarily commented out, and deleting would cost that Agent its
    identity and program; removing it is the operator's call.

**Out of scope:** reconciling a locally edited `[[supervisor]]` set against the last applied
offer at startup; an opt-in reaping of orphaned directories; migrating a Supervisor tree when its
root moves; a supervise-only mode for programs this Client does not install, which would need its
own decision and capability model; how a multi-file tree is unpacked and swapped
([ADR-0019](0019-package-delivery-on-the-agent.md)).

## Alternatives considered

- **An `accepts_packages` flag beside a program path that may be absolute.** Two keys for one
  truth that can disagree, and the configuration could express what the filesystem forbids.
- **Probing writability at startup and deriving consent from it.** A fleet-visible capability would
  depend on a `chmod` nobody recorded and race a rollout when permissions change.
- **Supervising machine-installed programs without updating them.** Every package, version and
  rollback decision would carry a second case, the fleet's picture of a host would depend on which
  file a block came from, and an operator "fixing" a path to an absolute one would silently revoke
  a capability.
- **Letting the Server deliver absolute program paths, or allowlisting some per host.** The first
  hands a Server arbitrary code execution outside package signing; the second is host-local policy
  for a capability the fleet path does not need. Warning and applying anyway is not a control.
- **A separate `bin_dir`, or a `supervisor_dir` per block.** Two knobs for a separation nobody has
  asked for; one root per Client matches how the Client's own root is treated.
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
- **The Server delivering the whole `supervisor.toml`.** Endpoint, credential and state directory
  are the host's trust anchors; one bad push could leave the Client unreachable.
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
  Supervisor, which a removed one is not; a renamed copy is a slower leak.
- **Reaping orphaned directories at startup.** Startup cannot tell a leftover from a deliberate
  hand edit, and the destructive reading deletes an identity that was not meant to go.

## Sources / Prior art

- [OpenTelemetry `opampsupervisor`](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/cmd/opampsupervisor)
  and its [specification](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/specification/README.md)
  — a per-supervisor `storage.directory` beside an absolute `agent.executable`; the remote-plus-local
  configuration merge; it manages only the agent it installs and has no Foreign-Agent path
  expansion (`opentelemetry-collector-contrib#36269`).
- [Elastic Agent version management](https://deepwiki.com/elastic/elastic-agent/6-version-management-and-upgrades)
  — a versioned home with a symlink to the active executable; manages only binaries it installs.
- [Bindplane collector install and uninstall](https://docs.bindplane.com/deployment/virtual-machine/collector/install-and-uninstall-bindplane-collectors)
  — managed vs detached collectors; removal deletes install directory and state as one act.
- [systemd.exec](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html) —
  specifiers expand in arguments but not in the executable path; `StateDirectory=` exports
  `$STATE_DIRECTORY`.
- [OpAMP specification v0.19.0](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)
  and [v0.20.0](https://github.com/open-telemetry/opamp-spec/blob/v0.20.0/specification.md) —
  `EffectiveConfig` may merge local configuration; `RemoteConfigStatuses`; `agent_disconnect` as an
  Agent's last word; `AcceptsPackages` as a per-Agent capability.
- [`toml_edit`](https://docs.rs/toml_edit) — format- and comment-preserving TOML editing, what
  `cargo add` edits manifests with.
- [Debian FAQ: remove vs purge](https://www.debian.org/doc/manuals/debian-faq/uptodate.en.html) —
  purge semantics, chosen because no operator is present after a Server-driven removal.

## Consequences

- Positive: an update writes only inside a directory the Client owns — no root, no permissions on
  a system `bin`, no `.rollback` files beside system binaries — and hardening can grant write
  access to exactly one tree. `supervisor_dir` takes programs off a `noexec` or undersized `/var`.
- Positive: one kind of Managed Process. Every package feature has one case, and a capability can
  never depend on how a path was spelled; a locally written block and a delivered one describe the
  same thing.
- Positive: a Foreign Agent's configuration path follows a relocated root or a rename.
- Positive: Supervisors are fleet-manageable end to end with the same rollout and Selector scoping
  as every Configuration; unchanged Supervisors ride through an apply; a delivered set can only run
  programs that arrive as verified packages.
- Positive: a removal is complete — no program-sized leftovers, no stale identity.
- Negative / trade-offs: a program installed by the machine cannot be supervised; adopting a host
  that already runs a vendor agent means a repack and a package rollout before the Agent appears.
- Negative / trade-offs: one copy of a program per Supervisor; an archive artifact is still written
  twice (unpacked), so the rename saving lands on bare-binary artifacts.
- Negative / trade-offs: an unknown placeholder passes through rather than failing, unlike an
  unknown key; an absolute path in an argument still works and can still drift.
- Negative / trade-offs: after the first applied offer, a local edit to the blocks drifts silently
  until the next publication overwrites it.
- Negative / trade-offs: removal is destructive and final; a Supervisor removed by a mis-scoped
  rollout loses its identity and its program. A crash between write and purge leaves an orphan
  only a human removes.
- Follow-ups (by topic): startup reconciliation of a locally edited set; a bundled-UI editor for
  Supervisor sets; an opt-in reap of orphaned directories; a purge option on `service uninstall`;
  pruning stale `.rollback` files on a schedule; a supervise-only mode as its own decision.

## Enforcement

- Directory and program, in `crates/fleet-agent/src/config.rs`:
  `the_supervisor_root_defaults_under_the_state_dir_and_is_relocatable`,
  `a_relative_state_dir_yields_absolute_directories`,
  `a_bare_name_resolves_and_everything_else_is_refused`,
  `an_absolute_program_path_is_refused_and_names_the_way_across`,
  `the_retired_package_keys_are_refused`, `a_program_path_must_stay_inside_the_package`.
- Consent and start, in `crates/fleet-agent/src/supervisor/mod.rs`:
  `every_supervisor_declares_package_acceptance`, `installs_packages_reflects_declared_package_acceptance`,
  `a_tree_supervisor_prepares_its_root_and_leaves_the_tree_to_the_package`,
  `a_program_path_that_is_neither_fails_the_build`, `a_block_without_a_program_fails_the_build`,
  `an_orphaned_supervisor_directory_is_not_reaped_at_startup`.
- Placeholders: `the_placeholders_name_this_supervisors_own_directories` and
  `an_unknown_placeholder_is_passed_through_untouched` (`crates/fleet-agent/src/supervisor/ports.rs`),
  `a_command_supervisors_arguments_are_expanded_to_its_own_directories`
  (`crates/fleet-agent/tests/supervisor.rs`), `the_spec_carries_the_environment` (`collector.rs`).
- The set apply, in `crates/fleet-agent/src/reconfigure.rs`: `foreign_top_level_keys_are_ignored`,
  `duplicate_supervisor_names_fail_the_offer`, `a_malformed_block_names_its_entry`,
  `blocks_are_collected_across_entries_in_name_order`,
  `a_server_delivered_block_may_not_name_an_absolute_program`,
  `a_server_delivered_block_naming_a_bare_program_is_accepted`,
  `a_delivered_collector_binary_must_be_owned_too`,
  `the_write_replaces_blocks_and_keeps_the_operators_file`,
  `the_rewrite_keeps_the_files_restrictive_mode`, `a_freshly_created_file_is_owner_only`,
  `removed_is_by_name_so_a_changed_block_is_not_removed`,
  `the_purge_deletes_exactly_the_removed_supervisors_directory`,
  `the_purge_does_not_follow_a_symlink_out_of_the_supervisors_root`,
  `an_empty_offer_removes_every_block`; `retiring_uninstalls_the_removed_and_only_stops_the_changed`
  (`crates/fleet-agent/src/engine.rs`); and end to end
  `a_config_change_reaches_both_supervised_agents_over_one_connection` (`crates/fleet-agent/tests/e2e.rs`),
  which adds, keeps and removes a Supervisor through a delivered set and checks the purge.
