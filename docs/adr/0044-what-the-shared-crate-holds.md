# ADR-0044: The shared crate holds what both ends implement *identically* — measured, not by category

- **Status:** 🟡 proposed
- **Date:** 2026-08-09
- **Deciders:** Markus Brigl

## Context

Most of this project's code sits in `crates/client` and `crates/server`; `crates/opamp` holds
roughly 500 lines. That prompts a reasonable question: should the *base* protocol implementation not
live in the protocol crate, with only the derived actions left in the two ends — the transports and
mutual TLS included, since both are part of the protocol?

The crate's charter today comes from [ADR-0005](0005-workspace-and-server-runtime.md) and
[ADR-0006](0006-proto-vendoring-and-codegen.md): the generated protobuf types, the WebSocket
framing, and the `instance_uid` type, so *"the two ends cannot drift on the wire format"*. ADR-0005
also rejected further crates as premature and said the hexagonal seams live as modules until a
concrete need appears; [ADR-0024](0024-client-library-target.md) followed exactly that rule when
`crates/client` grew a library target for a measured reason.

So the question is not whether the shared crate is *small* — it is whether anything outside it is
written twice. Taking the [critical stance](../../AGENTS.md) that rule asks for means measuring
rather than reasoning from the category "this is protocol, therefore it is shared". The measurement
splits in two.

### What is genuinely one thing implemented twice (~250 lines)

- **The Baseline's attribute keys, and the accessors over them.** Four near-identical helpers exist:
  `attr_value` (`crates/server/src/configs.rs`), `attr_map` plus a `lookup` closure
  (`crates/server/src/fleet.rs`), `reported_service_name` (`crates/server/src/packages.rs`), and
  `string_attr` (`crates/client/src/supervisor/agent.rs`). Around them sit some forty bare
  `"service.name"` / `"service.instance.name"` / `"service.namespace"` literals spread over
  `client/supervisor/agent.rs`, `client/config.rs`, `client/telemetry.rs`, `server/fleet.rs`,
  `server/packages.rs`, `server/labels.rs` and `server/api.rs`. These keys are fixed by the Baseline
  and given their meaning by [ADR-0033](0033-an-agents-type-and-its-instance-name-are-two-attributes.md);
  [ADR-0031](0031-per-platform-package-variants.md) and
  [ADR-0034](0034-a-package-states-the-agent-type-it-is-built-for.md) make them load-bearing for
  *which binary a host is offered*. A typo in one of them is a silent mis-targeting, not a compile
  error.

- **The OpAMP endpoint's protocol shell.** `crates/server/src/transport.rs` and
  `crates/client/src/gateway/mod.rs` both serve `/v1/opamp` — the Client *is* an OpAMP server
  downstream in Gateway Mode ([ADR-0037](0037-gateway-mode.md)). Both restate the path, the
  `application/x-protobuf` media type and its `starts_with` check, the receive limit, the Baseline's
  "never truncate, never ship" rule for an oversized reply, and the 1009 close.
  `client/transport/http.rs` and `client/connection.rs` restate the media type a third and fourth
  time. The Baseline's gzip MUST — and with it the gzip-bomb rule, that the limit applies *after*
  decompression — exists in exactly one of those places, which is the more interesting half of the
  finding: a rule that is stated once by accident is not a rule the second endpoint follows.

- **Reading a PEM certificate or key.** `read_certs`/`read_key` exist twice, some thirty lines each,
  in `crates/client/src/tls.rs` and `crates/server/src/tls.rs`.

### What is *not* duplication, however protocol-shaped it looks

- **The two transports.** The Client's side is a `tokio-tungstenite` connect with backoff plus a
  `reqwest` poll loop; the Server's is an axum router with a `WebSocketUpgrade` and a POST handler
  ([ADR-0007](0007-dual-transport-and-tls.md)). They share a *specification*, not a line of code.
  Moving both into `opamp` would relocate around 1,500 asymmetric lines and delete none, while the
  crate acquired axum, axum-server, tokio-tungstenite and reqwest.

- **Mutual TLS.** `server/ca.rs` signs certificate requests, `client/csr.rs` produces them and owns
  the renewal window, `server/tls.rs` is an `axum-server` `Accept` implementation that carries the
  handshake's peer certificate into a request
  ([ADR-0035](0035-mutual-tls-and-the-server-issued-client-certificate.md)). Each exists once. They
  are two ends of one flow, which is not the same thing as one implementation in two places. Only
  the PEM readers overlap.

- **Server-offered connection settings** (`client/connection.rs`): merging an offer over what is in
  force, applying it over `client.toml`, and the verify-by-actually-connecting the Baseline demands
  are Client obligations the Server has no counterpart for.

### Forces

- **The specification's non-goals bound the ambition.** This project does not ship a reusable OpAMP
  library; all three crates are `publish = false`. `opamp` is an internal seam, so "a protocol crate
  ought to contain the protocol" is an aesthetic argument here, not a requirement anyone can hold us
  to.
- **A crate boundary has a cost that a module boundary does not.** Every dependency `opamp` gains is
  gained by both ends, and every change to it recompiles both.
- **Simplicity first, and YAGNI.** ADR-0005's rejection of more crates applies in the other
  direction too: a *wider* shared crate is the same speculative structure, differently shaped.

## Decision

We will treat `crates/opamp` as holding **what both ends implement identically** — established by
measurement, not by whether a thing is conceptually "protocol".

The rule is the decision; the list below is what today's measurement yields under it. Three modules
follow from it now, and they are a **finding, not a ceiling**: further code belongs in the crate
whenever the same measurement supports it — something both ends implement identically, or would
have to. Adding it is then ordinary work under this ADR rather than a decision that has to overturn
it. What the rule does refuse is the other move: putting something in the shared crate because it
is conceptually "protocol", or to make the crate look less small.

1. **`opamp::attributes`** — no new dependencies.
   - Constants for the keys the Baseline fixes and this project matches on: `SERVICE_NAME`,
     `SERVICE_INSTANCE_NAME`, `SERVICE_NAMESPACE`, `SERVICE_VERSION`, `OS_TYPE`, `OS_DESCRIPTION`,
     `HOST_ARCH`.
   - `string_value(attrs: &[KeyValue], key: &str) -> Option<&str>` — the shared body of
     `configs::attr_value` and `packages::reported_service_name`, with the "an empty string is not a
     value" rule stated **once**. That rule is load-bearing in ADR-0034: an Agent reporting an empty
     type must not match an untyped package.
   - `string_attr(key: &str, value: &str) -> KeyValue` — moved from
     `client/supervisor/agent.rs`.
   - `server/fleet.rs::attr_map` **stays in the Server**: rendering non-string values in their debug
     form is a decision about the REST view ([ADR-0012](0012-selector-targeted-configurations-and-openapi-rest-api.md)),
     not about the protocol.

2. **`opamp::endpoint`** — adds `flate2` to the crate, already a workspace dependency of both ends.
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

3. **`opamp::pem`** — adds `rustls-pemfile` and `rustls` (for `pki_types`). Both ends already link
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

## Alternatives considered

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

## Sources / Prior art

- [`opamp-go`](https://github.com/open-telemetry/opamp-go) — the reference implementation, already
  this project's behavioural oracle under
  [ADR-0040](0040-interoperability-against-opamp-go.md). Its top-level layout is the closest thing
  to a direct answer to the question this ADR asks, and it draws the same line:
  - shared: [`protobufs`](https://github.com/open-telemetry/opamp-go/tree/main/protobufs) (the
    generated types — our `opamp::proto`),
    [`protobufshelpers`](https://github.com/open-telemetry/opamp-go/tree/main/protobufshelpers)
    (`anyvaluehelpers.go`, helpers over `AnyValue` — precisely our proposed `opamp::attributes`),
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
  concrete need) and [ADR-0024](0024-client-library-target.md) (the same rule applied with a
  measurement) — the in-repository precedent this ADR follows rather than invents.

## Consequences

- Positive: the Baseline's fixed strings exist once. `service.name` becomes a constant whose misuse
  is a compile error rather than a silent mis-targeting of a package (ADR-0031, ADR-0034).
- Positive: the gzip MUST and the post-decompression limit are implemented once, so the Gateway's
  plain-HTTP endpoint gets a Baseline rule it does not currently implement. This is the one place
  where the change fixes a real gap rather than tidying one.
- Positive: the crate's charter becomes a stated rule — *what both ends implement identically* —
  instead of an accident of what was written first, so the next "should this be shared?" is answered
  by measuring.
- Negative / trade-offs: `opamp` gains `flate2`, `rustls-pemfile` and `rustls`, so a bump to any of
  those recompiles both ends. Accepted: all three are already workspace dependencies of both
  binaries, so nothing new is linked into either artifact — only the rebuild graph widens.
- Negative / trade-offs: the same widening applies to what the crate gains later under this rule,
  and a **build** dependency is the cheaper case — it is compiled into the build script and linked
  into no artifact. The version helper is the first instance
  ([ADR-0045](0045-the-version-helper-lives-in-the-shared-crate.md)): it puts `git2` in this crate's
  `[build-dependencies]`, which nothing ships. Judge each addition by what it costs the two ends,
  not by the count of modules.
- Negative / trade-offs: `opamp::endpoint` being framework-free means each caller still writes its
  own `match` from `BodyError` to a status code, so the two endpoints can still answer the same fault
  differently. Accepted deliberately — the Server answers `413`/`415`, and the Gateway is a hop whose
  status codes are its own business (ADR-0037); forcing them together would put axum in the protocol
  crate to save four lines.
- Negative / trade-offs: this answers "no" to the larger question. Someone will ask again. The
  "what stays where it is" list above is the answer, and it is why it is written down.
- Follow-ups: **a shared OpAMP server-endpoint abstraction** — one axum-based endpoint that both
  `server::transport` and `gateway` sit on, with a trait for the differing outbound side. Worth its
  own ADR only if a third OpAMP-server surface appears; two implementations that share a well-tested
  helper module are not yet a reason to invert a dependency. Nothing in this ADR forecloses it.
