# Artifact: the GLPI Agent

[← User Manual](../manual/README.md) · [GLPI Agent recipe](../manual/glpi-agent.md) ·
[The Client](../manual/client.md) · [Command-line tools](../manual/tools.md)

**Who reads this.** Whoever changes how the GLPI Agent is packed, and whoever changes what the
`glpi` kind knows. It is the one place both sides state the same facts
([CLT-supervisor-mode-and-kinds §20](../adr/CLT-supervisor-mode-and-kinds.md),
[CLT-glpi-agent-kind](../adr/CLT-glpi-agent-kind.md)).

This is the artifact where the document earns most, because it is the one this project **repacks**
rather than ships as published: on Linux the tree's internal layout is ours, so an upstream that
moves something inside it is a change both sides have to answer
([CLT-glpi-agent-kind](../adr/CLT-glpi-agent-kind.md)).

| | |
|---|---|
| Packed by | `glpi_plans` and `repack_appimage` in `crates/fleet-tools/src/fetch.rs`, the AppImage's headers read by `squashfs_offset` and `squashfs_inodes` in `crates/fleet-tools/src/fetch/upstream.rs` |
| Run by | `crates/fleet-agent/src/supervisor/glpi.rs` |
| Agent type | `glpi-agent` |

## 1. Source

The GitHub releases of **`glpi-project/glpi-agent`**. Its tags carry **no `v`** and have two or
three numeric parts — `1.19`, `1.7.1` — which is what separates a release from `1.0-beta1`.

A release is fetched under its tag and uploaded as the Package version written as SemVer
([PKG-versions-high-water-mark-floor §6](../adr/PKG-versions-high-water-mark-floor.md)):
tag `1.19` is Package `1.19.0`, tag `1.7.1` is Package `1.7.1`. A later repack of one
release is numbered by `--repack <n>` as every repack is
([OPS-operator-program-fleetctl §28](../adr/OPS-operator-program-fleetctl.md)):
`--repack 2` of `1.19` is Package `1.19.1-0.r.2`. Below, `<tag>` is upstream's
spelling and `<version>` the Package version.

## 2. Assets per platform

| This fleet | Asset |
|---|---|
| `windows/amd64` | `glpi-agent-<tag>-x64.zip` |
| `linux/amd64` | `glpi-agent-<tag>-x86_64.AppImage` |

The zip's name **changed case at 1.9** (`glpi-agent-1.8-x64.zip` → `GLPI-Agent-1.9-x64.zip`), so it
is matched case-insensitively rather than by a spelling that is right for only some releases.

No other platform is packed. Upstream publishes more, but they are installers rather than
artifacts a Supervisor can install.

## 3. Integrity

One combined file per release, `glpi-agent-<tag>.sha256`, in `sha256sum` style — the artifact's
line is looked up by name. The 1.19 file holds a line for both assets this fleet takes. It is read
within 64 KiB, and each asset within 1 GiB, as every download of `package fetch` is.

**What this check is worth.** Upstream publishes no OpenPGP signature, no Sigstore bundle and no
other signature for these assets; none is among the 1.19 assets. The `.sha256` comes from the same
release, so it detects a download damaged on the way, such as one cut short, not an asset
changed on the release or on the way: whoever replaces an asset can replace its line too. From
the repack on, the fleet's own hash and Ed25519 signature carry the chain of trust
([CLT-glpi-agent-kind §5 and §6](../adr/CLT-glpi-agent-kind.md)).

## 4. Treatment

| | |
|---|---|
| Windows | **as published** — upstream's zip is already a self-contained tree, under its own name |
| Linux | **repacked** from the AppImage: its file system extracted without running it, then packed as a tree with normalized file modes under the wrapper directory `glpi-agent-<version>`, as `glpi-agent_<version>_linux_amd64.tar.gz` |

The Linux repack exists because an AppImage is not a tree a Supervisor can unpack and run: it is a
self-mounting image, an ELF program with a squashfs file system appended. **The repack never runs
it.** The file system starts where the ELF program ends, which the ELF header says (the end of its
section header table), and `unsquashfs` of squashfs-tools reads it from that offset as data. The
file is refused unless it is a 64-bit ELF with the AppImage type-2 mark and a squashfs superblock at
that offset. The superblock's inode count is read before anything is extracted: above the 10 000
members a tree may carry
([PKG-signed-packages-and-delivery §57](../adr/PKG-signed-packages-and-delivery.md)), the repack
refuses. The bytes `unsquashfs` writes
are not bounded before it runs, since the superblock states no unpacked size; the pack and the
Client's install bound the tree afterwards. `unsquashfs` must be 4.5.1 or later, which fixes
CVE-2021-40153 and CVE-2021-41072; the tool reads its version from `unsquashfs -version` before it
extracts, and a host without it, or with an older one, is told what the repack needs. The extracted
tree loses `.DirIcon`, a link to the icon no agent reads, and is refused outright if `AppRun` is not
there as a file of its own, since without it there is nothing to start. Neither step follows a link:
each path is asked what it is itself, and `.DirIcon` is removed as the link it is. Both of
upstream's runtimes travel as built, and `AppRun.env` unchanged, so `AppRun` chooses as upstream
ships it: `runtime/default`, the host's own C library, where the host's glibc is newer than the 2.27
that `AppRun.env` states, and `runtime/compat`, the glibc 2.27 the AppImage bundles, otherwise.
`runtime/default` reaches the host's C library through one absolute link,
`runtime/default/lib64/ld-linux-x86-64.so.2` → `/lib64/ld-linux-x86-64.so.2`. The kind names that
link, by that path and that target, as the one its tree keeps, and the tool states the same link:
the repack packs it as a link and the Client creates it, and every other link leading out still
fails the repack. `the_glpi_tree_keeps_the_link_the_kind_names` (packing side) and
`the_defaults_are_the_artifacts` (client side) hold both sides to it. The compat runtime's
`libcrypt.so.1` names a file that runtime does not hold and is dropped like every such link. The
rest is the core's repack
([OPS-operator-program-fleetctl §27](../adr/OPS-operator-program-fleetctl.md)):
links inside the tree dereferenced, a link that names nothing dropped, and every file stored `0o755`
if any execute bit is set on it and `0o644` otherwise.

One subtlety the repack has a regression test for: a directory reached **through a link** is packed
under the linked name too, with its contents. GLPI's AppImage reaches its Perl library through
`usr/share/perl/5.26`, a link to `5.26.1`, and packing that name as an empty directory produced a
tree whose agent could not find its own modules.

## 5. Form in the delivered tree

A **tree package** ([PKG-signed-packages-and-delivery](../adr/PKG-signed-packages-and-delivery.md)),
unpacked to `<supervisor_dir>/program/tree/`, and the two platforms differ in nearly everything
below it:

| | Linux | Windows |
|---|---|---|
| program | `AppRun` | `glpi-agent.exe` |
| `program_path` | `AppRun` | `perl/bin/glpi-agent.exe` |
| what is beside it | the AppImage's whole `usr/` | upstream's bundled Perl, `perl/` |
| the program's directory | the tree root | `perl/bin/` |

`AppRun` is the AppImage's entry point and bundles **several** programs, which is why the Linux
invocation has to pick one.

**Never `glpi-agent.bat`.** The batch file upstream ships on Windows is a wrapper: the supervised
child would be `cmd.exe` and the agent its grandchild, so a stop would kill the wrapper and orphan
the agent.

## 6. What the Client derives

| | Linux | Windows |
|---|---|---|
| program / `program_path` | `AppRun` / `AppRun` | `glpi-agent.exe` / `perl/bin/glpi-agent.exe` |
| `service_name` | `glpi-agent` | `glpi-agent` |
| working directory | — (the general rule already lands on the tree root) | the tree root, named by the kind |
| first arguments | `--script=glpi-agent` | `-I<tree>/perl/agent`, `-I<tree>/perl/site/lib`, `-I<tree>/perl/vendor/lib`, `-I<tree>/perl/lib`, then `<tree>/perl/bin/glpi-agent` |
| common arguments | `--daemon --no-fork --no-task=deploy --conf-file=<config_dir>/glpi-agent-conf --vardir=<supervisor_dir>/agent-state --logger=file --logfile=<supervisor_dir>/agent-log/glpi-agent.log --logfile-maxsize=16` | the same |
| version | the first arguments, then `--version`, in the working directory above; the number of a first line that is exactly `GLPI Agent (…)`, if it has a release tag's two or three numeric parts and at most a package revision `-<digits>`, which is dropped, written as SemVer, `1.19-1` → `1.19.0` — any other banner reports none | the same |
| preflight | the version probe, asked of the staged tree: its first arguments and working directory name the staged tree's root | the same |
| reload | none — applies by restarting | the same |
| what it reports | `service.version` alone, from the version probe | the same |
| `endpoint_port` | refused — the GLPI Agent speaks no OpAMP to us | refused |
| `OPAMP_SUPERVISOR_TOKEN` | not set — nothing the agent runs needs the Endpoint's token | not set |
| process group | its own — the daemon forks its tasks, and a stop signals the group | a job, where the second service starts it; the pid alone otherwise |

Five of those are not preferences:

- **`--daemon`.** Without it the agent runs its tasks once and exits, and the watchdog restarts it
  for ever.
- **`--no-fork`.** Without it the agent detaches, leaving the Supervisor holding a pid that ends
  immediately while the real process runs on unsupervised.
- **`--vardir` outside `program/`.** A package swap replaces that directory whole
  ([PKG-signed-packages-and-delivery](../adr/PKG-signed-packages-and-delivery.md)); state kept
  inside it would be thrown away with every update, taking the inventory history along. The agent
  does not create it and exits if it is missing, so the kind names it among the directories the
  spawn guarantees — an installation that failed for want of a directory would be this Client's
  failure, not the host's.
- **The file logging.** A daemon with no console has nowhere else to write.
- **`--no-task=deploy`.** The Deploy task runs the installers a GLPI server sends, as the account
  the agent runs under. The agent reads its command line over its configuration file, so a
  delivered `glpi-agent-conf` whose `server =` names another server cannot turn the task on.

The version probe and the preflight start the agent **exactly as the daemon is started**, first
arguments and working directory included. The program alone is not the agent on either platform:
on Windows `glpi-agent.exe` is Perl, which answers a bare `--version` with its own banner
(`v5.38.2`), and on Linux `AppRun` without `--script` is the AppImage's installer, which answers
with a version line of its own. Read off either, the reported `service.version` would be wrong, and
a preflight would prove only that the interpreter starts.

The **working directory** is the one place this kind overrides CLT-supervisor-mode-and-kinds's
general rule (start in the directory the program lives in). On Linux the program *is* the tree
root's `AppRun`, so the rule is already right. On Windows the program sits at `perl/bin/`, and the
bundled Perl expects the tree root — which is exactly what upstream's own portable `.bat` launcher
sets before invoking the agent.

## 7. Configurations

| Name | Aimed at | Body |
|---|---|---|
| `glpi-agent-conf` | every Agent of type `glpi-agent` | `config/examples/glpi-agent-conf.cfg` |

Uploaded beside the package when the Server has none of that name yet. The name is what
`--conf-file` points at, so it is not decoration: renamed on one side only, the agent starts
against a file that does not exist.
`the_glpi_configuration_is_the_file_the_kind_points_its_agent_at` (packing side) and
`the_agent_reads_the_configuration_by_its_contracted_name` (client side) hold both sides to the
one name.

## 8. What can change, and what goes red

| An upstream that… | breaks | caught by |
|---|---|---|
| renames or re-cases its zip | the Windows plan (silently: no plan is produced) | `glpi_finds_both_zip_spellings_and_repacks_only_linux` (packing side) |
| renames its AppImage or its `.sha256` | the Linux plan, or the checksum of both | the same test, which asserts each plan's checksum file by name |
| moves the program inside the Windows zip | the spawn | `windows_runs_the_bundled_perl` (client side) |
| makes the program answer `--version` differently from the agent | the version probe and the preflight | `the_version_is_asked_of_the_agent_the_daemon_runs` pins that both start the agent as the daemon is; `a_two_line_glpi_block_runs_reports_and_applies` runs a stub that answers as the program it replaces does |
| changes the AppImage's entry point | the spawn on Linux | `linux_runs_the_apprun_entry_point` (client side) |
| moves the bundled Perl's library roots | the agent, at run time — it starts and finds no modules | `windows_runs_the_bundled_perl` pins the paths this repository writes; it cannot know upstream moved them |
| changes its `--version` banner | the reported `service.version`, which then goes absent; the Package version still tracks the install | `the_version_banner_reads_as_the_package_version` pins the banner this repository reads, `a_banner_that_is_not_a_release_reports_no_version` what it refuses |
| grows its AppImage past 10 000 inodes | the repack, which refuses before it extracts; 1.19 packs about 7 100 members once its links are dereferenced; its inode count was not read here | `a_file_system_of_more_inodes_than_a_tree_may_carry_is_not_extracted` (packing side) |
| re-enables Collect's `runCommand`, disabled in every release from 1.0 to 1.21 | nothing goes red: the GLPI server a Configuration names could then run commands on the host, as the agent's account | no test: the kind cannot see it; `remote_config_disabled` is the answer where Q-5 must hold |
| renames the Deploy task or drops `--no-task` | the agent's start, if it refuses the flag, or the hardening, if the task runs under another name | `the_deploy_task_is_off_and_the_agent_is_stopped_as_a_group` pins the flag this repository writes; it cannot know upstream renamed the task |
| moves `AppRun` out of the tree root | the repack, which refuses the tree | `an_extracted_tree_without_apprun_is_refused` (packing side) |
| moves the default runtime's loader link, or adds another absolute link | the repack, which fails naming the link | `an_extracted_tree_keeps_upstreams_runtimes_as_built`, `the_glpi_tree_keeps_the_link_the_kind_names` and `the_repack_follows_no_link_of_the_download` (packing side), `the_defaults_are_the_artifacts` (client side); a link added and needed only on some hosts shows only when the repacked tree is run there |
| fixes or moves the compat runtime's dangling `libcrypt.so.1` | nothing on a host newer than glibc 2.27; on an older host the agent takes the host's `libcrypt.so.1` or fails to start | no test: a host with glibc 2.27 or older was not run |
| publishes a self-contained Linux archive | nothing — but the repack and half of this document could then go away | — |

The row on the library roots is the one to read before a version bump: the four `-I` paths are the
only derived values here that a program can fail on **after** starting successfully.
