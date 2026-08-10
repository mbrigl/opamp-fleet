# ADR-0045: The single version helper lives in `crates/opamp`, so both ends report one number

- **Status:** 🟡 proposed
- **Date:** 2026-08-10
- **Deciders:** Markus Brigl

Records **where** the helper that [ADR-0009](0009-version-derivation-and-baking.md) requires now
lives. It changes none of that ADR's decisions — the strict `MAJOR.MINOR.PATCH` grammar, the
`+<hash>` metadata, the `-dev` marker, compile-time resolution, and *one* helper every surface reads
are all untouched, as is [ADR-0026](0026-version-from-cargo-toml.md)'s rule that `Cargo.toml` decides
the number. ADR-0009 itself is amended in the same change set — by instruction, and against the rule
that an accepted ADR is never edited — so that it states the *single version implementation* without
naming the file it used to live in. See the last consequences below; the waiver is recorded there
rather than passed over.

## Context

ADR-0009 decides that every surface reporting a version reads one helper, never
`CARGO_PKG_VERSION`, "which knows nothing of tags or commits". It also anticipated the second
binary: *"The Server adopts the same `build.rs` and helper when its builds need it."*

The Server never adopted it. `crates/server/src/main.rs` printed `env!("CARGO_PKG_VERSION")`, so on
the same commit the two binaries of one workspace disagreed about what they were:

```console
$ server --version
server 0.1.3
$ opamp-fleet-client --version
opamp-fleet-client 0.1.3-dev+ade2775
```

The number in `Cargo.toml` is the release a build is *heading for* (ADR-0026), so the Server's
output claimed to be a release it was not, and named no commit. That is the exact failure ADR-0009's
`-dev` marker exists to prevent — "a build heading for a release is unmistakably not it" — reported
by the one binary that had opted out of the mechanism.

The obstacle is mechanical rather than architectural: `cargo:rustc-env` reaches only the crate whose
build script emitted it, so `env!("OPAMP_BUILD_VERSION")` can only be read from inside the crate
that resolved it. Taken literally, "the Server adopts the same `build.rs`" therefore means a second
copy of some 120 lines — the tag glob, the strict component grammar, the three failure modes — in a
second build script, free to drift from the first. That is the shape
[ADR-0044](0044-what-the-shared-crate-holds.md) is currently measuring against.

Both ends already depend on `crates/opamp`, and that crate already owns version *handling* for both
of them: `opamp::version::parse`, `identity`, and `same_release` (ADR-0029) are there precisely
because "the Client writes these strings and the Server displays them". What was missing was the
string itself.

## Decision

We will resolve the baked version in **`crates/opamp/build.rs`** and expose it as
**`opamp::version::current()`** — the one helper both binaries read.

1. The resolution logic moves unchanged from `crates/client/build.rs` into the shared crate's
   existing build script, beside the protobuf codegen. Its inputs, its grammar, its three failure
   modes and its `OPAMP_FLEET_VERSION` override are the same code, not a reimplementation.
2. `current()` joins `parse`/`identity`/`same_release` in `opamp::version`, which is where this
   project already answers questions about a version string. No new module.
3. `crates/client/build.rs` and `crates/client/src/version.rs` are deleted, and every call site on
   both ends — the OpAMP `service.version` attribute, both CLIs' `--version`, the ADR-0010 install
   layout, the self-check token — reads `opamp::version::current()`.
4. `opamp` gains `git2` as a **build**-dependency. It is linked into no artifact.

This is behaviour-preserving for the Client, whose reported string does not change, and a fix for
the Server, whose string now names the build it is.

## Alternatives considered

- **A second `build.rs` in `crates/server`** — the literal reading of ADR-0009's sentence. Rejected:
  it duplicates the tag grammar and the three failure modes into a place that can drift, to avoid a
  build dependency the workspace already resolves. A rule implemented twice is the failure ADR-0044
  measures; a version that disagrees between two binaries of one workspace is exactly how it shows.
- **Leave the Server on `CARGO_PKG_VERSION`** — rejected; that is the defect, and it is not
  cosmetic. A support question that starts "which build is this?" cannot be answered from a Server's
  own output, and a release build and a development build of the same number are indistinguishable.
- **A fourth crate holding only build support** — rejected by [ADR-0005](0005-workspace-and-server-runtime.md)'s
  standing rule that more crates are premature until a concrete need appears. One function shared by
  two build scripts is not that need when a shared crate already sits between the two ends.
- **`include!` a shared build-script fragment from both scripts** — rejected. It leaves two build
  scripts, needs `git2` in both manifests anyway, and hides a compiled file from the crate graph, so
  a reader of either manifest cannot see where the code comes from.
- **Shell out to `git` instead of linking `git2`** — rejected as out of scope. It would remove the
  build dependency from both ends, but it changes ADR-0009's mechanism and makes builds depend on a
  `git` executable being installed. Worth its own decision if the build dependency ever hurts.

## Sources / Prior art

- [The Cargo Book, build scripts](https://doc.rust-lang.org/cargo/reference/build-scripts.html#outputs-of-the-build-script)
  — `cargo::rustc-env` sets an environment variable "for the compilation of the crate being built",
  which is the constraint that makes this a decision about *where* the resolution runs rather than a
  free choice of module.
- [ADR-0009](0009-version-derivation-and-baking.md) §Decision — the "one helper" rule this
  implements, and the sentence about the Server adopting it, which this ADR answers.
- [ADR-0044](0044-what-the-shared-crate-holds.md) — the measurement this follows: the shared crate
  holds what both ends implement identically. One version rule needed by both ends is that case; the
  Client's two transports, measured the same way, are not.
- [ADR-0005](0005-workspace-and-server-runtime.md) — one workspace, one lockfile, and the standing
  rejection of further crates, which rules out the build-support-crate alternative above.

## Consequences

- Positive: `server --version` reports `0.2.0-dev+<hash>` — the build it is, with the commit it came
  from. An operator can tell a release from a build heading for one, on both binaries, by the same
  rule.
- Positive: the tag grammar, the release/development distinction and the "a tag that disagrees with
  the file fails the build" rule exist once. A future third surface adopts them by calling a
  function.
- Negative / trade-offs: `opamp` gains `git2` as a build dependency, so `cargo build -p server`
  alone now compiles `libgit2` where it did not before. A whole-workspace build already did — the
  Client pulled it in — so CI's cost is unchanged and only a Server-only build pays.
- Negative / trade-offs: every commit changes the version, and the version is now baked by the crate
  both ends depend on, so a new commit rebuilds `opamp` and therefore both binaries. It already
  rebuilt the Client and everything downstream of it, which is both binaries in a workspace build;
  what is new is a Server-only build no longer being able to skip it.
- Nothing of `git2` reaches an artifact. A build dependency is compiled into the build script alone,
  which ADR-0009 already stated and this change keeps true: `cargo tree -e normal -p server` names
  no `git2`, and neither binary carries a libgit2 symbol.
- **ADR-0009 was edited in place rather than superseded**, on the maintainer's explicit instruction
  and against this project's own rule that an accepted ADR is never edited
  ([`AGENTS.md`](../../AGENTS.md) §3.3). The sentence naming `crates/client/src/version.rs` now
  speaks of the single version implementation without pointing at a path, so it stays true wherever
  the code sits. Recorded here because a rule waived once must at least be visible: the git history
  of that file is the only other trace.
- **Relation to ADR-0044**, amended in the same change set: its three modules are a finding under its rule,
  not a ceiling, so a fourth thing entering the shared crate — this helper — is ordinary work under
  that ADR rather than a conflict with it. Its trade-off list now carries the `git2` build
  dependency this adds.
