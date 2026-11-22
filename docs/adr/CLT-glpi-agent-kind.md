# CLT-glpi-agent-kind: The GLPI Agent gets a kind of its own, and its packages are upstream's portable zip as published and its AppImage repacked as a tree

- **Status:** 🟡 proposed
- **Date:** 2026-10-10
- **Deciders:** Markus Brigl
- **Applies to:** crates/fleet-agent/src/supervisor/glpi.rs, the `glpi` feature of crates/fleet-agent/Cargo.toml, `glpi_plans`, `repack_appimage`, `repack_appimage_with`, `appimage_file_system`, `admit_inodes`, `require_unsquashfs`, `extract_squashfs`, `pack_extracted` and `GLPI_TREE_LINKS` in crates/fleet-tools/src/fetch.rs, `squashfs_offset` and `squashfs_inodes` in crates/fleet-tools/src/fetch/upstream.rs, fuzz/fuzz_targets/appimage_header.rs, the `squashfs-tools` package of .devcontainer/devcontainer.json, config/examples/glpi-agent-conf.cfg, docs/artifacts/glpi-agent.md, docs/manual/glpi-agent.md

## Context

The fleet manages the [GLPI Agent](https://glpi-agent.readthedocs.io/) — the inventory agent of the
GLPI asset-management suite — beyond the Collector. It should be an ordinary Managed Process: a
package the Client installs into its own Supervisor directory, updates with a health gate and rolls
back, a Configuration rolled out from the Server, a restart issued from it. A kind knows its own
agent ([CLT-supervisor-mode-and-kinds](CLT-supervisor-mode-and-kinds.md)): what
follows from the artifact and the platform is compiled in, and a block holds only what an operator
decides.

The GLPI Agent is a Perl application, not a single binary:

- `--daemon --no-fork` runs it as a foreground daemon on every platform (the launcher instantiates
  the same platform-neutral `GLPI::Agent::Daemon` class whether or not it has a console). Without
  `--daemon` it runs its tasks once and exits; without `--no-fork` it detaches.
- Its configuration is a file named by `--conf-file`. It has no reload signal.
- It never creates a missing `--vardir`; it exits at startup.
- Its daemon forks a child for each task run (`runTask` of `GLPI::Agent::Daemon`), and its
  tasks start programs of their own. On `SIGTERM` the daemon passes the signal on to its children.
- Its *Deploy* task runs the installers and commands its GLPI server sends. The server is the one
  the configuration file's `server =` names.
- Its command line wins over its configuration file: `GLPI::Agent::Config` reads the file first
  and lays the command line over it, at startup and on every reload. A `no-task` on the command
  line replaces the file's whole.
- `--version` prints `GLPI Agent (X.Y[.Z])`, which upstream's build may follow with a package
  revision: the 1.19 AppImage prints `GLPI Agent (1.19-1)`. The number sits in parentheses, and
  most releases carry a two-component version (`1.15`, `1.19`), so a strict SemVer read of the
  banner finds none.
- On Windows the wrapper `glpi-agent.bat` would make `cmd.exe` the supervised child: the bounded
  stop would kill the wrapper and orphan the agent, and pid-based telemetry would sample the wrong
  process.

What upstream publishes (surveyed on release 1.19): for **Windows** a self-contained portable zip —
bundled Strawberry Perl at `perl/bin/glpi-agent.exe`, `etc/`, `var/`, relative `.bat` wrappers;
5 259 files, ~101 MB. For **Linux** no archive: the `tar.gz` is the source distribution riding the
system Perl, `.deb`/`.rpm` ride it too, the snap needs `snapd`. The one self-contained Linux build
is the x86_64 **AppImage**, which as published needs `libfuse2` or re-extracts ~45 MB on every
start. Its extracted tree is relocatable by design (`AppRun.env` resolves everything from
`$ORIGIN`, and a bundled glibc-2.27 compatibility runtime, `runtime/compat`, serves hosts whose
own glibc is not newer); it holds 219 symlinks, 38 of them dangling. On a newer host `AppRun`
starts the agent through `runtime/default` instead, the host's own C library, reached through
one absolute link to the host's program loader, a link out of the tree. Dereferenced, with that
one link kept, it runs (about 7 100 files, 248 MB — inside the tree limits of
[PKG-signed-packages-and-delivery §57](PKG-signed-packages-and-delivery.md)).
Five links are directories, and one is load-bearing: `usr/share/perl/5.26` → `5.26.1` is the name
the bundled `PERL5LIB` uses.

Upstream signs none of these assets. The release carries one checksum file,
`glpi-agent-<tag>.sha256`, with a line for every asset; the 1.19 release carries no OpenPGP
signature, no Sigstore bundle and no other signature file.

The two platform invocations differ in nearly everything — program, its place in the tree, working
directory, and on Windows four Perl `-I` paths and the script by path — and none of those
differences is anybody's decision.

## Decision

We will supervise the GLPI Agent with a `glpi` kind whose block is `type` and `name` alone. We will
deliver it as packages built from upstream's own artifacts:

- the Windows portable zip as published;
- the Linux AppImage, repacked deterministically into a tree whose one link is the host loader's.

1. **The `glpi` kind has no settings.** Its block is two lines on both platforms:

   ```toml
   [[supervisor]]
   type = "glpi"
   name = "glpi"
   ```

   - Its strict parse (`Plugin::check`) accepts an empty table and refuses every key.
   - A block carrying `args`, `version_args`, `working_dir`, `env` or `reload_signal` fails at
     startup. The message names what supplies the value.
   - An installation that needs another invocation uses the generic `command` kind. It writes the
     whole invocation itself.

2. **The kind knows the invocation, per platform.** The tree is unpacked at
   `${supervisor_dir}/program/tree`:

   | | Linux (repacked AppImage) | Windows (portable zip) |
   |---|---|---|
   | program / `program_path` | `AppRun` / `AppRun` | `glpi-agent.exe` / `perl/bin/glpi-agent.exe` |
   | working directory | none named — the program's own directory is already the tree root | the tree root, named by the kind |
   | first arguments | `--script=glpi-agent` (the AppImage bundles several programs) | `-I<tree>/perl/agent`, `-I<tree>/perl/site/lib`, `-I<tree>/perl/vendor/lib`, `-I<tree>/perl/lib`, then `<tree>/perl/bin/glpi-agent` |

   - Windows is the one place the kind overrides the general working-directory rule. The bundled
     Perl expects the tree root, which upstream's portable `.bat` sets.
   - **The kind never runs `glpi-agent.bat`.**

3. **The common tail is part of the kind, not a default an operator may drop:**
   `--daemon --no-fork --no-task=deploy --conf-file=${config_dir}/glpi-agent-conf`
   `--vardir=${supervisor_dir}/agent-state --logger=file`
   `--logfile=${supervisor_dir}/agent-log/glpi-agent.log --logfile-maxsize=16`.
   - `--daemon` and `--no-fork` are supervision requirements. Without `--daemon` the watchdog
     restarts a run-once agent for ever. Without `--no-fork` the Supervisor holds a pid that ends
     while the real process runs unsupervised.
   - **`--no-task=deploy` keeps the `Deploy` task off, whatever the delivered Configuration says.**
     - The `Deploy` task runs the installers a GLPI server sends, as the account the agent runs
       under. For the whole inventory that account is root or `LocalSystem`.
     - The delivered `glpi-agent-conf` names the GLPI server. Without the flag, a Configuration
       naming another server would let that server run programs on the host (Q-1, Q-5).
     - The agent lays its command line over its configuration file, at startup and on every
       reload. A `no-task` in the file therefore has no effect.
     - The agent drops a task named in `no-task` from its available tasks. A `tasks =` line of the
       file orders and selects among the available tasks only.
     - The agent's `/now` request runs planned tasks only. So no Configuration turns the task back
       on.
     - `tasks =` is the way left to a Configuration to narrow the plan further.
   - The state directory lies **beside** `program/`, never inside it. So `deviceid` and caches
     survive package swaps and rollbacks. The kind names it among the directories made before
     every spawn, because the agent exits when it is missing.
   - File logging exists because a daemon with no console has nowhere else to write. The log lies
     in `agent-log/`, a directory of its own beside `program/`. The kind names it among the
     directories made before every spawn. So a process account running the agent may write it
     ([CLT-accounts-service-runs-under §21](CLT-accounts-service-runs-under.md)).
   - `agent-state/` and `agent-log/` are this kind's data directories in the sense of
     CLT-accounts-service-runs-under §21, and only they are. The kind names both in
     `ensure_dirs`. Under a process account each gets what that clause gives a kind's data
     directories:
     - the owning account and the group;
     - the mode `2770`;
     - the Windows ACL.
   - **The agent leads its own process group**
     ([CLT-supervisor-mode-and-kinds §53](CLT-supervisor-mode-and-kinds.md)).
     - The daemon forks a child for each task run, and the tasks start programs of their own.
     - The daemon passes `SIGTERM` on to its children. The escalation to `SIGKILL` reaches only
       the pid it is sent to.
     - On Linux and macOS the Runner's stop therefore signals the group, so no task outlives a
       stop.
     - On Windows a program the second service starts runs in a job, and a stop ends the job with
       everything in it.
     - On Windows a program the Client starts itself is stopped by its pid. Perl emulates `fork`
       with threads of that one process, so the task runs end with it. A program a task started
       may outlive the stop. The Consequences name that residual.

4. **GLPI's identity, configuration and version.**
   - `service_name = "glpi-agent"` is the Agent type every GLPI Configuration and Package is for.
   - A Configuration named `glpi-agent-conf` lands where `--conf-file` reads it. It applies by
     restart, since the agent has no reload signal.
   - The version probe and the preflight are `--version`, asked of the agent started exactly as
     the daemon is. They run after the first arguments of clause 2 and in its working directory,
     as [CLT-supervisor-mode-and-kinds §57](CLT-supervisor-mode-and-kinds.md) decides.
   - The program alone is not the agent. The bundled Perl answers a bare `--version` with its own
     banner. `AppRun` without `--script` is the AppImage's installer.
   - The kind reads the version only from a first line that is exactly `GLPI Agent (…)`. The
     number must have a release tag's shape (clause 7). At most one package revision `-<digits>`
     may follow it, and the kind drops it.
   - The kind writes the release as SemVer by the rule the Package is named by
     ([PKG-versions-high-water-mark-floor §6](PKG-versions-high-water-mark-floor.md)).
     `GLPI Agent (1.19-1)` and `GLPI Agent (1.19)` report the `service.version` `1.19.0`. That is
     the upstream version, and the Package version of the first repack (clause 7).
   - Kept, the revision would be a SemVer pre-release and rank the build below its release.
   - After a later repack the agent still reports the upstream version. The Server decides by the
     `supervisor.package.version` the Supervisor reports
     ([SRV-deployments-and-pinned-rollouts §14 and §25](SRV-deployments-and-pinned-rollouts.md)).
   - Any other banner reports no version rather than a guess. This covers `1.22-dev`,
     `1.19-1-gabcdef` and `1.19-1.2`, suffixes upstream does not document. The
     `supervisor.package.version` alone then tracks the install.
   - That `service.version` is the one attribute the kind reads from its agent. It is a
     `service.*` key the fold admits
     ([CLT-what-an-agent-reports §8](CLT-what-an-agent-reports.md)).
   - Until the first Configuration arrives, the agent refuses to start and the Supervisor holds
     after its crash budget. The first apply ends the hold.

5. **The Windows package is upstream's portable zip, byte for byte.**
   - The Client opens it as
     [PKG-signed-packages-and-delivery §62](PKG-signed-packages-and-delivery.md) decides.
   - Taken as published, the hash an Agent verifies is upstream's own.
   - It is uploaded to the Server like every artifact
     ([PKG-signed-packages-and-delivery §3](PKG-signed-packages-and-delivery.md)).
   - **The `.sha256` of the same release is the one check of the download.** The tool verifies the
     zip against the release's `glpi-agent-<tag>.sha256`, looked up by name.
     [OPS-operator-program-fleetctl §29](OPS-operator-program-fleetctl.md) verifies an upstream's
     OpenPGP signature. GLPI publishes none, so that clause has nothing to verify here.
   - The check detects a download damaged on the way, such as one cut short. It does not detect a
     change by anyone who can change the download at its source, since the checksum comes from
     the same place. The Consequences name that risk.

6. **The Linux package is the AppImage, repacked by `opamp-fleetctl package fetch`.**
   - The tool verifies the AppImage against the release's `glpi-agent-<tag>.sha256`, as clause 5
     verifies the zip. That check is the one check of this download too.
   - It extracts the AppImage's file system without running it.
   - It refuses a tree without `AppRun` and drops `.DirIcon`.
   - It packs the result by the repack of
     [OPS-operator-program-fleetctl §27](OPS-operator-program-fleetctl.md). The wrapper directory
     is `glpi-agent-<version>`. The artifact is `glpi-agent_<version>_linux_amd64.tar.gz`.
   - `<version>` is the Package version (clause 7), and `<tag>` upstream's own.
   - The repack runs on any host with `unsquashfs`, such as the Dev Container. From there the
     fleet's own hash and Ed25519 signature are the chain of trust.
   - **The AppImage is never run.**
     - Its squashfs file system starts where its ELF program ends, which the ELF header says.
     - `unsquashfs` (squashfs-tools) reads the file system from that offset as data.
     - The tool refuses a file that is not a 64-bit type-2 AppImage with a squashfs file system
       there.
     - The headers are read by `squashfs_offset` and `squashfs_inodes`, each at a fixed length.
       Both parse downloaded bytes, so a fuzz target runs them (Q-2).
   - **The extraction is bounded by count, and its bytes are a named gap.**
     - Before `unsquashfs` runs, the tool reads the inode count of the squashfs superblock.
     - Above the 10 000 members a tree may carry
       ([PKG-signed-packages-and-delivery §57](PKG-signed-packages-and-delivery.md)), the tool
       refuses the repack, and nothing is extracted.
     - Every file, directory and link of the file system is an inode. The pack drops only the
       links that name nothing and `.DirIcon`. So a tree above the cap would nearly always fail
       the pack all the same, after the whole extraction.
     - Hard links and links to directories make more members than inodes. The pack's own cap
       bounds those, after the extraction.
     - The superblock states no unpacked size. The tool does not bound the bytes `unsquashfs`
       writes into the staging directory. The Consequences name that gap.
   - **`unsquashfs` is 4.5.1 or later.**
     - It parses a download whose only check is a `.sha256` from the same release.
     - Older squashfs-tools write outside the destination for a crafted file system. CVE-2021-40153
       is fixed in 4.5. CVE-2021-41072, its incomplete fix, is fixed in 4.5.1.
     - Before it extracts, the tool runs `unsquashfs -version`. It reads the version from the first
       line, `unsquashfs version <x.y[.z]> …`.
     - A version below 4.5.1, or a first line the tool cannot read, refuses the repack. The message
       names the version found and the one required, and nothing is extracted.
   - **The repacked tree keeps upstream's runtimes, and `AppRun` chooses between them as upstream
     ships it.**
     - `AppRun.env` states an `APPDIR_LIBC_VERSION` of 2.27.
     - Where the host's glibc is newer, `AppRun` starts the agent through `runtime/default`.
       Otherwise it starts it through `runtime/compat`, the glibc 2.27 the AppImage bundles.
     - `runtime/default` reaches the host's C library through one absolute link,
       `runtime/default/lib64/ld-linux-x86-64.so.2` → `/lib64/ld-linux-x86-64.so.2`.
     - The `glpi` kind names exactly that link, by that path and that target, as the one its tree
       keeps ([OPS-operator-program-fleetctl §27](OPS-operator-program-fleetctl.md),
       [PKG-signed-packages-and-delivery §57](PKG-signed-packages-and-delivery.md)).
     - The repack packs it as that link, and the Client creates it. Every other link leading out
       still fails the repack.
     - `AppRun.env` and every file of either runtime travel as upstream built them.
     - Upstream's compat runtime holds a link that names nothing:
       `runtime/compat/lib/x86_64-linux-gnu/libcrypt.so.1` names a `libcrypt-2.27.so` that
       directory does not hold. The repack drops it, as it drops every link that names nothing in
       the tree.
   - **Nothing before the pack follows a link of the download.**
     - The tree's root, `.DirIcon` and `AppRun` are each asked what they are themselves
       (`symlink_metadata`).
     - `.DirIcon` is removed only as a link or a file, which removes no target.
     - A root or an `AppRun` that is not a directory or a file of its own is refused, with nothing
       packed.

7. **GLPI ships for `windows/amd64` and `linux/amd64` only.**
   - The release's zip is matched case-insensitively, since its spelling changed case at 1.9.
   - Tags carry no `v` and have two or three numeric parts.
   - Both artifacts are entries of one Package of Agent type `glpi-agent`. Its version is the
     upstream version written as SemVer (PKG-versions-high-water-mark-floor §6).
   - The release tagged `1.19` is fetched under that tag and uploaded as Package `1.19.0`.
   - Hosts on other platforms get no GLPI package.
   - The Linux artifact is a repack. A later repack of one version, such as one packed by a changed
     rule, is numbered as [OPS-operator-program-fleetctl §28](OPS-operator-program-fleetctl.md)
     numbers it.
   - `package fetch --repack 2` of `1.19` uploads Package `1.19.1-0.r.2`. Its Windows entry is
     upstream's zip again.

8. **One GLPI Agent per host, and it is the fleet's.**
   - Where a native installation exists, the operator switches its autostart off first. On Linux
     that is `systemctl disable --now glpi-agent`. On Windows it is `EXECMODE=3` or a disabled
     `glpi-agent` service.
   - A foreground daemon skips the agent's PID-file single-instance check. Two agents would
     inventory the host twice.
   - This hand-over is the operator's step, documented in the manual.

9. **The GLPI Agent does not speak OpAMP to the Client.** The kind refuses an `endpoint_port`. Its
   Endpoint is bound, and nothing connects.

10. **The Configuration name is part of the contract.**
    - `opamp-fleetctl package fetch` uploads `glpi-agent-conf` beside the package when the Server
      holds none of that name. Its body is `config/examples/glpi-agent-conf.cfg`.
    - The tool never overwrites an existing Configuration and distributes nothing.
    - The name is the file name the kind points its agent at. Renamed on one side only, the agent
      reads a file that does not exist.
    - The example's `server =` names a placeholder host. The example says that the operator must
      edit it before a rollout.

11. **The kind has an artifact document held by two tests.**
    - `docs/artifacts/glpi-agent.md` states source, assets, integrity, treatment, form in the tree
      and what the Client derives.
    - It states what goes red when upstream moves something.
    - One test holds the packing side and one the client side. Each is `cfg`-gated per platform
      where the facts differ.

12. **The kind declares that the GLPI Agent gets no Supervisor Endpoint token.**
    [CLT-modes-and-gateway §28](CLT-modes-and-gateway.md) lets a kind declare that when its
    program never connects to its Supervisor Endpoint (clause 9). The token would serve no
    connection. It would reach every program the agent's tasks start.

**Out of scope:**

- a GLPI health probe against the agent's embedded web interface (port 62354) instead of process
  aliveness;
- a Linux arm64 GLPI package. No AppImage exists for it. A source distribution plus a relocatable
  Perl is the only visible route.

## Alternatives considered

- **A `command` recipe per platform.** Rejected: the block would be duplicated per host and per
  platform, and cannot follow an upstream release that moves a path.
- **One block with placeholders** (`${exe_suffix}` and friends). Rejected: it turns the operator's
  file into a template of the artifact's layout, and the Windows argument list is not the Linux one
  with a suffix — it has four extra paths.
- **Let the kind accept `args`** as an escape hatch. Rejected: a kind that needs one does not know
  its agent; an operator's choices belong in the agent's own configuration, and `command` remains
  for a genuinely different invocation.
- **A kind that drives the native service managers** (`systemctl`, `sc.exe`). Rejected: supervising
  a process the Client did not spawn means no watchdog, no health gate, no bounded stop.
- **Deliver the AppImage as a single file, unopened.** Rejected: it needs `libfuse2` on every host,
  or `APPIMAGE_EXTRACT_AND_RUN=1`, which re-extracts on every start the watchdog makes.
- **Repack the Windows zip into `.tar.gz`/`.7z`.** Rejected: the fleet would verify a hash the
  packing host invented instead of upstream's.
- **Assemble a Linux runtime ourselves** (relocatable Perl plus `cpanm`, staticperl, PAR::Packer).
  Rejected: a compile step per architecture and a dependency list that moves with every release,
  where the AppImage is exactly that assembly, built and tested upstream.
- **The snap.** Rejected: requires `snapd`, a second manager beside the fleet.
- **Loosen the tree rules** (links, member cap) to pack the AppImage tree as-is. Rejected: the
  dereferenced tree fits, and the rules protect every package on every host. The one link kept is
  named by this kind, by its exact path and target, and the core refuses every other.
- **Keep `runtime/default` with the packing host's loader copied in.** Rejected: the artifact
  would carry a file of the host that packed it, its bytes would depend on that host, and every
  fleet host would start the agent on another distribution's loader with its own C library.
- **Drop `runtime/default` and leave `AppRun.env` as published.** Rejected: on every host newer
  than glibc 2.27 `AppRun` still chooses the missing runtime and the agent does not start.
- **Always start on the compat runtime**: drop `runtime/default`, rewrite the
  `APPDIR_LIBC_VERSION` line of `AppRun.env` to a version no glibc exceeds, and repoint the
  compat runtime's dangling `libcrypt.so.1` at the `libcrypt-2.27.so` in the tree's
  `lib/x86_64-linux-gnu/`. It needs no link out of the tree, and every host runs the agent on the
  same C library. Rejected for hardening: a daemon that runs as root, with a web interface on
  port 62354 and tasks its server drives, would run on glibc 2.27, from
  Ubuntu 18.04, on every host, and no security fix of the host's C library would reach it; fixes
  would come only with a new upstream AppImage. It also edits two of upstream's files, an edit
  each repack has to repeat and each upstream change can break silently, and it has to walk
  directories of the download before the pack's own link checks run.
- **Leave the Deploy task on, and trust the delivered Configuration.** Rejected: the Server's
  Configuration would choose which GLPI server runs installers on the host, as root under the
  setup the manual recommends. That breaks Q-5's bound on what a remote configuration causes.
- **Name the GLPI server on the command line too.** Rejected: the server is the operator's choice
  and differs per fleet, and the kind has no settings (clause 1). With Deploy off, the server a
  Configuration names drives inventory, not installers.
- **Stop the agent by its pid alone.** Rejected: the daemon forks its tasks, and the escalation to
  `SIGKILL` would leave a running task behind.
- **Pin each release's digest in the tool.** Rejected for now: every GLPI release would need a
  change to this repository before an operator could fetch it. The pinned value would be read off
  the same release page, so it guards only against a release changed after the pin. It is the
  answer if upstream's release account must not be trusted.
- **Have the operator confirm a digest** (a `--sha256` read from a channel of their choosing).
  Rejected: upstream publishes its digests on one channel, the release, so the operator would
  confirm the value the tool already reads.
- **Bound the extraction's bytes** by listing the file system first (`unsquashfs -lls`) or by
  watching the staging directory. Rejected for now: the first parses the output of a program fed a
  crafted input, and the second races the extraction. The inode cap bounds the count, and the gap
  in bytes costs the packing host a full disk, not a fleet host.

## Sources / Prior art

- [GLPI Agent man page](https://glpi-agent.readthedocs.io/en/latest/man/glpi-agent.html) —
  `--daemon`, `--no-fork`, `--conf-file`, `--vardir`, logger options.
- [GLPI Agent usage](https://glpi-agent.readthedocs.io/en/latest/usage.html) — managed mode and the
  embedded web interface on port 62354.
- The
  [Windows installer reference](https://glpi-agent.readthedocs.io/en/latest/installation/windows-command-line.html)
  — `EXECMODE` 1/2/3 (service / task / manual).
- [`bin/glpi-agent`](https://github.com/glpi-project/glpi-agent/blob/develop/bin/glpi-agent) and
  [Windows packaging](https://github.com/glpi-project/glpi-agent/blob/develop/contrib/windows/glpi-agent-packaging.pl)
  — the platform-neutral daemon class, the version string, and the four `-I` paths its MSI
  registers for the service;
  [`template.bat.tt`](https://github.com/glpi-project/glpi-agent/blob/develop/contrib/windows/packaging/template.bat.tt)
  — the portable `.bat` wrapper, which moves to the tree root before it starts the agent.
- [GLPI Agent release 1.19](https://github.com/glpi-project/glpi-agent/releases/tag/1.19) — the
  surveyed artifact set;
  [portable discussion #273](https://github.com/glpi-project/glpi-agent/discussions/273)
  — the Windows zip as the supported portable form.
- The script
  [`make-linux-appimage.sh`](https://github.com/glpi-project/glpi-agent/blob/develop/contrib/unix/make-linux-appimage.sh),
  [`glpi-agent-appimage-hook`](https://github.com/glpi-project/glpi-agent/blob/develop/contrib/unix/glpi-agent-appimage-hook),
  [appimage-builder](https://appimage-builder.readthedocs.io/) — how the AppImage is built, how
  `AppRun` dispatches (`--script`, `GLPIAGENT_SCRIPT`), and the `$ORIGIN`/bundled-libc mechanism.
- [CVE-2021-40153](https://nvd.nist.gov/vuln/detail/CVE-2021-40153) and
  [CVE-2021-41072](https://nvd.nist.gov/vuln/detail/CVE-2021-41072) — `unsquashfs` before 4.5
  and 4.5 itself write outside the destination for a crafted file system; 4.5.1 fixes both
  ([squashfs-tools release notes](https://github.com/plougher/squashfs-tools/blob/master/CHANGES)).
- Verified against GLPI Agent 1.15 and 1.19 in the Dev Container: foreground daemon, clean exit on
  `SIGTERM`, hard failure on a missing `--conf-file` or `--vardir`, the relocated AppImage tree
  running without FUSE or root. The 1.19 tree as repacked here, unpacked by the Client's tree
  install with its one kept link, answers `AppRun --script=glpi-agent --version` with
  `GLPI Agent (1.19-1)` and runs a local inventory on Debian 12 (glibc 2.36) on the default
  runtime, which loads the host's `libc.so.6` and `libcrypt.so.1`.
- The daemon class
  [`GLPI::Agent::Daemon`](https://github.com/glpi-project/glpi-agent/blob/1.19/lib/GLPI/Agent/Daemon.pm)
  — `runTask` forks a child per task run, and `terminate` passes `SIGTERM` on to every child;
  [`GLPI::Agent::Config`](https://github.com/glpi-project/glpi-agent/blob/1.19/lib/GLPI/Agent/Config.pm)
  — the file is read first and the command line laid over it, at startup and on `reload`, and
  `no-task` is a comma-separated list;
  [`GLPI::Agent::Task::Collect::Command`](https://github.com/glpi-project/glpi-agent/blob/1.19/lib/GLPI/Agent/Task/Collect/Command.pm)
  — `runCommand`, disabled by upstream. It is commented out of the dispatch table of
  `GLPI::Agent::Task::Collect` in every release from 1.0 to 1.15, and a `disabled` constant from
  1.16 to 1.21, read 2026-11-22.
- [`GLPI::Agent`](https://github.com/glpi-project/glpi-agent/blob/1.19/lib/GLPI/Agent.pm) —
  `getAvailableTasks` skips every task `no-task` names, and `computeTaskExecutionPlan` builds the
  plan from the available tasks alone, in the order `tasks` gives;
  [`GLPI::Agent::HTTP::Server`](https://github.com/glpi-project/glpi-agent/blob/1.19/lib/GLPI/Agent/HTTP/Server.pm)
  and
  [`GLPI::Agent::Target`](https://github.com/glpi-project/glpi-agent/blob/1.19/lib/GLPI/Agent/Target.pm)
  — a `/now` request becomes a run-now event, and `triggerRunTasksNow` runs planned tasks only;
  the `-l`, `--local=PATH` option of
  [`bin/glpi-agent`](https://github.com/glpi-project/glpi-agent/blob/1.19/bin/glpi-agent) —
  "write tasks results locally", which the configuration file may set as `local`.
- The asset list of
  [release 1.19](https://api.github.com/repos/glpi-project/glpi-agent/releases/tags/1.19),
  read 2026-11-22: 27 assets, no signature file among them, and `glpi-agent-1.19.sha256` with a
  line for `GLPI-Agent-1.19-x64.zip` and for `glpi-agent-1.19-x86_64.AppImage`.
- [squashfs format](https://dr-emann.github.io/squashfs/) — the superblock's magic `hsqs` and its
  inode count, a 32-bit little-endian number at byte 4.
- [AppImageCrafters/AppRun](https://github.com/AppImageCrafters/AppRun), the `AppRun` the
  AppImage carries — `setup_runtime` in `src/apprun/runtime_interpreter.c` chooses
  `runtime/default` only where `gnu_get_libc_version()` exceeds `APPDIR_LIBC_VERSION`.

## Consequences

- Positive: one block for both platforms and for a mixed fleet; no host carries Perl, FUSE, snapd or
  a preinstalled agent.
- Positive: the Windows artifact travels exactly as upstream published it.
- Positive: on a host with glibc newer than 2.27 the agent runs on the host's own C library,
  which the distribution patches; one Linux artifact serves old and new distributions, and the
  repack changes none of upstream's files beyond removing `.DirIcon`.
- Negative: the tree keeps one link to a path on every host, `/lib64/ld-linux-x86-64.so.2`. A
  newer host whose program loader is not there does not start the agent.
- Negative: on a host with glibc 2.27 or older `AppRun` starts the agent on upstream's compat
  runtime as upstream ships it, its dangling `libcrypt.so.1` included, and the repack drops that
  link. There the bundled loader looks for `libcrypt.so.1` on the host's library path and takes
  the host's, built for the host's own glibc, or finds none and the agent does not start. No such
  host was run.
- Negative: we own the Linux repack — one tool run and an upload per release; the artifact does not
  match upstream's hash, so upstream's `.sha256` is checked at packing time. Artifacts are large
  (~30–70 MB per platform, 248 MB unpacked on Linux), and dereferencing duplicates shared
  libraries.
- Negative: an agent packed differently does not fit; the answer is the packing tool, not a key.
  An extra flag needs a code change or the agent's own configuration.
- Negative: one more plugin to keep, with a document and two tests. The disabled native autostart
  is an operator duty the fleet cannot verify.
- Negative: the agent runs as the account that starts it: the Client's, or the process account where
  one is registered, through the second service
  ([CLT-accounts-service-runs-under §16 and §18](CLT-accounts-service-runs-under.md)). Its
  inventory reads the DMI tables and the hardware,
  which needs root on Linux and an administrator on Windows. On Linux, where the packages run the
  Client under its own account, a host that needs the whole agent registers with `service install
  --run-as root` and no process account (CLT-accounts-service-runs-under §1). On Windows a first
  registration runs the Client as the virtual account `NT SERVICE\opamp-fleet`, which the install
  grants no group and no privilege (CLT-accounts-service-runs-under §1 and §2): the agent
  inventories less, and a host that needs the whole agent registers with `service install --run-as
  LocalSystem` and no process account. A re-registration keeps the account the SCM holds, so a host
  moves only when the operator names the account. A process account is never root or `LocalSystem`
  (CLT-accounts-service-runs-under §16), so an agent under one inventories less.
- Negative: the Linux repack needs `unsquashfs` 4.5.1 or later on the packing host. A
  distribution that backports the fixes into an older version is refused all the same, since the
  tool reads the version and not the patches.
- Negative: upstream signs none of its assets, and the `.sha256` comes from the same release
  (clauses 5 and 6). Whoever can publish a GLPI release can replace an asset and its line
  together. The tool then verifies the replacement, and the fleet signs it with its own key. The
  operator's look at the release before a fetch is the only defence left against that.
- Negative: the bytes `unsquashfs` writes are not bounded (clause 6). A crafted file system within
  the inode cap can fill the disk the staging directory lies on. The tool removes the staging
  directory after every repack, unless the process itself is killed.
- Negative: the Deploy task is off on every host (clause 3). The fleet's own packages are the way
  to install software on a fleet host. A `no-task` line in the delivered Configuration has no
  effect, since the command line replaces it whole.
- Negative: a delivered Configuration still chooses the GLPI server. That server receives the
  host's inventory and drives every task but Deploy, as the agent's account. Its Collect task reads
  files, registry values and WMI. Its `runCommand` function is disabled in every release from 1.0
  to 1.21, the ones `package fetch` offers. A later release that enables it would let that server
  run commands on the host, and nothing in the fleet goes red.
- Negative: a delivered Configuration also steers what the agent writes and reads, as its account.
  `local = <dir>` writes the inventory into any directory that account may write, outside the
  Supervisor's configuration directory that Q-5 bounds. `include` reads further files, and the
  `httpd-*` keys open the agent's web interface on a port and to addresses the Configuration
  names.
- Negative: on Windows, a program a GLPI task started may outlive a stop of the agent where the
  Client starts it itself (clause 3). Under the second service the job ends it.
- The answer to the two Configuration risks above: a host where Q-5 must hold against the Server
  lists the Supervisor in
  `[supervisors] remote_config_disabled`
  ([docs/HARDENING.md](../HARDENING.md),
  [docs/manual/client.md](../manual/client.md#switching-remote-configuration-off-for-one-supervisor)).
  It places `glpi-agent-conf` by hand.
- Follow-ups: the health probe and Linux arm64 named under Out of scope.

## Enforcement

- `crates/fleet-agent/src/supervisor/glpi.rs`:
  - `the_block_has_no_settings` and `the_recipes_keys_are_refused_by_name` (clause 1, `env`
    included);
  - `the_defaults_are_the_artifacts` (the client side of clause 11);
  - `linux_runs_the_apprun_entry_point` and `windows_runs_the_bundled_perl` (clause 2);
  - `the_invocation_carries_what_supervision_requires` (clause 3: both flags, `--conf-file`, and
    `--vardir` beside `program/`);
  - `the_deploy_task_is_off_and_the_agent_is_stopped_as_a_group` (clause 3: `--no-task=deploy` and
    the process group);
  - `the_agent_logs_to_a_file_in_a_directory_made_for_it` (clause 3's logging);
  - `the_agent_reads_the_configuration_by_its_contracted_name` (the client side of clause 10);
  - `the_version_is_asked_of_the_agent_the_daemon_runs`,
    `the_version_banner_reads_as_the_package_version` and
    `a_banner_that_is_not_a_release_reports_no_version` (clause 4);
  - `the_agent_is_handed_no_endpoint_token` (clause 12).
- `crates/fleet-agent/tests/glpi_supervisor.rs`: `a_two_line_glpi_block_runs_reports_and_applies`.
  - The two-line block runs and reports its version.
  - The stub answers as the program it replaces, so the version is the agent's only if the probe
    starts the agent.
  - The test finds the agent's directories made and an apply acknowledged.
  - The test writes the entry `glpi-agent-conf` itself, so it shows no delivered entry landing.
- `crates/fleet-tools/src/fetch.rs`:
  - `glpi_finds_both_zip_spellings_and_repacks_only_linux` (the packing side of clause 11, the
    checksum file of clauses 5 and 6, and the Package version of clauses 6 and 7);
  - `an_appimages_file_system_is_found_where_its_program_ends`,
    `the_repack_runs_nothing_it_downloaded` and `a_repack_without_unsquashfs_names_it` (clause 6);
  - `a_file_system_of_more_inodes_than_a_tree_may_carry_is_not_extracted` (clause 6's inode cap);
  - `an_extracted_tree_is_packed_without_its_desktop_icon`,
    `an_extracted_tree_without_apprun_is_refused` and
    `a_repack_extracts_with_unsquashfs_and_packs_the_tree` (clause 6);
  - `an_extracted_tree_keeps_upstreams_runtimes_as_built` (clause 6's runtimes and the one kept
    link, installed by the Client);
  - `the_repack_follows_no_link_of_the_download` (clause 6's pre-pack edits);
  - `the_glpi_tree_keeps_the_link_the_kind_names`, with `the_defaults_are_the_artifacts` in
    `glpi.rs` (the kept link on the packing and the client side);
  - `unsquashfs_older_than_4_5_1_is_refused` and `the_unsquashfs_version_is_read_from_its_banner`
    (clause 6's oldest version of `unsquashfs`);
  - `the_glpi_configuration_is_the_file_the_kind_points_its_agent_at` (the packing side of clause
    10);
  - `a_tag_is_a_version_only_when_it_is_this_agents_release` (clause 7's tags);
  - `a_later_glpi_repack_is_numbered_as_the_core_numbers_one` (clause 7's later repack).
- `fuzz/fuzz_targets/appimage_header.rs` runs `squashfs_offset` and `squashfs_inodes` on
  arbitrary bytes in CI (clause 6, Q-2).
- The downloads of clauses 5 and 6 are bounded by the caps every download of `package fetch` has.
  The tests of those caps belong to Q-2, not to this ADR.

**Not mechanically decidable:**

- The native-autostart hand-over (clause 8) happens on a host outside the fleet's view.
- Whether upstream keeps the bundled Perl's library roots is a run-time property no repository
  test can see. The artifact document names these rows as warnings.
- The tests of clause 6 run a stand-in for `unsquashfs`. A repack in the Dev Container shows that
  the real one extracts a real AppImage as the stand-in does. It also shows that `AppRun` then
  starts the agent, on the default runtime, on a host newer than glibc 2.27.
- That the agent forks its tasks, and lays its command line over its file, is read from
  upstream's source (Sources). No test runs the real agent.
- That upstream publishes no signature is a property of each release. The tool does not look for
  one.
- That `no-task` keeps a task out of every path that runs one is read from upstream's source of
  1.19 (Sources). It is a property of each release, and no test runs the real agent.
- That Collect's `runCommand` stays disabled is a property of each release. A release that enables
  it turns nothing red.
