# ADR-0018: One directory per Supervisor — the Client manages only programs it installs, and a Foreign Agent finds its own directories by placeholder

- **Status:** 🟢 accepted
- **Date:** 2026-08-19
- **Deciders:** Markus Brigl

## Context

A Supervisor whose program is someone else's file cannot take a package update:

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

A configuration with two keys for one truth can express something the filesystem forbids, and
nothing checks it. `accepts_packages` says *whether* the Server may replace this binary, `binary`
says *where* it is — and [ADR-0015](0015-package-delivery-for-managed-processes.md) and
[ADR-0016](0016-a-package-is-a-versioned-set.md) tie them together nowhere. The combination that
works and the combination that cannot are equally spellable.

Most of what the fix needs already exists. Every Supervisor has its own directory,
`<state_dir>/supervisors/<name>/` (`build_engine`), holding its `instance-uid`, the received
`remote-config.pb`, `installed-package.json`, and the written `config/` entries; the block's `name`
is validated through `parse_instance_name`, so it is by construction a legal directory name on all
three platforms ([ADR-0010](0010-client-os-service-and-installation-layout.md)'s grammar), and duplicates are
refused at startup. What a package update writes belongs in that directory too.

Two further forces point the same way:

- **The download staging is fleet-wide.** An artifact streamed to `<state_dir>/packages/<name>.staged`
  (`packages.rs`) has to be *copied* into the target directory — explicitly a copy and not a rename,
  because the two may sit on different filesystems (`install_executable`). For a Collector of a few
  hundred megabytes that is a full second write of the artifact on every update.
- **`state_dir` is state, and a binary is not.** The FHS calls `/var/lib` variable *state*; hardened
  hosts mount it `noexec`, and it is often sized for state rather than for several Collector
  binaries. ADR-0010 already puts the Client's own binary under a root of that kind — but it lets
  the operator choose that root (`--root`, "no fixed installation path").

**A program the Client does not own splits the product in two.** A fleet that cannot update an
Agent is supervising, not managing. An absolute-path route delivers health, restart and central
configuration, and explicitly *not* updates — a coherent product, but a different one. Every
capability this project builds for the managed case — Selector-targeted packages (ADR-0016), the
version rules ([ADR-0035](0035-what-reaches-an-agent.md)), rollback
([ADR-0015](0015-package-delivery-for-managed-processes.md)), the health gate (ADR-0015) —
stops at that boundary, and every feature has to be reasoned about twice: once for a program the
Client owns and once for a program it merely runs.

**A consent derived from a path is invisible where it matters and irreversible where it does not.**
An operator who "fixes" a path to an absolute one would silently revoke a fleet-visible capability.
And the Server cannot correct it:
[ADR-0029](0029-supervisor-set-from-the-server.md) refuses to
deliver a block naming an absolute path, so a host that has drifted into the unmanaged shape can only
be fixed by hand, on the host. ADR-0029 draws that line because the local file is the operator's
authority — but it means the fleet's model of a host would depend on which of two files the block
came from, and only one of them the fleet can see.

**The alternative route exists.** [ADR-0034](0034-repacked-icinga-2-artifacts.md)
repacks vendor software as relocatable trees, and
[ADR-0031](0031-the-glpi-agent.md) does it for GLPI Agent on
both platforms. A machine-installed absolute path is therefore not the only route to a vendor agent —
it is the route that keeps one outside the fleet. Because the path's shape is the whole of the
consent and no separate capability flag exists, refusing the absolute form needs no flag to retire.

**A Foreign Agent is told where its configuration is through its own command line**, and those
arguments are passed verbatim (`CommandSettings::args` → `ProcessSpec::args` in
`crates/client/src/supervisor/command.rs`). A block that hard-codes the path:

```toml
args = ["-c", "/var/lib/opamp-fleet/client/default/state/supervisors/fluent-bit/config/fluent-bit-conf"]
```

drifts as soon as `supervisor_dir` is set: the written configuration moves while that argument does
not. Fluent Bit then starts — successfully — on a file the Server no longer writes to. Nothing
errors: the process is healthy, its Agent reports healthy, and the fleet's configuration silently
stops arriving. The Collector plugin has no such problem: its `--config` flags are built from the
Supervisor's `config_dir`, so they follow the directory wherever it goes. Only the operator-written
command line of a Custom Supervisor can drift, and it can drift in three ways — a path that is wrong
from the start, one that `supervisor_dir` moves out from under, and one that a *rename* of the
Supervisor breaks. ADR-0008 fixes the configuration as hand-edited TOML, so whatever addresses this
must be readable in a file and fail loudly when it is wrong.

Constraints this decision has to respect: ADR-0008 (hand-edited TOML, a typo fails loudly at
startup), ADR-0010 (the operator chooses the root; the name grammar), ADR-0015 (the swap, the health
gate, the rollback), ADR-0016 (the **Server** chooses which artifact; the host only consents — and
the precedent of refusing a removed key loudly rather than ignoring it),
[ADR-0015](0015-package-delivery-for-managed-processes.md) (the artifact and how it is opened, untouched here),
and [ADR-0017](0017-client-self-update-and-its-consent.md) (the Client's own
consent names its package).

## Decision

We will require every Managed Process to be a program **this Client installs and owns**, give every
Supervisor one directory it owns, let the operator place that directory, and let a Foreign Agent's
command line name that directory by placeholder.

### The Supervisor's directory and its program

1. **A relocatable Supervisor root.** A top-level key `supervisor_dir` defaults to
   `<state_dir>/supervisors`. Under it, one directory per Supervisor, holding everything that
   Supervisor needs:

   ```
   <supervisor_dir>/<name>/
     instance-uid
     remote-config.pb
     installed-package.json
     config/
     program/<binary>        # the Managed Process, with its .rollback and .staged siblings
     packages/               # this Supervisor's download staging
   ```

   One knob moves the whole tree, state and program together — not a second knob for the program
   alone. The staging sitting beside `program/` makes the install a rename within one filesystem
   instead of a copy across two. This is
   [ADR-0011](0011-supervisor-mode-and-lifecycle-port.md)'s per-Supervisor state directory,
   with two more subdirectories and a root the operator can choose.

   The directory is called `program/` and not `bin/` deliberately: a multi-file package unpacks under
   the same root ([ADR-0015](0015-package-delivery-for-managed-processes.md)), so no path on disk moves. A directory name
   is cheap and a layout migration is not.

2. **The program's path is a bare file name, and it means the Client owns the program** — for
   `binary` (Collector plugin) and `command` (`command` plugin) alike:

   | Value | Meaning |
   |---|---|
   | a **bare file name** — no path separator, no `..` | `<supervisor_dir>/<name>/program/<value>`, or `program/tree/<program_path>` for a multi-file package (ADR-0015). The Client owns the directory, so it may replace what is in it: **every** Agent declares `AcceptsPackages`. |
   | anything else — an absolute path, `./x`, `a/b`, `../x` | **Startup error**, naming the rule. |

   There is no second row for someone else's file. A bare name cannot escape the directory, so the
   rule needs no traversal guard and there is nothing to sanitize. It also keeps the name the archive
   member is matched against (ADR-0015) exactly where it is. A multi-file package names its program
   inside the delivered tree with `program_path` (ADR-0015 clause 10); where a kind derives the
   program's file name, the rule applies to the derived value
   ([ADR-0037](0037-a-kind-knows-its-own-agent.md) clause 2).

   **An absolute path is refused naming both the rule and the way across**: the program belongs to
   the machine, and a program the fleet is to manage must be delivered as a package (ADR-0015,
   ADR-0034). The message says which block, which value, and what to do — it is the only notice an
   operator upgrading into this rule gets, so it carries the whole explanation rather than a rule
   number. The Windows drive-relative case (`\Program Files\…`, no drive letter) folds into the same
   error; it is only a near-miss of the absolute form.

   A bare name **does not mean "search `$PATH`"**. `Command::new` has `execvp` semantics, so a bare
   `command = "fluent-bit"` would search the path. That behaviour was undocumented, used in no
   example, and fragile under a service manager whose `PATH` is minimal — we take the break rather
   than add a case to preserve it.

3. **`accepts_packages` is removed and refused loudly.** A configuration still carrying it fails at
   startup with a message naming the path rule — the same treatment ADR-0016 gave `package`, for the
   same reason: never silently ignore a key an operator believes in.

4. **Where the program is, is logged once per Supervisor at startup** — `supervisor otelcol: packages
   accepted, program in <dir>`. Consent is not derived, because there is nothing left to derive: the
   `AcceptsPackages` capability is a constant of this Client, not a function of its configuration,
   and it is discharged by the type system rather than by a rule.

### What deliberately does not change

5. **ADR-0016 stands.** *Which* artifact an Agent receives is still the Server's decision, expressed
   as the package's Selector. This decision replaces only how a host says *yes*, not who chooses.

6. **ADR-0015 stands, whole.** One member is lifted out of an archive to a destination this Client
   picked; no archive path is ever used. Only the destination moves.

7. **ADR-0015's swap stands.** Rename aside, write, rename into place, health-gate, roll back — the
   same file-level mechanics, in a directory the Client owns.

8. **`[self_update]` keeps naming its package explicitly**, and nothing here touches ADR-0017's
   consent rule. The asymmetry is intentional: a package written over the Client takes the host out
   of reach, which is exactly where implicit, path-derived consent would be wrong.

9. **No `versions/` + `current` layout for Managed Processes.** ADR-0010 needs it because the
   running Client cannot overwrite itself; a Managed Process is stopped before its swap and
   `.rollback` already covers the fallback.

### What the Server may deliver, and how a vendor agent comes in

10. **The Server's delivery check stays, and cannot fire.** ADR-0029's rule — a delivered
    `[[supervisor]]` block must name a program the Client owns — is satisfied by every block that
    parses. The check remains as defence in depth against a future shape nobody has thought of yet,
    with a comment saying so; deleting a guard because it currently cannot trigger is how it comes
    back.

11. **A vendor agent is brought in by repacking, and that is the supported route.** ADR-0031 for GLPI
    Agent and ADR-0034 for Icinga 2 are what an operator uses instead of naming a machine-installed
    program. The manual describes the fleet-delivered routes for every case a machine-installed
    walkthrough documented, so no case is left without a page.

### Placeholders for a Foreign Agent's own directories

12. **Two placeholders, both directories the Client alone decides the location of**, are substituted
    into a Custom Supervisor's `args` and `env` values:

    | Placeholder | Expands to |
    |---|---|
    | `${supervisor_dir}` | `<supervisor_dir>/<name>` — everything that Supervisor owns |
    | `${config_dir}` | `<supervisor_dir>/<name>/config` — where the received configuration's entry files are written (ADR-0012) |

    The fluent-bit example becomes `args = ["-c", "${config_dir}/fluent-bit-conf"]`, which cannot
    drift: both halves are derived from the same place. A Managed Process's working directory is not
    a key; it is derived (ADR-0037 clause 2). Which kinds carry `args` and `env` at all is ADR-0037
    clause 4.

13. **The program is excluded**, in `binary` and `command` alike. This follows systemd, where
    specifiers are expanded in a unit's arguments but explicitly *not* in the executable path — and
    here there is a second reason: the program's path is the rule of clause 2, so a substituted
    program path would make what the Client owns depend on something the file does not literally
    say.

14. **An unrecognized `${…}` is left verbatim.** It is not an error and not silently emptied. A
    Foreign Agent's own configuration language may use the same syntax — Fluent Bit's does — and a
    Client that ate or rejected those would break a working deployment to catch a typo. The names are
    substituted; everything else is the process's business.

15. **Substitution happens once, at startup**, on the values as written. Nothing re-expands when a
    configuration arrives, because none of these paths change while the Client runs.

## Alternatives considered

- **Keep `accepts_packages` and reject the combination with an absolute path** — factually wrong:
  `/opt/otelcol/bin/otelcol`, owned by the service user, is a perfectly updatable setup, and the
  rule would forbid it. The real predicate is "does the Client own this directory", which
  *absoluteness* only approximates. It also leaves two keys that can still disagree.
- **Keep both keys and merely document the permission requirement** (a comment in the configuration)
  — the trap survives. The configuration still expresses what the filesystem forbids, and the failure
  still appears at rollout time across the fleet rather than at startup on one host.
- **Probe writability at startup and derive consent from that** — makes a fleet-visible capability
  depend on a `chmod` nobody recorded, and races a rollout when permissions change afterwards.
  Whose file it is, is a decision; it should be written down, not measured.
- **A separate `bin_dir` beside `state_dir`** — two knobs for a separation nobody has asked for.
  If state and program must diverge, the operator can still symlink; the tree stays one thing.
- **A per-Supervisor `supervisor_dir` inside each `[[supervisor]]` block** — more expressive than any
  reported need; one root per Client matches how ADR-0010 already treats the Client's own root.
- **Require `./` for the Client-owned meaning, keeping a bare name as a `$PATH` lookup** — another
  case in the rule, and a leading `./` reads as noise in TOML (and worse on Windows), all to preserve
  behaviour that was undocumented and unused.
- **Keep absolute paths and leave the consequence to operators.** It costs nothing in the rule itself.
  Rejected because the cost is in everything built beside it: every package, version and rollback
  decision carries a second case, and the fleet's picture of a host depends on which file a block
  came from.
- **Deprecate rather than remove — warn at startup, remove later.** Genuinely tempting, but with
  nothing installed there is no host to warn. A deprecation period protects deployments, and there
  are none. What it would buy instead is time to prove ADR-0034 on Windows — see the trade-off below,
  which is the real cost of not waiting.
- **Keep absolute paths for *supervision only* — a documented, capability-less mode.** This is what
  an absolute-path row is, named honestly. Rejected because naming it does not reduce it: the two
  cases still exist in the code, in every feature's reasoning, and in the fleet view. If the mode is
  worth having, it is worth having as its own decision with its own model, not as a fallthrough in a
  path parser.
- **Let the Server deliver absolute paths too, and drop ADR-0029's restriction instead.** The
  opposite resolution of the same asymmetry: make both principals equal by widening rather than
  narrowing. Rejected outright — it hands a Server the ability to run any binary on any host by
  absolute path, which is the escalation ADR-0029 exists to prevent.
- **Default `working_dir` to the Supervisor's own directory**, so relative arguments simply resolve
  there. Fewer moving parts and no new syntax — but it changes the meaning of every existing
  `command` block silently, and it points a foreign process's *working directory* at a tree whose
  layout the Client owns. A process that drops a file beside itself would litter that directory, and
  one that writes `config` or `program` would collide with it. Buying a smaller diff with a shared
  directory is the wrong trade.
- **Resolve a relative `working_dir` against the Supervisor's directory** (reusing clause 2's rule
  on a second key) — attractive for its consistency, but clause 2's bare name resolves into
  `program/`, and a working directory wants the Supervisor root. The same-looking rule would mean
  two different things depending on the key, which is worse than a second mechanism that admits it
  is one.
- **Export the paths as environment variables** (`OPAMP_CONFIG_DIR`, as systemd's `StateDirectory=`
  exports `$STATE_DIRECTORY`) — nothing expands variables in `argv`, so it would not reach the case
  the placeholders exist for. Worth revisiting for agents that read their environment, but not as the
  answer here.
- **Leave it, and document that an absolute argument must be kept in sync with `supervisor_dir`** —
  the failure is silent and the two settings live in the same file, so the only thing keeping them
  consistent would be the operator's memory.
- **A general templating engine** over the whole configuration — far past any present need, and it
  turns a configuration file into a program.

## Sources / Prior art

- **OpenTelemetry `opampsupervisor`** — a per-supervisor `storage.directory` (default
  `/var/lib/otelcol/supervisor`, `%ProgramData%/Otelcol/Supervisor` on Windows) alongside an
  absolute `agent.executable`: the same split between the supervisor's own tree and a foreign
  binary, except that upstream never expresses consent at all. It writes the merged Collector
  configuration to a directory it owns and passes it with `--config`, exactly as this project's
  Collector plugin does; it has no Foreign-Agent equivalent, and expansion in its own configuration
  is still an open request upstream (`opentelemetry-collector-contrib#36269`).
  <https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/cmd/opampsupervisor>
- **Elastic Agent** — versioned home directory plus a symlink to the active executable, the shape
  ADR-0010 already follows for the Client itself; consulted for whether Managed Processes should get
  the same (decided against, clause 9).
  <https://deepwiki.com/elastic/elastic-agent/6-version-management-and-upgrades>
- **Elastic Agent and the OpenTelemetry Collector's own supervisor** both manage only binaries they
  install; neither offers a supervise-but-never-update mode for a distribution-packaged program.
- **Bindplane** — distinguishes collectors it manages from detached ones, i.e. the same
  ownership boundary drawn here, expressed as an install mode rather than as a path shape.
  <https://docs.bindplane.com/deployment/virtual-machine/collector/install-and-uninstall-bindplane-collectors>
- **systemd unit specifiers** — `%S` (state directory), `%t` (runtime directory) and friends expand
  in a unit's arguments, and the manual states the executable path itself may not contain them: the
  same split clause 13 draws, for a related reason.
  <https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html>
- **systemd `StateDirectory=`** creates the directory and exports `$STATE_DIRECTORY` — the
  environment-variable alternative, rejected above because `argv` is not expanded.
- **ADR-0034's constraints** are the honest measure of what replaces a machine-installed program:
  glibc cannot be bundled, so a relocatable tree is per distribution family, and ADR-0015 forbids
  symlinks and hard links in a package tree. Repacking is a real path, not a free one.
- **The OpAMP Baseline's `AcceptsPackages`** is a per-Agent capability, not a per-fleet one; making
  it constant here is a narrowing of this implementation, not of the protocol, and a future decision
  could widen it again without a wire change.
- The general principle behind clause 2 — make the illegal state unrepresentable rather than
  validate it — is what removes a whole class of configuration error instead of reporting it.

## Consequences

- Positive: **the trap is gone.** A Client writes only inside a directory it owns; updating a Managed
  Process needs no root and no permissions on a system `bin`.
- Positive: **one kind of Managed Process.** Every feature that touches packages — targeting,
  versions, rollback, the health gate, the version probe — has one case to reason about, and the
  class of bug where a capability depends on how a path was spelled is gone.
- Positive: **the fleet's picture of a host does not depend on which file a block came from.** With
  ADR-0029's restriction matching what the loader accepts, a locally written block and a
  Server-delivered one describe the same kind of thing.
- Positive: **a silent revocation is impossible.** An operator who writes an absolute path gets a
  refusal at startup instead of an Agent that comes up managed-looking and quietly takes no packages.
- Positive: a **raw** artifact is installed by moving it rather than copying it — staging and target
  sit in one tree on one filesystem, so the install costs a metadata update instead of a second full
  write of several hundred megabytes. This does **not** extend to an archive, which has to be
  unpacked; since upstream Collector releases ship as `.tar.gz`, the most common case keeps writing
  the program twice, and the saving lands on artifacts published as bare binaries.
- Positive: `.rollback` and `.staged` never appear next to system binaries.
- Positive: hardening becomes expressible — systemd `ReadWritePaths=`/`StateDirectory=` over exactly
  one tree instead of write access to `/usr/local/bin`.
- Positive: `supervisor_dir` gets programs off a `noexec` or undersized `/var`.
- Positive: **no security property is traded away.** ADR-0015's containment holds unchanged, because
  this decision moves where a member is written and not how it is chosen.
- Positive: a Foreign Agent's configuration path is derived from the same value the Client derives
  it from, so relocating `supervisor_dir` — or renaming a Supervisor — cannot leave the process
  reading a file nobody writes, and the example configuration ships no hard-coded absolute path that
  is wrong for every host that does not use the defaults.
- Negative / trade-offs: **a configuration with `accepts_packages` is refused.** Keeping the program
  means moving it into the Supervisor's `program/` directory and reducing the path to a bare name.
  One edit per host, and it belongs in the release note.
- Negative / trade-offs: a bare name does not search `$PATH`. Anyone relying on that gets a
  different path with no error — clause 4's log line is the only thing that surfaces it.
- Negative / trade-offs: **the supervise-but-do-not-update use case is gone.** An operator who wants
  the host's package manager to keep owning an agent's updates while the fleet watches and configures
  it has no way to say so. This is the deliberate content of the decision, not a side effect: that
  operator must either repack the agent or not manage it here.
- Negative / trade-offs: **Icinga 2 on Windows has no machine-installed fallback.**
  [ADR-0033](0033-icinga-2-supervision-and-enrolment.md) planned one because Windows was
  unproven — if the MSI payload cannot be relocated, the same kind would supervise a machine-installed
  Icinga 2 there. If the Windows repack proves impossible, the Icinga 2 kind is Linux-only until some
  other decision addresses it — and this decision should be revisited rather than worked around.
- Negative / trade-offs: **adoption is harder.** Taking over a host that already runs a vendor agent
  is a repack plus a package rollout before the Agent appears in the fleet at all, not a one-line
  block naming the existing binary.
- Negative / trade-offs: changing `supervisor_dir` on a running host leaves the old tree behind —
  `instance-uid` included, so each Supervisor re-registers as a **new** Agent on the Server, losing
  its history there. Nothing migrates automatically; that is an operator action.
- Negative / trade-offs: one program per Supervisor instead of one shared copy. Three Supervisors
  running the same Collector distribution cost three copies of it.
- Negative / trade-offs: a typo in a placeholder name (`${config-dir}`) is passed through to the
  process rather than refused, which is the opposite of how ADR-0008 treats an unknown *key*. That
  is deliberate — the alternative breaks agents whose own syntax overlaps — but it is an
  inconsistency in the configuration's behaviour, and the documentation has to say so where the
  placeholders are listed.
- Negative / trade-offs: two ways to spell the same path in an argument exist, since an absolute
  path still works there. The example leads with the placeholder; nothing forces it.
- Negative / trade-offs: the placeholders are a second mechanism for "a path inside the Supervisor's
  directory", beside clause 2's rule for the program. They are deliberately not unified (see the
  alternatives), but a reader meets both and has to learn which applies where.
- Follow-ups: whether Managed Processes eventually need ADR-0010's versioned side-by-side layout; a
  decision on migrating an existing Supervisor tree when its root moves; whether the placeholders
  should be available in the Collector plugin's extra `args`, which has no need for them so far;
  whether a Foreign Agent that reads its environment should additionally be handed these paths as
  variables; whether a supervise-only mode should exist as its own decision, with its own capability
  model rather than as a path-parser fallthrough; and what happens to the Icinga 2 kind if the
  Windows repack cannot be made to work. None of these is decided here.
