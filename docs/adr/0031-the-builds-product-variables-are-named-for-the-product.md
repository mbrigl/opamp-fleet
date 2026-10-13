# ADR-0031: The build's product variables are named for the product, not for this project

- **Status:** 🟢 accepted
- **Date:** 2026-10-12
- **Deciders:** Markus Brigl
- **Applies to:** the environment variables `crates/fleet-agent/build.rs` and `crates/fleet-core/build.rs` read and emit, the constants of `crates/fleet-agent/src/product.rs` and `fleet_core::version::current`, and every document, test or workflow that names one of them
- **Supersedes:** [ADR-0011](0011-versions-resolved-in-the-internal-crate.md) clauses 1 and 4,
  [ADR-0018](0018-the-client-supervisor-installed-service-releases-and-installers.md) clause 2

## Context

Two build scripts read and emit four environment variables that describe the product a build
produces:

| Today | Set by | What it carries |
|---|---|---|
| `OPAMP_FLEET_PRODUCT_NAME` | whoever builds a variant (ADR-0018 clause 2) | the product's slug: install path, service, package, `PATH` symlink |
| `OPAMP_FLEET_PRODUCT_DISPLAY_NAME` | whoever builds a variant (ADR-0018 clause 2) | the product's prose name |
| `OPAMP_FLEET_VERSION` | whoever states a version (ADR-0011 clause 1) | the base `MAJOR.MINOR.PATCH` |
| `OPAMP_BUILD_VERSION` | `crates/fleet-core/build.rs` only (ADR-0011 clause 4) | the resolved version string |

The first three carry the product name `opamp-fleet` in their own name. ADR-0018 makes that name a
build-time value precisely so a variant can carry another one: a build of `acme-agent` sets
`OPAMP_FLEET_PRODUCT_NAME=acme-agent`, and the variable then names a product the build is not.
The fourth names a *build* version beside a *fleet* version, and a reader cannot tell from the two
names which is the input and which the result.

Names without a prefix (`PRODUCT_NAME`, `PRODUCT_VERSION`) were considered and refused: they are
common in other toolchains — Xcode exports `PRODUCT_NAME` to every build phase — and a stray value
of the slug would pass ADR-0018's grammar check and silently rename the service and move the
install path. `OPAMP_` is the prefix the build already uses for `OPAMP_BASELINE`
([ADR-0009](0009-five-crates-the-whole-opamp-communication-layer-in-the-opamp-crate-and-toml-configuration.md))
and names the protocol, not the product, so it holds in every variant.

The clauses that name the variables are superseded by clause
([ADR-0030](0030-an-adr-may-supersede-named-clauses-of-another.md)); what they decide stands.

## Decision

We will name the build's product variables `OPAMP_PRODUCT_NAME`, `OPAMP_PRODUCT_TITLE`,
`OPAMP_PRODUCT_VERSION` and `OPAMP_PRODUCT_RELEASE`, and the display-name constant `PRODUCT_TITLE`.

1. **Replaces ADR-0018 clause 2, names only.** `PRODUCT_NAME` is a build-time constant, default
   **`opamp-fleet`**, overridable with `OPAMP_PRODUCT_NAME`; the grammar and the build failure stay
   as clause 2 states them. The display name, default **`OpAMP Fleet Agent`**, is the constant
   `PRODUCT_TITLE`, overridable with `OPAMP_PRODUCT_TITLE`, still only required to be non-blank and
   still appearing only where clause 2 lets it appear. The constant `PRODUCT_DISPLAY_NAME` is
   renamed with it, so the variable and the constant read alike.
2. **Replaces ADR-0011 clause 1, the override only.** The environment variable
   `OPAMP_PRODUCT_VERSION` overrides the base, held to the same grammar.
3. **Replaces ADR-0011 clause 4, the names only.** `crates/fleet-core/build.rs` emits
   `cargo:rustc-env=OPAMP_PRODUCT_RELEASE=<full string>` and `rerun-if-env-changed=OPAMP_PRODUCT_VERSION`;
   everything else of clause 4 stands.
4. **The old names are not read.** A build that still sets one of them gets the default, as for any
   other unknown variable; there is no alias and no transition period.

**Out of scope:** `OPAMP_BASELINE` and `OPAMP_SUPERVISOR_TOKEN`, which keep their names; the
constant `PRODUCT_NAME` and `fleet_core::version::current`, which keep theirs; the product's
default name and title themselves.

## Alternatives considered

- **Keep the names** — the variables keep naming one product in every variant build.
- **No prefix (`PRODUCT_NAME`, …)** — the human's first proposal; refused for the ambient
  collision above, which for the slug fails silently.
- **Fail the build when an old name is set** — protects a variant pipeline that was not updated
  from building `opamp-fleet` under its own banner; refused by the human as ceremony for a
  rename with no released variant build yet. Recorded as a trade-off below.
- **Accept old and new names for a while** — a second name per thing, against the spirit of
  ADR-0018 clause 29, for a project with no external variant build to migrate.
- **Supersede ADR-0011 and ADR-0018 whole** — about 1,400 lines restated to change four
  identifiers; what ADR-0030 exists to avoid.

## Sources / Prior art

- Xcode Build Settings Reference, `PRODUCT_NAME` — exported to the environment of every Run Script
  build phase. Recalled, not re-verified for this ADR.
- [Cargo build scripts, `rerun-if-env-changed` and `rustc-env`](https://doc.rust-lang.org/cargo/reference/build-scripts.html)
  — the mechanism both scripts use; the rename changes nothing of it.

## Consequences

- Positive: a variant build sets variables that name what it is; input and result of the version
  read as one pair, `OPAMP_PRODUCT_VERSION` → `OPAMP_PRODUCT_RELEASE`.
- Negative / trade-offs: a script or pipeline that sets an old name is ignored without a word and
  builds the default product, name and version alike. No variant build exists yet, so the
  exposure is the documentation and local habits.
- Follow-ups: the rename itself — both build scripts, `product.rs`, `version.rs`, the comments and
  tests that name the variables, and a CHANGELOG entry; the existing CHANGELOG entry that names the
  old variable stays, being history.

## Enforcement

`crates/fleet-agent/src/product.rs` and `crates/fleet-core/src/version.rs` read the emitted names
through `env!`, so a build script that emits another name fails the compilation. The
self-update end-to-end tests in `crates/fleet-agent/tests/self_update_e2e.rs` build their newer
Client with `OPAMP_PRODUCT_VERSION` and fail when the override is not read, because that Client
then reports no upgrade; the rename adds `ADR-0031` to their `Verifies:` markers.

**Not mechanically decidable:** that no document still teaches an old name is review; a search for
`OPAMP_FLEET_` and `OPAMP_BUILD_VERSION` outside the CHANGELOG's history and the superseded clauses
finds them.
