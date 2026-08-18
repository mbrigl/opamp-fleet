# ADR-0070: Repacked vendor packages as relocatable Icinga 2 trees — everything but glibc rides along, and the build host sets the floor

- **Status:** 🟢 accepted
- **Date:** 2026-08-17
- **Deciders:** Markus Brigl

## Context

[ADR-0068](0068-icinga-2-is-supervised-by-a-kind-of-its-own.md) supervises a fleet-delivered Icinga 2;
this decides what is actually delivered. Icinga publishes distribution packages (`.deb`, `.rpm`) and
a Windows MSI — no AppImage, no portable directory, so the shape
[ADR-0064](0064-self-contained-glpi-agent-packages-for-both-platforms.md) could take for GLPI (*"the
Windows zip byte-for-byte as published"*) does not transfer.

What the Client's package reader fixes ([ADR-0023](0023-multi-file-packages.md)): a tree may contain
neither symlinks nor hard links — one link refuses the whole archive — at most 10 000 members and
2 GiB unpacked, and only `.tar.gz` carries file modes.

What a spike against Icinga 2.14.6-1 measured, which is what makes this decidable at all:

- The real binary carries **`RUNPATH`, not `RPATH`**. `LD_LIBRARY_PATH` is searched first, so bundled
  libraries win **without `patchelf`** — verified by `ldd` resolving into the relocated tree.
- The closure outside glibc is **20 shared objects** (five Boost libraries, OpenSSL, `libsystemd`,
  `libedit`, `libstdc++`, and their transitive dependencies). No ICU: this build needs no
  `boost_regex`.
- The whole tree — binary, libraries, the 29-file ITL, one plugin — is **39 MB in 52 files**,
  comfortably inside the limits.
- **glibc cannot be bundled**, and the effective floor came out at `GLIBC_2.39` — from a *bundled
  library*, not from the binary, which needs 2.38. A tree built on Debian trixie therefore runs
  neither on Debian 12 nor on RHEL 9.
- Debian splits the payload: `icinga2-bin` holds the binary, **`icinga2-common` holds the ITL**. One
  tree needs both packages.

## Decision

We will produce Icinga 2 artifacts by **repacking the vendor packages into a normalised, link-free
tree**, built by `opamp-package-fetch`, and we will **bundle everything except glibc**.

Bound by this decision:

- **One normalised layout per operating system**, so one `program_path` serves every distribution:

  ```
  icinga2-<version>/
    sbin/icinga2            lib/                 share/icinga2/include/
    plugins/                doc/copyright
  ```

  What is deliberately left out: `/etc/icinga2` (the fleet delivers configuration), the systemd unit
  and init script, `prepare-dirs` and `safe-reload` (they need the `nagios` user), and documentation
  beyond the copyright files.
- **`.tar.gz` on every platform**, including Windows: it is the only container that carries the
  executable bit, and the Client unpacks it everywhere (ADR-0064's rule, applied without the
  exception GLPI could take because upstream published a zip).
- **Libraries ride along, glibc does not.** The `NEEDED` closure minus glibc and the loader is copied
  into `lib/` with links dereferenced, and the Supervisor points `LD_LIBRARY_PATH` at it. Bundling a
  libc without its loader does not work, and with its loader would mean an `exec` indirection the
  program path cannot express.
- **Therefore one artifact per distribution family, built on the oldest member it must serve** — the
  glibc floor is a property of the build host, not of Icinga. The tool **measures and prints that
  floor** so coverage is known before a rollout rather than discovered host by host.
- **Two Linux Sets, distinguished by name.** A Set carries one entry per `(os, arch)`
  ([ADR-0031](0031-per-platform-package-variants.md)) and versions compare without build metadata
  ([ADR-0029](0029-a-version-is-compared-and-shown-without-its-build-metadata.md)), so a
  Debian-built and an EL-built `linux/amd64` tree cannot share one Set and cannot be told apart by a
  version suffix. They are separate Sets of the same Agent type, aimed by a Selector at an attribute
  the operator sets on the Client.
- **A plan may name several sources.** `opamp-package-fetch`'s plan grows from one URL to a list, so
  `icinga2-bin` + `icinga2-common` (+ plugins) become one artifact; every source is verified before
  anything is repacked.
- **Checksums come from the repository index.** Icinga publishes no per-file digest sidecars and
  signs its repositories with GPG instead — but the `Packages` index carries a `SHA256` field per
  file, and that is what every source is verified against before anything is repacked. It is also
  where the artifact's **reach** comes from: the same stanza's `Depends: libc6 (>= …)` is the
  vendor's own statement of the oldest glibc this build runs on, printed before the rollout rather
  than discovered host by host.
- **Extraction shells out** — `dpkg-deb`, `rpm2cpio`/`cpio`, and an MSI extractor — following the
  precedent of ADR-0064's AppImage repack, which also executes and is restricted to the platform it
  works on. A missing helper is refused by name, never worked around, and the Dev Container gains
  the ones it lacks.
- **Repacking is redistribution.** The vendor copyright files travel in the tree.

## Alternatives considered

- **Build Icinga 2 from source with a prefix of our own.** The robust answer to relocation, and the
  one ADR-0064 already weighed for GLPI: *"a compile step per architecture and a dependency list that
  moves with every release"*. Rejected while repacking demonstrably works — and it stays the fallback
  if a platform turns out not to relocate.
- **Ship the vendor `.deb`/`.rpm`/MSI and install it on the host.** Standard paths, no relocation
  problem — and an installation beside the fleet, needing root, a package manager, and a service the
  Client does not supervise. That is the outcome this whole line of work exists to avoid.
- **Bundle glibc too, and ship the loader.** Would make one Linux artifact serve every distribution.
  Rejected: the program would have to be the loader, which the block's program path cannot express,
  and a mismatched loader/libc pair is a class of failure worse than a clear refusal.
- **`patchelf` the RUNPATH instead of setting `LD_LIBRARY_PATH`.** Unnecessary — RUNPATH loses to
  `LD_LIBRARY_PATH`, measured — and it would rewrite a vendor binary, which is a change to the thing
  whose checksum was just verified.
- **`.7z` or `.zip` for Windows.** Rejected for the reason ADR-0064 gives: they carry Windows
  attributes, so the tree would arrive without executable bits.
- **One Set with a distro-suffixed version** (`2.14.6+el9`). Rejected: ADR-0029 compares without
  build metadata, so the two would compare equal and the offer would be a coin toss.

## Sources / Prior art

- Spike against Icinga 2.14.6-1 from Debian trixie (2026-08-17): `readelf -d` (RUNPATH), the `ldd`
  closure, `objdump -T` for the `GLIBC_` floor, the 39 MB / 52-file tree, and a relocated run.
- [ADR-0064](0064-self-contained-glpi-agent-packages-for-both-platforms.md) — the repack precedent:
  dereferencing links, deterministic `.tar.gz`, verifying upstream's hash at packing time, and the
  container/mode rule.
- [ADR-0018](0018-packages-imported-from-a-url.md), [ADR-0023](0023-multi-file-packages.md),
  [ADR-0031](0031-per-platform-package-variants.md), [ADR-0052](0052-a-package-is-a-versioned-set.md)
  — the containers, the tree limits, one entry per platform, and the Set model this fits into.
- [packages.icinga.com](https://packages.icinga.com/) — the vendor repositories the artifacts come
  from, and their GPG-signed rather than digest-listed shape.

## Consequences

- Positive: an Icinga 2 that installs nothing on the host, updates and rolls back like any other
  package, and carries its own libraries, ITL and check plugins.
- Positive: no build system for a C++ project with Boost and OpenSSL enters this repository.
- Negative / trade-offs: one artifact per distribution family, and the build host's glibc decides
  reach. Stated in the manual per artifact rather than discovered on a host.
- Negative / trade-offs: the tool grows extraction helpers it must shell out to, and therefore
  platform restrictions on where a repack can run.
- Negative / trade-offs: the build host must carry the vendor package's own dependencies, because
  the tree bundles what `ldd` resolves *there*. One that cannot be resolved is refused by name
  rather than packed around — a tree missing a library would otherwise ship and die on its first
  start.
- Follow-ups: the RPM family (its `repomd` index is the equivalent source of digests); the Windows MSI
  payload is unproven — if it cannot be relocated, Windows keeps a machine-installed Icinga 2 under
  the same Supervisor kind (ADR-0068).
