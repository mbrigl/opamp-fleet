# ADR-0033: Icinga 2 is supervised by a kind of its own — the Icinga master stays the CA, the ticket travels as a Configuration, and the block keeps only what enrolment needs

- **Status:** 🟢 accepted
- **Date:** 2026-08-21
- **Deciders:** Markus Brigl

Applies [ADR-0037](0037-a-kind-knows-its-own-agent.md) — *derivable means derived; a key exists only
for a decision* — to the `icinga2` kind. The rule, the `Plugin` seam, the migration mechanism and the
artifact-document requirement are stated there and not repeated here.

## Context

Icinga 2 should reach a host the way every other Managed Process does: as a package the Server
offers, that the Client unpacks, updates, and rolls back — never as a distribution package installed
beside the fleet. The Agent role is the target, so each host also needs a certificate signed by an
Icinga master.

A Foreign Agent needs no plugin of its own when every difference between the platforms is a value
the block already expresses — program path, arguments — not a behavior the `Runner` lacks. Icinga 2
is the case where that stops holding, and a spike against Icinga 2.14.6 measured why rather than
assuming it. Running a repacked tree from an arbitrary directory works — no compiled-in path is
touched at all, verified with `strace` — but only under conditions the block cannot carry:

- **Every invocation must name the account it runs under.** Without `-D RunAsUser=`/`-D RunAsGroup=`
  *every* subcommand — `daemon`, `pki`, even a validation — refuses with *"Please re-run this command
  as a privileged user or using the `nagios` account"*, because the compiled-in user does not exist
  on a fleet-managed host and the Client's service account ([ADR-0010](0010-client-os-service-and-installation-layout.md)
  clause 18) is not it.
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
  and port 5665. The bounded stop of ADR-0011 escalates to exactly that signal when the budget runs
  out, so the escalation can leave a second instance behind on the very host the fleet is managing.

Two further measurements shape the decision rather than force it: `SIGHUP` **keeps the pid** of the
umbrella process (only its worker is replaced), so the `Runner`'s watchdog and the reload of
[ADR-0011](0011-supervisor-mode-and-lifecycle-port.md) fit Icinga 2 as they are; and `--version`
prints `r2.14.6-1`, which the strict SemVer probe rejects.

**Most of what a block could say describes an artifact this project builds itself.** Every path in
an Icinga block — the program, `program_path`, the ITL, the plugins, `LD_LIBRARY_PATH` — follows
from the tree `opamp-package-fetch --agent icinga2` produces
([ADR-0034](0034-repacked-icinga-2-artifacts.md)), and on Windows three of
those values differ — `icinga2.exe`, `sbin/icinga2.exe`, and a `plugin_dir` pointing at `sbin`,
because a Windows program finds its DLLs beside itself. The five state directories are derived
already (`Icinga2Plugin::layout` defaults them to `${supervisor_dir}/…`). **What is left over is not
the agent — it is the installation the host is joining**: the parent's address, the ticket, the
pinned certificate, and the name the ticket was issued for. Those cannot be computed here.

**Who obtains the certificate, and with what secret**, is the other question a fleet-delivered
Icinga 2 has to answer. The forces are unusually clear:

- **This project already has a CA, and it is the wrong one.** [ADR-0013](0013-opamp-endpoint-admission.md)
  lets the fleet Server sign client certificates for the OpAMP link. Reusing it for Icinga would make
  the fleet Server an Icinga CA — a second trust root inside somebody else's monitoring topology,
  which nothing in the specification asks for and which no Icinga master would accept anyway.
- **The Icinga flow is ticket-based and non-interactive.** The master computes a ticket as an HMAC of
  the node's common name under its own `TicketSalt`; the node generates a key locally, sends a CSR,
  and receives a signed certificate. The private key never leaves the host.
- **The one-shot bootstrap must not touch `ConfigDir`.** `icinga2 node setup` does the whole dance in
  one command — and writes `zones.conf`, `api.conf` and `constants.conf` into `ConfigDir`, the single
  constant that is not reliably relocatable. The lower-level `pki` subcommands (`new-cert`,
  `save-cert`, `request`, `verify`) take **every path as an argument**; a spike confirmed they touch
  no system directory at all and write the private key `0600` themselves.
- **The Server already has a way to deliver a per-host secret.** [ADR-0012](0012-selector-targeted-configurations-and-rest-api.md)'s
  `supplementary` role writes a Configuration entry as a plain file the Supervisor does not pass to
  its process, and [ADR-0012](0012-selector-targeted-configurations-and-rest-api.md)'s
  Selectors already aim a Configuration at exactly one Agent. A ticket is precisely that: a per-host
  string that must land as a file and go nowhere else.
- **A Configuration apply empties the entry directory.** Every apply deletes the entry files before
  writing the new set, so anything needed *after* the enrolment must be copied out of it.

## Decision

We will add a **compiled-in Supervisor Plugin `icinga2`** whose block keeps only its enrolment, keep
**the Icinga master as the only CA**, and have the Supervisor enrol **once, on the host**, with the
ticket and the parent's certificate delivered as ordinary Configurations.

### The kind

1. **One module, `Runner` unchanged.** The kind is one module in `crates/client/src/supervisor/` and
   one line in `registry()`, the extension point ADR-0011 named and ADR-0011 anticipated for *"a
   kind whose installation is not a file swap"*. It **reuses `Runner` unchanged** for spawn,
   watchdog, backoff, bounded stop, package swap, rollback, retention and health.

2. **The kind derives the arguments.** It assembles `daemon -c … -D IncludeConfDir=… -D RunAsUser=…
   -D RunAsGroup=… -D NodeName=… -D DataDir=… -D LogDir=… -D CacheDir=… -D SpoolDir=…
   -D InitRunDir=… -D PluginDir=… -x …`. Nine derived arguments in `args` on every host is a typo
   waiting to happen, and three of them (`RunAsUser`, `RunAsGroup`, `IncludeConfDir`) are not
   operator choices at all — they follow from the Client's own account and its own directory layout.

3. **The kind supplies what the tree decides**, per platform: the program name (`icinga2` +
   `EXE_SUFFIX`), `program_path` (`sbin/icinga2[.exe]`), `include_dir`
   (`<tree>/share/icinga2/include`), `plugin_dir` (`<tree>/plugins`, and `<tree>/sbin` on Windows),
   `service_name` (`icinga2`), and `LD_LIBRARY_PATH` on Unix. The five state directories keep the
   values they already default to. None of these is a key; on ADR-0037's terms a block still
   carrying one fails at startup naming the derived value. The program is always the tree the
   Client installed (ADR-0018).

4. **The state directories exist before every spawn**, under the data directory and its siblings,
   0700 — guaranteed by the process specification (ADR-0037 clause 2). The kind does not run
   `prepare-dirs`, does not create users, and drives no service manager.

5. **Foreground, one child.** `daemon` without `-d` and without `--close-stdio`; stdout and stderr are
   inherited into the Client's logging (ADR-0026).

6. **State lives beside the tree, never in it:** `data/` (which holds the certificates), the enrolment
   marker, and the pinned parent certificate are siblings of `program/` and `config/`, so a package
   swap replaces the tree without touching the identity, and ADR-0029's purge still takes everything.

7. **A configuration is validated before it is applied.** `daemon -C` runs against the delivered
   configuration first; on failure the running daemon is not touched and the apply is answered
   `ConfigApplied{Err}` with the validator's message. This is the measured silent reload abort, and
   ADR-0011's rule that an adapter must not acknowledge what it cannot verify.

8. **Reload by `SIGHUP` on unix**, restart on Windows — the existing reload-or-restart of ADR-0011,
   enabled because the umbrella pid was measured to be stable.

9. **`build()` yields no process until both the main configuration and a certificate exist.** The
   `Runner` then reports plainly what is missing instead of crash-looping toward a hold.

10. **The daemon is stopped as a group, not as a pid.** The process is started in its own process
    group and the stop signals the group, so the escalation to `SIGKILL` cannot leave the worker
    behind. Without this, the one path that is *supposed* to guarantee a stopped process is the one
    that produces a second instance.

11. **Three changes to shared code**, stated as such because the other kinds see them:
    - **A package is proved to run before it is installed.** After the artifact is unpacked into
      `program/.staging` and before the swap, the `Runner` runs the staged program once with a
      plugin-supplied preflight (for Icinga 2: `--version`, ~30 ms, no privileges, no state). A
      failure answers `PackageApplied{Err}` carrying the dynamic linker's own message — *"version
      `GLIBC_2.39' not found"*, *"cannot open shared object file"* — and **nothing is swapped**, so a
      Managed Process is never stopped for a package that could not have run. The health gate and
      rollback of ADR-0015 stay as the second line for what a preflight cannot see.
    - **The version probe may bring its own parser.** `VersionProbe` takes an optional parse
      function, defaulting to strict SemVer, so `r2.14.6-1` is reported as `2.14.6` instead of not
      at all.
    - **A process may ask to own its process group**, which is what makes the group stop above
      possible. Opt-in per kind, so the Collector and the `command` kind keep their behaviour; on
      Windows the equivalent (a job object) is left for the platform work, and the kind's stop there
      remains a plain stop.

### Enrolment

12. **The fleet Server signs nothing and holds no `TicketSalt`.** It transports two artefacts that are
    not private keys: the **ticket** (an HMAC over the common name, useless for any other node) and
    the **parent's certificate** (public by nature). No private key ever reaches the Server, and the
    OpAMP PKI of ADR-0013 and the Icinga PKI never touch.

13. **The ticket is a Configuration named for the Supervisor's use, with `role = "supplementary"` and
    a Selector matching one Agent.** It lands as a file the Supervisor reads and nothing else
    consumes.

14. **The parent's certificate is pinned, not trusted on sight.** It arrives the same way, and is
    **copied out of the entry directory** at enrolment, because the next apply would delete it.
    `icinga2 pki save-cert` — trust on first use — remains as a fallback, taken only when no pinned
    certificate was delivered, and logged with the fingerprint it accepted.

15. **Enrolment is three `pki` calls, never `node setup`:** `pki new-cert` (key and CSR, explicit paths),
    then `pki request` against the parent with the ticket, the pinned certificate, and the target
    paths. Nothing is written outside the Supervisor's own directory.

16. **The certificate on disk is the state; the marker is a hint.** Enrolment runs when there is no
    usable certificate — verified with `pki verify`, not by looking at a flag — or when it is inside
    its renewal window, or when the common name or the parent changed. A marker file records what was
    enrolled, so the ordinary start costs no subprocess.

17. **Without a ticket, enrolment is still correct.** The CSR reaches the master's signing queue and
    waits for `icinga2 ca sign` there. The Agent stays unhealthy until it is signed, saying so.

18. **An unreachable master is a wait, not a failure.** Health reports what is missing, the attempt
    backs off and repeats, and the daemon is not started — no crash loop, and the Client's own startup
    never blocks on somebody else's master.

19. **Renewal is the daemon's own**, with the Supervisor as a start-time safety net only: a certificate
    past its renewal window is re-requested before the daemon starts, never while it runs.

20. **Revocation stays on the master.** Removing the Supervisor removes its key material with the
    directory (ADR-0029); the master's `icinga2 ca remove` is the operator's, and the Supervisor says
    so when it is retired rather than pretending it cleaned up.

### The block

21. **`parent_host` carries the port.** `master.example.com:5665`, defaulting to Icinga's 5665; there
    is no `parent_port`. One address is one value; splitting it bought a key that was wrong in exactly
    one direction (a port without a host names no parent).

22. **`renew_before_days`, `run_as_user`, `run_as_group` and `log_level` are not keys.** Renewal is at
    30 days. The account is the one this Client runs as — a Managed Process the fleet installed has no
    business under another. And logging belongs in Icinga's own configuration, where `object
    FileLogger` carries a `severity`: raising verbosity is a Configuration the fleet rolls out rather
    than a flag in one host's file.

23. **The root Configuration is marked by the fleet, as a role.** Which delivered Configuration is
    Icinga's root — the one file the daemon is pointed at, from which it `include`s the rest — cannot
    be derived from the entries: `icinga2-conf` and `icinga2-zones` are both delivered without a role,
    and being unroled says *"this is configuration"*, not *"this is the root"*. So the fleet says it
    with the field the Baseline provides for exactly this, in exactly these words:

    > Optional role of the content in the body field. **The values and their semantics are Agent
    > type-specific.**

    A vocabulary of one kind's own is therefore the field working as intended, not a corner of it
    being borrowed: `main` means *this* to `icinga2` and nothing to anyone else. The root carries
    **`role = "main"`** ([ADR-0012](0012-selector-targeted-configurations-and-rest-api.md), whose empty/`supplementary`
    pair stays the reading for kinds that define nothing further), and the kind takes that entry.
    Where nothing carries it, the conventional name `icinga2-conf` — what `opamp-package-fetch`
    uploads — is the fallback. The role stays interpreted per kind: to a Collector any non-empty role
    still means "written, never passed as `--config`". A block carrying `main_config` is refused.

24. **Four keys stay, and all four are the enrolment.** This kind supervises the **Agent role and
    never a master**; these are not settings *of* a master but what the Agent must know to reach one,
    and every one of them is consumed on this side:

    | Key | What the Agent does with it |
    |---|---|
    | `node_name` | its own `NodeName`, the CN of its certificate, its Endpoint name |
    | `parent_host` | the address it dials, to enrol and then to connect |
    | `ticket_file` | the ticket it presents when asking for a certificate |
    | `trusted_cert_file` | the parent's certificate it pins, instead of trusting on sight |

    `node_name` **defaults to the host's FQDN** — Icinga's own convention (`hostname --fqdn`) and what
    an operator following Icinga's instructions feeds `pki ticket --cn` — rather than the Supervisor's
    name, which the instance-name grammar of ADR-0010 cannot even spell as an FQDN and which would be
    wrong on nearly every host.

    The FQDN is **resolved**, not taken from what this Agent already reports: `host.name` is
    `gethostname`, which the semantic conventions permit to be either form and which is the short
    name on most Linux hosts — a default that fails enrolment rather than merely reading oddly.
    `getaddrinfo` with `AI_CANONNAME` is the same route `hostname --fqdn` takes, and **only a name
    containing a dot is accepted**: an unqualified answer is what a resolver hands back for a host
    with no domain, and taking it would reintroduce the Supervisor-name default. Resolved once per
    process, and only where no `node_name` is configured, so a slow resolver costs one lookup and an
    operator who states the name costs none. On Windows nothing is resolved and the Supervisor's name
    stays the default — this kind is unproven there, and reaching for a platform API this crate does
    not otherwise use would be a cost paid for a case nobody runs yet. The key survives because a host
    whose master knows it under another CN must still be able to say so: a mismatch does not read
    oddly in a dashboard, **enrolment fails**.

    **All four are already rollable, which is why they need no mechanism of their own.** The
    `[[supervisor]]` blocks are the fleet-managed half of `supervisor.toml`
    ([ADR-0029](0029-supervisor-set-from-the-server.md)), so a Configuration
    for the Client's own Agent carries every one of them; and the two that are *files* travel as
    supplementary Configurations, exactly as the ticket reaches a host (clause 13, ADR-0012). What
    matters is how many of them have to be **written** — see the consequence about a fleet-wide Icinga
    set below.

25. **`args` and `env` are not keys.** ADR-0037 keeps the escape hatch only on the kinds that
    know nothing about their agent. A wrapper that needed one would be a wrapper that does not know
    its agent — and where an operator genuinely needs to change how Icinga runs, Icinga's own
    configuration is the place, which the fleet already delivers.

26. **This kind's artifact document is `docs/artifacts/icinga2.md`**, in the shape ADR-0037 clause 9
    defines, pinned by the two tests it requires: one against the repack plan in
    `opamp-package-fetch` (the Debian repack, the MSI payload, the wrapper directory name, the
    output names), one against the constants above, `cfg`-gated per platform.

## Alternatives considered

- **A recipe under the `command` kind.** It can express the arguments, but not the directory
  preparation, not the pre-start gate on the certificate, and not the validation. Its failure mode is
  the bad one — a fleet reporting `APPLIED` for a configuration Icinga refused, and a crash loop while
  a host waits for a certificate. Rejected on evidence, not on taste.
- **Operator-written hook keys** (`pre_start_cmd`, `validate_cmd`) on the generic kind. Rejected for
  the reason ADR-0011 already rejected them: *"a key that is the mechanism"* moves the decision into
  every host's configuration file and makes the Supervisor's behaviour unreviewable.
- **Driving `systemctl` / `sc.exe`.** Rejected: no watchdog, no health gate, no bounded stop, and it
  supervises a process the Client did not start.
- **`icinga2 node setup` for the whole bootstrap.** One command instead of several, and it writes
  into `ConfigDir`, which the spike showed to be the one path that cannot be relocated. Rejected on
  that alone; the `pki` subcommands do the same work with explicit paths.
- **Requiring the host to carry a `nagios` user.** Would remove the `RunAsUser` arguments and add a
  provisioning step outside the fleet, on every host, for no gain: the Client's account already owns
  the directories in question.
- **Preflight by inspecting the artifact statically** (reading the required `GLIBC_` symbols out of
  the tree and comparing them against the host). Rejected in favour of running the program once:
  a start attempt is the definition of "does this run here", it catches a missing library as well as
  a too-old libc, and it produces the message the operator needs without this project maintaining a
  model of dynamic linking.
- **Let the fleet Server issue Icinga certificates** by extending ADR-0013's CA. Rejected: it makes
  the fleet Server a trust root in the monitoring topology, requires the Icinga master to trust it,
  and doubles the blast radius of a Server compromise for a certificate the master could sign itself.
- **Deliver the finished certificate and private key as Configurations.** Simplest to implement and
  the worst outcome: a private key generated centrally, travelling through the Server, stored in its
  Configuration store, and readable to whoever reads the fleet. Rejected outright — the Icinga flow
  exists precisely so the key stays put.
- **Trust on first use as the default.** Convenient at scale, and it hands the first attacker in the
  path a permanent foothold. Kept only as a logged fallback for the case where nothing was pinned.
- **A ticket in `client.toml` on every host.** Works, and puts a per-host secret in the file the
  fleet also rewrites (ADR-0029). Rejected as the default: the Selector-targeted Configuration is
  the mechanism this project already has for exactly this.
- **Enrolment in `Plugin::start`** rather than in the adapter task. Rejected: it would block the
  Client's startup on the reachability of an Icinga master.
- **Keep the paths as overrides with the derived values as defaults.** Nothing breaks, and a
  differently packed tree still works. Rejected on ADR-0037's general ground — two sources of truth
  for a value the Client computes — and on a specific one: the values are not *this host's*, they
  are the artifact's, so a per-host override can only ever be wrong or redundant.
- **Probe the unpacked tree for `sbin/icinga2*` instead of compiling the layout in.** It would
  survive a repacked tree. Rejected: a wrong guess is silent and lands on a host, while a constant
  is testable against the artifact this project builds and fails loudly when that artifact moves.
- **Require the root Configuration to be named `icinga2-conf`, full stop.** Simplest: no key, no
  role. Rejected: a fleet that does not use `opamp-package-fetch` names its Configurations itself,
  and nothing would be left to say which one is the root — the failure being a daemon pointed at a
  file that is not its configuration.
- **Derive `node_name` from the FQDN and drop the key.** Two lines for Icinga, like `glpi` and
  `telegraf`. Rejected: a host already enrolled under another CN could not be expressed at all, and
  the failure is a refused enrolment rather than a cosmetic mismatch.

## Sources / Prior art

- Spike against **Icinga 2.14.6-1** (Debian trixie packages, 2026-08-17): relocated tree with
  bundled libraries, `strace`-verified absence of any compiled-in path access, the `-D` matrix, the
  `RunAsUser` refusal, the `-I` vs. `IncludeConfDir` measurement, pid stability across `SIGHUP`, the
  silent reload abort, `SIGTERM` within two seconds, and a 39 MB / 52-file tree; `pki new-cert` and
  `pki verify` with explicit paths touch no system directory, write the key `0600`, and require the
  same `-D RunAsUser=`/`-D RunAsGroup=` as every other invocation.
- [Icinga 2 CLI commands](https://icinga.com/docs/icinga-2/latest/doc/11-cli-commands/) — the
  `daemon` flags; `pki new-cert`, `save-cert`, `request`, `verify`, `ticket`, `ca sign`.
- [Icinga 2 language reference](https://icinga.com/docs/icinga-2/latest/doc/17-language-reference/)
  — the constants and the include semantics.
- [Icinga 2 — Configuration](https://icinga.com/docs/icinga-2/latest/doc/04-configuration/) — the
  constants the daemon takes as `-D`, and `object FileLogger` with its `severity`, which is where
  logging verbosity belongs.
- [Icinga 2 — Distributed Monitoring](https://icinga.com/docs/icinga-2/latest/doc/06-distributed-monitoring/):
  the ticket as *"a client ticket … generated on the master"*, CSR auto-signing, and the on-demand
  signing queue; `NodeName` *"should be set to FQDN which is the default if not set"*, the
  requirement that `NodeName`, the certificate CN and the Endpoint object name are the same string,
  and `icinga2 pki ticket --cn '<fqdn>'` as the way a ticket is minted.
- [`icinga-app/icinga.cpp`](https://github.com/Icinga/icinga2/blob/master/icinga-app/icinga.cpp) —
  where the path constants come from per platform, and that `-D` is applied before they are frozen.
- [ADR-0011](0011-supervisor-mode-and-lifecycle-port.md) (a kind is a module plus a registry
  line), [ADR-0011](0011-supervisor-mode-and-lifecycle-port.md) (the lifecycle vocabulary and the
  extension point).
- [ADR-0013](0013-opamp-endpoint-admission.md) — the other PKI in this
  project, and the one this decision deliberately does not reuse.
- [ADR-0012](0012-selector-targeted-configurations-and-rest-api.md) and [ADR-0012](0012-selector-targeted-configurations-and-rest-api.md)
  — the delivery mechanism for a per-host file that is not the process's configuration.
- **[ADR-0034](0034-repacked-icinga-2-artifacts.md)** — the tree whose
  shape this kind compiles in; **[ADR-0034](0034-repacked-icinga-2-artifacts.md)**
  — the Windows payload, whose plugin location is the one platform difference that survives as a
  constant.
- **`crates/client/src/supervisor/icinga2.rs`, `Icinga2Plugin::layout`** — the five directory
  defaults.

## Consequences

- Positive: Icinga 2 becomes an ordinary fleet citizen — rolled out, updated, rolled back, and
  configured through the same acts as every other Managed Process, with no host-side installation.
- Positive: the nine relocation arguments are written once, in code, instead of once per host; the
  three that are not operator choices cannot be got wrong at all.
- Positive: the preflight makes every tree package safer, not only Icinga's — a Collector built
  against a newer libc is refused before the running one is stopped.
- Positive: a host enrols itself with no operator on it, and the fleet never becomes a certificate
  authority for a system it does not own.
- Positive: the private key is generated where it is used and never travels; the worst a compromised
  fleet Server leaks is a ticket bound to one common name.
- **Positive: one Supervisor set can configure every Icinga host.** The blocks are fleet-managed
  (ADR-0029), but a fleet-wide one is only useful if every host may run the *same* block. With the
  FQDN default `node_name` need not be written: one Configuration carries `parent_host` for everyone,
  the per-host ticket keeps arriving as its own supplementary Configuration, and one Configuration
  serves the fleet rather than one per host.
- **Positive: there is no Windows block.** The three values that differ on Windows are `cfg`-gated
  constants with a test behind them.
- Negative / trade-offs: a plugin kind to maintain, and one that shells out to its own program for
  validation and enrolment. Accepted: the alternative is a Supervisor that lies about applies.
- Negative / trade-offs: two shared-code changes touch what the Collector and the `command` kind
  use. Both are additive and default to the generic behaviour.
- Negative / trade-offs: the Managed Process runs under the Client's service account, not `nagios`,
  so checks needing elevated capabilities (`check_icmp`) fail. Documented, not worked around.
- Negative / trade-offs: the fleet Server must be given per-host Configurations for tickets, which is
  a Configuration per enrolling host until it is signed. Selectors make that mechanical, and the
  ticket may be withdrawn afterwards.
- Negative / trade-offs: a host waiting for on-demand signing looks like an unhealthy Agent for as
  long as nobody signs. Correct, and it belongs in the manual's troubleshooting table so it is not
  reported as a defect.
- Negative / trade-offs: the Supervisor shells out to `icinga2 pki` and depends on its exit codes and
  output. Bounded by a timeout, and the alternative — reimplementing the CSR protocol — is worse.
- **Negative: a differently packed Icinga tree does not fit.** The answer is `opamp-package-fetch`,
  not a key — ADR-0037 clause 8 states the trade generally, and this is the agent where it bites
  first, because the tree is repacked from vendor packages rather than published as one.
- **Negative: a role value of its own is a convention the fleet has to get right.** `role = "main"` is
  read by this kind and ignored by the others, and an operator who renames the root Configuration
  *and* forgets the role gets a Supervisor that will not start. The message names both ways across —
  the role, or the conventional name — which is the least this trade deserves: what would be a key on
  every host is a field on one Configuration.
- **Negative: Windows is unproven.** If the Windows repack proves impossible, the kind is Linux-only
  until some other decision addresses it (ADR-0018).
- **Follow-ups:** the artifacts this kind expects are [ADR-0034](0034-repacked-icinga-2-artifacts.md).
  Renewal is left to the daemon and only netted at start; if that turns out not to hold in practice,
  an explicit renewal schedule is its own decision. Whether the four enrolment values should become
  concepts the **fleet** holds rather than values riding inside a rolled-out block — the ticket
  already travels that way, and with the other three following, this block would be `type` and
  `name` alone.
