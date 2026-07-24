# ADR-0011: Four crates in one Cargo workspace on tokio and axum, a shared crate by measurement, and TOML configuration

- **Status:** 🟢 accepted
- **Date:** 2026-08-15
- **Deciders:** Markus Brigl
- **Applies to:** Cargo.toml, crates/opamp/, crates/client/src/lib.rs and main.rs, crates/package-tools/, the bundled UI under crates/server/static/, server.toml and supervisor.toml, and every new crate, module placement or dependency

## Context

The [specification](../SPECIFICATION.md) fixes the language (both ends in Rust) and the deployables:
one Server (Linux only, API-first, with a rudimentary bundled UI) and one Client binary covering every
Client Mode ([ADR-0009](0009-client-modes-and-the-gateway.md)). Everything is async I/O: the Server
holds many long-lived connections, the Client a Connection Pool and supervised processes. The Server
must serve protobuf bodies and WebSocket upgrades on one route set. The Dev Container is lean
([ADR-0002](0002-dev-container-runtime.md)), so a choice that needs OpenSSL headers, cmake or node
carries a real cost, and the UI is explicitly not the product.

Three questions recur once the code exists. **What goes into the shared crate?** Conceptually
everything "protocol" might, but a crate boundary costs what a module boundary does not: every
dependency of `opamp` is gained by both ends, and every change to it recompiles both. Measured, the
two transports and mutual TLS are two ends of one flow, each written once; what was genuinely
written twice was the Baseline's attribute keys and the lookups over them, the OpAMP endpoint's
protocol shell (path, media type, gzip and its post-decompression limit), and the PEM readers.
**How does a test reach the Client?** Cargo hands a test a helper binary's path
(`CARGO_BIN_EXE_<name>`) only in an integration test, and an integration test links only a
package's library. **Where do operator tools live?** `opamp-package-sign` and `opamp-package-fetch`
run on an operator's machine, ship in no release, and use a few Client items: the 7z Unix-mode
convention, the unpackers, and the TLS provider helper.

Operators hand-edit both configuration files, often over SSH; a format's failure modes matter more
than its expressiveness. YAML's indentation and implicit typing (`no` → `false`) are classic sources
of silent fleet misconfiguration. The payloads the Server distributes are the Managed Process's own
format and are not affected.

## Decision

We will build one Cargo workspace of four crates (`opamp`, `server`, `client`, `package-tools`) on
`tokio` and `axum`, keep in `opamp` exactly what both ends implement identically as measured, make
the Client a library under a thin binary, keep the operator tools in their own crate depending on the
Client, and configure both binaries from strict TOML files.

1. **One workspace, four crates, one lockfile, one toolchain.** `crates/opamp` (the shared wire
   layer), `crates/server`, `crates/client` and `crates/package-tools`. Versions are pinned once in
   `[workspace.dependencies]`; `rust-toolchain.toml` pins the compiler. Every crate is
   `publish = false`. A further crate needs a concrete need a module cannot meet (compile time,
   reuse, a dependency boundary); hexagonal seams live as modules first.

2. **`tokio` and `tracing`.** The multi-threaded `tokio` runtime on both ends; `tracing` with
   `tracing-subscriber` for structured logging; `serde` for the REST API's JSON and for configuration
   files.

3. **`axum` is the workspace's HTTP server stack.** The Server's OpAMP endpoint, REST API and UI are
   axum routes, with the `ws` feature so upgrades and plain routes share one router; the Client's
   Gateway endpoint uses it too. How the routes are bound to listeners is
   [ADR-0012](0012-transports-tls-and-the-servers-two-planes.md)'s.

4. **The bundled UI is static assets embedded in the Server binary.** Plain HTML, CSS and JS under
   `crates/server/static/`, embedded with `include_str!`; no frontend toolchain. The REST API is the
   contract, and the UI is one client of it.

5. **CI enforces the Definition of Done on this stack.** `cargo build`, `cargo test`,
   `cargo fmt --check` and `cargo clippy` with warnings denied on Linux; the Client is also checked
   and tested on Windows and macOS, and release-built on all three; the Server is built on Linux only.

6. **The shared crate holds what both ends implement identically, established by measurement.** Code
   enters `opamp` when both ends implement it identically, or would have to; adding it is ordinary
   work under this clause. Nothing enters because it is conceptually "protocol" or to make the crate
   look less small. Each addition is judged by what it costs the two ends; a build dependency is the
   cheaper case, since it links into no artifact.

7. **What the measurement places in `opamp`.**
   - `proto` and `frame`: the generated types and the WebSocket framing
     ([ADR-0010](0010-protocol-baseline-and-conformance.md)); `uid`: the `instance_uid` type.
   - `attributes`: constants for the Baseline keys this project matches on (`SERVICE_NAME`,
     `SERVICE_INSTANCE_NAME`, `SERVICE_NAMESPACE`, `SERVICE_VERSION`, `OS_TYPE`, `OS_DESCRIPTION`,
     `HOST_ARCH`) and the accessors over them. `string_value` states once that an empty string is
     not a value.
   - `endpoint`: `OPAMP_PATH` (`/v1/opamp`), `PROTOBUF_CONTENT_TYPE`, `is_protobuf`, and
     `decode_body`, which applies the Baseline's gzip MUST with the size limit enforced *after*
     decompression. It is framework-free (bytes in, bytes out), and its `BodyError` separates
     unsupported encoding, undecodable gzip and too large, so each caller maps it to its own status
     code.
   - `pem`: `certificates` and `private_key` from PEM bytes. The path-based wrappers and their error
     wording stay in each end, where what the file means is known.
   - `version`: the version helper ([ADR-0013](0013-versions.md)).

8. **What the measurement leaves where it is.** The Client's two transports, the Server's router and
   admission, the Server's CA and peer-certificate acceptor, the Client's CSR flow and connection
   settings, and the Server's `attr_map` (a decision about the REST view). Each exists once. A later
   measurement that finds one written twice moves it under clause 6.

9. **The Client is a library with a thin binary on top.** `crates/client/src/lib.rs` declares the
   module tree; `src/main.rs` keeps only what starting a process needs: parsing the command line,
   handing off to the daemon or the `service` verbs, and the exit code.

10. **Visibility is widened by need.** An item becomes `pub` when a test or another workspace target
    has to reach it. `pub` in the Client means "another target here reaches it", not a published
    interface. Where widening would expose an invariant only the module can hold, the item stays
    private and the test moves to where it can see it.

11. **Tests reach what they test through the library.** Integration tests import constants rather
    than restating them, and tests that spawn a real program spawn the Client's own cross-platform
    stubs (`stub_agent`, `stub_crasher`, under `crates/client/src/bin/`) instead of a shell, so they
    run on all three platforms. A `#[cfg(unix)]` gate sits only on what is genuinely a Unix fact,
    such as a file mode. The stubs stay in `client`, because `CARGO_BIN_EXE_*` resolves only inside
    the crate that declares them.

12. **The operator package tools are their own crate, depending on the Client.**
    `crates/package-tools` produces `opamp-package-fetch` and `opamp-package-sign` and has no library.
    The arrow points one way: `package-tools` uses `client::archive` and `client::tls` rather than
    restating them, and its tests open what the tools produce with the Client's own unpacker. Nothing
    in `client`, `server` or `opamp` depends on `package-tools`. Tool-only dependencies (the 7z
    writer, release listing) are declared there, so `cargo build -p client` does not build them.

13. **Both binaries are configured from TOML.** `server.toml` and `supervisor.toml`, parsed with the
    `toml` crate into `serde` structs. Each binary takes the path from `--config`, defaulting to the
    file of that name in the working directory, and runs on defaults when the file does not exist.
    The file is the whole configuration: no setting is read from the environment (`RUST_LOG`
    filters log output and configures nothing else).

14. **A configuration that does not parse fails startup, loudly.** Every setting has a documented
    default; unknown keys are rejected (`deny_unknown_fields`, or the equivalent strict parse for a
    `[[supervisor]]` block's kind-specific keys), and an invalid value or half-written setting is an
    error naming it. The same holds for files a binary reads at startup to restore its state: one
    that does not parse fails startup rather than being skipped or silently defaulted.

**Out of scope:** where an installed service looks for its configuration file on each OS
([ADR-0014](0014-the-client-as-an-installed-service.md)); the listener layout
([ADR-0012](0012-transports-tls-and-the-servers-two-planes.md)); whether a release ships the operator
tools.

## Alternatives considered

- **`actix-web`.** Its own runtime flavour and actor heritage; axum is plain `tokio` and `tower`, and
  is where the OpenTelemetry Rust ecosystem sits.
- **Raw `hyper`.** We would hand-write routing, upgrades and extractors that axum provides as a thin
  layer over hyper anyway.
- **A frontend framework and bundler for the UI.** A node toolchain in the Dev Container and CI is a
  large standing cost for a page the specification caps at rudimentary.
- **Transports and mutual TLS in `opamp`.** It would relocate about 1,500 lines that exist once,
  delete no duplication, make the shared crate the union of both binaries' dependencies, and turn
  every Server-only change into a Client recompile. `opamp-go` draws the same line: generated types,
  helpers and framing shared; client and server machinery not.
- **A shared axum endpoint for the Server and the Gateway.** Their outbound sides differ (a watch over
  desired state versus replies through a pool); unifying them needs a trait and axum inside `opamp`.
  Worth its own decision only if a third OpAMP-server surface appears.
- **Feature-gating `opamp::pem`.** Both ends want it unconditionally.
- **Resolving the stub from the test binary's directory.** The suite would pass or fail depending on
  the command that ran it.
- **Moving the supervision core into its own crate for testability.** A library target meets the need
  without turning every internal seam into a crate API.
- **Operator tools inside `client`, or `archive` moved into `opamp`.** The first keeps tooling in the
  crate that runs on every managed host; the second widens the shared crate with something only the
  Client implements, since the Server never opens an artifact.
- **A separate repository for the tools, or duplicated items.** The tools' correctness is defined by
  what the Client can install, and `unix_mode_attributes` must have one definition.
- **YAML.** The OpenTelemetry ecosystem's format, but with indentation and implicit-typing failure
  modes in hand-edited files, and `serde_yaml` is unmaintained.
- **JSON, or environment variables and flags only.** JSON has no comments; flags cannot carry the
  Client's per-Supervisor structure, and a file is what an installer lays down and an operator diffs.

## Sources / Prior art

- [`axum`](https://docs.rs/axum/0.8) and [`tokio`](https://tokio.rs/) — the router with built-in
  WebSocket upgrade, and the de-facto async runtime.
- [Bindplane](https://bindplane.com/) — the look-and-feel reference for the bundled UI (fleet table,
  status chips, config drawer).
- [`opamp-go`](https://github.com/open-telemetry/opamp-go) — `protobufs`, `protobufshelpers` and
  `internal` (`wsmessage.go`, `limits.go`) shared; the WebSocket and HTTP machinery inside `client`
  and `server`.
- [The Cargo Book, environment variables](https://doc.rust-lang.org/cargo/reference/environment-variables.html)
  — `CARGO_BIN_EXE_<name>` is set only for integration tests and benchmarks, which build the
  package's binaries automatically; [Cargo targets](https://doc.rust-lang.org/cargo/reference/cargo-targets.html)
  — integration tests and binaries use the package's library;
  [workspaces](https://doc.rust-lang.org/cargo/reference/workspaces.html) and
  [RFC 2906](https://rust-lang.github.io/rfcs/2906-cargo-workspace-deduplicate.html) — one lockfile,
  one resolved version per dependency.
- [`ripgrep`](https://github.com/BurntSushi/ripgrep) — a library with a thin `main.rs`; `kubectl`
  beside `kubelet` — an operator tool built with the system it drives but not run on a node.
- [TOML v1.0](https://toml.io/), the [`toml` crate](https://crates.io/crates/toml), and the
  [`serde_yaml` deprecation notice](https://github.com/dtolnay/serde-yaml);
  [`opampsupervisor` configuration](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/specification/README.md)
  — the YAML prior art not followed for this project's own files.

## Consequences

- Positive: one lockfile and one toolchain, and both ends share `opamp`, so they cannot drift on the
  wire types, the Baseline's fixed strings, or the gzip rule. A misspelt attribute key is a compile
  error rather than a silent mis-targeting.
- Positive: the Server is one binary embedding its UI. The operations that write to a host (binary
  swap, health gate, rollback, tree install) are tested on all three platforms.
- Positive: `crates/client` is what runs on a managed host, and its manifest says so.
- Negative / trade-offs: `tokio` and axum are a deep commitment; reversing them touches every I/O
  boundary. A UI change needs a Server rebuild.
- Negative / trade-offs: every dependency of `opamp` recompiles both ends. `endpoint` being
  framework-free means each caller writes its own mapping from `BodyError` to a status code.
- Negative / trade-offs: `pub` inside the Client does not mean "interface", and the compiler does
  not warn about a `pub` item nothing uses.
- Negative / trade-offs: operators coming from OpenTelemetry meet a second format for the fleet
  tooling itself.
- Follow-ups: a shared test-support target if two suites want the same helpers; a shared
  OpAMP-server endpoint if a third surface appears; environment-variable overrides if containerised
  deployments need them.

## Enforcement

- [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml): `build-test-lint` (clause 5),
  `client-platform-check` on Windows and macOS, and `release-build` (clauses 5, 11). The workspace
  manifest lists the four members, and Cargo refuses a dependency cycle, so `client` cannot come to
  depend on `package-tools` (clause 12).
- [`crates/client/tests/supervisor_process.rs`](../../crates/client/tests/supervisor_process.rs)
  spawns `stub_agent` through `CARGO_BIN_EXE_stub_agent`, and
  [`crates/client/tests/self_update_e2e.rs`](../../crates/client/tests/self_update_e2e.rs) imports
  `client::selfupdate::EXIT_RESTART_FOR_UPDATE` (clauses 9, 11).
- `crates/opamp/src/attributes.rs` `an_empty_string_is_not_a_value`;
  `crates/opamp/src/endpoint.rs` `a_gzip_bomb_buys_no_more_memory_than_a_plain_body_would`,
  `what_is_not_gzip_under_a_gzip_header_is_refused`,
  `an_encoding_this_endpoint_does_not_implement_names_itself`; `crates/opamp/src/pem.rs`
  `a_file_holding_no_certificate_is_an_error` (clause 7).
- `crates/server/src/config.rs` `rejects_unknown_keys` and `crates/client/src/config.rs`
  `rejects_an_unknown_scheme_and_unknown_keys` (clause 14).

**Not mechanically decidable:** whether a piece of code is implemented identically by both ends
(clauses 6, 8), and whether a widened item is needed (clause 10), are judgements made in review.
