# ADR-0065: The operator package tools live in their own crate, depending on the Client rather than shipping inside it

- **Status:** 🟢 accepted
- **Date:** 2026-08-15
- **Deciders:** Markus Brigl

## Context

Two operator command-line tools have grown inside `crates/client`:
`opamp-package-sign` (build, hash, sign an artifact) and, newly, `opamp-package-fetch` (fetch an
upstream agent release, verify it, hand it to the Server). Both are *package management* tools:
they exist to get software into a fleet, and nothing the Server or a Client does at runtime
depends on either.

They landed there for a reason that was good at the time and is no longer sufficient. ADR-0005
fixed the workspace at **exactly three crates** — `opamp`, `server`, `client` — and ADR-0024,
which made the Client a library with a thin binary on top, recorded that "the two other binaries
(`stub_agent`, `opamp-package-sign`) stay binaries" in that crate: a second binary in an existing
crate cost nothing, needed no new manifest, and could reach `crate::archive` by path.

What that costs is now visible:

- **The crate that ships the daemon also carries operator tooling.** `cargo build -p client`
  builds an interactive downloader; a reader of `crates/client` finds a GitHub release client and
  a prompt library beside the supervision core. The Client is what runs on every managed host, and
  the boundary of that crate should say so.
- **The tools are not a Client concern at all.** They run on an operator's machine, and a release
  ships neither of them: the `.7z` artifacts and the `.deb`/`.rpm`/`.msi` installers carry
  `opamp-fleet-client` alone.
- **Growth pressure.** `opamp-package-fetch` is 700 lines that know four upstream projects'
  release conventions, and those conventions change (the Collector moved its checksum layout at
  0.158.0). That is a maintenance surface with its own tempo, unrelated to the Client's.

The coupling to the Client is real but small, and worth keeping rather than cutting:

| What the tools use | Where | Why it should stay |
|---|---|---|
| `archive::unix_mode_attributes` | `opamp-package-sign`, at runtime | the 7z Unix-mode convention has one definition, beside the code that decodes it |
| `archive::extract_*` | both tools, **in tests** | an artifact is checked by opening it with the Client's own unpacker — the property worth asserting is not "this is a valid archive" but "*this* code installs it" |
| `tls::install_ring_provider` | `opamp-package-fetch`, at runtime | `reqwest` is built with `rustls-no-provider` (ADR-0007) and refuses to work without one |

## Decision

We will move both operator package tools into a **fourth crate, `package-tools`**, which depends
on `client` as a library and produces the two binaries under their existing names.

Concretely:

- **`crates/package-tools/` holds `opamp-package-fetch` and `opamp-package-sign`.** The binary
  names do not change: they are documented, and `opamp-package-sign` is named in the release
  workflow.
- **The dependency arrow points one way: `package-tools` → `client`.** The tools use the Client's
  `archive` and `tls` items rather than restating them, and their tests open what they produce
  with the Client's own unpacker. Nothing in `client`, `server`, or `opamp` ever depends on
  `package-tools`.
- **The test stubs stay in `client`.** `stub_agent` and `stub_crasher` are fixtures the Client's
  own integration tests reach through `CARGO_BIN_EXE_*`, which resolves only within the crate
  that declares them. They are not operator tools and have no business moving.
- **Tool-only dependencies move with the tools.** Whatever serves only the tools —
  `dialoguer`'s prompts, the HTTP client's use for release listings — is declared by
  `package-tools`, so `cargo build -p client` stops building them. Where the Client needs the
  same crate for its own reasons (`dialoguer` for the interactive install, `reqwest` for the
  polling transport), both declare it; a workspace dependency is shared, not duplicated.
- **The release workflow follows the crate.** Its packer step becomes
  `cargo build --release -p package-tools --bin opamp-package-sign --locked`; nothing else about
  the pipeline changes, and the artifacts it produces are unchanged.

This **amends ADR-0005** — the workspace is four crates, not three; every other decision that ADR
made (tokio, axum, one port, embedded UI) is untouched — and **amends ADR-0024** on one point:
of the binaries that ADR left in the client crate, the operator tool moves and the test stubs
stay. ADR-0024's actual subject — the Client being a library so a test can reach what it tests —
is what makes this move cheap, and is reaffirmed rather than changed.

## Alternatives considered

- **Leave them in `crates/client`.** The status quo, and it costs nothing to type. Rejected: it
  is precisely the boundary problem above — the crate that runs on every managed host would keep
  carrying tooling that never runs there, and every reader of the Client would keep meeting it.
- **Move only `opamp-package-fetch`.** Half the change for half the benefit: `opamp-package-sign`
  is the *same kind of thing*, and splitting the two would leave a crate boundary that no one can
  state a rule for. If package tooling has a home, both tools are in it.
- **Move `archive` (and the provider helper) into the shared `opamp` crate so the tool crate need
  not depend on `client`.** Rejected on ADR-0044's rule: the shared crate holds what **both ends
  implement identically**, measured rather than assumed — unpacking a package artifact is the
  Client's alone, and the Server explicitly never opens one (ADR-0018). Moving it would widen the
  shared crate to make a dependency arrow prettier.
- **A separate repository.** Rejected: the tools' correctness is defined by what the Client can
  install, and their tests assert exactly that by calling into it. One workspace keeps that check
  compiling; two repositories would replace it with a version constraint and a hope.
- **Duplicate the few items the tools need.** Rejected: `unix_mode_attributes` is a convention
  that must have one definition, and a second copy is how two of them drift apart.

## Sources / Prior art

- [ADR-0005](0005-workspace-and-server-runtime.md) (three crates), [ADR-0024](0024-client-library-target.md)
  (the Client as a library, and which binaries stay), [ADR-0044](0044-what-the-shared-crate-holds.md)
  (what belongs in the shared crate) — the decisions this one amends or is bounded by.
- [The Cargo Book, workspaces](https://doc.rust-lang.org/cargo/reference/workspaces.html) — the
  shape being used: several crates, one lockfile, one `cargo build --workspace`, per-crate
  dependencies so a binary's dependency set is the crate's own.
- Comparable splits in this ecosystem — `rust-analyzer`'s `xtask`-style tooling crates, and
  Kubernetes' `kubectl` beside `kubelet` — where the operator's command-line tool is built and
  versioned with the system it drives but is not part of what runs on a managed node.

## Consequences

- Positive: `crates/client` is again only what runs on a managed host, and its dependency list
  says so. The tools gain a manifest of their own, so a dependency added for a release-listing
  quirk is visibly the tools' and not the daemon's. `cargo build -p client` gets smaller — and
  measurably so in one place: the 7z **writer** (`sevenz-rust2/compress`) was enabled for the
  whole workspace only because `pack` was a bin target of the client crate, and it goes back to
  being what it always should have been, a dependency of the tools and of the Client's own tests.
- Negative / trade-offs: a fourth crate is a fourth manifest, and a workspace member whose only
  purpose is two binaries. The tools now build the Client library to compile — which they already
  did, invisibly, as binaries inside it. One line of the release workflow changes, and any
  operator muscle memory of `cargo run -p client --bin …` becomes
  `cargo run -p package-tools --bin …` (or the unchanged `cargo run --bin …`, which resolves
  across the workspace).
- Follow-ups (by topic): whether a release should ship the operator tools at all — today it ships
  neither, and an operator builds them from a checkout, which the manual now states plainly; if
  that changes, it is the release pipeline's decision to make, not this one's.
