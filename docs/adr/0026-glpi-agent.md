# ADR-0026: The GLPI Agent gets a kind of its own, and its packages are upstream's portable zip as published and its AppImage repacked as a self-contained tree

- **Status:** 🟡 proposed
- **Date:** 2026-10-10
- **Deciders:** Markus Brigl
- **Applies to:** crates/fleet-agent/src/supervisor/glpi.rs, the `glpi` feature of crates/fleet-agent/Cargo.toml, glpi_plans and repack_appimage in opamp-fleetctl package fetch, config/examples/glpi-agent-conf.cfg, docs/artifacts/glpi-agent.md, docs/manual/glpi-agent.md

## Context

The fleet manages the [GLPI Agent](https://glpi-agent.readthedocs.io/) — the inventory agent of the
GLPI asset-management suite — beyond the Collector. It should be an ordinary Managed Process: a
package the Client installs into its own Supervisor directory, updates with a health gate and rolls
back, a Configuration rolled out from the Server, a restart issued from it. A kind knows its own
agent ([ADR-0016](0016-supervisor-mode-kinds-directories-and-what-the-server-may-change.md)): what
follows from the artifact and the platform is compiled in, and a block holds only what an operator
decides.

The GLPI Agent is a Perl application, not a single binary:

- `--daemon --no-fork` runs it as a foreground daemon on every platform (the launcher instantiates
  the same platform-neutral `GLPI::Agent::Daemon` class whether or not it has a console). Without
  `--daemon` it runs its tasks once and exits; without `--no-fork` it detaches.
- Its configuration is a file named by `--conf-file`. It has no reload signal.
- It never creates a missing `--vardir`; it exits at startup.
- `--version` prints `GLPI Agent (X.Y[.Z])`, and most releases carry a two-component version
  (`1.15`, `1.19-1`), which a strict SemVer read does not accept.
- On Windows the wrapper `glpi-agent.bat` would make `cmd.exe` the supervised child: the bounded
  stop would kill the wrapper and orphan the agent, and pid-based telemetry would sample the wrong
  process.

What upstream publishes (surveyed on release 1.19): for **Windows** a self-contained portable zip —
bundled Strawberry Perl at `perl/bin/glpi-agent.exe`, `etc/`, `var/`, relative `.bat` wrappers;
5 259 files, ~101 MB. For **Linux** no archive: the `tar.gz` is the source distribution riding the
system Perl, `.deb`/`.rpm` ride it too, the snap needs `snapd`. The one self-contained Linux build is
the x86_64 **AppImage**, which as published needs `libfuse2` or re-extracts ~45 MB on every start.
Its extracted tree is relocatable by design (`AppRun.env` resolves everything from `$ORIGIN`, and a
bundled glibc-2.27 compatibility runtime serves older and newer hosts); it holds 219 symlinks, 38 of
them dangling. Dereferenced and link-free it still runs (8 284 members, 259 MB — inside the tree
limits of [ADR-0024](0024-packages-signed-deployments-offered-downloads-and-verified-delivery.md)).
Five links are directories, and one is load-bearing: `usr/share/perl/5.26` → `5.26.1` is the name
the bundled `PERL5LIB` uses.

The two platform invocations differ in nearly everything — program, its place in the tree, working
directory, and on Windows four Perl `-I` paths and the script by path — and none of those
differences is anybody's decision.

## Decision

We will supervise the GLPI Agent with a `glpi` kind whose block is `type` and `name` alone, and
deliver it as packages built from upstream's own artifacts — the Windows portable zip as published,
the Linux AppImage repacked deterministically into a link-free tree.

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
   `--daemon --no-fork --conf-file=${config_dir}/glpi-agent-conf --vardir=${supervisor_dir}/agent-state
   --logger=file --logfile=${supervisor_dir}/glpi-agent.log --logfile-maxsize=16`.
   - `--daemon` and `--no-fork` are supervision requirements: without the first the watchdog
     restarts a run-once agent for ever; without the second the Supervisor holds a pid that ends
     while the real process runs unsupervised.
   - The state directory lies **beside** `program/`, never inside it, so `deviceid` and caches
     survive package swaps and rollbacks; the kind names it among the directories made before every
     spawn, because the agent exits when it is missing.
   - File logging exists because a daemon with no console has nowhere else to write.

4. **GLPI's identity, configuration and version.** `service_name = "glpi-agent"` is the Agent type
   every GLPI Configuration and Package is for. A Configuration named `glpi-agent-conf` lands where
   `--conf-file` reads it and applies by restart — there is no reload signal. The version probe and
   the preflight are `--version`, read as strict SemVer: three-component releases report a
   `service.version`, two-component ones report none. The gap is accepted; the Package version is
   what tracks installs. Until the first Configuration arrives the agent refuses to start and the
   Supervisor holds after its crash budget; the first apply ends the hold.

5. **The Windows package is upstream's portable zip, byte for byte**, opened as
   [ADR-0024](0024-packages-signed-deployments-offered-downloads-and-verified-delivery.md) clause 66
   decides. Taken as published, the hash an Agent verifies is upstream's own, and the artifact may
   even be a referenced package pointing at the release asset.

6. **The Linux package is the AppImage, repacked by `opamp-fleetctl package fetch`.** The tool
   verifies the AppImage against the release's `glpi-agent-<version>.sha256` (looked up by name),
   extracts it, refuses if `AppRun` is missing, drops `.DirIcon` and the dangling links, and packs
   the result in the deterministic, link-free repack of
   [ADR-0024](0024-packages-signed-deployments-offered-downloads-and-verified-delivery.md) clause 67
   — every other link dereferenced, a linked directory packed under the linked name too, with its
   contents — as `.tar.gz` with file modes under the wrapper directory `glpi-agent-<version>`, as
   `glpi-agent_<version>_linux_amd64.tar.gz`. The repack runs on a Linux x86_64 host such as the Dev
   Container. From there the fleet's own hash and Ed25519 signature are the chain of trust.

7. **GLPI ships for `windows/amd64` and `linux/amd64` only.** The release's zip is matched
   case-insensitively (its spelling changed case at 1.9); tags carry no `v` and have two or three
   numeric parts. Both artifacts are entries of one Package of Agent type `glpi-agent` at the
   upstream version. Hosts on other platforms get no GLPI package.

8. **One GLPI Agent per host, and it is the fleet's.** Where a native installation exists, its
   autostart is switched off first — `systemctl disable --now glpi-agent` on Linux, `EXECMODE=3` or a
   disabled `glpi-agent` service on Windows — because a foreground daemon skips the agent's PID-file
   single-instance check and two agents would inventory the host twice. This hand-over is the
   operator's step, documented in the manual.

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
  packing host invented instead of upstream's, and the referenced-package route would close.
- **Assemble a Linux runtime ourselves** (relocatable Perl plus `cpanm`, staticperl, PAR::Packer).
  Rejected: a compile step per architecture and a dependency list that moves with every release,
  where the AppImage is exactly that assembly, built and tested upstream.
- **The snap.** Rejected: requires `snapd`, a second manager beside the fleet.
- **Loosen the tree rules** (links, member cap) to pack the AppImage tree as-is. Rejected: the
  dereferenced tree fits, and the rules protect every package on every host.

## Sources / Prior art

- [GLPI Agent man page](https://glpi-agent.readthedocs.io/en/latest/man/glpi-agent.html) —
  `--daemon`, `--no-fork`, `--conf-file`, `--vardir`, logger options.
- [GLPI Agent usage](https://glpi-agent.readthedocs.io/en/latest/usage.html) — managed mode and the
  embedded web interface on port 62354.
- [Windows installer reference](https://glpi-agent.readthedocs.io/en/latest/installation/windows-command-line.html)
  — `EXECMODE` 1/2/3 (service / task / manual).
- [`bin/glpi-agent`](https://github.com/glpi-project/glpi-agent/blob/develop/bin/glpi-agent) and
  [Windows packaging](https://github.com/glpi-project/glpi-agent/blob/develop/contrib/windows/glpi-agent-packaging.pl)
  — the platform-neutral daemon class, the version string, the four `-I` paths, the `.bat` wrapper.
- [GLPI Agent release 1.19](https://github.com/glpi-project/glpi-agent/releases/tag/1.19) — the
  surveyed artifact set; [portable discussion #273](https://github.com/glpi-project/glpi-agent/discussions/273)
  — the Windows zip as the supported portable form.
- [`make-linux-appimage.sh`](https://github.com/glpi-project/glpi-agent/blob/develop/contrib/unix/make-linux-appimage.sh),
  [`glpi-agent-appimage-hook`](https://github.com/glpi-project/glpi-agent/blob/develop/contrib/unix/glpi-agent-appimage-hook),
  [appimage-builder](https://appimage-builder.readthedocs.io/) — how the AppImage is built, how
  `AppRun` dispatches (`--script`, `GLPIAGENT_SCRIPT`), and the `$ORIGIN`/bundled-libc mechanism.
- Verified against GLPI Agent 1.15 and 1.19 in the Dev Container: foreground daemon, clean exit on
  `SIGTERM`, hard failure on a missing `--conf-file` or `--vardir`, the relocated and link-free
  AppImage tree running without FUSE or root.

## Consequences

- Positive: one block for both platforms and for a mixed fleet; no host carries Perl, FUSE, snapd or
  a preinstalled agent.
- Positive: the Windows artifact travels exactly as upstream published it.
- Positive: the bundled glibc-compat runtime lets one Linux artifact serve old and new
  distributions.
- Negative: we own the Linux repack — one tool run and an upload per release; the artifact does not
  match upstream's hash, so upstream's `.sha256` is checked at packing time. Artifacts are large
  (~30–70 MB per platform, 259 MB unpacked on Linux), and dereferencing duplicates shared
  libraries.
- Negative: an agent packed differently does not fit; the answer is the packing tool, not a key.
  An extra flag needs a code change or the agent's own configuration.
- Negative: one more plugin to keep, with a document and two tests. The disabled native autostart is
  an operator duty the fleet cannot verify.
- Follow-ups: the health probe and Linux arm64 named under Out of scope.

## Enforcement

- `crates/fleet-agent/src/supervisor/glpi.rs`: `the_block_has_no_settings`,
  `the_recipes_keys_are_refused_by_name`, `the_defaults_are_the_artifacts` (the client side of
  clause 11), `the_invocation_carries_what_supervision_requires` (both flags, `--conf-file`,
  `--vardir` beside `program/`), `linux_runs_the_apprun_entry_point`,
  `windows_runs_the_bundled_perl`.
- `crates/fleet-agent/tests/wrapped_supervisors.rs`: `a_two_line_glpi_block_runs_reports_and_applies`
  — the two-line block runs, reports its version, receives a Configuration, and finds its
  directories made.
- `crates/fleet-tools/src/fetch.rs`: `glpi_finds_both_zip_spellings_and_repacks_only_linux` (the
  packing side of clause 11), `a_published_checksum_is_read_from_either_shape`,
  `every_agent_carries_a_storable_default_configuration`,
  `a_default_configuration_the_server_already_holds_is_not_written_over`.

**Not mechanically decidable:** the native-autostart hand-over (clause 8) happens on a host outside
the fleet's view, and whether upstream keeps the bundled Perl's library roots is a run-time
property no repository test can see; the artifact document names these rows as warnings.
