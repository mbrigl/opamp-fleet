# ADR-0021: One directory per Supervisor — a bare program name means the Client owns it and updates it, an absolute path means it does not

- **Status:** 🟢 accepted
- **Date:** 2026-08-06
- **Deciders:** Markus Brigl

## Context

A Supervisor configured exactly as [`config/client.toml`](../../config/client.toml) documents it
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
[ADR-0017](0017-selector-targeted-packages.md) tied them together nowhere. The combination that
works and the combination that cannot are equally spellable.

Most of what the fix needs already exists. Every Supervisor already has its own directory,
`<state_dir>/supervisors/<name>/` (`build_engine`), holding its `instance-uid`, the received
`remote-config.pb`, `installed-package.json`, and the written `config/` entries; the block's `name`
is already validated through `parse_instance_name`, so it is by construction a legal directory name
on all three platforms ([ADR-0010](0010-client-os-service-and-cli.md)'s grammar), and duplicates are
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

**What this ADR deliberately leaves open.** A Managed Process is not always one file. A Foreign
Agent — the specification's term for a Managed Process under a Custom Supervisor — may ship as a
tree: an executable plus shared objects, plugins, and data files; on Windows a `.dll` beside its
`.exe` is a load requirement, not a convenience. Such an agent cannot be delivered as a package
today, because [ADR-0018](0018-packages-imported-from-a-url.md) lifts exactly **one** member out of
an archive, and it still cannot after this ADR. That gap is left open on purpose: closing it means
giving up ADR-0018's guarantee that *"the archive never chooses a path"* in exchange for a
traversal defence, and no deployment needs it yet. The layout below is chosen so that closing it
later is a widening — a configuration written under this ADR stays valid under that one — rather
than a second change to the same key. Standard Collector distributions are single binaries and are
fully served by what is decided here.

Constraints this decision has to respect: ADR-0008 (hand-edited TOML, a typo fails loudly at
startup), ADR-0010 (the operator chooses the root; the name grammar), ADR-0015 (the swap, the health
gate, the rollback), ADR-0017 (the **Server** chooses which artifact; the host only consents — and
the precedent of refusing a removed key loudly rather than ignoring it), ADR-0018 (the artifact and
how it is opened, untouched here), and [ADR-0020](0020-client-self-update.md) (the Client's own
consent is explicit and names its package).

## Decision

We will make the **shape of the program's path the whole of a Supervisor's consent to package
updates**, give every Supervisor one directory it owns, and let the operator place that directory.

### 1. A relocatable Supervisor root

A new optional top-level key `supervisor_dir` defaults to `<state_dir>/supervisors` — unchanged
behaviour when it is absent. Under it, one directory per Supervisor, holding everything that
Supervisor needs:

```
<supervisor_dir>/<name>/
  instance-uid            # as today
  remote-config.pb        # as today
  installed-package.json  # as today
  config/                 # as today
  program/<binary>        # new: the Managed Process, with its .rollback and .staged siblings
  packages/               # new: this Supervisor's download staging
```

One knob moves the whole tree, state and program together — not a second knob for the program alone.
The staging moving in beside `program/` makes the install a rename within one filesystem instead of
a copy across two. This is [ADR-0011](0011-supervisor-mode-hexagonal-core-and-plugins.md)'s
per-Supervisor state directory, given two more subdirectories and a root the operator can choose;
what a Supervisor owns is unchanged, only where it may be put.

The directory is called `program/` and not `bin/` deliberately. It holds one file today; if the
Foreign-Agent tree of the Context is ever unpacked, it lands under the same root and no path on disk
moves. A directory name is cheap now and a layout migration is not.

### 2. The path shape decides — for `binary` (Collector plugin) and `command` (`command` plugin) alike

| Value | Meaning |
|---|---|
| a **bare file name** — no path separator, no `..` | `<supervisor_dir>/<name>/program/<value>`. The Client owns the directory, so it may replace what is in it: the Agent declares `AcceptsPackages`. |
| an **absolute path** | Someone else's file — a distribution package, configuration management. Spawned exactly as today; the Agent declares **no** package capability. |
| anything else (`./x`, `a/b`, `../x`) | **Startup error**, naming the rule. |

The third row is what keeps the rule from needing a traversal guard: a bare name cannot escape the
directory, so there is nothing to sanitize. It also keeps the name the archive member is matched
against (ADR-0018) exactly where it already is, and it is the row a later decision on multi-file
packages would relax — into "any relative path without `..`" — without invalidating anything written
under this one.

A bare name consequently **stops meaning "search `$PATH`"**. `Command::new` has `execvp` semantics,
so `command = "fluent-bit"` searches the path today. That behaviour is undocumented, used in no
example, and fragile under a service manager whose `PATH` is minimal — we take the break rather than
add a fourth case to preserve it.

### 3. `accepts_packages` is removed and refused loudly

A `client.toml` still carrying it fails at startup with a message naming the path rule — the same
treatment ADR-0017 gave `package`, for the same reason: never silently ignore a key an operator
believes in.

### 4. The consent is logged, once per Supervisor, at startup

Because consent is now implicit, an operator who "fixes" a path to an absolute one silently revokes
a capability the Server can see. One line — `supervisor otelcol: packages accepted, program in
<dir>` or `supervisor otelcol: packages declined, program is an absolute path` — makes the derived
state readable without inferring it from the config.

### What deliberately does not change

- **ADR-0017 stands.** *Which* artifact an Agent receives is still the Server's decision, expressed
  as the package's Selector. This ADR replaces only how a host says *yes*, not who chooses.
- **ADR-0018 stands, whole.** One member is lifted out of an archive to a destination this Client
  picked; no archive path is ever used. Only the destination moves.
- **ADR-0015's swap stands.** Rename aside, write, rename into place, health-gate, roll back — the
  same file-level mechanics, in a directory the Client owns.
- **ADR-0020's `[self_update]` stays explicit and keeps naming its package.** The asymmetry is
  intentional: a package written over the Client takes the host out of reach, which is precisely
  where implicit, path-derived consent would be wrong.
- **No `versions/` + `current` layout for Managed Processes.** ADR-0010 needs it because the running
  Client cannot overwrite itself; a Managed Process is stopped before its swap and `.rollback`
  already covers the fallback.

## Alternatives considered

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
  ADR-0018's *guarantee* ("no archive path is ever used") for a *defence* (every archive path
  checked, links refused, total size and member count bounded), and defences have bugs that
  guarantees cannot. Nothing deployed needs it: standard Collector distributions are single
  binaries. Deferred, and deferrable at no cost — see the follow-up below.
- **A separate `bin_dir` beside `state_dir`** — two knobs for a separation nobody has asked for.
  If state and program must diverge, the operator can still symlink; the tree stays one thing.
- **A per-Supervisor `supervisor_dir` inside each `[[supervisor]]` block** — more expressive than any
  reported need; one root per Client matches how ADR-0010 already treats the Client's own root.
- **Require `./` for the new meaning, keeping a bare name as a `$PATH` lookup** — a fourth case in
  the rule, and a leading `./` reads as noise in TOML (and worse on Windows), all to preserve
  behaviour that is undocumented and unused.

## Sources / Prior art

- **OpenTelemetry `opampsupervisor`** — a per-supervisor `storage.directory` (default
  `/var/lib/otelcol/supervisor`, `%ProgramData%/Otelcol/Supervisor` on Windows) alongside an
  absolute `agent.executable`: the same split between the supervisor's own tree and a foreign
  binary, except that upstream never expresses consent at all.
  <https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/cmd/opampsupervisor>
- **Elastic Agent** — versioned home directory plus a symlink to the active executable, the shape
  ADR-0010 already follows for the Client itself; consulted for whether Managed Processes should get
  the same (decided against, for now).
  <https://deepwiki.com/elastic/elastic-agent/6-version-management-and-upgrades>
- **Bindplane** — distinguishes collectors it manages from detached ones, i.e. the same
  ownership boundary this ADR draws, expressed as an install mode rather than as a path shape.
  <https://docs.bindplane.com/deployment/virtual-machine/collector/install-and-uninstall-bindplane-collectors>
- The general principle behind part 2 — make the illegal state unrepresentable rather than validate
  it — is what removes a whole class of configuration error instead of reporting it.

## Consequences

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
  layout does not currently allow for Managed Processes.
- Positive: **no security property is traded away.** ADR-0018's containment holds unchanged, because
  this ADR moves where a member is written and not how it is chosen.
- Negative / trade-offs: **this breaks every `client.toml` with `accepts_packages`.** The key is
  refused at startup, and keeping updates means moving the program into the Supervisor's `program/`
  directory and reducing the path to a bare name. Leaving the absolute path is a valid choice — it
  just declines updates from then on. One edit per host, and it belongs in the release note.
- Negative / trade-offs: a bare name no longer searches `$PATH`. Anyone relying on that gets a
  different path with no error — part 4's log line is the only thing that surfaces it.
- Negative / trade-offs: **consent becomes implicit.** Editing a path now changes a capability the
  Server sees. The log line makes it visible; it does not make it explicit, and that is the price
  paid for removing the contradictory-configuration class.
- Negative / trade-offs: a Foreign Agent that is more than one file **still cannot be packaged**.
  The gap is unchanged from today, but this ADR makes the Client's ownership of the program
  directory explicit, which will make the gap look more surprising than it did before.
- Negative / trade-offs: changing `supervisor_dir` on a running host leaves the old tree behind —
  `instance-uid` included, so each Supervisor re-registers as a **new** Agent on the Server, losing
  its history there. Nothing migrates automatically; that is an operator action.
- Negative / trade-offs: one program per Supervisor instead of one shared copy. Three Supervisors
  running the same Collector distribution now cost three copies of it.
- Follow-ups: **multi-file packages for Foreign Agents** — unpacking an archive whole into
  `program/`, relaxing part 2's relative form to any path without `..`, and swapping the tree by
  directory rename. That decision has to carry its own containment (member paths normalized and
  refused rather than skipped, symlink and hard-link members refused, a total unpacked size and
  member count beside today's per-member limit) and should weigh making the tree an explicit opt-in
  on the block rather than the default, so only the hosts that need it carry the risk. It should
  also settle how a version disappears from a configured path, since upstream releases lay their
  program under a versioned directory.
- Follow-ups: whether `working_dir` and `args` should resolve relative paths the same way — today's
  `fluent-bit` example hard-codes an absolute path into the config directory that `supervisor_dir`
  can now move underneath it; whether Managed Processes eventually need ADR-0010's versioned
  side-by-side layout; whether a stale `.rollback` should be pruned on a schedule rather than only
  on the next apply; and a decision on migrating an existing Supervisor tree when its root moves.
