# ADR-0030: The GLPI Agent through the fleet — supervised as the Supervisor's own child, machine-installed or from self-contained packages on both platforms

- **Status:** 🟢 accepted
- **Date:** 2026-08-14
- **Deciders:** Markus Brigl

## Context

The fleet should manage the [GLPI Agent](https://glpi-agent.readthedocs.io/) — the inventory
agent of the GLPI asset-management suite — on Windows and Linux hosts alike: fleet-visible
health, centrally rolled-out configuration, and a restart the operator issues from the Server.

### What the GLPI Agent is

From research against its documentation and source:

- A **Perl application**, not a single binary. On Linux the distribution's package installs it
  (e.g. `/usr/bin/glpi-agent` on the system Perl) with a systemd unit; on Windows an MSI
  installs it under `C:\Program Files\GLPI-Agent` with a **bundled Strawberry Perl** whose
  interpreter ships as `perl\bin\glpi-agent.exe`, registers a Windows service `glpi-agent`
  (`EXECMODE=1`, the default) that runs that interpreter with four `-I` library paths and the
  agent script `perl\bin\glpi-agent`, and exposes `glpi-agent.bat` — a batch wrapper around the
  same invocation — as the command line.
- `--daemon --no-fork` runs it as a **foreground daemon on every platform**: the launcher
  script instantiates the same platform-neutral `GLPI::Agent::Daemon` class whether or not the
  process has a console; the Win32-service integration is a separate code path the service
  wrapper uses, not a requirement of daemon mode.
- Configuration is a file, selected with `--conf-file=FILE`. There is **no signal-based
  reload**; the daemon optionally re-reads its file on a timer (`conf-reload-interval`,
  minimum 60 s, default never).
- `--version` prints `GLPI Agent (X.Y[.Z])` — and most releases carry a **two-component**
  version (`1.11`, `1.15`), which the version probe's strict SemVer 2.0.0 matcher does not
  accept.

### Forces from this project's architecture

- The supervision runtime manages **children it spawns** — spawn, watchdog, bounded stop,
  health-gated apply (ADR-0011's `Runner`). Nothing drives a foreign service manager
  (`systemctl`, `sc.exe`) for a Managed Process, and nothing supervises a process it did not
  start.
- A program named by **absolute path is machine-owned**: the Client runs it but never updates
  it, and the Supervisor's Agent does not accept packages (ADR-0019). A Server-pushed
  `[[supervisor]]` block may name only Client-owned programs, so a block with an absolute path
  is written locally in `client.toml`, never pushed (ADR-0019).
- Spawning `glpi-agent.bat` would make `cmd.exe` the supervised child and the Perl process a
  grandchild: the bounded stop would kill the wrapper and **orphan the agent**, and pid-based
  telemetry would sample the wrong process. The batch file is a footgun, not an entry point.

The first question is whether any of this needs a new plugin kind — the sanctioned path of
ADR-0011, one module and one registry line — or whether the shipped `command` kind already
expresses it.

### Self-contained archives for both platforms

A machine-owned installation leaves updates with the machine. The goal goes further: **for both
platforms there must be an archive — zip, tar.gz, or 7z — holding everything the agent needs,
with no further dependencies on the host system**; where upstream publishes none, building our
own self-contained archive by script must be possible. With such an archive the GLPI Agent
becomes a Client-owned program (bare name, ADR-0019): `AcceptsPackages`, Server-pushable
`[[supervisor]]` blocks (ADR-0019), versioned Sets with health-gated updates and rollback
(ADR-0016, ADR-0015).

What upstream offers, surveyed on [release 1.19](https://github.com/glpi-project/glpi-agent/releases/tag/1.19):

- **Windows: a self-contained archive exists.** `GLPI-Agent-1.19-x64.zip` is the portable
  build — bundled Strawberry Perl (`perl/bin/glpi-agent.exe` and the scripts beside it),
  `etc/` with `conf.d`, empty `var/` and `logs/`, relative `.bat` wrappers; 5 259 files,
  ~101 MB unpacked (verified by inspection).
- **Linux: none.** `GLPI-Agent-1.19.tar.gz` is the **source distribution** (it rides the
  system Perl); the `.deb`/`.rpm`/installer ride it too; the snap needs `snapd`. The one
  self-contained Linux build is the **AppImage** — a single x86_64 ELF, not an archive, and
  running it as published needs `libfuse2` on the host or a full re-extraction on every start.

Constraints from this project's own machinery decide the archive formats:

- The Client opens **`.tar.gz` and `.7z` only**; a `.zip` is installed *as* the program
  (ADR-0017). Repacking the Windows zip would satisfy that — but it would break the very
  principle ADR-0017 is built on: *nothing alters the artifact between its author and the
  host*, so the hash an Agent verifies is **the same SHA-256 upstream published**. A repacked
  zip is our artifact with our hash; the provenance line stops at the packing host. ADR-0017
  excluded zip on the evidence before it (the Collector releases publish none) — the GLPI
  portable zip is new evidence, and the honest answer is to extend the container set, not to
  repack around it.
- A tree package (ADR-0015) refuses **symlinks and hard links**, more than **10 000 members**,
  and more than **2 GiB** unpacked; members are checked before anything is written.
- A `.7z` carries Windows attributes, so on unpack only the program itself is made executable —
  fine for a Windows tree, fatal for a Linux tree full of executables; a `.zip` shares that
  property. **The Linux tree must be `.tar.gz`**, which carries file modes.

Feasibility was verified empirically against 1.19 in the Dev Container:

- The **extracted AppImage tree is relocatable by design**: its `AppRun.env` resolves
  everything from `$ORIGIN` (`APPDIR`, `PERL5LIB`, library paths), and it bundles a
  glibc-2.27 compatibility runtime, so the tree runs on hosts with older or newer glibc —
  without FUSE, without root, from any directory. `AppRun` dispatches to the agent with
  `--script=glpi-agent` (or `GLPIAGENT_SCRIPT` in the environment).
- The tree holds **219 symlinks, 38 of them dangling** (Debian packaging leftovers — systemd
  units and the like). After dropping the dangling ones and **dereferencing the rest**, the
  link-free tree (7 080 files, ~248 MB unpacked — inside both limits) still runs: `--version`
  answers, and a foreground `--daemon --no-fork` run works from a moved directory. Five of the
  links are **directories**, and one of them is load-bearing: `usr/share/perl/5.26` points at
  `5.26.1`, and it is the linked name that the bundled `PERL5LIB` uses — packed as an empty
  directory, the agent finds no module at all.
- The agent **never creates a missing `--vardir`** — it exits at startup. Its state
  (`deviceid`, target caches) must live *outside* `program/tree/`, or every update would wipe
  it; `${supervisor_dir}` itself always exists, is Client-owned, and survives tree swaps.
- `opamp-package-sign pack` deliberately packs **one file only**; tree artifacts are built
  with `tar` today. A repacked artifact is ours, not upstream's, so the packing step — not the
  download — is where upstream's published SHA-256 must be checked, and our own signing
  (`opamp-package-sign sign`) is the chain of trust from there to the fleet.

## Decision

### The machine-installed GLPI Agent: a `command` recipe per platform

We will supervise the GLPI Agent with the **existing `command` kind — no new plugin, no code**:
one `[[supervisor]]` block per platform, running the machine's own GLPI installation as a
foreground child via `--daemon --no-fork`, documented as a recipe in the manual.

Concretely, the recipe binds:

- **The machine owns the program** (ADR-0019): the block names it absolutely — on Linux the
  packaged `/usr/bin/glpi-agent`, on Windows the MSI's bundled interpreter
  (`perl\bin\glpi-agent.exe`) invoked exactly as the MSI's own service registration does: the
  four `-I` library paths, then the agent script. Never `glpi-agent.bat`.
- **The native autostart is switched off**, so exactly one GLPI Agent runs per host: on
  Windows the MSI is installed with `EXECMODE=3` (or the service is disabled), on Linux the
  distribution's unit is `systemctl disable --now`'d. This hand-over is the operator's step and
  part of the recipe.
- **`service_name = "glpi-agent"`** is the Agent type every GLPI Configuration and package
  selector targets (ADR-0023, ADR-0012).
- **Configuration arrives as a file and applies by restart**: the block passes
  `--conf-file=${config_dir}/glpi-agent-conf` (ADR-0019), so a rolled-out Configuration named
  `glpi-agent-conf` lands exactly where the process reads it — a Configuration name carries no
  extension, following the same grammar as every other name here (ADR-0010: lowercase letters,
  digits and `-`, no dot), while `--conf-file` reads whatever path it is given. No
  `reload_signal` — the GLPI Agent has no reload signal to send.
- **`version_args = ["--version"]` is set, with a known gap**: only three-component releases
  (`1.7.1`) yield a `service.version`; two-component releases report none, because the probe
  accepts strict SemVer only. The gap is accepted — it is display metadata, and no package
  logic depends on it (the program is machine-owned).

A sketch of the Linux block, to fix the shape (the manual carries the full pair):

```toml
[[supervisor]]
type = "command"
name = "glpi"
service_name = "glpi-agent"
command = "/usr/bin/glpi-agent"
args = ["--daemon", "--no-fork", "--conf-file=${config_dir}/glpi-agent-conf"]
```

### Self-contained packages: the Windows zip as published, the Linux AppImage repacked as a tree

We will make the GLPI Agent fleet-deliverable on both platforms as **self-contained tree
packages** — and an official artifact that already *is* one travels **as published**:

- **Windows (windows/amd64): the official portable zip, byte for byte.** The Client learns to
  open **`.zip` as a third container** (extending ADR-0017): detected by its leading bytes
  like the other two, held to exactly the member and tree rules of ADR-0015, encryption not
  supported (an operator who needs confidentiality packs a `.7z`, as today). Like a `.7z` it
  carries no Unix modes, which on a Windows tree costs nothing. The block sets
  `program_path = "perl/bin/glpi-agent.exe"` — and the artifact can even stay off the fleet
  Server entirely: a *referenced* package pointing at the release asset URL with **upstream's
  own `.sha256` value** is the unbroken provenance line ADR-0017 was written for.
- **Linux (linux/amd64): repacked, because upstream publishes no archive.** The official
  AppImage, verified against the release's `.sha256`, extracted (`--appimage-extract`),
  dangling links deleted, remaining links dereferenced — a linked *directory* packed under the
  linked name too, since that is the name the agent reaches its Perl library by — and packed as
  `.tar.gz` with file modes under one top-level directory by **a tool this repository ships**
  (`opamp-package-fetch`, whose repack step runs on a Linux x86_64 host such as the Dev
  Container); `program_path = "AppRun"`, and the block selects the agent with
  `--script=glpi-agent` as its first argument.
- **The repacked artifact is deterministic**: it is packed with fixed ordering, zeroed times and
  ownership (as `opamp-package-sign pack` does for single files), so repacking the same release
  yields the same hash and no accidental rollout.
- **State lives beside the tree, not in it**: the blocks pass `--vardir=${supervisor_dir}`
  (and `--conf-file=${config_dir}/glpi-agent-conf` as in the machine-owned recipe), so identity
  and caches survive updates and rollbacks.
- **One Set, `service_name = "glpi-agent"`, one entry per platform** (ADR-0017, ADR-0016),
  version taken from the upstream release. Hosts on other platforms — Linux arm64 has no
  AppImage — stay on the machine-owned recipe above; the two paths coexist per host and the
  manual says when to use which.

The machine-owned recipe stays as it is beside the packages. The container set of ADR-0017
gains `.zip`, read-only and unencrypted, through the
[`zip`](https://crates.io/crates/zip) crate taken with `default-features = false` and
`deflate` only, so decompression runs on the `flate2`/`miniz_oxide` chain the Client already
carries and the pure-Rust build (ADR-0006, ADR-0007) is undisturbed.

## Alternatives considered

### Supervision

- **A dedicated `glpi` plugin kind** carrying the platform defaults (`cfg`-gated program
  discovery, fixed arguments). Rejected: every difference between the platforms is a *value*
  the block already expresses — program path, arguments — not a *behavior* the `Runner` lacks.
  A new kind is warranted when the lifecycle differs (ADR-0011); encoding default paths in
  Rust buys convenience at the price of a module, and simplicity first says no.
- **A kind that drives the native service managers** (`systemctl`/`sc.exe` for the GLPI
  service, instead of spawning a child). Rejected: supervising a process the Client did not
  spawn is a different supervision model — no watchdog, no health gate, no bounded stop — and
  nothing in the runtime implements it. The unified-lifecycle direction (currently proposed)
  is about install/uninstall side effects, not about babysitting foreign service managers.
- **Windows Task mode (`EXECMODE=2`) or leaving the native services in place.** Rejected:
  then nothing is under management — no fleet-visible health, no config rollout, no restart —
  which is the task, not an implementation detail.

### Packages

- **Deliver the AppImage as a single-file package, unopened.** Rejected: the AppImage is
  x86_64-only, and as published it needs `libfuse2` on every fleet host — precisely the system
  dependency the goal forbids — or `APPIMAGE_EXTRACT_AND_RUN=1`, which re-extracts ~45 MB on
  every start, a price the watchdog would pay on every restart. Extracting once at packing time
  removes FUSE from the equation entirely.
- **Wait for (or request) an official self-contained Linux tarball.** None exists across the
  surveyed releases; the published `tar.gz` is source. The AppImage *is* upstream's
  self-contained Linux build — repacking it stays on artifacts upstream builds and tests.
- **Repack the Windows zip into `.tar.gz`/`.7z` instead of teaching the Client zip.** It needs
  no code. Rejected on ADR-0017's own principle: the conversion's only product is a format
  change, and its price is the provenance — the fleet would verify a hash the packing host
  invented instead of the one upstream published, and the referenced-package route (URL plus
  upstream checksum, no upload at all) would be closed. One read-only container, held to the
  existing member rules, is the cheaper honesty.
- **Start from the original artifacts and add only what is missing.** For Windows this *is*
  the decision — the portable zip is taken exactly as published. For Linux the published
  `tar.gz` is source code, and "what is missing" is the whole runtime: a Perl interpreter, every CPAN
  dependency including compiled XS modules, and their C libraries. Assembling that ourselves
  (a relocatable Perl plus `cpanm` at packing time, or staticperl/PAR::Packer) means owning a
  build system with a compile step per architecture and a dependency list that moves with
  every GLPI release — whereas the AppImage *is* exactly this assembly, made and tested by
  upstream. Rejected as the primary path; it remains the only visible route to a **Linux
  arm64** package (prebuilt relocatable Perl exists for arm64), noted as a follow-up should
  arm64 fleet ownership become a requirement.
- **The snap.** Rejected: requires `snapd` — a system dependency and a second manager beside
  the fleet.
- **Loosen the tree rules** (allow symlinks, raise the member cap) to pack the AppImage tree
  as-is. Rejected: both verified trees fit the existing limits once dereferenced, and the
  rules protect every package on every host — not worth weakening for one agent.
- **Stay machine-owned only (the `command` recipe alone).** Remains available — and remains the
  only path off amd64 — but fails the stated goal: no fleet-owned install, no dependency-free
  archive.

## Sources / Prior art

- [GLPI Agent man page](https://glpi-agent.readthedocs.io/en/latest/man/glpi-agent.html) —
  `--daemon`, `--no-fork`, `--conf-file`, `--conf-reload-interval`, logger and httpd options.
- [GLPI Agent usage](https://glpi-agent.readthedocs.io/en/latest/usage.html) — managed mode
  is "daemon under Unix, service under Windows"; embedded web interface on port 62354.
- [Windows installer reference](https://glpi-agent.readthedocs.io/en/latest/installation/windows-command-line.html)
  — MSI, `INSTALLDIR`, `EXECMODE` 1/2/3 (service / task / manual).
- [`bin/glpi-agent` source](https://github.com/glpi-project/glpi-agent/blob/develop/bin/glpi-agent)
  — `--daemon` instantiates the platform-neutral `GLPI::Agent::Daemon`; `--version` prints
  `$VERSION_STRING` (`GLPI Agent (X.Y[.Z])`).
- [Windows packaging source](https://github.com/glpi-project/glpi-agent/blob/develop/contrib/windows/glpi-agent-packaging.pl)
  (and `packaging/template.bat.tt`) — the service registration
  (`glpi-agent.exe -I"…perl\agent" -I"…site\lib" -I"…vendor\lib" -I"…perl\lib"
  "…perl\bin\glpi-win32-service"`), the `glpi-agent` service name, and the `.bat` wrapper.
- [GLPI Agent release 1.19 assets](https://github.com/glpi-project/glpi-agent/releases/tag/1.19)
  — the surveyed artifact set (portable zip, AppImage, source tar.gz, distro packages, snap).
- [`make-linux-appimage.sh`](https://github.com/glpi-project/glpi-agent/blob/develop/contrib/unix/make-linux-appimage.sh)
  and [`glpi-agent-appimage-hook`](https://github.com/glpi-project/glpi-agent/blob/develop/contrib/unix/glpi-agent-appimage-hook)
  — how the AppImage is assembled (appimage-builder over the Debian packages) and how its
  entry point dispatches (`--script`, `GLPIAGENT_SCRIPT`).
- [AppImage / appimage-builder runtime](https://appimage-builder.readthedocs.io/) — the
  `AppRun.env` `$ORIGIN` mechanism and the bundled-libc compatibility layer the packages rely
  on for relocatability.
- [GLPI Agent portable discussion #273](https://github.com/glpi-project/glpi-agent/discussions/273)
  — the Windows tree is self-contained (bundled Perl), and the zip is the supported portable form.
- [`zip`](https://crates.io/crates/zip) crate — checked on crates.io: MIT-licensed, actively
  released (8.x); its **default features pull C-binding codecs** (bzip2, xz, zstd), so it must
  be taken with `default-features = false` and `deflate` only — the method ADR-0017 already
  applied to `sevenz-rust2` — which decompresses on `flate2`/`miniz_oxide`, both already in
  the Client's tree.
- **Verified against GLPI Agent 1.15 in the Dev Container** (installed via the official Linux
  installer): `--daemon --no-fork` stays one foreground process and exits cleanly on SIGTERM;
  it keeps running when the server is unreachable; a missing `--conf-file` is a hard startup
  failure (*"Config: non-existing file"*); an unwritable state directory is one too (*"Can't
  write in /var/lib/glpi-agent"*); `--version` prints `GLPI Agent (1.15-1)` — no strict-SemVer
  token, so the probe reports nothing. The release's `GLPI-Agent-1.15.tar.gz` is the **source
  distribution** (it rides the system Perl), not a runnable bundle; the self-contained Linux
  build is the AppImage, whose direct-run dispatch was verified the same way
  (`--script=glpi-agent --version`, and a foreground `--daemon --no-fork` run, without root
  and without FUSE via `APPIMAGE_EXTRACT_AND_RUN=1`).
- **Verified against GLPI Agent 1.19 in the Dev Container**: the extracted AppImage tree runs
  relocated (version query and foreground daemon, no FUSE, no root); dereferenced and
  link-free it still runs (8 284 members, 259 MB — inside the ADR-0015 limits); the Windows
  zip holds 5 259 files, ~101 MB, `perl/bin/glpi-agent.exe` and `var/`/`etc/` under one root;
  a missing `--vardir` is a hard startup failure, so state must live outside the swapped tree.

## Consequences

- Positive: the GLPI Agent appears as its own Agent — health, restart, centrally rolled-out
  configuration — on both platforms, with zero new code and both shipped plugins untouched.
  The recipe is the first documented Foreign Agent beyond the promtail walkthrough, and it
  exercises only accepted machinery.
- Positive: the GLPI Agent reaches **full fleet ownership on amd64 hosts of both platforms** —
  the Server pushes the block (ADR-0019), delivers versioned updates with health gate and
  rollback (ADR-0015), and no fleet host needs Perl, FUSE, snapd, or a preinstalled GLPI
  Agent. The bundled glibc-compat runtime makes one Linux artifact serve old and new distros
  alike. The Windows artifact travels **byte for byte as upstream published it** — verifiable
  against the release's own `.sha256`, uploadable or referenced straight from the release
  page with no packing step at all. And the standing zip footgun disappears: a `.zip` was the
  one common container the Client silently installed *as* the program (the rollout
  walkthrough warns about exactly this); now it is opened and held to the same member rules
  as the other two.
- Negative / trade-offs of the machine-owned recipe: the `[[supervisor]]` block is
  **local-only** — ADR-0019 rightly refuses to push absolute-path blocks, so the block reaches
  `client.toml` by hand or by the operator's configuration management. GLPI Agent **updates
  stay with the machine's package manager**, invisible to the fleet. The disabled native
  autostart is an operator duty the fleet cannot verify — a forgotten hand-over means two
  agents inventorying the host. The reported version is usually absent (two-component
  releases). Configuration applies by restart only, which for an inventory agent is harmless
  (no in-flight state worth keeping).
- Before the first Configuration rollout, `${config_dir}/glpi-agent-conf` is absent and the
  agent refuses to start (verified): the Supervisor crash-loops three times and holds, and the
  first apply ends the hold by restarting onto the written file. The recipe documents this
  window — and the static-`--server` variant for a host whose configuration stays local —
  rather than hiding it.
- Negative / trade-offs of the packages: **the Client grows code and a dependency** — a third
  archive format parsing untrusted input on every managed host, which must pass the same
  traversal, link, and bound tests as the other two (the tree-rule table applies verbatim), and
  `.zip` support is read-only and unencrypted by design. **We still own the Linux repack** —
  every GLPI release the fleet should run needs one script invocation and an upload; the
  repacked artifact no longer matches upstream's published hash, so the tool verifies
  upstream's `.sha256` at packing time, and from there the fleet's own hash and Ed25519
  signature are the chain of trust.
- Negative: the artifacts are large (~30–70 MB per platform and version, 259 MB unpacked on
  Linux — well inside the 2 GiB bound but not free), and dereferencing duplicates shared
  libraries on disk. `service.version` stays unreported (upstream's `1.19-1` is not strict
  SemVer); `packages[].version` is what tracks installs. Linux arm64 and every other platform
  stay on the machine-owned recipe.
- Follow-ups (by topic): optionally teaching `opamp-package-sign pack` a reproducible `--tree`
  mode so tree artifacts get the same deterministic packing as single files without
  hand-rolled `tar` flags; a Linux arm64 package built from the source distribution plus a
  relocatable Perl, should arm64 fleet ownership become a requirement; a native-installer kind
  under the proposed unified lifecycle vocabulary; a health probe against the agent's embedded
  httpd (port 62354) instead of process-aliveness.
