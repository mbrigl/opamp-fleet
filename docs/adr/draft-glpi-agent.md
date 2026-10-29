# ADR-draft-glpi-agent: The GLPI Agent gets a kind of its own, and its packages are upstream's portable zip as published and its AppImage repacked as a tree

- **Status:** 🟡 proposed
- **Date:** 2026-10-10
- **Deciders:** Markus Brigl
- **Applies to:** crates/fleet-agent/src/supervisor/glpi.rs, the `glpi` feature of crates/fleet-agent/Cargo.toml, glpi_plans and repack_appimage in opamp-fleetctl package fetch, config/examples/glpi-agent-conf.cfg, docs/artifacts/glpi-agent.md, docs/manual/glpi-agent.md

## Context

The fleet manages the [GLPI Agent](https://glpi-agent.readthedocs.io/) — the inventory agent of the
GLPI asset-management suite — beyond the Collector. It should be an ordinary Managed Process: a
package the Client installs into its own Supervisor directory, updates with a health gate and rolls
back, a Configuration rolled out from the Server, a restart issued from it. A kind knows its own
agent ([ADR-0017](0017-supervisor-mode-and-its-kinds.md)): what
follows from the artifact and the platform is compiled in, and a block holds only what an operator
decides.

The GLPI Agent is a Perl application, not a single binary:

- `--daemon --no-fork` runs it as a foreground daemon on every platform (the launcher instantiates
  the same platform-neutral `GLPI::Agent::Daemon` class whether or not it has a console). Without
  `--daemon` it runs its tasks once and exits; without `--no-fork` it detaches.
- Its configuration is a file named by `--conf-file`. It has no reload signal.
- It never creates a missing `--vardir`; it exits at startup.
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
[ADR-0027](0027-packages-signatures-and-delivery.md)).
Five links are directories, and one is load-bearing: `usr/share/perl/5.26` → `5.26.1` is the name
the bundled `PERL5LIB` uses.

The two platform invocations differ in nearly everything — program, its place in the tree, working
directory, and on Windows four Perl `-I` paths and the script by path — and none of those
differences is anybody's decision.

## Decision

We will supervise the GLPI Agent with a `glpi` kind whose block is `type` and `name` alone, and
deliver it as packages built from upstream's own artifacts — the Windows portable zip as published,
the Linux AppImage repacked deterministically into a tree whose one link is the host loader's.

1. **The `glpi` kind has no settings.** Its block is two lines on both platforms:

   ```toml
   [[supervisor]]
   type = "glpi"
   name = "glpi"
   ```

   Its strict parse (`Plugin::check`) accepts an empty table and refuses every key. A block carrying
   `args`, `version_args`, `working_dir`, `env` or `reload_signal` fails at startup naming what
   supplies the value. An installation that genuinely needs another invocation uses the generic
   `command` kind and writes the whole invocation itself.

2. **The kind knows the invocation, per platform.** The tree is unpacked at
   `${supervisor_dir}/program/tree`:

   | | Linux (repacked AppImage) | Windows (portable zip) |
   |---|---|---|
   | program / `program_path` | `AppRun` / `AppRun` | `glpi-agent.exe` / `perl/bin/glpi-agent.exe` |
   | working directory | none named — the program's own directory is already the tree root | the tree root, named by the kind |
   | first arguments | `--script=glpi-agent` (the AppImage bundles several programs) | `-I<tree>/perl/agent`, `-I<tree>/perl/site/lib`, `-I<tree>/perl/vendor/lib`, `-I<tree>/perl/lib`, then `<tree>/perl/bin/glpi-agent` |

   Windows is the one place the kind overrides the general working-directory rule: the bundled Perl
   expects the tree root, which is what upstream's portable `.bat` sets. **Never `glpi-agent.bat`.**

3. **The common tail is part of the kind, not a default an operator may drop:**
   `--daemon --no-fork --conf-file=${config_dir}/glpi-agent-conf`
   `--vardir=${supervisor_dir}/agent-state --logger=file`
   `--logfile=${supervisor_dir}/agent-log/glpi-agent.log --logfile-maxsize=16`.
   - `--daemon` and `--no-fork` are supervision requirements: without the first the watchdog
     restarts a run-once agent for ever; without the second the Supervisor holds a pid that ends
     while the real process runs unsupervised.
   - The state directory lies **beside** `program/`, never inside it, so `deviceid` and caches
     survive package swaps and rollbacks; the kind names it among the directories made before every
     spawn, because the agent exits when it is missing.
   - File logging exists because a daemon with no console has nowhere else to write. The log lies
     in `agent-log/`, a directory of its own beside `program/` that the kind names among the
     directories made before every spawn, so that a process account running the agent may write
     it ([ADR-0037](0037-the-accounts-the-service-runs-under.md) clause 21).
   - `agent-state/` and `agent-log/` are this kind's data directories in the sense of ADR-0037
     clause 21, and only they are: the kind names both in `ensure_dirs`, and under a process
     account each gets the owner, the group, the mode `2770` and the Windows ACL that clause gives
     a kind's data directories.

4. **GLPI's identity, configuration and version.** `service_name = "glpi-agent"` is the Agent type
   every GLPI Configuration and Package is for. A Configuration named `glpi-agent-conf` lands where
   `--conf-file` reads it and applies by restart — there is no reload signal. The version probe and
   the preflight are `--version`, asked of the agent started exactly as the daemon is: after the
   first arguments of clause 2 and in its working directory, as
   [ADR-draft-supervisor-mode-and-its-kinds](draft-supervisor-mode-and-its-kinds.md) clause 56
   decides for the probe and the preflight. The program alone is not the agent — the bundled Perl
   answers a bare `--version` with its own banner, and `AppRun` without `--script` is the
   AppImage's installer. The kind reads the version only from a first line that is exactly
   `GLPI Agent (…)`, and only a number of a release tag's shape (clause 7), followed at most by
   one package revision `-<digits>`, which it drops. It writes the release as SemVer by the rule
   the Package is named by ([ADR-0028](0028-package-versions-high-water-mark-and-floor.md)
   clause 6): `GLPI Agent (1.19-1)` and `GLPI Agent (1.19)` report the `service.version`
   `1.19.0`, the upstream version, which is the Package version of the first repack (clause 7).
   Kept, the revision would be a SemVer pre-release and rank the build below its release. After
   a later repack the agent still reports the upstream version, and the Server decides by the
   `supervisor.package.version` the Supervisor reports
   ([ADR-draft-deployments-and-rollouts](draft-deployments-and-rollouts.md) clauses 14 and 25).
   Any other banner reports none rather than a guess — `1.22-dev`, `1.19-1-gabcdef` and
   `1.19-1.2` included, suffixes upstream does not document — and the
   `supervisor.package.version` alone tracks the install. That
   `service.version` is the one attribute the kind reads from its agent, a
   `service.*` key the fold admits
   ([ADR-0016](0016-what-an-agent-reports-about-itself.md) clause 8). Until
   the first Configuration arrives the agent refuses to start and the Supervisor holds after its
   crash budget; the first apply ends the hold.

5. **The Windows package is upstream's portable zip, byte for byte**, opened as
   [ADR-0027](0027-packages-signatures-and-delivery.md) clause 62
   decides. Taken as published, the hash an Agent verifies is upstream's own. It is uploaded to the
   Server like every artifact
   ([ADR-0027](0027-packages-signatures-and-delivery.md)
   clause 3).

6. **The Linux package is the AppImage, repacked by `opamp-fleetctl package fetch`.** The tool
   verifies the AppImage against the release's `glpi-agent-<tag>.sha256` (looked up by name),
   extracts its file system without running it, refuses if `AppRun` is missing, drops `.DirIcon`,
   and packs the result by the repack of
   [ADR-draft-the-operator-program-opamp-fleetctl](draft-the-operator-program-opamp-fleetctl.md)
   clause 27 under the wrapper directory `glpi-agent-<version>`, as
   `glpi-agent_<version>_linux_amd64.tar.gz`, `<version>` being the Package version (clause 7)
   and `<tag>` upstream's own. **The AppImage is never run**: its squashfs file system starts
   where its ELF program ends, which the ELF header says, and `unsquashfs` (squashfs-tools) reads
   it from that offset as data; a file that is not a 64-bit type-2 AppImage with a squashfs file
   system there is refused. The repack runs on any host with `unsquashfs`, such as the Dev
   Container. From there the fleet's own hash and Ed25519 signature are the chain of trust.
   - **`unsquashfs` is 4.5.1 or later.** It parses a download whose only check is a `.sha256`
     from the same release, and older squashfs-tools write outside the destination for a crafted
     file system (CVE-2021-40153, fixed in 4.5, and CVE-2021-41072, its incomplete fix, fixed in
     4.5.1). Before it extracts, the tool runs `unsquashfs -version` and reads the version from
     the first line, `unsquashfs version <x.y[.z]> …`. A version below 4.5.1, or a first line it
     cannot read, refuses the repack with a message that names the version found and the one
     required, and nothing is extracted.
   - **The repacked tree keeps upstream's runtimes, and `AppRun` chooses between them as
     upstream ships it.** `AppRun` starts the agent through `runtime/default` where the host's
     glibc is newer than the `APPDIR_LIBC_VERSION` that `AppRun.env` states, 2.27, and through
     `runtime/compat`, the glibc 2.27 the AppImage bundles, otherwise. `runtime/default` reaches
     the host's C library through one absolute link,
     `runtime/default/lib64/ld-linux-x86-64.so.2` → `/lib64/ld-linux-x86-64.so.2`. The `glpi`
     kind names exactly that link, by that path and that target, as the one its tree keeps
     ([ADR-draft-the-operator-program-opamp-fleetctl](draft-the-operator-program-opamp-fleetctl.md)
     clause 27,
     [ADR-draft-packages-signatures-and-delivery](draft-packages-signatures-and-delivery.md)
     clause 57): the repack packs it as that link and the Client creates it; every other link
     leading out still fails the repack. `AppRun.env` and every file of either runtime travel as
     upstream built them. Upstream's compat runtime holds a link that names nothing:
     `runtime/compat/lib/x86_64-linux-gnu/libcrypt.so.1` names a `libcrypt-2.27.so` that
     directory does not hold. The repack drops it, as it drops every link that names nothing in
     the tree.
   - **Nothing before the pack follows a link of the download.** The tree's root, `.DirIcon`
     and `AppRun` are each asked what they are themselves (`symlink_metadata`). `.DirIcon` is
     removed only as a link or a file, which removes no target; a root or an `AppRun` that is not
     a directory or a file of its own is refused, with nothing packed.

7. **GLPI ships for `windows/amd64` and `linux/amd64` only.** The release's zip is matched
   case-insensitively (its spelling changed case at 1.9); tags carry no `v` and have two or three
   numeric parts. Both artifacts are entries of one Package of Agent type `glpi-agent` at the
   upstream version written as SemVer (ADR-0028 clause 6): the release tagged `1.19` is fetched
   under that tag and uploaded as Package `1.19.0`. Hosts on other platforms get no GLPI package.
   The Linux artifact is a repack, so a later repack of one version, such as one packed by a
   changed rule, is numbered as clause 28 of
   [ADR-draft-the-operator-program-opamp-fleetctl](draft-the-operator-program-opamp-fleetctl.md)
   numbers it: `package fetch --repack 2` of `1.19` uploads Package `1.19.1-0.r.2`, its Windows
   entry upstream's zip again.

8. **One GLPI Agent per host, and it is the fleet's.** Where a native installation exists, its
   autostart is switched off first — `systemctl disable --now glpi-agent` on Linux, `EXECMODE=3`
   or a disabled `glpi-agent` service on Windows — because a foreground daemon skips the agent's
   PID-file single-instance check and two agents would inventory the host twice. This hand-over is
   the operator's step, documented in the manual.

9. **The GLPI Agent does not speak OpAMP to the Client.** The kind refuses an `endpoint_port`; its
   Endpoint is bound and nothing connects.

10. **The Configuration name is part of the contract.** `opamp-fleetctl package fetch` uploads
    `glpi-agent-conf` (body `config/examples/glpi-agent-conf.cfg`) beside the package when the
    Server holds none of that name, never overwriting an existing one and distributing nothing. The
    name is the file name the kind points its agent at, so renaming it on one side only leaves the
    agent reading a file that does not exist.

11. **The kind has an artifact document held by two tests.** `docs/artifacts/glpi-agent.md` states
    source, assets, integrity, treatment, form in the tree, what the Client derives, and what goes
    red when upstream moves something — one test on the packing side, one on the client side,
    `cfg`-gated per platform where the facts differ.

12. **The kind declares that the GLPI Agent gets no Supervisor Endpoint token,** as clause 28 of
    [ADR-draft-client-modes-and-the-gateway](draft-client-modes-and-the-gateway.md) lets a kind
    whose program never connects to its Supervisor Endpoint (clause 9). The agent's Deploy task
    runs what the GLPI server sends it, so the token would serve no connection and would reach
    whatever that task starts.

**Out of scope:** a GLPI health probe against the agent's embedded web interface (port 62354)
instead of process aliveness; a Linux arm64 GLPI package (no AppImage exists for it; a source
distribution plus a relocatable Perl is the only visible route).

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
  port 62354 and a Deploy task that runs what its server sends, would run on glibc 2.27, from
  Ubuntu 18.04, on every host, and no security fix of the host's C library would reach it; fixes
  would come only with a new upstream AppImage. It also edits two of upstream's files, an edit
  each repack has to repeat and each upstream change can break silently, and it has to walk
  directories of the download before the pack's own link checks run.

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
  (~30–70 MB per platform, 259 MB unpacked on Linux), and dereferencing duplicates shared
  libraries.
- Negative: an agent packed differently does not fit; the answer is the packing tool, not a key.
  An extra flag needs a code change or the agent's own configuration.
- Negative: one more plugin to keep, with a document and two tests. The disabled native autostart
  is an operator duty the fleet cannot verify.
- Negative: the agent runs as the account that starts it: the Client's, or the process account
  where one is registered, through the second service
  ([ADR-0037](0037-the-accounts-the-service-runs-under.md) clauses 16 and 18). Its inventory reads
  the DMI tables and the hardware, and its Deploy task installs packages, which need root on Linux
  and an administrator on Windows. On Linux, where the packages run the Client under its own
  account, a host that needs the whole agent registers with `service install --run-as root` and
  no process account (ADR-0037 clause 1). On Windows a first registration runs the Client as the
  virtual account `NT SERVICE\opamp-fleet`, which the install grants no group and no privilege
  (ADR-0037 clauses 1 and 2): the agent inventories less and deploys nothing that needs an
  administrator, and a host that needs the whole agent registers with
  `service install --run-as LocalSystem` and no process account. A re-registration keeps the
  account the SCM holds, so a host moves only when the operator names the account. A process
  account is never root or `LocalSystem` (ADR-0037 clause 16), so an agent under one inventories
  less and deploys nothing that needs either.
- Negative: the Linux repack needs `unsquashfs` 4.5.1 or later on the packing host. A
  distribution that backports the fixes into an older version is refused all the same, since the
  tool reads the version and not the patches.
- Follow-ups: the health probe and Linux arm64 named under Out of scope.

## Enforcement

- `crates/fleet-agent/src/supervisor/glpi.rs`: `the_block_has_no_settings`,
  `the_recipes_keys_are_refused_by_name` (clause 1, `env` included),
  `the_defaults_are_the_artifacts` (the client side of clause 11),
  `the_invocation_carries_what_supervision_requires` (both flags, `--conf-file`, `--vardir` beside
  `program/`), `the_agent_logs_to_a_file_in_a_directory_made_for_it` (clause 3's logging),
  `the_agent_reads_the_configuration_by_its_contracted_name` (the client side of clause 10),
  `linux_runs_the_apprun_entry_point`, `windows_runs_the_bundled_perl`,
  `the_version_is_asked_of_the_agent_the_daemon_runs`,
  `the_version_banner_reads_as_the_package_version` and
  `a_banner_that_is_not_a_release_reports_no_version` (clause 4),
  `the_agent_is_handed_no_endpoint_token` (clause 12).
- `crates/fleet-agent/tests/glpi_supervisor.rs`:
  `a_two_line_glpi_block_runs_reports_and_applies` — the two-line block runs, reports its
  version, receives a Configuration, and finds its directories made; the stub answers as the
  program it replaces, so the version is the agent's only if the probe starts the agent.
- `crates/fleet-tools/src/fetch.rs`: `glpi_finds_both_zip_spellings_and_repacks_only_linux` (the
  packing side of clause 11, the checksum file of clause 6, and the Package version of clauses 6
  and 7),
  `an_appimages_file_system_is_found_where_its_program_ends`,
  `the_repack_runs_nothing_it_downloaded`, `a_repack_without_unsquashfs_names_it`,
  `an_extracted_tree_is_packed_without_its_desktop_icon`,
  `an_extracted_tree_without_apprun_is_refused` and
  `a_repack_extracts_with_unsquashfs_and_packs_the_tree` (clause 6),
  `an_extracted_tree_keeps_upstreams_runtimes_as_built` (clause 6's runtimes and the one kept
  link, installed by the Client), `the_repack_follows_no_link_of_the_download` (clause 6's
  pre-pack edits), and `the_defaults_are_the_artifacts` in `glpi.rs` (the kept link on the
  client side),
  `unsquashfs_older_than_4_5_1_is_refused` and `the_unsquashfs_version_is_read_from_its_banner`
  (clause 6's minimum version),
  `the_glpi_configuration_is_the_file_the_kind_points_its_agent_at` (the packing side of
  clause 10),
  `a_tag_is_a_version_only_when_it_is_this_agents_release` (clause 7's tags),
  `a_later_glpi_repack_is_numbered_as_the_core_numbers_one` (clause 7's later repack),
  `a_published_checksum_is_read_from_either_shape`,
  `every_agent_carries_a_storable_default_configuration`,
  `a_default_configuration_the_server_already_holds_is_not_written_over`.

**Not mechanically decidable:** the native-autostart hand-over (clause 8) happens on a host outside
the fleet's view, and whether upstream keeps the bundled Perl's library roots is a run-time
property no repository test can see; the artifact document names these rows as warnings. The
tests of clause 6 run a stand-in for `unsquashfs`; that the real one extracts a real AppImage as
the stand-in does is shown by a repack in the Dev Container, not by a test, and so is that
`AppRun` then starts the agent, on the default runtime on a host newer than glibc 2.27.
