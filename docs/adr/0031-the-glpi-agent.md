# ADR-0031: The GLPI Agent gets a kind of its own, delivered as self-contained packages — the Windows zip as published, the Linux AppImage repacked as a tree

- **Status:** 🟢 accepted
- **Date:** 2026-08-21
- **Deciders:** Markus Brigl

Applies [ADR-0037](0037-a-kind-knows-its-own-agent.md) to the agent that shows most plainly why that
rule exists. The rule itself, the `Plugin` seam and the artifact-document requirement are stated
there.

## Context

The fleet should manage the [GLPI Agent](https://glpi-agent.readthedocs.io/) — the inventory
agent of the GLPI asset-management suite — on Windows and Linux hosts alike: fleet-visible
health, centrally rolled-out configuration, a restart the operator issues from the Server, and
versioned updates with rollback. A Managed Process is always a program this Client installs
([ADR-0018](0018-supervisor-directory-and-client-installed-programs.md)), so the GLPI Agent has to reach a
host as a package.

What the GLPI Agent is, from research against its documentation and source:

- A **Perl application**, not a single binary. On Windows its installation carries a **bundled
  Strawberry Perl** whose interpreter ships as `perl\bin\glpi-agent.exe`; the Windows service
  `glpi-agent` runs that interpreter with four `-I` library paths and the agent script
  `perl\bin\glpi-agent`, and `glpi-agent.bat` is a batch wrapper around the same invocation.
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
- Spawning `glpi-agent.bat` would make `cmd.exe` the supervised child and the Perl process a
  grandchild: the bounded stop would kill the wrapper and **orphan the agent**, and pid-based
  telemetry would sample the wrong process. The batch file is a footgun, not an entry point.

The supervision runtime manages **children it spawns** — spawn, watchdog, bounded stop,
health-gated apply (ADR-0011's `Runner`). Nothing drives a foreign service manager
(`systemctl`, `sc.exe`) for a Managed Process, and nothing supervises a process it did not
start.

**For both platforms there must be an archive — zip, tar.gz, or 7z — holding everything the agent
needs, with no further dependencies on the host system**; where upstream publishes none, building
our own self-contained archive by script must be possible. What upstream offers, surveyed on
[release 1.19](https://github.com/glpi-project/glpi-agent/releases/tag/1.19):

- **Windows: a self-contained archive exists.** `GLPI-Agent-1.19-x64.zip` is the portable
  build — bundled Strawberry Perl (`perl/bin/glpi-agent.exe` and the scripts beside it),
  `etc/` with `conf.d`, empty `var/` and `logs/`, relative `.bat` wrappers; 5 259 files,
  ~101 MB unpacked (verified by inspection).
- **Linux: none.** `GLPI-Agent-1.19.tar.gz` is the **source distribution** (it rides the
  system Perl); the `.deb`/`.rpm`/installer ride it too; the snap needs `snapd`. The one
  self-contained Linux build is the **AppImage** — a single x86_64 ELF, not an archive, and
  running it as published needs `libfuse2` on the host or a full re-extraction on every start.

Constraints from this project's own machinery decide the archive formats:

- The Client opens **`.tar.gz` and `.7z`** (ADR-0015). Repacking the Windows zip into one of them
  would break the very principle ADR-0015 is built on: *nothing alters the artifact between its
  author and the host*, so the hash an Agent verifies is **the same SHA-256 upstream published**. A
  repacked zip is our artifact with our hash; the provenance line stops at the packing host. The
  GLPI portable zip is evidence that the honest answer is to extend the container set, not to
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
- `opamp-package-sign pack` deliberately packs **one file only**. A repacked artifact is ours, not
  upstream's, so the packing step — not the download — is where upstream's published SHA-256 must
  be checked, and our own signing (`opamp-package-sign sign`) is the chain of trust from there to
  the fleet.

**Packed this way, the two platforms still differ in nearly everything an invocation needs:**

| | Linux | Windows |
|---|---|---|
| program | `AppRun` | `glpi-agent.exe` |
| `program_path` | `AppRun` | `perl/bin/glpi-agent.exe` |
| `working_dir` | — | `${supervisor_dir}/program/tree` |
| `args` | `--script=glpi-agent`, then the daemon flags | four Perl `-I` paths, the script by path, then the daemon flags |

Not one of those differences is a decision. They follow from `std::env::consts::EXE_SUFFIX` and from
where the AppImage puts its interpreter — facts of the artifact this project packs itself. Written
as a recipe for the generic `command` Supervisor, they are transcribed into every host's file by
hand, as two blocks of seven and eight keys.

**The rest of the invocation is equally fixed.** `--conf-file` points at `glpi-agent-conf`, the name
`opamp-package-fetch` uploads; `--vardir` points beside the tree because a package swap replaces the
tree whole; the file logging exists so a daemon with no console has somewhere to write; `--daemon
--no-fork` is not optional at all — forking would hand the Supervisor a pid it does not own, which
is a supervision bug, not a preference. And `service_name = "glpi-agent"` is the Agent type every
GLPI Configuration is aimed at (ADR-0022): the same string on every host.

**The one thing an operator does decide** — which hosts run a GLPI Agent — is `type` and `name`.

## Decision

We will add a **`glpi` kind**, whose block is two lines on both platforms, and make the GLPI Agent
fleet-deliverable on both platforms as **self-contained tree packages** — where an official artifact
already *is* one, it travels **as published**.

### The `glpi` kind

1. **The kind knows the invocation, per platform**: the program name and `program_path` from the
   table above, the full argument list including the Perl `-I` paths, `--conf-file` against
   `glpi-agent-conf`, `--vardir` beside the tree, the file logging, `--daemon --no-fork`, how it is
   asked for its version, and `service_name = "glpi-agent"`. On Windows it runs the bundled
   interpreter with the four `-I` library paths and the agent script — never `glpi-agent.bat`.

   **The working directory is where the two platforms part.** ADR-0037's general rule — the
   directory the program lives in — is already right on Linux, where the program *is* the tree
   root's `AppRun`, so the kind names nothing. On Windows it is not: the program sits at
   `perl/bin/`, and the bundled Perl expects the tree root, which is why upstream's own portable
   `.bat` sets it before invoking the agent. So the kind names it there — the one place a wrapper
   overrides the general derivation, and the case that shows why the derivation is a default rather
   than a law.

2. **It has no settings of its own.** Its strict parse (`Plugin::check`) accepts an empty table and
   refuses every key, so a block carrying one fails at startup naming what supplies the value now.
   A wrapper that needed an escape hatch would be a wrapper that does not know its agent.

   ```toml
   [[supervisor]]
   type = "glpi"
   name = "glpi"
   ```

3. **`--daemon --no-fork` is part of the kind, not a default an operator may drop**, and the two
   flags answer two different failures. Without `--daemon` the agent runs its tasks once and exits,
   and the watchdog restarts it for ever. Without `--no-fork` it detaches, leaving the Supervisor
   holding a pid that ends immediately while the real process runs on unsupervised. Both are
   properties of the kind, which is where a supervision requirement belongs.

4. **Its artifact document is `docs/artifacts/glpi-agent.md`**, in ADR-0037 clause 9's shape, with
   the two tests that clause requires — one against the AppImage repack plan in
   `opamp-package-fetch`, one against the constants above, `cfg`-gated per platform. GLPI is the
   agent where those tests earn the most: its Linux artifact is the one this project **repacks**
   rather than ships as published, so its internal layout is ours to keep in step.

### The packages

5. **Windows (windows/amd64): the official portable zip, byte for byte.** The Client opens
   **`.zip` as a third container**: detected by its leading bytes like the other two, held to
   exactly the member and tree rules of ADR-0015, encryption not supported (an operator who needs
   confidentiality packs a `.7z`). Like a `.7z` it carries no Unix modes, which on a Windows tree
   costs nothing. The program sits at `perl/bin/glpi-agent.exe`, which the kind derives
   (clause 1) — and the artifact can even stay off the fleet Server entirely: a *referenced*
   package pointing at the release asset URL with **upstream's own `.sha256` value** is the
   unbroken provenance line ADR-0015 was written for.

6. **Linux (linux/amd64): repacked, because upstream publishes no archive.** The official
   AppImage, verified against the release's `.sha256`, extracted (`--appimage-extract`),
   dangling links deleted, remaining links dereferenced — a linked *directory* packed under the
   linked name too, since that is the name the agent reaches its Perl library by — and packed as
   `.tar.gz` with file modes under one top-level directory by **a tool this repository ships**
   (`opamp-package-fetch`, whose repack step runs on a Linux x86_64 host such as the Dev
   Container). The program is the tree root's `AppRun`, selecting the agent with
   `--script=glpi-agent` as its first argument, both derived by the kind (clause 1).

7. **The repacked artifact is deterministic**: it is packed with fixed ordering, zeroed times and
   ownership (as `opamp-package-sign pack` does for single files), so repacking the same release
   yields the same hash and no accidental rollout.

8. **State lives beside the tree, not in it**: the kind passes a `--vardir` under
   `${supervisor_dir}` and `--conf-file=${config_dir}/glpi-agent-conf` (clause 1), so identity and
   caches survive updates and rollbacks. The spawn guarantees that `--vardir` exists
   (ADR-0037 clause 2).

9. **One Set, `service_name = "glpi-agent"`, one entry per platform** (ADR-0021, ADR-0016),
   version taken from the upstream release. Hosts on other platforms — Linux arm64 has no
   AppImage — have no GLPI package; a machine-installed GLPI Agent is not a Managed Process
   (ADR-0018 clause 2).

10. **The `.zip` container is read-only and unencrypted**, read through the
    [`zip`](https://crates.io/crates/zip) crate taken with `default-features = false` and
    `deflate` only, so decompression runs on the `flate2`/`miniz_oxide` chain the Client already
    carries and the pure-Rust build (ADR-0006, ADR-0007) is undisturbed.

### On the host

11. **The native autostart is switched off**, so exactly one GLPI Agent runs per host. This applies
    where a native installation is already present: on Windows the MSI is installed with
    `EXECMODE=3` (or the service is disabled), on Linux the distribution's unit is
    `systemctl disable --now`'d. This hand-over is the operator's step.

12. **Configuration arrives as a file and applies by restart**: a rolled-out Configuration named
    `glpi-agent-conf` lands exactly where `--conf-file` reads it — a Configuration name carries no
    extension, following the same grammar as every other name here (ADR-0010: lowercase letters,
    digits and `-`, no dot), while `--conf-file` reads whatever path it is given. The GLPI Agent
    has no reload signal to send.

13. **The version probe has a known gap**: the kind asks `--version`, and only three-component
    releases (`1.7.1`) yield a `service.version`; two-component releases report none, because the
    probe accepts strict SemVer only (upstream's `1.19-1` is not strict SemVer). The gap is
    accepted — `packages[].version` is what tracks installs.

## Alternatives considered

- **Leave it as a `command` recipe and fix the documentation.** Rejected: the documentation is not
  wrong, it is *duplicated per host* — and it is duplicated per platform on top of that. A recipe
  cannot be updated when an upstream release moves a path; a kind can.
- **One block with placeholders instead of two.** `${exe_suffix}` and friends would collapse the two
  blocks into one. Rejected: it makes the operator's file a template of the artifact's layout, which
  is the transcription the kind removes rather than shortens — and the Windows argument list is not
  the Linux one with a suffix appended, it has four extra paths.
- **A kind that drives the native service managers** (`systemctl`/`sc.exe` for the GLPI
  service, instead of spawning a child). Rejected: supervising a process the Client did not
  spawn is a different supervision model — no watchdog, no health gate, no bounded stop — and
  nothing in the runtime implements it.
- **Windows Task mode (`EXECMODE=2`) or leaving the native services in place.** Rejected:
  then nothing is under management — no fleet-visible health, no config rollout, no restart —
  which is the task, not an implementation detail.
- **Deliver the AppImage as a single-file package, unopened.** Rejected: as published it needs
  `libfuse2` on every fleet host — precisely the system dependency the goal forbids — or
  `APPIMAGE_EXTRACT_AND_RUN=1`, which re-extracts ~45 MB on every start, a price the watchdog
  would pay on every restart. Extracting once at packing time removes FUSE from the equation
  entirely.
- **Wait for (or request) an official self-contained Linux tarball**, after which the repack and
  half the Windows/Linux divergence would go away. None exists across the surveyed releases; the
  published `tar.gz` is source. The AppImage *is* upstream's self-contained Linux build — repacking
  it stays on artifacts upstream builds and tests. Nor is it a reason to wait with the kind: the
  divergence in the block is real today, and if that release comes, the kind is the one place to
  change.
- **Repack the Windows zip into `.tar.gz`/`.7z` instead of teaching the Client zip.** It needs no
  code. Rejected on ADR-0015's own principle: the conversion's only product is a format change, and
  its price is the provenance — the fleet would verify a hash the packing host invented instead of
  the one upstream published, and the referenced-package route (URL plus upstream checksum, no
  upload at all) would be closed. One read-only container, held to the existing member rules, is
  the cheaper honesty.
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
  `AppRun.env` `$ORIGIN` mechanism and the bundled-libc compatibility layer this decision
  relies on for relocatability.
- [GLPI Agent portable discussion #273](https://github.com/glpi-project/glpi-agent/discussions/273)
  — the Windows zip as the supported portable form, a self-contained tree with bundled Perl.
- [`zip`](https://crates.io/crates/zip) crate — checked on crates.io: MIT-licensed, actively
  released (8.x); its **default features pull C-binding codecs** (bzip2, xz, zstd), so it must
  be taken with `default-features = false` and `deflate` only — the method ADR-0015 already
  applied to `sevenz-rust2` — which decompresses on `flate2`/`miniz_oxide`, both already in
  the Client's tree.
- **Verified against GLPI Agent 1.15 in the Dev Container**: `--daemon --no-fork` stays one
  foreground process and exits cleanly on SIGTERM; it keeps running when the server is
  unreachable; a missing `--conf-file` is a hard startup failure (*"Config: non-existing file"*);
  an unwritable state directory is one too (*"Can't write in /var/lib/glpi-agent"*); `--version`
  prints `GLPI Agent (1.15-1)` — no strict-SemVer token, so the probe reports nothing.
- **Verified against GLPI Agent 1.19 in the Dev Container**: the extracted AppImage tree runs
  relocated (version query and foreground daemon, no FUSE, no root); dereferenced and
  link-free it still runs (8 284 members, 259 MB — inside the ADR-0015 limits); the Windows
  zip holds 5 259 files, ~101 MB, `perl/bin/glpi-agent.exe` and `var/`/`etc/` under one root;
  a missing `--vardir` is a hard startup failure, so state must live outside the swapped tree.
- **`docs/manual/glpi-agent.md`** — the reasons for `--daemon` (*"the agent runs its tasks once and
  exits, and the watchdog would restart it forever"*) and `--no-fork` (*"it stays the Supervisor's
  direct child, one process, on every platform"*), which are clause 3; and the warning never to
  spawn `glpi-agent.bat`, whose wrapper would make the supervised child `cmd.exe`.
- **`crates/package-tools/src/bin/opamp-package-fetch.rs`** — `glpi_plans` and the `AgentKind` entry
  naming `glpi-agent` and the Configuration `glpi-agent-conf`, which is where this kind's `--conf-file`
  points.

## Consequences

- **Positive: full fleet ownership on amd64 hosts of both platforms** — the Server pushes the block
  (ADR-0029), delivers versioned updates with health gate and rollback (ADR-0015), and no fleet host
  needs Perl, FUSE, snapd, or a preinstalled GLPI Agent. The bundled glibc-compat runtime makes one
  Linux artifact serve old and new distros alike.
- **Positive: the Windows artifact travels byte for byte as upstream published it** — verifiable
  against the release's own `.sha256`, uploadable or referenced straight from the release page with
  no packing step at all. And the zip footgun disappears: a `.zip` is opened and held to the same
  member rules as the other two containers rather than installed *as* the program.
- **Positive: one block, both platforms.** The GLPI page documents one block, and a host's file says
  nothing about Perl.
- **Positive: the repack and the kind can be kept in step.** They are the two ends of the same
  artifact, and they are tested against one document instead of against a manual page.
- **Negative: the Client grows code and a dependency** — a third archive format parsing untrusted
  input on every managed host, which must pass the same traversal, link, and bound tests as the
  other two (the tree-rule table applies verbatim), and `.zip` support is read-only and unencrypted
  by design.
- **Negative: we own the Linux repack** — every GLPI release the fleet should run needs one script
  invocation and an upload; the repacked artifact does not match upstream's published hash, so the
  tool verifies upstream's `.sha256` at packing time, and from there the fleet's own hash and
  Ed25519 signature are the chain of trust. The artifacts are large (~30–70 MB per platform and
  version, 259 MB unpacked on Linux — well inside the 2 GiB bound but not free), and dereferencing
  duplicates shared libraries on disk.
- **Negative: Linux arm64 and every other platform have no GLPI package.**
- **Negative: the reported version is usually absent** (two-component releases).
  `packages[].version` says which release is installed.
- **Negative: the disabled native autostart is an operator duty the fleet cannot verify** — a
  forgotten hand-over means two agents inventorying the host.
- **Negative: configuration applies by restart only**, which for an inventory agent is harmless (no
  in-flight state worth keeping).
- **Before the first Configuration rollout, `${config_dir}/glpi-agent-conf` is absent and the agent
  refuses to start** (verified): the Supervisor crash-loops three times and holds, and the first
  apply ends the hold by restarting onto the written file. The manual documents this window rather
  than hiding it.
- **Negative: a GLPI Agent that somebody packed differently no longer fits.** ADR-0037 clause 8 in
  the concrete: the answer is the packing tool, not a key.
- **Negative: an operator who needs an extra GLPI flag needs a code change** — or the agent's
  own configuration, which is where most of what one would want to pass belongs anyway. `command`
  stays available for an installation that genuinely needs to invoke it differently, at the price of
  writing the whole invocation again.
- **Follow-ups (by topic):** optionally teaching `opamp-package-sign pack` a reproducible `--tree`
  mode so tree artifacts get the same deterministic packing as single files without hand-rolled
  `tar` flags; a Linux arm64 package built from the source distribution plus a relocatable Perl,
  should arm64 fleet ownership become a requirement; a health probe against the agent's embedded
  httpd (port 62354) instead of process-aliveness.
