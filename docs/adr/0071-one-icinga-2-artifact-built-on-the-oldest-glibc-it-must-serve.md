# ADR-0071: One Icinga 2 artifact, built on the oldest glibc it must serve — the distribution family is not the criterion

- **Status:** 🟢 accepted
- **Date:** 2026-08-17
- **Deciders:** Markus Brigl

## Context

[ADR-0070](0070-repacked-vendor-packages-as-relocatable-icinga-2-trees.md) decided **one artifact per
distribution family**, aimed by a Selector on an operator-set attribute, and derived it from a
correct premise: glibc cannot be bundled, so the build host's glibc bounds the artifact's reach.

The step from that premise to *"per family"* is where it went too far. glibc is **backward
compatible**: a program built against an older one runs against every newer one. So what a tree can
reach is decided by one number, not by which packaging tradition its binary came from — and the
vendor states that number in every package it publishes:

| Vendor build | `Depends: libc6 (>= …)` | Runs on |
|---|---|---|
| Debian 11 (bullseye) | 2.30 | Debian 11/12/13, Ubuntu 20.04+, **RHEL 9** (2.34), RHEL 10 |
| Debian 12 (bookworm) | 2.34 | Debian 12/13, RHEL 9 — not RHEL 8 (2.28) |
| Debian 13 (trixie) | 2.38 | only very recent systems |

Everything else the daemon needs already travels with it — 21 shared objects, measured, with
`LD_LIBRARY_PATH` beating the binary's own `RUNPATH`. So an artifact built on Debian 11 runs on a
RHEL 9 host, and the family split buys nothing there.

Two further facts, both found by looking rather than by reasoning:

- **Icinga's open RPM repository ends at EL 8.** Everything from RHEL 9 on is behind
  `packages.icinga.com/subscription/`, which answers `401` without credentials. The per-family
  decision therefore prescribed an artifact that cannot currently be built at all without a
  subscription — while the Debian-built one it would replace serves those same hosts today.
- **OpenSSL's configuration directory is compiled in**, and the two families differ: Debian's
  `libcrypto` looks in `/usr/lib/ssl`, Red Hat's in `/etc/pki/tls`. For Icinga's cluster TLS this is
  inert — certificates are named by explicit paths (ADR-0069) — but anything reaching for the
  *system* trust store on a Red Hat host would find the Debian path.

## Decision

We will ship **one Icinga 2 artifact per platform, built on the oldest glibc it must serve**, and
drop the per-family rule with the machinery that served it.

Bound by this decision:

- **The build host is the reach.** Choosing it is the operator's decision, and the tool states its
  consequence: it prints the `libc6` floor from the vendor's own package before anything is
  uploaded, and refuses to build for a distribution the host is not (both from ADR-0070, unchanged).
- **The Set is named after the Agent type**, as every other agent's is. `opamp-package-fetch` loses
  the `--package-name` flag it grew for the two-Set case — a flag that never worked, and now has
  nothing to do.
- **Two Sets remain possible, without the tool knowing.** A deployment that genuinely needs a second
  artifact — a host too old for the common floor, a family whose vendor binary it must run for
  support reasons — creates that Set through the REST API under a name of its own and aims it with
  a Selector. Nothing in the Server or the Client ever required the tool's help for that.
- **The Red Hat caveat is documented, not designed around.** The manual states that a Debian-built
  tree carries Debian's OpenSSL layout, so a check that uses the system trust store on a Red Hat
  host is the one thing to verify before relying on it.
- **This supersedes ADR-0070 on its "one artifact per distribution family" clause only.** Everything
  else that ADR decides — the normalised layout, bundling everything but glibc, `.tar.gz`, digests
  from the repository index, the refusals — stands unchanged.

## Alternatives considered

- **Keep one artifact per family.** Conservative, and what ADR-0070 said. Rejected on the evidence:
  it is stricter than the constraint it was derived from, it doubles the build and test surface for
  no reachability gained, and for Red Hat it currently prescribes an artifact that cannot be fetched
  without a subscription.
- **Ship the Red Hat artifact anyway, from the subscription repository.** Possible for an operator
  who has one, and a credential this tool would then have to carry. Rejected as the *default*: a
  single artifact already serves those hosts, so the subscription becomes a choice rather than a
  requirement.
- **Build on the newest distribution and require recent hosts.** Simplest to produce, and it quietly
  excludes exactly the hosts most likely to be running an old agent. Rejected — the floor should be
  a decision, not a side effect of what the build machine happened to be.
- **Bundle glibc after all**, making the artifact universal. Rejected in ADR-0070 and unchanged: the
  program would have to be the loader, which a Supervisor's program path cannot express.

## Sources / Prior art

- Measured against the vendor repositories (2026-08-17): the `Depends: libc6 (>= …)` of
  `icinga2-bin` for bullseye, bookworm and trixie; the open EL repository ending at 8; and
  `packages.icinga.com/subscription/` answering `401`.
- The relocation measurements ADR-0070 rests on: `RUNPATH` rather than `RPATH`, the 21-object
  closure, and a relocated run out of the repacked tree.
- glibc's symbol versioning, which is what makes "built old, runs new" true and its converse false.
- [ADR-0031](0031-per-platform-package-variants.md), [ADR-0052](0052-a-package-is-a-versioned-set.md)
  — one entry per platform in a Set, which is why two artifacts for one platform ever needed two
  Sets in the first place.

## Consequences

- Positive: one artifact, one Set, one upload — and Red Hat hosts are served today rather than after
  a subscription is bought.
- Positive: a flag that did nothing leaves the tool, and the manual loses a two-Set procedure nobody
  has to follow.
- Negative / trade-offs: the artifact carries the build distribution's OpenSSL layout onto hosts of
  another family. Inert for cluster TLS; documented for the checks where it is not.
- Negative / trade-offs: running a vendor binary on a family it was not built for is a support
  question this project cannot answer for an operator. Stated in the manual, not decided here.
- Negative / trade-offs: picking the build host is now a real decision, with a floor that has to be
  chosen deliberately. The tool prints it; the manual says to build on the oldest host you serve.
- Follow-ups: the RPM path stays unimplemented, and it is now optional rather than required — what
  would revive it is a deployment that must run the vendor's own Red Hat binary, and that decision
  brings the subscription credentials with it.
