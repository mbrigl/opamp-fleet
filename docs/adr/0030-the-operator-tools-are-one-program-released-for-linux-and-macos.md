# ADR-0030: The operator tools are one program, `opamp-fleetctl`, published with every release for Linux and macOS

- **Status:** 🟡 proposed
- **Date:** 2026-10-10
- **Deciders:** Markus Brigl
- **Applies to:** `crates/fleet-tools/` (the program it builds, its command tree and the names of its commands), the operator-tool build and its archives in `.github/workflows/release.yml`, the release notes, and every document or script that names an operator command

## Context

An operator needs three things this project makes before a fleet does anything useful: the
fleet's certificates (G-17), a package signing key, and signed package artifacts (G-10, Q-1).
Today they come from three places. The certificates come from `server pki`, a command tree of the
Server binary
([ADR-0029](0029-the-operator-tool-makes-the-fleets-certificates-and-the-server-says-before-they-end.md)); the signing key, the
packing and the signature from `opamp-package-sign`; a release of a known agent from
`opamp-package-fetch`. The two tools are built from `crates/fleet-tools` and published in no
release, so an operator who wants to roll out a single package needs a checkout of this
repository and a Rust toolchain on their own machine.

The forces:

- **The keys these tools make do not belong on the Server host.** Whoever holds the package
  signing key and can make an offer runs code on every host it reaches, which is why the Client
  installs only what a key the operator holds has signed
  ([ADR-0037](0037-packages-signed-deployments-offered-downloads-and-verified-delivery.md)). The
  server CA's and the bootstrap CA's keys are kept off the Server host for the same reason
  (ADR-0029). A program that makes those keys is a program for the operator's machine, and the
  binary that serves the fleet has no use for it.
- **The Server binary is the one program an operator certainly has,** which is why ADR-0029 put
  certificate making there and rejected a separate operator tool as one more binary to build.
  That argument holds only while the operator tool is not published. Once it is, it is the program
  an operator downloads first, before any Server or Client exists.
- **One audience, three entry points.** The certificates, the signing key, the packing and the
  fetch are all done once per fleet or once per release, by the same person, on the same machine.
  Two tool names and a Server subcommand tree for that one job is two names too many, and the
  certificate commands have no natural home in a program called `-package-sign`.
- **Keys need file modes.** Every key these commands write is `0600` in a `0700` directory
  (ADR-0029 clause 10). Linux and macOS give that by `chmod`; Windows has no such modes, and an ACL
  that does the same is a piece of work of its own.
- **Names are a public contract,** and the obvious one is taken: `fleetctl` is the command-line
  tool of FleetDM, an actively maintained fleet-management product for the same audience, installed
  with `npm install -g fleetctl` and from its GitHub releases. Two `fleetctl` on one `PATH` cannot
  both be found, and a search for either finds the other.
- **The release is a contract too.** It is one workflow run from one version
  ([ADR-0035](0035-the-client-supervisor-installed-service-releases-and-installers.md) clause 30),
  and it already builds the packer for every Client target, because the release archives are
  packed by it (clause 31 there). Its assets are listed exhaustively (clause 34 there), and
  nothing in it is signed: one `SHA256SUMS` covers every asset.

## Decision

We will build the operator tools as one program, `opamp-fleetctl`, from `crates/fleet-tools`, and
publish it in every release as a `.tar.gz` for Linux and macOS on amd64 and arm64.

1. **One program, two command trees.** `opamp-fleetctl package` holds `fetch`, `pack`, `sign`,
   `keygen` and `public-key`, each with the options and output it has today;
   `opamp-fleetctl pki` holds `init`, `server-cert`, `bootstrap-cert` and `status` as ADR-0029 states them.
   `opamp-fleetctl --version` prints the version every binary of this project prints
   ([ADR-0011](0011-versions-resolved-in-the-internal-crate.md)). No other program name exists for
   any of these commands: no alias, no wrapper, no symlink.

2. **The crate builds exactly this program.** `crates/fleet-tools` has no library and one binary,
   `opamp-fleetctl`, and keeps the dependency direction of
   [ADR-0031](0031-five-crates-the-whole-opamp-communication-layer-in-the-opamp-crate-and-toml-configuration.md)
   clause 22: it uses the Client's and the internal crate's items, and nothing depends on it.

3. **Built in the release run, from the release's version, for four targets.**
   `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin` and
   `x86_64-apple-darwin`, each in the row of the Client's build matrix for the same target. Each
   row whose target runs on its runner asserts that `opamp-fleetctl --version` reports the release
   version, as the Client's build does. No Windows build is made.

4. **One archive per target, named like the Client's.**
   `opamp-fleetctl_<version>_<os>_<arch>.tar.gz`, by the grammar and platform tokens of
   ADR-0035 clauses 32 and 33, packed by `opamp-fleetctl package pack --format tar.gz` with the one
   member `opamp-fleetctl` and its executable mode. No `.deb`, `.rpm` or `.msi` carries it. The
   four archives are assets of the release beside the Client's and are covered by the same
   `SHA256SUMS`.

5. **It is not a package of the fleet.** The release notes list the four archives as the operator
   tool and say they are downloaded, not uploaded to the Server. Everything that selects the
   Client's archives selects them by their `supervisor_` stem, never by version and platform
   alone: the notes' upload loop, which would otherwise put an `opamp-fleetctl` archive into the
   `supervisor` Package as the entry of its platform, and `opamp-fleetctl package fetch --agent
   supervisor`. An upload made by hand anyway installs nothing: the Client's self-update refuses
   an artifact whose member is not `supervisor` (ADR-0035 clause 29).

6. **Unsigned, as the Client's artifacts are.** The archives carry what every asset of a release
   carries: their entry in `SHA256SUMS`. Provenance for the release's assets is one decision for
   all of them, not a property of this program alone.

**Out of scope:** Windows; native packages, Homebrew, `npm` or any other installer; signing or
attesting the release's assets; commands that drive the running fleet through the Operator plane
beyond the upload `package fetch` makes today; what the `pki` commands write, which ADR-0029
states.

## Alternatives considered

- **Keep two programs and publish both.** It lifts the toolchain requirement, but leaves two names
  for one person's one job, and the certificate commands either stay in the Server binary, which
  then carries the means to make the keys that are to be kept off its host, or land in a program
  named after packages.
- **Put the commands into the Server binary,** beside `pki status`, `hash-credential` and
  `audit-verify`. One program fewer to download, but the package signing key and the offline CA
  keys would be made by the binary of the host they are kept away from, and the archive writers
  and the release fetcher would be linked into the binary that faces the fleet — attack surface
  with no use in serving
  ([ADR-0037](0037-packages-signed-deployments-offered-downloads-and-verified-delivery.md)
  clause 6 has the Server never create or open an artifact).
- **Put them into the Client binary.** The Client is published already, but it lands on every
  host, and every host would then carry the means to make a CA and a signing key (ADR-0029 rejected
  this for the certificates).
- **Name it `fleetctl`.** The `kubectl` analogy the crate split already draws, but the name belongs
  to FleetDM's tool (Context).
- **Name it `opamp-operator` or `opamp-fleet`.** The first says who uses it, but a name ending in
  `-ctl` says it is the command-line tool of a system, which is what an operator looks for. The
  second is the build-time product name the Client's service is registered under (ADR-0035), so a
  program of that name reads as the Client.
- **All five targets, Windows included.** Windows would need its own way to keep a key readable by
  its owner alone, a piece of work this decision does not take on.
- **Linux only.** Simpler by two builds, but an operator on macOS — a likely operator machine —
  would again need a toolchain, and macOS gives the same file modes as Linux.
- **Keep the tools out of the release.** No release change at all, and the status quo: every
  operator builds them from a checkout, and a fleet cannot install a single signed package
  without a Rust toolchain on someone's machine.

## Sources / Prior art

- Kubernetes publishes `kubectl` per platform as a download of its own, apart from the components
  that run on a node or the control plane. <https://kubernetes.io/releases/download/>
- FleetDM's `fleetctl`: installed with `npm install -g fleetctl`, from an install script, or from
  its GitHub releases. <https://fleetdm.com/guides/fleetctl>,
  <https://www.npmjs.com/package/fleetctl>
- HashiCorp Nomad and Consul put their certificate commands into the one binary that also runs the
  servers (`nomad tls`, `consul tls`) — the arrangement this decision does not follow, because here
  the operator's keys are to stay off the server host.
  <https://developer.hashicorp.com/nomad/commands/tls/ca-create>,
  <https://developer.hashicorp.com/consul/commands/tls/ca>
- This project: ADR-0029 on which keys stay off the Server host, ADR-0037 on why a package is
  signed by a key the operator holds, ADR-0035 clauses 29 to 34 on the release, its naming and its
  assets.

## Consequences

- Positive: an operator downloads one program and can make the fleet's certificates, a signing
  key and signed packages without a toolchain (G-10, G-17, Q-1); the keys that are to stay off the
  Server host are made by a program that has no reason to be there; the Server binary sheds the
  commands that make its offline keys; the packer the release uses is the one an operator
  downloads.
- Negative / trade-offs: four more builds and four more assets per release; the release now
  carries a program that makes keys, and like every asset it is checked by its `SHA256SUMS` alone;
  an operator on Windows still builds from a checkout; renaming the tools changes the text of
  every accepted ADR that named them, which is why ADR-0031 to ADR-0037 restate them, and the
  references to the superseded numbers across the code and the manual follow when those are
  accepted.
- Follow-ups: provenance for every release asset; the operator tool on Windows with key files
  only their owner can read; a command tree for the running fleet, if one is ever wanted, under
  this program's name.

## Enforcement

Tests that will carry `Verifies: ADR-0030`:

- The command tree: `opamp-fleetctl --help` lists `package` and `pki` and nothing else beside
  `help`; `package --help` lists `fetch`, `pack`, `sign`, `keygen` and `public-key`; `pki --help`
  lists `init`, `server-cert`, `bootstrap-cert` and `status`; `--version` prints the baked version
  (clause 1).
- The crate: `crates/fleet-tools` declares exactly one binary target, `opamp-fleetctl`, and no
  library (clause 2).
- The release workflow, read as data: it builds `opamp-fleetctl` for exactly the four targets of
  clause 3, asserts its version where the target runs, names each archive
  `opamp-fleetctl_<version>_<os>_<arch>.tar.gz`, packs it with `opamp-fleetctl package pack
  --format tar.gz`, and includes it in `SHA256SUMS`; no Windows row and no installer names it
  (clauses 3 and 4).

- The release notes' upload loop, read out of the workflow, selects `supervisor_<version>_*.tar.gz`
  and matches no `opamp-fleetctl_` archive (clause 5).

**Not mechanically decidable:** that the release notes describe the archives as the operator
tool and not as packages (clause 5) is held by review of the release notes in the workflow.
