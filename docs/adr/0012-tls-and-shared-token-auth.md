# ADR-0012: TLS transport and shared-token authentication, opt-in, on both sides

- **Status:** 🟢 accepted
- **Date:** 2026-07-19
- **Deciders:** Maintainer

## Context

Every listener the project ships is currently **plain and unauthenticated**, deliberately and with a
named follow-up:

- The OpAMP endpoint (`:4320`) is plain-`ws`, unauthenticated ([ADR-0006](0006-rust-opamp-server-from-spec.md));
  TLS + shared-token auth were explicitly deferred to "their own ADR".
- The fleet UI + REST API (`:4321`) authenticate nobody ([ADR-0007](0007-rest-api-and-fleet-ui.md));
  authentication is called "the open flank, deferred to a later ADR".
- The Collector Supervisor holds TLS + auth out of scope ([ADR-0008](0008-collector-supervisor-go-reference-compat.md)).
- The own-telemetry offer's `certificate` / `tls` fields are deferred **to this ADR**
  ([ADR-0010](0010-collector-supervisor-own-telemetry.md)).
- The new mutating **restart endpoint** on the unauthenticated `:4321` names authenticating it as **this
  ADR's** job ([ADR-0011](0011-server-agent-control-beyond-config.md)).

This ADR is that named security follow-up, spanning the whole stack. OpAMP's own security model is
**header-based authorization** (an `Authorization` header / access token in a custom header) plus
**optional client-side TLS certificates (mTLS)**; the vendored schema carries `headers`, `certificate`
(`TLSCertificate`), and `tls` (`TLSConnectionSettings`) in its connection settings. The Rust stack does
not yet pull a TLS library: `axum` is built with only the `ws` feature and `tokio-tungstenite` with no
TLS feature, so `wss://` needs a dependency decision — which is why this is architecture-relevant.

## Decision

We will secure the transport with **TLS (rustls)** and authenticate with a **shared bearer token**, both
**opt-in via configuration** and **defaulting to today's plain, unauthenticated dev behaviour** so the
dev environment is unchanged until secured.

- **OpAMP endpoint (`:4320`) — server-side TLS + token.** With a cert configured, the Server serves
  `wss://` (rustls, via `axum-server`'s `bind_rustls`). The Supervisor connects with
  `connect_async_tls_with_config`, validating the Server certificate against a configured CA (or the
  platform roots), and sends `Authorization: Bearer <token>` on the WebSocket handshake. The Server
  rejects the upgrade (`401`) when the token is missing or wrong.
- **UI/API listener (`:4321`) — token on every request, optional TLS.** The same shared token gates every
  request via an `axum` middleware/extractor that checks `Authorization`; the **restart** and config-write
  endpoints (ADR-0007/ADR-0011) are thereby authenticated. TLS on `:4321` is offered from the same cert
  configuration.
- **Own-telemetry offer TLS (completes [ADR-0010](0010-collector-supervisor-own-telemetry.md)).** An
  `https://` destination is honoured with proper certificate validation, and the offer's `tls.ca_pem`
  (and, when present, `certificate`) is mapped into the Collector exporter's `tls` settings, rather than
  silently pretending to a security property as the ADR-0010 interim did.
- **Scope now: server-only TLS + a shared bearer token — not mTLS.** Client certificates (mTLS) and
  OpAMP's certificate **registration/rotation** flow (`CertificateRequest`) are the heavier follow-on that
  this ADR's transport TLS enables, deferred to their own ADR.
- **Dependencies (justified here):** Server — `axum-server` (rustls feature) and `rustls-pemfile`;
  Supervisor — `tokio-tungstenite`'s `rustls-tls-*` feature, `tokio-rustls`, and `rustls-pemfile`. We
  choose **rustls** (pure-Rust) over `native-tls` to avoid a system OpenSSL dependency, consistent with
  the project's few-moving-parts stance.
- **Configuration.** Server flags `-tls-cert <pem>`, `-tls-key <pem>`, `-auth-token <token|@file>`;
  Supervisor `supervisors.yaml` gains a `wss://` server URL, an `auth_token`, and an optional
  `tls: { ca_cert?, insecure? }`. Nothing configured → plain `ws` + unauthenticated, exactly as today. A
  token or cert configured on one side but not honoured on the other is a **fail-closed** config error
  where detectable, with a clear message.

## Alternatives considered

- **Mutual TLS (client certificates) instead of a bearer token.** Stronger, and the OpAMP-native path
  (the schema models cert issuance/rotation), but heavier: it needs a certificate lifecycle and the
  `CertificateRequest` exchange. Deferred as the follow-on this ADR's transport TLS unlocks; a shared
  token matches [ADR-0006](0006-rust-opamp-server-from-spec.md)'s "shared-token authentication".
- **`native-tls` (system OpenSSL/SChannel) instead of rustls.** Rejected: it pulls a system TLS library
  and complicates the Dev Container and cross-compilation; rustls is pure-Rust and already the ecosystem
  default for `axum`/`tokio-tungstenite`.
- **Terminate TLS / auth at a reverse proxy (nginx, Envoy) in front.** Punts security to a component the
  project does not ship, does not authenticate at the application layer, and leaves the built-in
  endpoints open on the host. Rejected for the built-in path (an operator may still front the server).
- **Per-agent tokens or OAuth/OIDC.** Overkill for the shared-secret fleet model now; a single shared
  token is the smallest thing that closes the flank. Per-agent credentials are a later refinement.
- **Make TLS/auth mandatory (no plain fallback).** Rejected for now: it breaks the zero-setup dev
  environment ([ADR-0003](0003-compose-dev-environment-with-opamp-sidecars.md)); opt-in with a loud
  "unauthenticated" banner (already present) is the pragmatic default until the project is deployed.

## Sources / Prior art

- OpAMP specification — transport security, header-based authorization, and client-cert/mTLS connection
  settings: <https://opentelemetry.io/docs/specs/opamp/> and
  <https://github.com/open-telemetry/opamp-spec/blob/main/specification.md>.
- Bearer token usage (`Authorization: Bearer`): RFC 6750 —
  <https://datatracker.ietf.org/doc/html/rfc6750>.
- TLS in Rust — rustls: <https://docs.rs/rustls>; server via `axum-server`:
  <https://docs.rs/axum-server/latest/axum_server/tls_rustls/>; client via
  `tokio_tungstenite::connect_async_tls_with_config`: <https://docs.rs/tokio-tungstenite>.
- The deferrals this ADR resolves: [ADR-0006](0006-rust-opamp-server-from-spec.md),
  [ADR-0007](0007-rest-api-and-fleet-ui.md), [ADR-0008](0008-collector-supervisor-go-reference-compat.md),
  [ADR-0010](0010-collector-supervisor-own-telemetry.md), [ADR-0011](0011-server-agent-control-beyond-config.md).
- The vendored schema — `OpAMPConnectionSettings.headers`, `TLSCertificate`, `TLSConnectionSettings` in
  [`crates/opamp-proto/proto/opamp/v1/opamp.proto`](../../crates/opamp-proto/proto/opamp/v1/opamp.proto).

## Consequences

- Positive: the whole stack can run authenticated over TLS; the restart endpoint and config writes are no
  longer open; own-telemetry to `https://` destinations is validated. This closes the named security
  flank of ADR-0006/0007/0011 and the ADR-0010 TLS deferral.
- Negative / trade-offs: new TLS dependencies on both crates and longer build times; a certificate/token
  lifecycle to manage out-of-band (rotation is a follow-up); a misconfiguration can lock agents out
  (fail-closed) — mitigated by clear errors and the opt-in, default-off model. The dev environment stays
  plain unless explicitly secured, so "works in dev, fails once secured" is a real risk the tests and docs
  must cover.
- Follow-ups: **mTLS / client-certificate issuance** via OpAMP's `CertificateRequest` flow; **token
  rotation** and per-listener or per-agent tokens; the own-telemetry **client certificate**
  (`offer.certificate`) if a fleet needs it.
