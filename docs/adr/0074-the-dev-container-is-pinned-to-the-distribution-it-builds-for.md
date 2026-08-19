# ADR-0074: The Dev Container is pinned to the distribution it builds for — its glibc is the artifact's reach

- **Status:** 🟢 accepted
- **Date:** 2026-08-18
- **Deciders:** Markus Brigl

## Context

[ADR-0002](0002-dev-container-runtime.md) chose `mcr.microsoft.com/devcontainers/base:debian` — the
floating tag — and it chose it for a container that only ever compiled this repository's own code.
For that purpose the tag is right: Rust arrives through a Feature, the base image is scenery, and
which Debian it happens to be is nobody's decision.

[ADR-0070](0070-repacked-vendor-packages-as-relocatable-icinga-2-trees.md) and
[ADR-0071](0071-one-icinga-2-artifact-built-on-the-oldest-glibc-it-must-serve.md) changed what that
container is. The Icinga 2 artifact is repacked from vendor packages, bundles **everything except
glibc**, and therefore reaches exactly the hosts whose glibc is at least the build host's. The build
host is not a flag and cannot be one — `opamp-package-fetch` refuses to build for a distribution the
host is not, because the tree carries the libraries `ldd` resolves *there*. ADR-0071 states the
consequence plainly: *"picking the build host is now a real decision, with a floor that has to be
chosen deliberately."*

Two facts make the floating tag the wrong shape for that decision:

- **A floating tag moves the reach.** `:debian` pointed at bookworm when this was written and will
  point at trixie; the day it does, every artifact built here silently stops running on Debian 12,
  Ubuntu 22.04 and RHEL 9. Nothing in the repository would change, no review would see it, and the
  failure surfaces on the fleet as *"does not run on this host"* after a rollout.
- **The container could not build the artifact at all.** It carried none of Icinga's runtime
  libraries, so the repack refused by name (correctly) rather than shipping a tree missing them. The
  manual worked around this with a throwaway `rust:bookworm` container per build — which meant the
  Dev Container could not run the operator tool this repository ships.

## Decision

We will **pin the Dev Container image to a Debian release rather than the floating tag, chosen as
the oldest distribution the fleet's artifacts must serve**, and equip it with the vendor runtime
libraries the repack draws its closure from.

Bound by this decision:

- **`mcr.microsoft.com/devcontainers/base:debian12`.** bookworm's vendor packages declare
  `libc6 >= 2.34`, so artifacts built here reach Debian 12+, Ubuntu 22.04+ and RHEL 9+ — across
  families, because glibc is backward compatible (ADR-0071).
- **The image line is the reach.** Changing it is a decision about which hosts the fleet can serve,
  not a maintenance chore, and the comment on it says so.
- **The libraries `icinga2-bin` needs beyond the base image are installed with the developer
  tooling** — six Boost packages and `libprotobuf-lite32`. They are not used by anything in this
  repository; they are what the artifact is built *from*, which is why they are named here rather
  than left to a per-build container.
- **The Dev Container becomes the documented build host for the Linux artifact.** A container of
  another distribution stays the way to build a *different* reach, which is the case the recipe now
  presents as the exception it is.
- **This supersedes ADR-0002 on its image-tag clause only.** Everything else that ADR decides — no
  Docker Feature, no host socket, host-side container management through `remote.extensionKind`,
  each project adding its own toolchain — stands unchanged.

## Alternatives considered

- **Keep the floating `:debian` tag.** Less to maintain, and correct for every purpose the container
  had before ADR-0070. Rejected: the reach of a shipped artifact would then change on whichever day
  upstream retags, with no commit to review it in.
- **Keep building in a throwaway `rust:bookworm` container, as the manual says today.** Works, and
  it keeps the Dev Container free of Boost. Rejected as the default: it costs a container per build
  and leaves the Dev Container unable to run a tool this repository ships — a tool whose whole
  contract is that it builds for the host it runs on.
- **Pin to `bullseye` for a 2.30 floor.** Widest reach ADR-0071 tabulates, and it would serve
  Debian 11 and Ubuntu 20.04 too. Rejected for now: it dates the whole development environment for
  hosts nobody in this deployment runs, and the pin can be lowered the day one appears — which is
  exactly the deliberate decision this ADR wants it to be.
- **Install the libraries at build time from the tool instead of the image.** Would keep the image
  neutral, and it would mean an operator tool running `apt-get install` on its host. Rejected: the
  refusal that names the packages is the better contract, and ADR-0070 already settled that the
  build host is equipped in advance.

## Sources / Prior art

- Measured in this container (2026-08-18): `bookworm`, glibc 2.36; `icinga2-bin` 2.16.5's unresolved
  closure was seven sonames, provided by six Boost packages and `libprotobuf-lite32`, and
  `monitoring-plugins-basic` needed nothing the base image lacked.
- [ADR-0002](0002-dev-container-runtime.md) — the container this narrows, on its image clause alone.
- [ADR-0070](0070-repacked-vendor-packages-as-relocatable-icinga-2-trees.md),
  [ADR-0071](0071-one-icinga-2-artifact-built-on-the-oldest-glibc-it-must-serve.md) — the bundling
  rule and the reach rule this follows from.
- Debian's glibc versions per release, and Icinga's `Depends: libc6 (>= …)` per vendor build.

## Consequences

- Positive: the reach of every Linux artifact is a line in a reviewed file rather than a property of
  the day it was built.
- Positive: the Dev Container can run `opamp-package-fetch --agent icinga2` directly, so the recipe
  loses a container and a `cargo run` that had to happen somewhere else.
- Negative / trade-offs: the image needs a deliberate bump, and a stale pin is a real cost —
  the development environment ages with the oldest host the fleet serves.
- Negative / trade-offs: the container grows Boost and, through `libboost-regex1.74.0`, ICU. The
  artifact grows with it: 2.16.5 links `boost_regex`, which the 2.14.6 spike in ADR-0070 did not.
- Negative / trade-offs: the package names carry bookworm's versions (`1.74.0`, `32`) and have to be
  updated together with the image — two lines that must move as one.
- Follow-ups: none for Windows, whose artifact comes from an MSI and needs no such host. If the RPM
  path is ever built (ADR-0071 left it optional), it needs a build host of its own, and this
  decision is the shape that question takes.
