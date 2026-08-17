# ADR-0068: Icinga 2 is supervised by a kind of its own — the relocation arguments, the directories, and the validation are the Supervisor's, not the operator's

- **Status:** 🟢 accepted
- **Date:** 2026-08-17
- **Deciders:** Markus Brigl

## Context

Icinga 2 should reach a host the way every other Managed Process does: as a package the Server
offers, that the Client unpacks, updates, and rolls back — never as a distribution package installed
beside the fleet. The Agent role is the target, so each host also needs a certificate signed by an
Icinga master ([ADR-0069](0069-the-icinga-master-signs-the-ticket-travels-as-a-configuration.md)).

[ADR-0063](0063-the-glpi-agent-is-supervised-by-the-command-kind.md) established that a Foreign
Agent needs no plugin of its own when *"every difference between the platforms is a value the block
already expresses — program path, arguments — not a behavior the `Runner` lacks"*. Icinga 2 is the
case where that stops holding, and a spike against Icinga 2.14.6 measured why rather than assuming
it. Running a repacked tree from an arbitrary directory works — no compiled-in path is touched at
all, verified with `strace` — but only under conditions the block cannot carry:

- **Every invocation must name the account it runs under.** Without `-D RunAsUser=`/`-D RunAsGroup=`
  *every* subcommand — `daemon`, `pki`, even a validation — refuses with *"Please re-run this command
  as a privileged user or using the `nagios` account"*, because the compiled-in user does not exist
  on a fleet-managed host and the Client's service account ([ADR-0062](0062-the-service-runs-under-an-operator-named-account.md))
  is not it.
- **The ITL is found through `-D IncludeConfDir=`, not `-I`.** Measured: with `-I` alone, `include
  <itl>` still resolved to the *host's* `/usr/share/icinga2`, silently using a copy the fleet does
  not control — the worst kind of working. `-D IncludeConfDir=` resolves into the delivered tree.
- **Icinga creates none of its directories.** With `DataDir`, `LogDir`, `CacheDir`, `SpoolDir` or
  `InitRunDir` pointing at a path that does not exist, startup fails on the first write. Debian's
  packages solve this with `ExecStartPre=prepare-dirs`, which hard-fails without the `nagios` user
  and is therefore not usable here.
- **A failed reload is silent from the outside.** After `SIGHUP` with a broken configuration the
  daemon logs *"Found error in config: reloading aborted"* **to stderr** and keeps running the old
  configuration, with the same pid and the same worker. A Supervisor that acknowledged the apply
  because the process survived would report `APPLIED` for a configuration that never took effect.

- **A killed umbrella orphans its worker.** Icinga 2 runs as an umbrella process with a worker child.
  `SIGTERM` to the umbrella takes the worker with it, measured, within two seconds — but `SIGKILL`
  leaves the worker running and reparented to init, still holding the data directory, the log file
  and port 5665. The bounded stop of ADR-0060 escalates to exactly that signal when the budget runs
  out, so the escalation can leave a second instance behind on the very host the fleet is managing.

Two further measurements shape the decision rather than force it: `SIGHUP` **keeps the pid** of the
umbrella process (only its worker is replaced), so the `Runner`'s watchdog and the reload of
[ADR-0060](0060-unified-supervisor-lifecycle-port.md) fit Icinga 2 as they are; and `--version`
prints `r2.14.6-1`, which the strict SemVer probe rejects — the same gap ADR-0063 recorded for GLPI.

## Decision

We will add a **compiled-in Supervisor Plugin `icinga2`** — one module in
`crates/client/src/supervisor/` and one line in `registry()`, the extension point ADR-0011 named and
ADR-0060 anticipated for *"a kind whose installation is not a file swap"*. It **reuses `Runner`
unchanged** for spawn, watchdog, backoff, bounded stop, package swap, rollback, retention and health.

Bound by this decision:

- **The block states values; the kind derives arguments.** The operator writes the parent, the node
  name, and where things live; the kind assembles `daemon -c … -D IncludeConfDir=… -D RunAsUser=…
  -D RunAsGroup=… -D NodeName=… -D DataDir=… -D LogDir=… -D CacheDir=… -D SpoolDir=…
  -D InitRunDir=… -D PluginDir=… -x …`. Nine derived arguments in `args` on every host is a typo
  waiting to happen, and three of them (`RunAsUser`, `RunAsGroup`, `IncludeConfDir`) are not
  operator choices at all — they follow from the Client's own account and its own directory layout.
- **The program key is `binary`**, as the Collector's is: the thing that gets swapped. Bare name
  means the tree is the Client's and takes packages, an absolute path means the machine's
  (ADR-0021) — **both are supported**, so a platform where the repacked tree turns out not to run
  keeps the same supervision and configuration model with a machine-installed Icinga 2.
- **The kind creates the state directories** before every spawn, under `data_dir` and its siblings,
  0700. It does not run `prepare-dirs`, does not create users, and drives no service manager.
- **Foreground, one child.** `daemon` without `-d` and without `--close-stdio`; stdout and stderr are
  inherited into the Client's logging (ADR-0041).
- **State lives beside the tree, never in it:** `data/` (which holds the certificates), the enrolment
  marker, and the pinned parent certificate are siblings of `program/` and `config/`, so a package
  swap replaces the tree without touching the identity, and ADR-0059's purge still takes everything.
- **A configuration is validated before it is applied.** `daemon -C` runs against the delivered
  configuration first; on failure the running daemon is not touched and the apply is answered
  `ConfigApplied{Err}` with the validator's message. This is the measured case above, and ADR-0060's
  rule that an adapter must not acknowledge what it cannot verify.
- **Reload by `SIGHUP` on unix**, restart on Windows — the existing reload-or-restart of ADR-0060,
  enabled because the umbrella pid was measured to be stable.
- **`build()` yields no process until both the main configuration and a certificate exist.** The
  `Runner` then reports plainly what is missing instead of crash-looping toward a hold.
- **The daemon is stopped as a group, not as a pid.** The process is started in its own process
  group and the stop signals the group, so the escalation to `SIGKILL` cannot leave the worker
  behind. Without this, the one path that is *supposed* to guarantee a stopped process is the one
  that produces a second instance.
- **Three changes to shared code**, stated as such because both existing kinds see them:
  - **A package is proved to run before it is installed.** After the artifact is unpacked into
    `program/.staging` and before the swap, the `Runner` runs the staged program once with a
    plugin-supplied preflight (for Icinga 2: `--version`, ~30 ms, no privileges, no state). A failure
    answers `PackageApplied{Err}` carrying the dynamic linker's own message — *"version `GLIBC_2.39'
    not found"*, *"cannot open shared object file"* — and **nothing is swapped**, so a Managed
    Process is never stopped for a package that could not have run. The health gate and rollback of
    ADR-0058 stay as the second line for what a preflight cannot see.
  - **The version probe may bring its own parser.** `VersionProbe` gains an optional parse function,
    defaulting to today's strict SemVer, so `r2.14.6-1` is reported as `2.14.6` instead of not at all.
  - **A process may ask to own its process group**, which is what makes the group stop above
    possible. Opt-in per kind, so the Collector and the `command` kind keep today's behaviour; on
    Windows the equivalent (a job object) is left for the platform work, and the kind's stop there
    remains what it is today.

## Alternatives considered

- **A recipe under the `command` kind, as for GLPI.** The honest first try, and the reason this ADR
  exists: it can express the arguments, but not the directory preparation, not the pre-start gate on
  the certificate, and not the validation. Its failure mode is the bad one — a fleet reporting
  `APPLIED` for a configuration Icinga refused, and a crash loop while a host waits for a
  certificate. Rejected on evidence, not on taste.
- **Operator-written hook keys** (`pre_start_cmd`, `validate_cmd`) on the generic kind. Rejected for
  the reason ADR-0060 already rejected them: *"a key that is the mechanism"* moves the decision into
  every host's configuration file and makes the Supervisor's behaviour unreviewable.
- **Driving `systemctl` / `sc.exe`.** Rejected as ADR-0063 rejected it: no watchdog, no health gate,
  no bounded stop, and it supervises a process the Client did not start.
- **`icinga2 node setup` for the whole bootstrap.** Convenient, and wrong here: it writes into
  `ConfigDir`, the one constant that is not reliably relocatable — see ADR-0069.
- **Requiring the host to carry a `nagios` user.** Would remove the `RunAsUser` arguments and add a
  provisioning step outside the fleet, on every host, for no gain: the Client's account already owns
  the directories in question.
- **Preflight by inspecting the artifact statically** (reading the required `GLIBC_` symbols out of
  the tree and comparing them against the host). Rejected in favour of running the program once:
  a start attempt is the definition of "does this run here", it catches a missing library as well as
  a too-old libc, and it produces the message the operator needs without this project maintaining a
  model of dynamic linking.

## Sources / Prior art

- Spike against **Icinga 2.14.6-1** (Debian trixie packages, 2026-08-17): relocated tree with
  bundled libraries, `strace`-verified absence of any compiled-in path access, the `-D` matrix, the
  `RunAsUser` refusal, the `-I` vs. `IncludeConfDir` measurement, pid stability across `SIGHUP`, the
  silent reload abort, `SIGTERM` within two seconds, and a 39 MB / 52-file tree.
- [Icinga 2 CLI commands](https://icinga.com/docs/icinga-2/latest/doc/11-cli-commands/) and
  [language reference](https://icinga.com/docs/icinga-2/latest/doc/17-language-reference/) — the
  `daemon` flags, the constants, and the include semantics.
- [`icinga-app/icinga.cpp`](https://github.com/Icinga/icinga2/blob/master/icinga-app/icinga.cpp) —
  where the path constants come from per platform, and that `-D` is applied before they are frozen.
- [ADR-0011](0011-supervisor-mode-hexagonal-core-and-plugins.md) (a kind is a module plus a registry
  line), [ADR-0060](0060-unified-supervisor-lifecycle-port.md) (the lifecycle vocabulary and the
  extension point), [ADR-0063](0063-the-glpi-agent-is-supervised-by-the-command-kind.md) (the
  foreground-daemon shape and what a Foreign Agent recipe looks like when no plugin is needed).

## Consequences

- Positive: Icinga 2 becomes an ordinary fleet citizen — rolled out, updated, rolled back, and
  configured through the same acts as every other Managed Process, with no host-side installation.
- Positive: the nine relocation arguments are written once, in code, instead of once per host; the
  three that are not operator choices cannot be got wrong at all.
- Positive: the preflight makes every tree package safer, not only Icinga's — a Collector built
  against a newer libc is now refused before the running one is stopped.
- Negative / trade-offs: a third plugin kind to maintain, and one that shells out to its own program
  for validation and enrolment. Accepted: the alternative is a Supervisor that lies about applies.
- Negative / trade-offs: two shared-code changes touch what the Collector and the `command` kind
  use. Both are additive and default to today's behaviour.
- Negative / trade-offs: the Managed Process runs under the Client's service account, not `nagios`,
  so checks needing elevated capabilities (`check_icmp`) fail. Documented, not worked around.
- Follow-ups: the artifacts this kind expects are [ADR-0070](0070-repacked-vendor-packages-as-relocatable-icinga-2-trees.md);
  enrolment is ADR-0069. Windows is unproven — if the MSI payload cannot be relocated, the same kind
  supervises a machine-installed Icinga 2 there, which is why the absolute-path form is in scope
  from the start.
