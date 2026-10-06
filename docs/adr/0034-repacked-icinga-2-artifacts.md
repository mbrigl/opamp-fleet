# ADR-0034: Repacked vendor packages as relocatable Icinga 2 trees — one artifact built on the oldest glibc it must serve, and the Windows artifact verified by its publisher

- **Status:** 🟢 accepted
- **Date:** 2026-08-17
- **Deciders:** Markus Brigl

## Context

[ADR-0033](0033-icinga-2-supervision-and-enrolment.md) supervises a fleet-delivered Icinga 2;
this decides what is actually delivered. Icinga publishes distribution packages (`.deb`, `.rpm`) and
a Windows MSI — no AppImage, no portable directory, so the shape
[ADR-0031](0031-the-glpi-agent.md) could take for GLPI (*"the
Windows zip byte-for-byte as published"*) does not transfer.

What the Client's package reader fixes ([ADR-0015](0015-package-delivery-for-managed-processes.md)): a tree may contain
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

**glibc is backward compatible**: a program built against an older one runs against every newer
one. So what a tree can reach is decided by one number, not by which packaging tradition its binary
came from — and the vendor states that number in every package it publishes:

| Vendor build | `Depends: libc6 (>= …)` | Runs on |
|---|---|---|
| Debian 11 (bullseye) | 2.30 | Debian 11/12/13, Ubuntu 20.04+, **RHEL 9** (2.34), RHEL 10 |
| Debian 12 (bookworm) | 2.34 | Debian 12/13, RHEL 9 — not RHEL 8 (2.28) |
| Debian 13 (trixie) | 2.38 | only very recent systems |

Everything else the daemon needs already travels with it — 21 shared objects, measured, with
`LD_LIBRARY_PATH` beating the binary's own `RUNPATH`. So an artifact built on Debian 11 runs on a
RHEL 9 host, and a split by distribution family buys nothing there.

Two further facts, both found by looking rather than by reasoning:

- **Icinga's open RPM repository ends at EL 8.** Everything from RHEL 9 on is behind
  `packages.icinga.com/subscription/`, which answers `401` without credentials. A per-family
  artifact for Red Hat cannot currently be built at all without a subscription — while a
  Debian-built one serves those same hosts.
- **OpenSSL's configuration directory is compiled in**, and the two families differ: Debian's
  `libcrypto` looks in `/usr/lib/ssl`, Red Hat's in `/etc/pki/tls`. For Icinga's cluster TLS this is
  inert — certificates are named by explicit paths (ADR-0033) — but anything reaching for the
  *system* trust store on a Red Hat host would find the Debian path.

**The Windows artifact has no index.** `packages.icinga.com/windows/` is a directory listing of
`.msi` files with **no digest sidecars at all** — `.sha256`, `.sha256sum`, `.asc` and `.md5` all
answer `404`. What the MSI does carry is an **Authenticode signature**, and it checks out where it
matters:

```
Signer: /C=DE/ST=Bayern/L=Nuernberg/O=Icinga GmbH/CN=Icinga GmbH
Issuer: GlobalSign GCC R45 CodeSigning CA 2020
Number of verified signatures: 1
Timestamp Server Signature CRL verification: ok
Error: unable to get local issuer certificate
```

The signature covers the file's contents and is timestamped; only the chain to a root fails, because
a Linux CA bundle carries web PKI roots and not the code-signing roots Windows trusts. That is a
property of the build host, not of the artifact.

A signature is **not the weaker substitute** for a digest here: a digest published beside a file on
the same server is only as trustworthy as that server, while a signature is bound to a key the server
does not hold — so an attacker who controls the mirror can rewrite both file and digest, and cannot
forge the signature. And the question generalises: any future artifact published without an index —
a macOS package, a vendor's direct download — meets it too.

## Decision

We will produce Icinga 2 artifacts by **repacking the vendor packages into a normalised, link-free
tree**, built by `opamp-package-fetch`, and we will **bundle everything except glibc**. **Nothing is
repacked before it is verified.**

### The tree

1. **One normalised layout per operating system**, so one `program_path` serves every distribution:

   ```
   icinga2-<version>/
     sbin/icinga2            lib/                 share/icinga2/include/
     plugins/                doc/copyright
   ```

   What is deliberately left out: `/etc/icinga2` (the fleet delivers configuration), the systemd unit
   and init script, `prepare-dirs` and `safe-reload` (they need the `nagios` user), and documentation
   beyond the copyright files.

2. **`.tar.gz` on every platform**, including Windows: it is the only container that carries the
   executable bit, and the Client unpacks it everywhere (ADR-0031's rule, applied without the
   exception GLPI could take because upstream published a zip).

3. **Libraries ride along, glibc does not.** The `NEEDED` closure minus glibc and the loader is copied
   into `lib/` with links dereferenced, and the Supervisor points `LD_LIBRARY_PATH` at it. Bundling a
   libc without its loader does not work, and with its loader would mean an `exec` indirection the
   program path cannot express.

### Reach

4. **One Icinga 2 artifact per platform, built on the oldest glibc it must serve.** The glibc floor
   is a property of the build host, not of Icinga, and the distribution family is not the criterion.
   **The build host is the reach.** Choosing it is a decision, and the tool states its consequence:
   it **measures and prints** the `libc6` floor from the vendor's own package before anything is
   uploaded, so coverage is known before a rollout rather than discovered host by host, and it
   refuses to build for a distribution the host is not. The documented build host for the Linux
   artifact is the Dev Container (ADR-0002).

5. **The Set is named after the Agent type**, as every other agent's is. `opamp-package-fetch` has no
   `--package-name` flag.

6. **Two Sets remain possible, without the tool knowing.** A deployment that genuinely needs a second
   artifact — a host too old for the common floor, a family whose vendor binary it must run for
   support reasons — creates that Set through the REST API under a name of its own and aims it with
   a Selector. Nothing in the Server or the Client requires the tool's help for that.

### Sources and verification

7. **A plan may name several sources.** `opamp-package-fetch`'s plan is a list of URLs, so
   `icinga2-bin` + `icinga2-common` (+ plugins) become one artifact; every source is verified before
   anything is repacked.

8. **Checksums come from the repository index.** Icinga publishes no per-file digest sidecars and
   signs its repositories with GPG instead — but the `Packages` index carries a `SHA256` field per
   file, and that is what every source is verified against before anything is repacked. It is also
   where the artifact's **reach** comes from: the same stanza's `Depends: libc6 (>= …)` is the
   vendor's own statement of the oldest glibc this build runs on, printed before the rollout rather
   than discovered host by host.

9. **Extraction shells out** — `dpkg-deb`, `rpm2cpio`/`cpio`, and an MSI extractor — following the
   precedent of ADR-0031's AppImage repack, which also executes and is restricted to the platform it
   works on. A missing helper is refused by name, never worked around, and the Dev Container carries
   the ones it needs.

10. **Repacking is redistribution.** The vendor copyright files travel in the tree.

11. **The Red Hat caveat is documented, not designed around.** The manual states that a Debian-built
    tree carries Debian's OpenSSL layout, so a check that uses the system trust store on a Red Hat
    host is the one thing to verify before relying on it.

### The Windows artifact

12. **The Windows MSI is verified by its Authenticode signature, pinned to the publisher**, and
    repacked only when that verification passes. **The check is two conditions, both required**: the
    embedded signature verifies against the file's contents, and the signer's subject names the
    expected publisher — `O=Icinga GmbH` for Icinga's MSI. A file that is unsigned, altered, or signed
    by somebody else is refused by name, and nothing is unpacked.

13. **The chain to a root is reported, not required.** A Linux build host has no Authenticode root
    store, and carrying one inside this tool would be key management nobody asked it to do. What the
    refusal cannot claim, it does not claim: the tool says the signature is valid and whose it is, and
    says that the issuing chain was not validated locally.

14. **The expected publisher is part of the agent's own definition**, beside its repository — not a
    flag. A publisher an operator can pass on the command line is a check that argues with itself.

15. **`osslsigncode` is the verifier**, shelled out to as the extraction helpers are (clause 9), and
    refused by name when absent. The Dev Container carries it.

16. **This says nothing about whether the artifact runs.** Whether a repacked Windows tree relocates
    without the MSI's own product registration is an open question, and it needs a Windows host.
    Verification is about the bytes being the vendor's; the recipe states Windows as unproven until
    someone runs it there.

## Alternatives considered

- **Build Icinga 2 from source with a prefix of our own.** The robust answer to relocation, and the
  one ADR-0031 already weighed for GLPI: *"a compile step per architecture and a dependency list that
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
- **`.7z` or `.zip` for Windows.** Rejected for the reason ADR-0031 gives: they carry Windows
  attributes, so the tree would arrive without executable bits.
- **One artifact per distribution family**, aimed by a Selector on an operator-set attribute.
  Rejected on the evidence: it is stricter than the glibc constraint it would be derived from, it
  doubles the build and test surface for no reachability gained, and for Red Hat it prescribes an
  artifact that cannot be fetched without a subscription.
- **One Set with a distro-suffixed version** (`2.14.6+el9`). Rejected: ADR-0009 compares without
  build metadata, so the two would compare equal and the offer would be a coin toss.
- **Ship the Red Hat artifact anyway, from the subscription repository.** Possible for an operator
  who has one, and a credential this tool would then have to carry. Rejected as the *default*: a
  single artifact already serves those hosts, so the subscription becomes a choice rather than a
  requirement.
- **Build on the newest distribution and require recent hosts.** Simplest to produce, and it quietly
  excludes exactly the hosts most likely to be running an old agent. Rejected — the floor should be
  a decision, not a side effect of what the build machine happened to be.
- **Require an operator-supplied `--sha256` for the MSI.** Rejected: it moves the trust decision to
  whoever types the command, with a digest they took from the same page as the file — and it makes
  the ordinary path a manual one, which is how verification comes to be skipped.
- **Trust the TLS connection to `packages.icinga.com` and repack.** Rejected outright: it would put
  the artifact's integrity in the hands of whoever serves that path, which is exactly what the
  verification rule exists to avoid.
- **Carry the GlobalSign code-signing root and validate the full chain.** Stronger on paper. Rejected
  for now: it pins this tool to one CA's roots and their rotation, and the publisher check already
  binds the artifact to a key an attacker on the mirror does not have. Revisit if a deployment needs
  a chain-verified provenance claim.
- **Verify on a Windows host instead**, where the chain validates. Rejected as a requirement: the
  repack itself runs on Linux (the MSI extracts there), and needing a second platform to check a
  download would make the Windows artifact harder to produce than to trust.
- **Skip Windows until it is proven to run.** Tempting, and it confuses two questions. Whether the
  bytes are the vendor's is answerable today; whether the tree relocates is not, and is recorded as
  open.

## Sources / Prior art

- Spike against Icinga 2.14.6-1 from Debian trixie (2026-08-17): `readelf -d` (RUNPATH), the `ldd`
  closure, `objdump -T` for the `GLIBC_` floor, the 39 MB / 52-file tree, and a relocated run.
- Measured against the vendor repositories (2026-08-17): the `Depends: libc6 (>= …)` of
  `icinga2-bin` for bullseye, bookworm and trixie; the open EL repository ending at 8; and
  `packages.icinga.com/subscription/` answering `401`.
- Measured against `Icinga2-v2.16.4-x86_64.msi` (2026-08-17): no digest sidecar published; the
  embedded signature verifies with a timestamp, signer `O=Icinga GmbH`, issuer GlobalSign GCC R45
  CodeSigning CA 2020; chain validation fails on a Linux CA bundle only.
- glibc's symbol versioning, which is what makes "built old, runs new" true and its converse false.
- [ADR-0031](0031-the-glpi-agent.md) — the repack precedent:
  dereferencing links, deterministic `.tar.gz`, verifying upstream's hash at packing time, and the
  container/mode rule.
- [ADR-0015](0015-package-delivery-for-managed-processes.md), [ADR-0015](0015-package-delivery-for-managed-processes.md),
  [ADR-0021](0021-one-platform-vocabulary.md), [ADR-0016](0016-a-package-is-a-versioned-set.md)
  — the containers, the tree limits, one entry per platform, and the Set model this fits into; one
  entry per platform is why two artifacts for one platform need two Sets.
- [ADR-0015](0015-package-delivery-for-managed-processes.md), [ADR-0015](0015-package-delivery-for-managed-processes.md)
  — the fleet's own model, where a package's content hash and Ed25519 signature protect what a
  Client installs; this decides the *other* end, what the operator's tool is willing to repack.
- [packages.icinga.com](https://packages.icinga.com/) — the vendor repositories the artifacts come
  from, and their GPG-signed rather than digest-listed shape.
- [`osslsigncode`](https://github.com/mtrojnar/osslsigncode) — the OpenSSL-based Authenticode
  verifier, packaged by Debian; the same shell-out pattern as `dpkg-deb` and `msiextract`.

## Consequences

- Positive: an Icinga 2 that installs nothing on the host, updates and rolls back like any other
  package, and carries its own libraries, ITL and check plugins.
- Positive: no build system for a C++ project with Boost and OpenSSL enters this repository.
- Positive: one artifact, one Set, one upload — and Red Hat hosts are served without a subscription.
- Positive: the Windows artifact is bound to Icinga's signing key rather than to a mirror's honesty
  — a stronger claim than the Linux path's digest, from a source that publishes less.
- Positive: the question of an artifact without an index has an answer for the next one.
- Negative / trade-offs: the build host's glibc decides reach. Stated in the manual per artifact
  rather than discovered on a host; picking the build host is a real decision, with a floor that has
  to be chosen deliberately. The tool prints it; the manual says to build on the oldest host you
  serve.
- Negative / trade-offs: the artifact carries the build distribution's OpenSSL layout onto hosts of
  another family. Inert for cluster TLS; documented for the checks where it is not.
- Negative / trade-offs: running a vendor binary on a family it was not built for is a support
  question this project cannot answer for an operator. Stated in the manual, not decided here.
- Negative / trade-offs: the tool shells out to extraction helpers and to a signature verifier, and
  therefore has platform restrictions on where a repack can run and more that a build host must
  carry.
- Negative / trade-offs: the build host must carry the vendor package's own dependencies, because
  the tree bundles what `ldd` resolves *there*. One that cannot be resolved is refused by name
  rather than packed around — a tree missing a library would otherwise ship and die on its first
  start.
- Negative / trade-offs: the chain is unvalidated locally, so what is proved is "signed by a key
  whose certificate says Icinga GmbH" rather than "signed by a certificate a trusted root vouches
  for today". Stated in the output rather than glossed.
- Negative / trade-offs: a publisher rename, or a signing certificate issued to a differently spelled
  subject, breaks the build until this project is updated. Accepted: a pin that never fails is not a
  pin.
- Follow-ups: the RPM path (its `repomd` index is the equivalent source of digests) is unimplemented
  and optional — what would revive it is a deployment that must run the vendor's own Red Hat binary,
  and that decision brings the subscription credentials with it. Whether the repacked Windows tree
  runs relocated needs a Windows host. Chain validation against a carried root, if provenance ever
  has to be provable rather than merely bound.
