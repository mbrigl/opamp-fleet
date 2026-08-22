# Artifact: the GLPI Agent

[← User Manual](../manual/README.md) · [GLPI Agent recipe](../manual/glpi-agent.md) ·
[The Client](../manual/client.md) · [Command-line tools](../manual/tools.md)

**Who reads this.** Whoever changes how the GLPI Agent is packed, and whoever changes what the
`glpi` kind knows. It is the one place both sides state the same facts
([ADR-0091](../adr/0091-a-kind-knows-its-own-agent.md) clause 9,
[ADR-0093](../adr/0093-the-glpi-agent-gets-a-kind-of-its-own.md)).

This is the artifact where the document earns most, because it is the one this project **repacks**
rather than ships as published: on Linux the tree's internal layout is ours, so an upstream that
moves something inside it is a change both sides have to answer
([ADR-0064](../adr/0064-self-contained-glpi-agent-packages-for-both-platforms.md)).

| | |
|---|---|
| Packed by | `glpi_plans` in `crates/package-tools/src/bin/opamp-package-fetch.rs` |
| Run by | `crates/client/src/supervisor/glpi.rs` |
| Agent type | `glpi-agent` |

## 1. Source

The GitHub releases of **`glpi-project/glpi-agent`**. Its tags carry **no `v`** and have two or
three numeric parts — `1.19`, `1.7.1` — which is what separates a release from `1.0-beta1`.

## 2. Assets per platform

| This fleet | Asset |
|---|---|
| `windows/amd64` | `glpi-agent-<version>-x64.zip` |
| `linux/amd64` | `glpi-agent-<version>-x86_64.AppImage` |

The zip's name **changed case at 1.9** (`glpi-agent-1.8-x64.zip` → `GLPI-Agent-1.9-x64.zip`), so it
is matched case-insensitively rather than by a spelling that is right for only some releases.

No other platform is packed. Upstream publishes more, but they are installers rather than
artifacts a Supervisor can install.

## 3. Integrity

One combined file per release, `glpi-agent-<version>.sha256`, in `sha256sum` style — the artifact's
line is looked up by name.

## 4. Treatment

| | |
|---|---|
| Windows | **as published** — upstream's zip is already a self-contained tree |
| Linux | **repacked** from the AppImage: extracted, then packed as a tree under the wrapper directory `glpi-agent-<version>` |

The Linux repack exists because an AppImage is not a tree a Supervisor can unpack and run: it is a
self-mounting image. Two things are removed on the way out, because neither survives a package
round-trip — `.DirIcon`, and the dangling links an AppImage build leaves pointing at files no tree
carries. The extraction is refused outright if `AppRun` is not there, since without it there is
nothing to start.

One subtlety the repack has a regression test for: a directory reached **through a link** is packed
under the linked name too, with its contents. GLPI's AppImage reaches its Perl library through
`usr/share/perl/5.26`, a link to `5.26.1`, and packing that name as an empty directory produced a
tree whose agent could not find its own modules.

## 5. Form in the delivered tree

A **tree package** ([ADR-0023](../adr/0023-multi-file-packages.md)), unpacked to
`<supervisor_dir>/program/tree/`, and the two platforms differ in nearly everything below it:

| | Linux | Windows |
|---|---|---|
| program | `AppRun` | `glpi-agent.exe` |
| `program_path` | `AppRun` | `perl/bin/glpi-agent.exe` |
| what is beside it | the AppImage's whole `usr/` | upstream's bundled Perl, `perl/` |
| the program's directory | the tree root | `perl/bin/` |

`AppRun` is the AppImage's entry point and bundles **several** programs, which is why the Linux
invocation has to pick one.

**Never `glpi-agent.bat`.** The batch file upstream ships on Windows is a wrapper: the supervised
child would be `cmd.exe`, which exits immediately while the agent runs on unsupervised.

## 6. What the Client derives

| | Linux | Windows |
|---|---|---|
| program / `program_path` | `AppRun` / `AppRun` | `glpi-agent.exe` / `perl/bin/glpi-agent.exe` |
| `service_name` | `glpi-agent` | `glpi-agent` |
| working directory | — (the general rule already lands on the tree root) | the tree root, named by the kind |
| first arguments | `--script=glpi-agent` | `-I<tree>/perl/agent`, `-I<tree>/perl/site/lib`, `-I<tree>/perl/vendor/lib`, `-I<tree>/perl/lib`, then `<tree>/perl/bin/glpi-agent` |
| common arguments | `--daemon --no-fork --conf-file=<config_dir>/glpi-agent-conf --vardir=<supervisor_dir>/agent-state --logger=file --logfile=<supervisor_dir>/glpi-agent.log --logfile-maxsize=16` | the same |
| version | `--version` | the same |
| preflight | `--version` against the staged program | the same |
| reload | none — applies by restarting | the same |
| `endpoint_port` | refused — the GLPI Agent speaks no OpAMP to us | refused |

Four of those are not preferences:

- **`--daemon`.** Without it the agent runs its tasks once and exits, and the watchdog restarts it
  for ever.
- **`--no-fork`.** Without it the agent detaches, leaving the Supervisor holding a pid that ends
  immediately while the real process runs on unsupervised.
- **`--vardir` outside `program/`.** A package swap replaces that directory whole
  ([ADR-0023](../adr/0023-multi-file-packages.md)); state kept inside it would be thrown away with
  every update, taking the inventory history along. The agent does not create it and exits if it is
  missing, so the kind names it among the directories the spawn guarantees — an installation that
  failed for want of a directory would be this Client's failure, not the host's.
- **The file logging.** A daemon with no console has nowhere else to write.

The **working directory** is the one place this kind overrides ADR-0091's general rule (start in
the directory the program lives in). On Linux the program *is* the tree root's `AppRun`, so the
rule is already right. On Windows the program sits at `perl/bin/`, and the bundled Perl expects the
tree root — which is exactly what upstream's own portable `.bat` launcher sets before invoking the
agent.

## 7. Configurations

| Name | Aimed at | Body |
|---|---|---|
| `glpi-agent-conf` | every Agent of type `glpi-agent` | `config/examples/glpi-agent-conf.cfg` |

Uploaded beside the package when the Server has none of that name yet. The name is what
`--conf-file` points at, so it is not decoration: renamed on one side only, the agent starts
against a file that does not exist.

## 8. What can change, and what goes red

| An upstream that… | breaks | caught by |
|---|---|---|
| renames or re-cases its zip | the Windows plan (silently: no plan is produced) | `glpi_finds_both_zip_spellings_and_repacks_only_linux` (packing side) |
| renames its AppImage or its `.sha256` | the Linux plan, or the checksum | the same test |
| moves the program inside the Windows zip | the spawn | `windows_runs_the_bundled_perl` (client side) |
| changes the AppImage's entry point | the spawn on Linux | `linux_runs_the_apprun_entry_point` (client side) |
| moves the bundled Perl's library roots | the agent, at run time — it starts and finds no modules | `windows_runs_the_bundled_perl` pins the paths this repository writes; it cannot know upstream moved them |
| publishes a self-contained Linux archive | nothing — but the repack and half of this document could then go away | — |

The fifth row is the one to read before a version bump: the four `-I` paths are the only derived
values here that a program can fail on **after** starting successfully.
