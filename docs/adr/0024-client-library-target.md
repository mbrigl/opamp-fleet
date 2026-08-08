# ADR-0024: The Client is a library with a thin binary on top, so a test can reach what it tests

- **Status:** 🟢 accepted
- **Date:** 2026-08-08
- **Deciders:** Markus Brigl

## Context

`crates/client` has no library target. `src/main.rs` declares the whole Client as private modules
(`mod supervisor;`, `mod selfupdate;`, `mod archive;`, …), and everything else in the package works
around that:

- **22 tests are Unix-only for a reason that is not about Unix.** The supervision tests in
  `crates/client/src/supervisor/process.rs` are gated `#[cfg(all(test, unix))]` and spawn
  `/bin/sh -c` scripts as their Managed Process. They cannot spawn `stub_agent` — this project's
  own cross-platform test program, whose module doc says it exists so tests "behave identically on
  Linux, macOS, and Windows CI, no shell scripts" — because Cargo sets `CARGO_BIN_EXE_<name>` "only
  … when building an integration test or benchmark", and these are unit tests inside a binary
  crate. Measured, not assumed: `cargo test -p client --bin client` does not even build
  `stub_agent`. What is untested on Windows as a result is the binary swap, the health gate, the
  rollback, and the whole ADR-0023 tree path — the operations that write to a host.
- **The second binary re-compiles a module by path.** `src/bin/opamp-package-sign.rs` carries
  `#[cfg(test)] #[path = "../archive.rs"] mod archive;` so its tests can open an artifact with the
  same code the Client opens it with. That is the right intent — a container the tool produces and
  the Client cannot open would be discovered at rollout time, on every matched host — reached
  through the only mechanism a binary-only crate offers.
- **Integration tests restate constants they cannot import.**
  `crates/client/tests/self_update_e2e.rs` redeclares `EXIT_RESTART_FOR_UPDATE = 10` and the
  platform binary name, each with a comment saying why: "`client` is a binary crate, so a test
  cannot link it".

The forces:

- **The specification's testability is not optional.** `AGENTS.md` §5 requires new behaviour to ship
  with tests and treats an untested platform as what it is. Three platforms are in scope
  ([ADR-0010](0010-client-os-service-and-cli.md)), and today one of them cannot run the tests that
  cover the operations most likely to break a host.
- **Cargo's rule is fixed and not negotiable from our side.** Integration tests "can use the public
  API of the package's library"; there is no arrangement by which they link a binary target. A test
  that wants both the supervision core *and* a real helper executable therefore needs a library.
- **[ADR-0005](0005-workspace-and-server-runtime.md) says to keep hexagonal seams as modules until a
  concrete need makes them crates.** This is not a request for a new crate. It is the smaller step
  the same sentence anticipates: the modules stay exactly where they are, and the package grows the
  target that makes them reachable.
- **[ADR-0011](0011-supervisor-mode-hexagonal-core-and-plugins.md) already defines the seam.** The
  Ports are `ProcessCommand`/`ProcessEvent` and the `Plugin` factory; `Runner` is the adapter behind
  them. What a test needs to reach is exactly what that ADR already calls the core — so this
  publishes a boundary that was designed, not one invented here to make testing convenient.

## Decision

We will give `crates/client` a **library target** holding the modules it already has, and leave
`src/main.rs` as a thin binary that calls into it.

1. **`src/lib.rs` declares the module tree; `src/main.rs` keeps only `fn main`** and whatever
   belongs to starting a process (argument parsing hand-off, the exit code). No module moves on
   disk, no code moves between modules, and the two other binaries (`stub_agent`,
   `opamp-package-sign`) stay binaries.

2. **Visibility is widened by need, not by default.** A module or item becomes `pub` when a test or
   another target in this package has to reach it, and stays private otherwise. The library's public
   API is a *test and tooling* surface inside this workspace — `crates/client` is
   `publish = false`, so nothing here is a promise to anyone outside it. Where widening an item
   would expose an invariant that only the module can hold, the item stays private and the test
   moves to where it can see it.

3. **`opamp-package-sign` drops `#[path = "../archive.rs"]`** and uses the library, which is what
   that hack was imitating.

4. **`self_update_e2e` imports what it restates.** `EXIT_RESTART_FOR_UPDATE` and the platform binary
   name come from the library; the comments explaining why they were copied go with them.

5. **The 22 supervision tests move to an integration test and lose their `unix` gate.** They keep
   driving `Runner` directly — the assertions do not change — and spawn `stub_agent` instead of
   `/bin/sh`, which Cargo builds for them automatically ("Binaries are automatically built when the
   test is built"). Crash paths use the stub's `--exit-code`/`--exit-after-ms`; the default run
   sleeps until killed. Tests that assert on Unix file modes keep a `#[cfg(unix)]` on the assertion,
   because a mode is genuinely a Unix fact — the gate ends up on what is actually platform-specific
   rather than on the whole file.

## Alternatives considered

- **Resolve `stub_agent` from the test binary's own directory** (`current_exe()`'s parent's parent).
  The smallest diff, and it makes the suite depend on how it is invoked: `cargo test -p client`
  passes, `cargo test -p client --bin client` fails, because the stub is only built in the first
  case. A test that fails depending on the command that ran it teaches developers to distrust the
  suite, which costs more than the diff saves.
- **Rewrite the package and tree tests as full end-to-end tests** — real Client, real Server, real
  package offer, asserted through the fleet view, in the style of `self_update_e2e`. It needs no
  structural change and it tests more of the chain. It also replaces precise assertions with
  coarse ones: "the tree was rolled back whole and the previous one is intact" is a statement about
  a directory after a failed install, and reaching it through a Server, a Selector, a download and a
  health gate makes the test slower, flakier, and worse at saying what broke. Worth having *as well*
  — not instead.
- **Move the supervision core into its own crate.** It would give the same reachability and it is
  what ADR-0005 defers until there is a concrete need for a crate. The need here is a *test* that
  can link the code, which a library target inside the same package satisfies exactly; a crate
  boundary would additionally force every internal seam to become a public API, which is a larger
  and less reversible commitment than the problem asks for.
- **Port only the tests that need no executable artifact**, using each platform's shell (`/bin/sh`
  versus `cmd /C`). It leaves the binary swap, the rollback and the tree path — everything that
  writes to a host — Unix-only, which is the coverage that matters most on the platform with the
  least of it.
- **Leave it as it is and rely on the manual smoke checklist** (`README.md`). That checklist covers
  service registration, which genuinely cannot run in CI; it does not cover a package swap, and
  extending it would move a repeatable check to a human at release time.

## Sources / Prior art

- **The Cargo Book, environment variables** — `CARGO_BIN_EXE_<name>`: "The absolute path to a binary
  target's executable. **This is only set when building an integration test or benchmark.** … Binaries
  are automatically built when the test is built, unless the binary has required features that are
  not enabled." The first sentence is why the unit tests cannot reach the stub; the last is why an
  integration test needs no extra wiring to get it.
  <https://doc.rust-lang.org/cargo/reference/environment-variables.html>
- **The Cargo Book, Cargo targets** — "Integration tests can use the public API of the package's
  library", and separately, "Binaries can use the public API of the package's library". There is no
  arrangement under which a test links a binary target; the library is the only seam Cargo offers
  for either consumer.
  <https://doc.rust-lang.org/cargo/reference/cargo-targets.html>
- **This workspace's own precedent.** `crates/server` is a library with `src/main.rs` on top, which
  is why `crates/server/tests/` can drive it directly and why `self_update_e2e` can run a real
  Server in-process while it cannot link the Client it tests. The shape being proposed is the one
  the other half of this workspace already has.
- **`ripgrep`** ships `crates/core` as a library with a thin `main.rs`, and its own binary is one
  consumer among several — the widely-copied form of "a binary is a thin shell over a library",
  adopted there for the same reason: what is worth testing is not reachable through a `main`.
  <https://github.com/BurntSushi/ripgrep>

## Consequences

- Positive: the operations that write to a host — binary swap, health gate, rollback, tree install
  (ADR-0015, ADR-0023) — become testable on Windows and macOS, where they are today asserted by
  nothing. That is 22 tests going from one platform to three.
- Positive: the two workarounds disappear rather than being documented — the `#[path]` include in
  `opamp-package-sign` and the constants `self_update_e2e` restates. A restated constant is a
  correctness risk that comments can only mitigate.
- Positive: `cargo doc` starts producing something for the Client, which today documents nothing
  because a binary's modules are private.
- Negative / trade-offs: **items become `pub` that are not a public API in any meaningful sense.**
  The package is `publish = false`, so this binds nobody outside the workspace — but "pub" stops
  meaning "part of the interface" inside the crate, and the compiler will no longer warn about code
  that has quietly stopped being used. This is the real cost, and it is the part most worth
  reviewing.
- Negative / trade-offs: one more target to build, and a moment of churn in the diff — every module
  declaration moves file, even though no code does.
- Negative / trade-offs: the 22 tests move from beside the code they test into `tests/`, which is a
  longer reach when reading. Unit tests that need module internals and are genuinely platform-neutral
  should stay where they are; the ones moving are those that need to spawn a real program.
- Follow-ups: whether `crates/opamp`'s and `crates/server`'s test helpers should be shared through a
  small internal test-support target once two suites want the same stub. And whether the Windows
  coverage this unlocks changes what the manual smoke checklist in `README.md` still has to claim —
  it currently states that real service registration cannot run in CI, which stays true, but the
  sentence sits next to claims about package installation that will no longer be manual-only.
