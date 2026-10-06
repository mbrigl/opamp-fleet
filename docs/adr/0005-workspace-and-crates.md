# ADR-0005: Four-crate Cargo workspace on tokio and axum — the Client is a library under a thin binary, the shared crate holds what both ends implement identically, the package tools live in their own crate

- **Status:** 🟢 accepted
- **Date:** 2026-08-15
- **Deciders:** Markus Brigl

## Context

The [specification](../SPECIFICATION.md) fixes the language (both ends in Rust) and the deployables:
one **Server** (Linux only, API-first, with a rudimentary bundled UI) and one **Client** binary
covering all Client Modes ([ADR-0003](0003-client-modes-and-connection-multiplexing.md)). The code
lives in one Cargo workspace, and the toolchain is pinned to Rust stable via `rust-toolchain.toml`.
This decision fixes the crates of that workspace, the runtime and HTTP stack, how the Server exposes
its three surfaces — the OpAMP endpoint, the REST API, and the bundled UI — and what each crate
holds.

### Runtime and Server stack

- **Both transports on one endpoint.** The protocol serves plain HTTP and WebSocket on the same
  path (`/v1/opamp`, port 4320 by default), distinguished per request (see
  [`CONFORMANCE.md`](../CONFORMANCE.md)). The HTTP framework must therefore do protobuf request
  bodies and WebSocket upgrades on one route set.
- **Everything is async I/O.** The Server multiplexes many long-lived connections; the Client holds
  a Connection Pool and supervises processes. Rust async requires choosing an executor; the de-facto
  standard is `tokio`, and every serious Rust HTTP stack builds on it.
- **The UI must not grow a toolchain.** The specification bounds the UI to "rudimentary" and expects
  real UIs to live outside the project. A frontend build chain (npm, bundlers) would be a second
  toolchain to install, cache, and secure in CI for a UI that is explicitly not the product.
- **The Dev Container is deliberately lean** ([ADR-0002](0002-dev-container-runtime.md)): base
  Debian plus the Rust feature. Choices that demand extra system packages (OpenSSL headers, cmake,
  node) carry a real cost here.

### The Client needs a library target

Cargo's rules are fixed and not negotiable from our side. Integration tests "can use the public
API of the package's library"; there is no arrangement by which they link a binary target. Cargo
sets `CARGO_BIN_EXE_<name>` "only … when building an integration test or benchmark". A Client
without a library target therefore pays three ways:

- **Supervision tests become Unix-only for a reason that is not about Unix.** Unit tests inside a
  binary crate cannot spawn `stub_agent` — this project's own cross-platform test program, whose
  module doc says it exists so tests "behave identically on Linux, macOS, and Windows CI, no shell
  scripts". Measured, not assumed: `cargo test -p client --bin client` does not even build
  `stub_agent`. Such tests fall back to `/bin/sh -c` scripts gated `#[cfg(all(test, unix))]`. What
  goes untested on Windows is then the binary swap, the health gate, the rollback, and the whole
  ADR-0015 tree path — the operations that write to a host.
- **A second binary re-compiles a module by path.** A tool that must open an artifact with the same
  code the Client opens it with can only reach it through `#[cfg(test)] #[path = "../archive.rs"]`.
  The intent is right — a container the tool produces and the Client cannot open would be
  discovered at rollout time, on every matched host — but the mechanism is a workaround.
- **Integration tests restate constants they cannot import**, such as
  `EXIT_RESTART_FOR_UPDATE = 10` and the platform binary name.

The forces:

- **The specification's testability is not optional.** `AGENTS.md` §5 requires new behaviour to ship
  with tests and treats an untested platform as what it is. Three platforms are in scope
  ([ADR-0010](0010-client-os-service-and-installation-layout.md)).
- **A library target is not a new crate.** The modules stay where they are, and the package grows
  the target that makes them reachable — the smaller step clause 1's modules-first rule anticipates.
- **[ADR-0011](0011-supervisor-mode-and-lifecycle-port.md) already defines the seam.** The
  Ports are `ProcessCommand`/`ProcessEvent` and the `Plugin` factory; `Runner` is the adapter behind
  them. What a test needs to reach is exactly what that ADR calls the core — so this publishes a
  boundary that was designed, not one invented to make testing convenient.

### What the shared crate holds

`crates/opamp` is small beside `crates/client` and `crates/server`. That prompts a reasonable
question: should the *base* protocol implementation not live in the protocol crate, with only the
derived actions left in the two ends — the transports and mutual TLS included, since both are part
of the protocol? Its charter from [ADR-0006](0006-proto-vendoring-and-codegen.md) is the generated
protobuf types, the WebSocket framing, and the `instance_uid` type, so *"the two ends cannot drift
on the wire format"*.

So the question is not whether the shared crate is *small* — it is whether anything outside it is
written twice. Taking the [critical stance](../../AGENTS.md) that rule asks for means measuring
rather than reasoning from the category "this is protocol, therefore it is shared". The measurement
splits in two.

**What was genuinely one thing implemented twice (~250 lines):**

- **The Baseline's attribute keys, and the accessors over them.** Four near-identical helpers:
  `attr_value` (`crates/server/src/configs.rs`), `attr_map` plus a `lookup` closure
  (`crates/server/src/fleet.rs`), `reported_service_name` (`crates/server/src/packages.rs`), and
  `string_attr` (`crates/client/src/supervisor/agent.rs`). Around them sat some forty bare
  `"service.name"` / `"service.instance.name"` / `"service.namespace"` literals spread over
  `client/supervisor/agent.rs`, `client/config.rs`, `client/telemetry.rs`, `server/fleet.rs`,
  `server/packages.rs`, `server/labels.rs` and `server/api.rs`. These keys are fixed by the Baseline
  and given their meaning by [ADR-0022](0022-agent-type-instance-name-and-the-supervisor-name.md);
  [ADR-0021](0021-one-platform-vocabulary.md) and
  [ADR-0016](0016-a-package-is-a-versioned-set.md) make them load-bearing for
  *which binary a host is offered*. A typo in one of them is a silent mis-targeting, not a compile
  error.
- **The OpAMP endpoint's protocol shell.** `crates/server/src/transport.rs` and
  `crates/client/src/gateway/mod.rs` both serve `/v1/opamp` — the Client *is* an OpAMP server
  downstream in Gateway Mode ([ADR-0024](0024-gateway-mode.md)). Both restated the path, the
  `application/x-protobuf` media type and its `starts_with` check, the receive limit, the Baseline's
  "never truncate, never ship" rule for an oversized reply, and the 1009 close.
  `client/transport/http.rs` and `client/connection.rs` restated the media type a third and fourth
  time. The Baseline's gzip MUST — and with it the gzip-bomb rule, that the limit applies *after*
  decompression — existed in exactly one of those places: a rule that is stated once by accident is
  not a rule the second endpoint follows.
- **Reading a PEM certificate or key.** `read_certs`/`read_key` existed twice, some thirty lines
  each, in `crates/client/src/tls.rs` and `crates/server/src/tls.rs`.

**What is *not* duplication, however protocol-shaped it looks:**

- **The two transports.** The Client's side is a `tokio-tungstenite` connect with backoff plus a
  `reqwest` poll loop; the Server's is an axum router with a `WebSocketUpgrade` and a POST handler
  ([ADR-0007](0007-dual-transport-and-tls.md)). They share a *specification*, not a line of code.
  Moving both into `opamp` would relocate around 1,500 asymmetric lines and delete none, while the
  crate acquired axum, axum-server, tokio-tungstenite and reqwest.
- **Mutual TLS.** `server/ca.rs` signs certificate requests, `client/csr.rs` produces them and owns
  the renewal window, `server/tls.rs` is an `axum-server` `Accept` implementation that carries the
  handshake's peer certificate into a request
  ([ADR-0013](0013-opamp-endpoint-admission.md)). Each exists once. They
  are two ends of one flow, which is not the same thing as one implementation in two places. Only
  the PEM readers overlapped.
- **Server-offered connection settings** (`client/connection.rs`): merging an offer over what is in
  force, applying it over `client.toml`, and the verify-by-actually-connecting the Baseline demands
  are Client obligations the Server has no counterpart for.

Forces on the shared crate:

- **The specification's non-goals bound the ambition.** This project does not ship a reusable OpAMP
  library; every crate is `publish = false`. `opamp` is an internal seam, so "a protocol crate
  ought to contain the protocol" is an aesthetic argument here, not a requirement anyone can hold us
  to.
- **A crate boundary has a cost that a module boundary does not.** Every dependency `opamp` gains is
  gained by both ends, and every change to it recompiles both.
- **Simplicity first, and YAGNI.** The rejection of more crates applies in the other direction too:
  a *wider* shared crate is the same speculative structure, differently shaped.

### The operator package tools

Two operator command-line tools exist: `opamp-package-sign` (build, hash, sign an artifact) and
`opamp-package-fetch` (fetch an upstream agent release, verify it, hand it to the Server). Both are
*package management* tools: they exist to get software into a fleet, and nothing the Server or a
Client does at runtime depends on either. Carrying them inside `crates/client` costs:

- **The crate that ships the daemon would also carry operator tooling.** `cargo build -p client`
  would build an interactive downloader; a reader of `crates/client` would find a GitHub release
  client and a prompt library beside the supervision core. The Client is what runs on every managed
  host, and the boundary of that crate should say so.
- **The tools are not a Client concern at all.** They run on an operator's machine, and a release
  ships neither of them: the `.7z` artifacts and the `.deb`/`.rpm`/`.msi` installers carry the
  Client's own binary alone.
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

### The workspace and the Server stack

1. **One Cargo workspace with exactly four crates:** `opamp` (shared protocol library), `server`,
   `client`, and `package-tools` (clauses 19–23). The hexagonal seams from the specification live
   as modules; a module becomes a crate when a concrete need (compile time, reuse) appears, which is
   a reversible refactor.

2. **Runtime:** `tokio` (multi-threaded), `tracing` + `tracing-subscriber` for structured logging.

3. **Server HTTP:** `axum` with its `ws` feature — WebSocket upgrades and plain routes coexist on one
   router; `tower-http` middleware is available when needed.

4. **Serialization:** `serde` for the REST API's JSON and for configuration files.

5. **axum serves the OpAMP endpoint, the REST API, and the bundled UI:** `/v1/opamp` (OpAMP),
   `/api/*` (REST), `/` (UI). How they are split across listeners is decided by
   [ADR-0032](0032-agent-and-operator-planes-on-their-own-listeners.md). The REST API
   is the contract; the UI is a client of that API and nothing more.

6. **The UI ships as static assets embedded into the server binary** (`include_str!`), written in
   plain HTML/CSS/JS with no frontend toolchain.

7. **CI enforces the Definition of Done on this stack:** `cargo fmt --check`, `cargo clippy`
   (warnings denied), `cargo build`, `cargo test` on Linux; the Client is additionally built on
   Windows and macOS (the specification ships it on all three platforms, the Server on Linux only).

### The Client is a library with a thin binary on top

`crates/client` has a **library target** holding its modules, and `src/main.rs` is a thin binary
that calls into it.

8. **`src/lib.rs` declares the module tree; `src/main.rs` keeps only `fn main`** and whatever
   belongs to starting a process (argument parsing hand-off, the exit code). Giving the Client its
   library target moves no module on disk and no code between modules. The test stubs
   (`stub_agent`, `stub_crasher`) stay binaries of `crates/client` (clause 21); the operator tools
   live in `package-tools` (clause 19).

9. **Visibility is widened by need, not by default.** A module or item becomes `pub` when a test or
   another target in this package — or `package-tools` (clause 20) — has to reach it, and stays private otherwise. The library's
   public API is a *test and tooling* surface inside this workspace — `crates/client` is
   `publish = false`, so nothing here is a promise to anyone outside it. Where widening an item
   would expose an invariant that only the module can hold, the item stays private and the test
   moves to where it can see it.

10. **`opamp-package-sign` uses the library** for `archive`, never a `#[path = "../archive.rs"]`
    re-compilation of it.

11. **`self_update_e2e` imports what it would otherwise restate.** `EXIT_RESTART_FOR_UPDATE` and the
    platform binary name come from the library.

12. **The supervision tests are an integration test without a `unix` gate.** They drive `Runner`
    directly and spawn `stub_agent` instead of `/bin/sh`, which Cargo builds for them automatically
    ("Binaries are automatically built when the test is built"). Crash paths use the stub's
    `--exit-code`/`--exit-after-ms`; the default run sleeps until killed. Tests that assert on Unix
    file modes keep a `#[cfg(unix)]` on the assertion, because a mode is genuinely a Unix fact — the
    gate sits on what is actually platform-specific rather than on the whole file.

### What the shared crate holds

13. **`crates/opamp` holds what both ends implement identically** — established by measurement, not
    by whether a thing is conceptually "protocol". The rule is the decision; clauses 14–16 are what
    the measurement yields under it. They are a **finding, not a ceiling**: further code belongs in
    the crate whenever the same measurement supports it — something both ends implement identically,
    or would have to. Adding it is then ordinary work under this rule rather than a decision that
    has to overturn it. What the rule does refuse is the other move: putting something in the shared
    crate because it is conceptually "protocol", or to make the crate look less small.

14. **`opamp::attributes`** — no new dependencies.
    - Constants for the keys the Baseline fixes and this project matches on: `SERVICE_NAME`,
      `SERVICE_INSTANCE_NAME`, `SERVICE_NAMESPACE`, `SERVICE_VERSION`, `OS_TYPE`, `OS_DESCRIPTION`,
      `HOST_ARCH`.
    - `string_value(attrs: &[KeyValue], key: &str) -> Option<&str>` — the shared body of
      `configs::attr_value` and `packages::reported_service_name`, with the "an empty string is not
      a value" rule stated **once**. That rule is load-bearing in ADR-0016: an Agent reporting an
      empty type must not match an untyped package.
    - `string_attr(key: &str, value: &str) -> KeyValue`.
    - `server/fleet.rs::attr_map` **stays in the Server**: rendering non-string values in their
      debug form is a decision about the REST view
      ([ADR-0012](0012-selector-targeted-configurations-and-rest-api.md)), not about the
      protocol.

15. **`opamp::endpoint`** — adds `flate2` to the crate, a workspace dependency of both ends.
    Deliberately **framework-free**: it takes `&str` and `&[u8]` and returns owned bytes, so it
    pulls in no HTTP stack and both axum handlers can call it.
    - `OPAMP_PATH` (`/v1/opamp`) and `PROTOBUF_CONTENT_TYPE` (`application/x-protobuf`).
    - `is_protobuf(content_type: &str) -> bool`.
    - `decode_body(body: &[u8], content_encoding: &str, limit: usize) -> Result<Vec<u8>, BodyError>`
      — the Baseline's gzip MUST together with the post-decompression limit, in one place.
      `BodyError` distinguishes *unsupported encoding*, *undecodable gzip* and *too large*, so each
      caller maps it to the status code its own transport prescribes rather than inheriting the
      Server's.
    - Callers: `server/src/transport.rs`, `client/src/gateway/mod.rs`,
      `client/src/transport/http.rs`, `client/src/connection.rs`.

16. **`opamp::pem`** — adds `rustls-pemfile` and `rustls` (for `pki_types`). Both ends link both,
    so nothing new enters either binary.
    - `certificates(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, String>` and
      `private_key(pem: &[u8]) -> Result<PrivateKeyDer<'static>, String>`.
    - The path-based wrappers and their error wording stay in each end, where what the file
      *means* — a trust anchor, a listener's key, a client CA — is known.

17. **What stays where it is on this measurement:** the Client's two transports, the Server's router
    and its `Admission`, `server::ca::ClientCa`, `client::csr`, `server::tls::PeerCertAcceptor`, and
    `client::connection`. Each exists exactly once, so none of them is duplication to remove.
    Recorded so the question is not re-asked from the category alone — but a later measurement that
    finds one of them genuinely written twice moves it, without needing to supersede this decision.

18. **The extraction is behaviour-preserving.** No existing test changes; a suite that needs editing
    means something moved that should not have.

### The operator package tools live in their own crate

19. **`crates/package-tools/` holds `opamp-package-fetch` and `opamp-package-sign`**, depending on
    `client` as a library. The binary names do not change: they are documented, and
    `opamp-package-sign` is named in the release workflow.

20. **The dependency arrow points one way: `package-tools` → `client`.** The tools use the Client's
    `archive` and `tls` items rather than restating them, and their tests open what they produce
    with the Client's own unpacker. Nothing in `client`, `server`, or `opamp` ever depends on
    `package-tools`.

21. **The test stubs stay in `client`.** `stub_agent` and `stub_crasher` are fixtures the Client's
    own integration tests reach through `CARGO_BIN_EXE_*`, which resolves only within the crate
    that declares them. They are not operator tools and have no business moving.

22. **Tool-only dependencies belong to the tools.** Whatever serves only the tools — `dialoguer`'s
    prompts, the HTTP client's use for release listings — is declared by `package-tools`, so
    `cargo build -p client` does not build them. Where the Client needs the same crate for its own
    reasons (`dialoguer` for the interactive install, `reqwest` for the polling transport), both
    declare it; a workspace dependency is shared, not duplicated.

23. **The release workflow follows the crate.** Its packer step is
    `cargo build --release -p package-tools --bin opamp-package-sign --locked`.

## Alternatives considered

### Stack and Server surfaces

- **`actix-web`** — mature and fast, but it brings its own runtime flavour and actor heritage;
  axum is plain tokio + tower, matches `tokio-tungstenite` on the client side, and is the stack the
  OpenTelemetry Rust ecosystem gravitates to. No capability we need favours actix.
- **Raw `hyper`** — maximal control, but we would hand-write routing, upgrades, and extractors that
  axum provides as a thin layer over hyper anyway. More code for no protocol gain.
- **A frontend framework + bundler for the UI** — rejected. The specification caps the UI at
  rudimentary; a node toolchain in the Dev Container and CI is a large standing cost for a page that
  renders one fleet table and one config editor. Static embedded assets keep the server a single
  self-contained binary.
- **More crates (e.g. separate `fleet`, `ui`, per-plugin crates)** — premature. The hexagonal
  seams from the specification live as modules first; a module becomes a crate when a concrete need
  (compile time, reuse) appears, which is a reversible refactor.

### Reaching the Client from its tests

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
- **Move the supervision core into its own crate.** It would give the same reachability, and it is
  what clause 1 defers until there is a concrete need for a crate. The need here is a *test* that
  can link the code, which a library target inside the same package satisfies exactly; a crate
  boundary would additionally force every internal seam to become a public API, which is a larger
  and less reversible commitment than the problem asks for.
- **Port only the tests that need no executable artifact**, using each platform's shell (`/bin/sh`
  versus `cmd /C`). It leaves the binary swap, the rollback and the tree path — everything that
  writes to a host — Unix-only, which is the coverage that matters most on the platform with the
  least of it.
- **Rely on the manual smoke checklist** (`README.md`). That checklist covers service registration,
  which genuinely cannot run in CI; it does not cover a package swap, and extending it would move a
  repeatable check to a human at release time.

### The shared crate

- **Move the transports, TLS and mutual TLS into `opamp`** — rejected on the measurement above. It
  relocates ~1,500 lines that exist once, deletes no duplication, makes the protocol crate the union
  of both binaries' dependency sets, and turns every Server-only change into a Client recompile. It
  would also overturn the crate's charter (ADR-0006) in exchange for a structure that is tidier to
  describe and no safer to use. The reference implementation drew the same line (see Sources).
- **Leave the duplication in place** — rejected. Four copies of a media type, four attribute-lookup
  helpers and forty loose key literals are drift waiting to happen, and the gzip-bomb rule
  demonstrates the failure mode: a Baseline rule implemented on one of two endpoints that both
  accept bodies.
- **A further crate for the shared endpoint shell** — rejected as premature by clause 1's rule.
  The three modules fit the existing crate without changing what it meaningfully depends on.
- **Feature-gate `opamp::pem` behind a `tls` feature** — rejected. Both ends want it
  unconditionally; a feature that is always on is a knob nobody turns.
- **Unify `server::transport::serve_socket` and `gateway::serve_socket` behind one shared axum
  endpoint** — not rejected, deferred (see Follow-ups). The two loops differ in what drives their
  outbound side: a `watch` subscription over the fleet's desired state, versus an `mpsc` of replies
  coming back through the connection pool. Unifying them needs a trait *and* axum inside `opamp`,
  which is a materially larger decision than this one.

### Where the package tools live

- **Leave them in `crates/client`.** It costs nothing to type. Rejected: it is precisely the
  boundary problem above — the crate that runs on every managed host would keep carrying tooling
  that never runs there, and every reader of the Client would keep meeting it.
- **Move only `opamp-package-fetch`.** Half the change for half the benefit: `opamp-package-sign`
  is the *same kind of thing*, and splitting the two would leave a crate boundary that no one can
  state a rule for. If package tooling has a home, both tools are in it.
- **Move `archive` (and the provider helper) into the shared `opamp` crate so the tool crate need
  not depend on `client`.** Rejected on clause 13's rule: the shared crate holds what **both ends
  implement identically**, measured rather than assumed — unpacking a package artifact is the
  Client's alone, and the Server explicitly never opens one (ADR-0015). Moving it would widen the
  shared crate to make a dependency arrow prettier.
- **A separate repository.** Rejected: the tools' correctness is defined by what the Client can
  install, and their tests assert exactly that by calling into it. One workspace keeps that check
  compiling; two repositories would replace it with a version constraint and a hope.
- **Duplicate the few items the tools need.** Rejected: `unix_mode_attributes` is a convention
  that must have one definition, and a second copy is how two of them drift apart.

## Sources / Prior art

- [`axum`](https://docs.rs/axum/0.8) — router, extractors, and built-in WebSocket upgrade support
  (`ws` feature); maintained by the tokio project. Version 0.8 current on crates.io (checked
  2026-07-22).
- [`tokio`](https://tokio.rs/) — the de-facto standard async runtime for network services in Rust.
- Prior work in this repository's history (branch lineage at `719d49b` and `6fba83b`): an axum
  Server serving `/v1/opamp`, `/api/*`, and an embedded theme-aware `index.html` proved this exact
  composition end to end, including the Bindplane-style fleet table UI this project's bundled UI
  follows.
- [Bindplane](https://bindplane.com/) — the look-and-feel reference for the rudimentary UI (fleet
  table, status chips, config drawer); a design reference, not a dependency.
- [ADR-0002](0002-dev-container-runtime.md) — the lean-container constraint that penalizes stacks
  needing system packages or a second toolchain.
- **The Cargo Book, environment variables** — `CARGO_BIN_EXE_<name>`: "The absolute path to a binary
  target's executable. **This is only set when building an integration test or benchmark.** … Binaries
  are automatically built when the test is built, unless the binary has required features that are
  not enabled." The first sentence is why unit tests cannot reach the stub; the last is why an
  integration test needs no extra wiring to get it.
  <https://doc.rust-lang.org/cargo/reference/environment-variables.html>
- **The Cargo Book, Cargo targets** — "Integration tests can use the public API of the package's
  library", and separately, "Binaries can use the public API of the package's library". There is no
  arrangement under which a test links a binary target; the library is the only seam Cargo offers
  for either consumer.
  <https://doc.rust-lang.org/cargo/reference/cargo-targets.html>
- [The Cargo Book, workspaces](https://doc.rust-lang.org/cargo/reference/workspaces.html) — several
  crates, one lockfile, one `cargo build --workspace`, per-crate dependencies so a binary's
  dependency set is the crate's own.
- [Cargo workspaces](https://doc.rust-lang.org/book/ch14-03-cargo-workspaces.html) and
  [RFC 2906, workspace dependency deduplication](https://rust-lang.github.io/rfcs/2906-cargo-workspace-deduplicate.html)
  — one lockfile and one resolved version per dependency, which is what makes "a dependency added to
  `opamp` is added to both ends" literally true.
- **This workspace's own precedent.** `crates/server` is a library with `src/main.rs` on top, which
  is why `crates/server/tests/` can drive it directly and why `self_update_e2e` can run a real
  Server in-process. The Client has the same shape.
- **`ripgrep`** ships `crates/core` as a library with a thin `main.rs`, and its own binary is one
  consumer among several — the widely-copied form of "a binary is a thin shell over a library",
  adopted there for the same reason: what is worth testing is not reachable through a `main`.
  <https://github.com/BurntSushi/ripgrep>
- [`opamp-go`](https://github.com/open-telemetry/opamp-go) — the reference implementation, this
  project's behavioural oracle under
  [ADR-0004](0004-protocol-baseline-and-conformance.md). Its top-level layout is the closest thing
  to a direct answer to what the shared crate should hold, and it draws the same line:
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
- Ecosystem convention for the same shape: the established Rust pattern is a *minimal* shared crate
  holding the contract — generated types and the codecs over them — with each end owning its own I/O
  stack, rather than a shared crate that grows toward the union of both
  ([Cargo workspace practice, 2026-08-09](https://reintech.io/blog/cargo-workspace-best-practices-large-rust-projects)).
- Comparable splits in this ecosystem — `rust-analyzer`'s `xtask`-style tooling crates, and
  Kubernetes' `kubectl` beside `kubelet` — where the operator's command-line tool is built and
  versioned with the system it drives but is not part of what runs on a managed node.

## Consequences

- Positive: one workspace, one lockfile, one pinned toolchain; the Server is a single static-ish
  binary embedding its UI; client and server share the `opamp` crate so the two ends cannot drift
  apart on the wire types.
- Positive: axum's `ws` feature makes the dual-transport endpoint
  ([ADR-0007](0007-dual-transport-and-tls.md)) a routing concern rather than an architectural one.
- Positive: the operations that write to a host — binary swap, health gate, rollback, tree install
  (ADR-0015) — are testable on Windows and macOS as well as Linux.
- Positive: no `#[path]` include and no restated constant. A restated constant is a correctness risk
  that comments can only mitigate.
- Positive: `cargo doc` documents the Client, which a binary's private modules would not allow.
- Positive: the Baseline's fixed strings exist once. `service.name` is a constant whose misuse is a
  compile error rather than a silent mis-targeting of a package (ADR-0021, ADR-0016).
- Positive: the gzip MUST and the post-decompression limit are implemented once, so the Gateway's
  plain-HTTP endpoint follows the same Baseline rule as the Server's.
- Positive: the shared crate's charter is a stated rule — *what both ends implement identically* —
  instead of an accident of what was written first, so the next "should this be shared?" is
  answered by measuring.
- Positive: `crates/client` is only what runs on a managed host, and its dependency list says so.
  The tools have a manifest of their own, so a dependency added for a release-listing quirk is
  visibly the tools' and not the daemon's. The 7z **writer** (`sevenz-rust2/compress`) is a
  dependency of the tools and of the Client's own tests, not of the Client build.
- Negative / trade-offs: committing to tokio/axum is a deep dependency commitment — reversing it
  would touch every I/O boundary. Accepted: this is the mainstream Rust stack with the largest
  maintenance surface behind it.
- Negative / trade-offs: an embedded UI means a UI change requires a server rebuild. Accepted — the
  UI is rudimentary by charter, and this keeps deployment to one artifact.
- Negative / trade-offs: **items become `pub` that are not a public API in any meaningful sense.**
  The package is `publish = false`, so this binds nobody outside the workspace — but "pub" stops
  meaning "part of the interface" inside the crate, and the compiler no longer warns about code that
  has quietly stopped being used. This is the real cost of clause 9, and the part most worth
  reviewing.
- Negative / trade-offs: the supervision tests sit in `tests/` rather than beside the code they
  test, which is a longer reach when reading. Unit tests that need module internals and are
  genuinely platform-neutral stay where they are; only those that need to spawn a real program live
  in `tests/`.
- Negative / trade-offs: `opamp` carries `flate2`, `rustls-pemfile` and `rustls`, so a bump to any
  of those recompiles both ends. Accepted: all three are workspace dependencies of both binaries, so
  nothing new is linked into either artifact — only the rebuild graph widens.
- Negative / trade-offs: the same widening applies to what the crate gains later under clause 13,
  and a **build** dependency is the cheaper case — it is compiled into the build script and linked
  into no artifact. The version helper is one instance
  ([ADR-0009](0009-version-from-cargo-toml-and-git.md)): it puts `git2` in this crate's
  `[build-dependencies]`, which nothing ships. Judge each addition by what it costs the two ends,
  not by the count of modules.
- Negative / trade-offs: `opamp::endpoint` being framework-free means each caller still writes its
  own `match` from `BodyError` to a status code, so the two endpoints can still answer the same fault
  differently. Accepted deliberately — the Server answers `413`/`415`, and the Gateway is a hop whose
  status codes are its own business (ADR-0024); forcing them together would put axum in the protocol
  crate to save four lines.
- Negative / trade-offs: clause 17 answers "no" to the larger question of a wider protocol crate.
  Someone will ask again. The list in clause 17 is the answer, and it is why it is written down.
- Negative / trade-offs: a fourth crate is a fourth manifest, and a workspace member whose only
  purpose is two binaries. The tools build the Client library to compile. `cargo run -p client
  --bin …` does not reach the tools; `cargo run -p package-tools --bin …` (or `cargo run --bin …`,
  which resolves across the workspace) does.
- Follow-ups: an OpenAPI description for the REST API (specification goal 5) needs a decision on how
  it is authored or generated once the API grows past its seed.
- Follow-ups: whether `crates/opamp`'s and `crates/server`'s test helpers should be shared through a
  small internal test-support target once two suites want the same stub. And whether the Windows
  coverage of clause 12 changes what the manual smoke checklist in `README.md` still has to claim —
  real service registration cannot run in CI, which stays true, but package installation is no
  longer manual-only.
- Follow-ups: **a shared OpAMP server-endpoint abstraction** — one axum-based endpoint that both
  `server::transport` and `gateway` sit on, with a trait for the differing outbound side. Worth its
  own ADR only if a third OpAMP-server surface appears; two implementations that share a well-tested
  helper module are not yet a reason to invert a dependency. Nothing here forecloses it.
- Follow-ups: whether a release should ship the operator tools at all — it ships neither, and an
  operator builds them from a checkout, which the manual states plainly; if that changes, it is the
  release pipeline's decision to make, not this one's.
