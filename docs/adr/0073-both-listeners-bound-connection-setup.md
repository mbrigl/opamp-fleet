# ADR-0073: Both listeners bound connection setup — an HTTP/1 header-read timeout, set where hyper can honour it

- **Status:** 🟢 accepted
- **Date:** 2026-08-18
- **Deciders:** Markus Brigl

## Context

[ADR-0066](0066-the-agent-plane-and-the-operator-plane-get-their-own-listeners.md) gives the Server
two listeners. The Agent plane defaults to `0.0.0.0:4320` (`server::config::DEFAULT_LISTEN`) — it is
meant to be reachable by every host in the fleet, so it is exposed by default; the Operator plane
defaults to loopback.

What a connection to either plane is already bounded by is substantial: the message size limit on
both transports and the Baseline's post-decompression rule ([ADR-0007](0007-dual-transport-and-tls.md),
[ADR-0044](0044-what-the-shared-crate-holds.md)), `max_agents`, `max_package_size_bytes`, Admission
([ADR-0013](0013-opamp-endpoint-authentication.md), [ADR-0035](0035-mutual-tls-and-the-server-issued-client-certificate.md)),
and — on the TLS listeners — the 10-second handshake deadline `axum_server`'s `RustlsAcceptor`
applies by default, which `PeerCertAcceptor` inherits by wrapping it.

**What is bounded by nothing is time before a request exists.** Grepping the Server for a timeout
finds exactly one, and it is in a test (`api.rs`, the test client). A peer may complete the TCP
connection — and the TLS handshake — send `GET /v1/opamp HTTP/1.1\r\n`, and then send nothing, for
as long as it likes. It costs it one socket; it costs the Server a task and hyper's read buffer per
connection, and it happens *before* any route, any body limit and any Admission check runs, so no
credential is needed to do it many times over. This is the classic slow-header exposure.

### Why hyper's own default does not cover it

hyper 1.11 defaults `http1::Builder::header_read_timeout` to 30 seconds — but the default is inert
unless a `Timer` is installed. Its resolution is explicit about it
(`hyper-1.11.0/src/common/time.rs`, `Time::check`):

```rust
Dur::Default(Some(dur)) => match self {
    Time::Empty => { warn!("timeout `{}` has default, but no timer set", name); None }
    Time::Timer(..) => Some(dur),
},
Dur::Configured(Some(dur)) => match self {
    Time::Empty => panic!("timeout `{name}` set, but no timer set"),
    ...
```

Neither of the two servers this project uses installs one: there is no `.timer(` call anywhere in
`axum-0.8.9/src/serve/mod.rs` or in `axum-server-0.7.3/src/`. So today **both planes run with no
header-read timeout at all**, and the second arm of that `match` is why the fix is not a one-liner:
configuring the timeout without also installing the timer replaces a silent gap with a panic.

Upstream reached the same conclusion. axum [PR #3478](https://github.com/tokio-rs/axum/pull/3478)
("`axum::serve` now applies hyper's default `header_read_timeout`", merged 2025-09-16) installs the
timer in `axum::serve`, and follow-up work adds `Serve::header_read_timeout` /
`Serve::no_header_read_timeout`. It sits in the *Unreleased* section of the changelog; the newest
published axum is 0.8.9, which is what this workspace pins. That is prior art for the value and the
placement, not a fix we can consume.

### Why the bound belongs to connection setup, not to a request

A per-request timeout would be the wrong instrument here, and not only because it arrives too late
to see the header phase: the three routes that matter are all *meant* to take a long time. `/v1/opamp`
is a long-lived WebSocket; the package download streams an artifact of arbitrary size
([ADR-0015](0015-package-delivery-for-managed-processes.md)); `put_package_entry` accepts an upload with the body limit
deliberately disabled ([ADR-0008](0008-toml-configuration.md)). A blanket timeout would
break exactly those and leave the slow peer untouched.

### What it costs to set

`axum::serve` in 0.8.9 exposes no builder — it constructs its own `hyper_util` `Builder` internally
— so the plain-HTTP path cannot be configured where it stands. `axum_server::Server::http_builder()`
does expose it, and the TLS path already runs on `axum_server`. Moving the plain path there is
therefore consolidation rather than a new stack: both planes, both transports, one serving path.
Installing the timer needs `hyper_util::rt::TokioTimer`, so `hyper-util` becomes a *direct*
dependency of `crates/server` — it is already in the tree as axum's own dependency and already
linked into this binary, so nothing new is compiled or shipped.

Two things come along with that move, both improvements on what is there now: the TLS branch of
`main.rs` has **no graceful shutdown whatsoever** (a `tokio::select!` on `ctrl_c` that simply drops
both servers), while the plain branch waits for in-flight connections without a bound — which
includes every open Agent WebSocket. `axum_server`'s `Handle::graceful_shutdown(Some(duration))`
gives both planes the same bounded drain.

## Decision

We will **serve both planes through `axum_server`, with hyper's HTTP/1 timer installed and a
30-second header-read timeout**, and we will keep every per-request timeout out of it.

1. One place builds a plane's server — plain or TLS, Agent or Operator — installs
   `TokioTimer` on the HTTP/1 builder and sets `header_read_timeout` to 30 seconds. 30 s is hyper's
   own default and what axum will apply once #3478 ships, so this decision becomes "keep the
   default" rather than "hold a private opinion" on that day.
2. The TLS handshake deadline stays at `axum_server`'s 10 seconds, but is **stated in our code**
   rather than inherited silently, so it reads as a decision.
3. Shutdown becomes one bounded drain per plane via `axum_server`'s `Handle`, replacing the TLS
   branch's drop-on-signal and the plain branch's unbounded wait. `flush_agents`
   ([ADR-0051](0051-agent-records-persist-across-a-server-restart.md)) still runs after both.
4. **No request timeout, no body timeout, no `tower-http`.** The long routes stay long.
5. **No new `server.toml` key.** The value equals the framework default an operator would otherwise
   never see; a knob is added when a deployment needs a different one, not before.

The header-read timeout is a parameter of the serving function so an integration test can drive it
short: connect, send a partial request line, assert the Server hangs up within the deadline. That
test is what makes this behaviour rather than configuration.

## Alternatives considered

- **A `tower-http` `TimeoutLayer` on the routes** — rejected, and it is the alternative worth being
  explicit about: middleware runs *after* hyper has parsed the request line and headers, so it
  cannot see the phase this ADR is about. It would add a dependency, need exclusions for the
  WebSocket, the download and the upload, and still leave the hole open.
- **Wait for the axum release carrying #3478** — rejected. It is unreleased, and it fixes only the
  plain path: the TLS listeners run on `axum_server`, which would still have no timer.
- **Keep `axum::serve` and hand-roll the accept loop on `hyper-util`** — rejected. It buys the same
  knob for the price of owning connection tracking and shutdown, both of which `axum_server` already
  implements and this project already depends on.
- **A `header_read_timeout_secs` key per plane in `server.toml`** — deferred. YAGNI, and a key whose
  only sensible value is the framework default is a knob nobody turns.
- **Put the hardening in `crates/opamp` as a shared layer** — rejected under
  [ADR-0044](0044-what-the-shared-crate-holds.md)'s rule. This is one framework's builder knob in one
  binary; the Client's outbound transports have no counterpart to share with. The protocol-level
  hardening that *is* identical on both ends already lives there (`frame`, `endpoint`).

## Sources / Prior art

- [axum PR #3478](https://github.com/tokio-rs/axum/pull/3478) and the
  [axum changelog](https://github.com/tokio-rs/axum/blob/main/axum/CHANGELOG.md) — upstream installs
  the timer and applies hyper's 30 s default; unreleased as of 0.8.9, the version this workspace
  pins. Also the motivation measured there: an incomplete request costs ~1 MB per connection against
  ~2.5 kB for an idle one, and it blocks graceful shutdown.
- [axum issue #2741, "How to avoid Slowloris DoS Attack?"](https://github.com/tokio-rs/axum/issues/2741)
  — the same question asked from the outside, with the same answer: it is a connection-setup knob.
- [`hyper::server::conn::http1::Builder::header_read_timeout`](https://docs.rs/hyper/1.11.0/hyper/server/conn/http1/struct.Builder.html#method.header_read_timeout)
  — *"Requires a `Timer` set by `Builder::timer` to take effect. Panics if `header_read_timeout` is
  configured without a `Timer`."* — and `hyper-1.11.0/src/common/time.rs`, `Time::check`, quoted
  above, for what the untimed default actually resolves to.
- `axum-server-0.7.3/src/server.rs` (`http_builder`) and `src/tls_rustls/mod.rs` (the 10-second
  handshake default) — read from the vendored sources, since neither is prominent in the docs.
- Prior art for the value: Go's `http.Server` leaves `ReadHeaderTimeout` unset by default and the
  field exists precisely for this attack ([Diving into Go's HTTP server timeouts](https://adam-p.ca/blog/2022/01/golang-http-server-timeouts/)),
  with ~20 s a commonly recommended setting; nginx's `client_header_timeout` defaults to 60 s. 30 s
  sits inside that range and matches hyper.
- [`opamp-go`](https://github.com/open-telemetry/opamp-go/blob/main/server/serverimpl.go), this
  project's behavioural oracle under [ADR-0040](0040-interoperability-against-opamp-go.md), builds
  its `http.Server` with `Handler`, `Addr`, `TLSConfig` and `ConnContext` and **no timeouts at all**.
  The oracle is silent here rather than contrary: hardening a deployment is not protocol behaviour,
  so this ADR goes beyond it without diverging from it.

## Consequences

- Positive: an unauthenticated peer can no longer pin a connection open indefinitely on either
  plane. The exposure closes on the Agent plane, which is the one that is public by default.
- Positive: one serving path for plain and TLS, and a bounded graceful drain on both planes — the
  TLS listeners have none today.
- Positive: the 10-second TLS handshake deadline stops being an inherited default nobody chose.
- Negative / trade-offs: `hyper-util` becomes a direct dependency of `crates/server`. It is already
  in the lockfile and already linked into this binary through axum, so the cost is the entry in
  `Cargo.toml` and one more crate whose version bumps this workspace notices.
- Negative / trade-offs: this is **HTTP/1 only**. The TLS listeners offer `h2` by ALPN, and HTTP/2
  has no header-read equivalent; its analogues are keep-alive pings and `max_concurrent_streams`.
  Left undecided deliberately rather than guessed at.
- Negative / trade-offs: a peer that needs more than 30 s to send its request headers is hung up on.
  No Client in this project comes close, and the value is the one axum will impose anyway.
- Negative / trade-offs: connection *count* stays unbounded — a peer may still open many sockets,
  each now cheap and short-lived. Bounding that is a different instrument.
- Negative / trade-offs: this bounds the **Server's** two planes and nothing else. The Client serves
  the same protocol on two listeners of its own — the Gateway endpoint (ADR-0037) and the Supervisor
  Endpoint — and both are in exactly the state described above; the Supervisor Endpoint worse, since
  it serves connections one at a time and a stalled handshake blocks the Managed Process behind it.
  Found while measuring for this ADR, recorded as measure **H18** in
  [`HARDENING.md`](../HARDENING.md), and deliberately not folded in here: it is the same fix in
  another binary, not another decision.
- Follow-ups: the Client's two listeners (H18); a concurrent-connection cap per plane (H16); HTTP/2
  keep-alive and stream limits (H17); and revisiting this when axum ships #3478, at which point
  points 1 and 2 may reduce to *not* opting out of the framework's defaults.
