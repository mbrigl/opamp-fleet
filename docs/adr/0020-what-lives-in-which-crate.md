# ADR-0020: What lives in which crate — the Client is a library with a thin binary on top, so a test can reach what it tests

- **Status:** 🟢 accepted
- **Date:** 2026-08-08
- **Deciders:** Markus Brigl

This decision draws the crate boundaries of the workspace by what each crate is for: the Client is
a library with a thin binary on top, so a test can reach what it tests; the shared crate `opamp`
holds what both ends implement identically, measured rather than assumed.

The operator package tools live in a crate of their own, which depends on the Client rather than
shipping inside it.

## Context

### The Client has no library target

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
  rollback, and the whole ADR-0015 tree path — the operations that write to a host.
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
  ([ADR-0010](0010-client-os-service.md)), and today one of them cannot run the tests that
  cover the operations most likely to break a host.
- **Cargo's rule is fixed and not negotiable from our side.** Integration tests "can use the public
  API of the package's library"; there is no arrangement by which they link a binary target. A test
  that wants both the supervision core *and* a real helper executable therefore needs a library.
- **[ADR-0005](0005-workspace-and-server-runtime.md) says to keep hexagonal seams as modules until a
  concrete need makes them crates.** This is not a request for a new crate. It is the smaller step
  the same sentence anticipates: the modules stay exactly where they are, and the package grows the
  target that makes them reachable.
- **[ADR-0011](0011-supervisor-mode-core-and-plugins.md) already defines the seam.** The
  Ports are `ProcessCommand`/`ProcessEvent` and the `Plugin` factory; `Runner` is the adapter behind
  them. What a test needs to reach is exactly what that ADR already calls the core — so this
  publishes a boundary that was designed, not one invented here to make testing convenient.

### What the shared crate holds

Most of this project's code sits in `crates/client` and `crates/server`; `crates/opamp` holds
roughly 500 lines. That prompts a reasonable question: should the *base* protocol implementation not
live in the protocol crate, with only the derived actions left in the two ends — the transports and
mutual TLS included, since both are part of the protocol?

The crate's charter comes from [ADR-0005](0005-workspace-and-server-runtime.md) and
[ADR-0006](0006-proto-vendoring-and-codegen.md): the generated protobuf types, the WebSocket
framing, and the `instance_uid` type, so *"the two ends cannot drift on the wire format"*. ADR-0005
also rejected further crates as premature and said the hexagonal seams live as modules until a
concrete need appears; the Client's library target (point 1) follows exactly that rule,
for a measured reason.

So the question is not whether the shared crate is *small* — it is whether anything outside it is
written twice. Taking the [critical stance](../../AGENTS.md) that rule asks for means measuring
rather than reasoning from the category "this is protocol, therefore it is shared". The measurement
splits in two.

#### What is genuinely one thing implemented twice (~250 lines)

- **The Baseline's attribute keys, and the accessors over them.** Four near-identical helpers exist:
  `attr_value` (`crates/server/src/configs.rs`), `attr_map` plus a `lookup` closure
  (`crates/server/src/fleet.rs`), `reported_service_name` (`crates/server/src/packages.rs`), and
  `string_attr` (`crates/client/src/supervisor/agent.rs`). Around them sit some forty bare
  `"service.name"` / `"service.instance.name"` / `"service.namespace"` literals spread over
  `client/supervisor/agent.rs`, `client/config.rs`, `client/telemetry.rs`, `server/fleet.rs`,
  `server/packages.rs`, `server/labels.rs` and `server/api.rs`. These keys are fixed by the Baseline
  and given their meaning by [ADR-0023](0023-what-describes-an-agent.md);
  [ADR-0017](0017-package-artifacts.md) make them load-bearing for
  *which binary a host is offered*. A typo in one of them is a silent mis-targeting, not a compile
  error.

- **The OpAMP endpoint's protocol shell.** `crates/server/src/transport.rs` and
  `crates/client/src/gateway/mod.rs` both serve `/v1/opamp` — the Client *is* an OpAMP server
  downstream in Gateway Mode ([ADR-0025](0025-gateway-mode.md)). Both restate the path, the
  `application/x-protobuf` media type and its `starts_with` check, the receive limit, the Baseline's
  "never truncate, never ship" rule for an oversized reply, and the 1009 close.
  `client/transport/http.rs` and `client/connection.rs` restate the media type a third and fourth
  time. The Baseline's gzip MUST — and with it the gzip-bomb rule, that the limit applies *after*
  decompression — exists in exactly one of those places, which is the more interesting half of the
  finding: a rule that is stated once by accident is not a rule the second endpoint follows.

- **Reading a PEM certificate or key.** `read_certs`/`read_key` exist twice, some thirty lines each,
  in `crates/client/src/tls.rs` and `crates/server/src/tls.rs`.

#### What is *not* duplication, however protocol-shaped it looks

- **The two transports.** The Client's side is a `tokio-tungstenite` connect with backoff plus a
  `reqwest` poll loop; the Server's is an axum router with a `WebSocketUpgrade` and a POST handler
  ([ADR-0007](0007-dual-transport-and-tls.md)). They share a *specification*, not a line of code.
  Moving both into `opamp` would relocate around 1,500 asymmetric lines and delete none, while the
  crate acquired axum, axum-server, tokio-tungstenite and reqwest.

- **Mutual TLS.** `server/ca.rs` signs certificate requests, `client/csr.rs` produces them and owns
  the renewal window, `server/tls.rs` is an `axum-server` `Accept` implementation that carries the
  handshake's peer certificate into a request
  ([ADR-0013](0013-agent-admission-on-the-opamp-endpoint.md)). Each exists once. They
  are two ends of one flow, which is not the same thing as one implementation in two places. Only
  the PEM readers overlap.

- **Server-offered connection settings** (`client/connection.rs`): merging an offer over what is in
  force, applying it over `client.toml`, and the verify-by-actually-connecting the Baseline demands
  are Client obligations the Server has no counterpart for.

#### Forces on the shared crate

- **The specification's non-goals bound the ambition.** This project does not ship a reusable OpAMP
  library; all crates are `publish = false`. `opamp` is an internal seam, so "a protocol crate
  ought to contain the protocol" is an aesthetic argument here, not a requirement anyone can hold us
  to.
- **A crate boundary has a cost that a module boundary does not.** Every dependency `opamp` gains is
  gained by both ends, and every change to it recompiles both.
- **Simplicity first, and YAGNI.** ADR-0005's rejection of more crates applies in the other
  direction too: a *wider* shared crate is the same speculative structure, differently shaped.

### Operator tooling inside the Client crate

Two operator command-line tools sit inside `crates/client`: `opamp-package-sign` (build, hash, sign
an artifact) and `opamp-package-fetch` (fetch an upstream agent release, verify it, hand it to the
Server). Both are *package management* tools: they exist to get software into a fleet, and nothing
the Server or a Client does at runtime depends on either.

They sit there for a reason that was sufficient when they were written and is no longer.
ADR-0005 fixes the workspace at **exactly three crates** — `opamp`, `server`, `client` — and the
Client's library target kept the other binaries (`stub_agent`, `opamp-package-sign`) in that crate:
a second binary in an existing crate cost nothing, needed no new manifest, and could reach
`crate::archive` by path.

What that costs is visible:

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

### The Client is a library

We will give `crates/client` a **library target** holding the modules it already has, and leave
`src/main.rs` as a thin binary that calls into it.

1. **`src/lib.rs` declares the module tree; `src/main.rs` keeps only `fn main`** and whatever
   belongs to starting a process (argument parsing hand-off, the exit code). No module moves on
   disk, and no code moves between modules. The test stubs (`stub_agent`, `stub_crasher`) stay
   binaries of this package; the operator tool `opamp-package-sign` is a binary of the tools crate
   (point 9).

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

### The shared crate holds what both ends implement identically

We will treat `crates/opamp` as holding **what both ends implement identically** — established by
measurement, not by whether a thing is conceptually "protocol".

The rule is the decision; the list below is what today's measurement yields under it. Three modules
follow from it now, and they are a **finding, not a ceiling**: further code belongs in the crate
whenever the same measurement supports it — something both ends implement identically, or would
have to. Adding it is then ordinary work under this ADR rather than a decision that has to overturn
it. What the rule does refuse is the other move: putting something in the shared crate because it
is conceptually "protocol", or to make the crate look less small.

6. **`opamp::attributes`** — no new dependencies.
   - Constants for the keys the Baseline fixes and this project matches on: `SERVICE_NAME`,
     `SERVICE_INSTANCE_NAME`, `SERVICE_NAMESPACE`, `SERVICE_VERSION`, `OS_TYPE`, `OS_DESCRIPTION`,
     `HOST_ARCH`.
   - `string_value(attrs: &[KeyValue], key: &str) -> Option<&str>` — the shared body of
     `configs::attr_value` and `packages::reported_service_name`, with the "an empty string is not a
     value" rule stated **once**. That rule is load-bearing in ADR-0017: an Agent reporting an empty
     type must not match an untyped package.
   - `string_attr(key: &str, value: &str) -> KeyValue` — moved from
     `client/supervisor/agent.rs`.
   - `server/fleet.rs::attr_map` **stays in the Server**: rendering non-string values in their debug
     form is a decision about the REST view ([ADR-0012](0012-selector-targeted-configurations-and-rest-api.md)),
     not about the protocol.

7. **`opamp::endpoint`** — adds `flate2` to the crate, already a workspace dependency of both ends.
   Deliberately **framework-free**: it takes `&str` and `&[u8]` and returns owned bytes, so it pulls
   in no HTTP stack and both axum handlers can call it.
   - `OPAMP_PATH` (`/v1/opamp`) and `PROTOBUF_CONTENT_TYPE` (`application/x-protobuf`).
   - `is_protobuf(content_type: &str) -> bool`.
   - `decode_body(body: &[u8], content_encoding: &str, limit: usize) -> Result<Vec<u8>, BodyError>`
     — the Baseline's gzip MUST together with the post-decompression limit, in one place. `BodyError`
     distinguishes *unsupported encoding*, *undecodable gzip* and *too large*, so each caller maps it
     to the status code its own transport prescribes rather than inheriting the Server's.
   - Callers: `server/src/transport.rs`, `client/src/gateway/mod.rs`, `client/src/transport/http.rs`,
     `client/src/connection.rs`.

8. **`opamp::pem`** — adds `rustls-pemfile` and `rustls` (for `pki_types`). Both ends already link
   both, so nothing new enters either binary.
   - `certificates(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, String>` and
     `private_key(pem: &[u8]) -> Result<PrivateKeyDer<'static>, String>`.
   - The path-based wrappers and their error wording stay in each end, where what the file *means* —
     a trust anchor, a listener's key, a client CA — is known.

**What stays where it is on this measurement:** the Client's two transports, the Server's router and
its `Admission`, `server::ca::ClientCa`, `client::csr`, `server::tls::PeerCertAcceptor`, and
`client::connection`. Each exists exactly once, so none of them is duplication to remove. Recorded
so the question is not re-asked from the category alone — but a later measurement that finds one of
them genuinely written twice moves it, without needing to supersede this ADR.

The extraction is **behaviour-preserving**. No existing test changes; a suite that needs editing
means something moved that should not have.

### The operator package tools have their own crate

We will move both operator package tools into a **fourth crate, `package-tools`**, which depends
on `client` as a library and produces the two binaries under their existing names.

9. **`crates/package-tools/` holds `opamp-package-fetch` and `opamp-package-sign`.** The binary
   names do not change: they are documented, and `opamp-package-sign` is named in the release
   workflow.

10. **The dependency arrow points one way: `package-tools` → `client`.** The tools use the Client's
   `archive` and `tls` items rather than restating them, and their tests open what they produce
   with the Client's own unpacker. Nothing in `client`, `server`, or `opamp` ever depends on
   `package-tools`.

11. **The test stubs stay in `client`.** `stub_agent` and `stub_crasher` are fixtures the Client's
   own integration tests reach through `CARGO_BIN_EXE_*`, which resolves only within the crate
   that declares them. They are not operator tools and have no business moving.

12. **Tool-only dependencies move with the tools.** Whatever serves only the tools —
   `dialoguer`'s prompts, the HTTP client's use for release listings — is declared by
   `package-tools`, so `cargo build -p client` stops building them. Where the Client needs the
   same crate for its own reasons (`dialoguer` for the interactive install, `reqwest` for the
   polling transport), both declare it; a workspace dependency is shared, not duplicated.

13. **The release workflow follows the crate.** Its packer step becomes
   `cargo build --release -p package-tools --bin opamp-package-sign --locked`; nothing else about
   the pipeline changes, and the artifacts it produces are unchanged.

This **amends ADR-0005** — the workspace is four crates, not three; every other decision that ADR
made (tokio, axum, one port, embedded UI) is untouched. The Client being a library so a test can
reach what it tests (point 1) is what makes this move cheap.

## Alternatives considered

### For the Client's library target

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

### For the shared crate

- **Move the transports, TLS and mutual TLS into `opamp` as originally asked** — rejected on the
  measurement above. It relocates ~1,500 lines that exist once, deletes no duplication, makes the
  protocol crate the union of both binaries' dependency sets, and turns every Server-only change into
  a Client recompile. It would also supersede ADR-0005's and ADR-0006's charter for the crate in
  exchange for a structure that is tidier to describe and no safer to use. The reference
  implementation drew the same line (see Sources).
- **Leave everything as it is** — rejected. Four copies of a media type, four attribute-lookup
  helpers and forty loose key literals are drift waiting to happen, and the gzip-bomb rule already
  demonstrates the failure mode: a Baseline rule implemented on one of two endpoints that both
  accept bodies.
- **A fourth crate for the shared endpoint shell** — rejected as premature by ADR-0005's own rule.
  The three modules fit the existing crate without changing what it meaningfully depends on.
- **Feature-gate `opamp::pem` behind a `tls` feature** — rejected. Both ends want it
  unconditionally; a feature that is always on is a knob nobody turns.
- **Unify `server::transport::serve_socket` and `gateway::serve_socket` behind one shared axum
  endpoint** — not rejected, deferred (see Follow-ups). The two loops differ in what drives their
  outbound side: a `watch` subscription over the fleet's desired state, versus an `mpsc` of replies
  coming back through the connection pool. Unifying them needs a trait *and* axum inside `opamp`,
  which is a materially larger decision than this one.

### For the operator package tools

- **Leave them in `crates/client`.** It costs nothing to type. Rejected: it is precisely the
  boundary problem above — the crate that runs on every managed host would keep carrying tooling
  that never runs there, and every reader of the Client would keep meeting it.
- **Move only `opamp-package-fetch`.** Half the change for half the benefit: `opamp-package-sign`
  is the *same kind of thing*, and splitting the two would leave a crate boundary that no one can
  state a rule for. If package tooling has a home, both tools are in it.
- **Move `archive` (and the provider helper) into the shared `opamp` crate so the tool crate need
  not depend on `client`.** Rejected on the shared crate's rule above: it holds what **both ends
  implement identically**, measured rather than assumed — unpacking a package artifact is the
  Client's alone, and the Server explicitly never opens one (ADR-0017). Moving it would widen the
  shared crate to make a dependency arrow prettier.
- **A separate repository.** Rejected: the tools' correctness is defined by what the Client can
  install, and their tests assert exactly that by calling into it. One workspace keeps that check
  compiling; two repositories would replace it with a version constraint and a hope.
- **Duplicate the few items the tools need.** Rejected: `unix_mode_attributes` is a convention
  that must have one definition, and a second copy is how two of them drift apart.

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

- [`opamp-go`](https://github.com/open-telemetry/opamp-go) — the reference implementation, already
  this project's behavioural oracle under
  [ADR-0027](0027-interoperability-against-opamp-go.md). Its top-level layout is the closest thing
  to a direct answer to the question of what the shared crate holds, and it draws the same line:
  - shared: [`protobufs`](https://github.com/open-telemetry/opamp-go/tree/main/protobufs) (the
    generated types — our `opamp::proto`),
    [`protobufshelpers`](https://github.com/open-telemetry/opamp-go/tree/main/protobufshelpers)
    (`anyvaluehelpers.go`, helpers over `AnyValue` — precisely our `opamp::attributes`),
    and, in [`internal`](https://github.com/open-telemetry/opamp-go/tree/main/internal),
    `wsmessage.go` (our `opamp::frame`) and `limits.go` (our
    `frame::DEFAULT_MAX_MESSAGE_SIZE`);
  - **not shared:** the WebSocket and plain-HTTP machinery, which lives wholly inside
    [`client`](https://github.com/open-telemetry/opamp-go/tree/main/client) and
    [`server`](https://github.com/open-telemetry/opamp-go/tree/main/server). Two independent
    readings of the same specification arrived at the same boundary, which is the strongest evidence
    available that the boundary is in the protocol rather than in our taste.
  - One divergence worth noting: `internal/retryafter.go` is shared there. Here it is not a
    candidate — this Server never emits `RetryInfo`, so only the Client implements it
    (`client/supervisor/agent.rs`).
- [Cargo workspaces](https://doc.rust-lang.org/book/ch14-03-cargo-workspaces.html) and
  [RFC 2906, workspace dependency deduplication](https://rust-lang.github.io/rfcs/2906-cargo-workspace-deduplicate.html)
  — the mechanics this workspace already uses: one lockfile and one resolved version per dependency,
  which is what makes "a dependency added to `opamp` is added to both ends" literally true.
- Ecosystem convention for the same shape: the established Rust pattern is a *minimal* shared crate
  holding the contract — generated types and the codecs over them — with each end owning its own I/O
  stack, rather than a shared crate that grows toward the union of both
  ([Cargo workspace practice, 2026-08-09](https://reintech.io/blog/cargo-workspace-best-practices-large-rust-projects)).
- [ADR-0005](0005-workspace-and-server-runtime.md) ("More crates … premature"; modules until a
  concrete need) and the Client's library target above (the same rule applied with a
  measurement) — the in-repository precedent the shared crate's rule follows rather than invents.

- [ADR-0005](0005-workspace-and-server-runtime.md) (three crates) — the decision the tools crate
  amends.
- [The Cargo Book, workspaces](https://doc.rust-lang.org/cargo/reference/workspaces.html) — the
  shape being used: several crates, one lockfile, one `cargo build --workspace`, per-crate
  dependencies so a binary's dependency set is the crate's own.
- Comparable splits in this ecosystem — `rust-analyzer`'s `xtask`-style tooling crates, and
  Kubernetes' `kubectl` beside `kubelet` — where the operator's command-line tool is built and
  versioned with the system it drives but is not part of what runs on a managed node.

## Consequences

- Positive: the operations that write to a host — binary swap, health gate, rollback, tree install
  (ADR-0015) — become testable on Windows and macOS, where they are otherwise asserted by
  nothing. That is 22 tests going from one platform to three.
- Positive: the two workarounds disappear rather than being documented — the `#[path]` include in
  `opamp-package-sign` and the constants `self_update_e2e` restates. A restated constant is a
  correctness risk that comments can only mitigate.
- Positive: `cargo doc` produces something for the Client, which a binary's private modules never
  give it.

- Positive: the Baseline's fixed strings exist once. `service.name` becomes a constant whose misuse
  is a compile error rather than a silent mis-targeting of a package (ADR-0017).
- Positive: the gzip MUST and the post-decompression limit are implemented once, so the Gateway's
  plain-HTTP endpoint gets a Baseline rule it does not otherwise implement. This is the one place
  where the shared crate fixes a real gap rather than tidying one.
- Positive: the shared crate's charter is a stated rule — *what both ends implement identically* —
  instead of an accident of what was written first, so the next "should this be shared?" is answered
  by measuring.

- Positive: `crates/client` is only what runs on a managed host, and its dependency list says so.
  The tools have a manifest of their own, so a dependency added for a release-listing quirk is
  visibly the tools' and not the daemon's. `cargo build -p client` gets smaller — and measurably so
  in one place: the 7z **writer** (`sevenz-rust2/compress`) is not enabled for the whole workspace,
  because `pack` is not a bin target of the client crate; it is a dependency of the tools and of
  the Client's own tests only.

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

- Negative / trade-offs: `opamp` gains `flate2`, `rustls-pemfile` and `rustls`, so a bump to any of
  those recompiles both ends. Accepted: all three are already workspace dependencies of both
  binaries, so nothing new is linked into either artifact — only the rebuild graph widens.

- Negative / trade-offs: the same widening applies to what the crate gains later under this rule,
  and a **build** dependency is the cheaper case — it is compiled into the build script and linked
  into no artifact. The version helper is the first instance
  ([ADR-0009](0009-version-baking-comparison-and-display.md)): it puts `git2` in this crate's
  `[build-dependencies]`, which nothing ships. Judge each addition by what it costs the two ends,
  not by the count of modules.

- Negative / trade-offs: `opamp::endpoint` being framework-free means each caller still writes its
  own `match` from `BodyError` to a status code, so the two endpoints can still answer the same fault
  differently. Accepted deliberately — the Server answers `413`/`415`, and the Gateway is a hop whose
  status codes are its own business (ADR-0025); forcing them together would put axum in the protocol
  crate to save four lines.
- Negative / trade-offs: the shared crate's rule answers "no" to the larger question. Someone will
  ask again. The "what stays where it is" list above is the answer, and it is why it is written down.

- Negative / trade-offs: a fourth crate is a fourth manifest, and a workspace member whose only
  purpose is two binaries. The tools build the Client library to compile — which they did anyway,
  invisibly, as binaries inside it. One line of the release workflow changes, and any operator
  muscle memory of `cargo run -p client --bin …` becomes `cargo run -p package-tools --bin …` (or
  the unchanged `cargo run --bin …`, which resolves across the workspace).

- Follow-ups: whether `crates/opamp`'s and `crates/server`'s test helpers should be shared through a
  small internal test-support target once two suites want the same stub. And whether the Windows
  coverage this unlocks changes what the manual smoke checklist in `README.md` still has to claim —
  it states that real service registration cannot run in CI, which stays true, but the sentence
  sits next to claims about package installation that are no longer manual-only.

- Follow-ups: **a shared OpAMP server-endpoint abstraction** — one axum-based endpoint that both
  `server::transport` and `gateway` sit on, with a trait for the differing outbound side. Worth its
  own ADR only if a third OpAMP-server surface appears; two implementations that share a well-tested
  helper module are not yet a reason to invert a dependency. Nothing in this decision forecloses it.

- Follow-ups: whether a release should ship the operator tools at all — a release ships neither,
  and an operator builds them from a checkout, which the manual states plainly; if that changes,
  it is the release pipeline's decision to make, not this one's.
