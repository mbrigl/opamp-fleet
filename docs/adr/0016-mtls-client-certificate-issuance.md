# ADR-0016: mutual TLS with OpAMP client-certificate issuance (CertificateRequest), Server as a simple CA

- **Status:** 🟢 accepted
- **Date:** 2026-07-19
- **Deciders:** Maintainer

## Context

[ADR-0012](0012-tls-and-shared-token-auth.md) secured the transport with server-side TLS and a shared
bearer token, and named **mutual TLS (client certificates) via OpAMP's `CertificateRequest` flow** as the
heavier follow-on. That flow is:

1. The Agent generates a key pair and a **PEM CSR** and sends it in
   `AgentToServer.connection_settings_request.opamp.certificate_request`.
2. The Server (a CA, or fronting one) **issues a certificate** and returns it in
   `ServerToAgent.connection_settings.opamp.certificate` (`TLSCertificate`: the issued cert, optionally a
   private key, and the CA cert).
3. The Agent uses the issued **client certificate** for mTLS on subsequent OpAMP connections; the flow
   also supports **revocation/rotation** by issuing a new certificate.

The vendored schema already carries `CertificateRequest`, `TLSCertificate`, and the connection-settings
plumbing. This is the strongest authentication OpAMP offers, and it makes the **Server a certificate
authority** — a significant, security-sensitive architecture decision, hence this ADR.

## Decision

We will implement **OpAMP client-certificate issuance and mutual TLS**, with the **Server acting as a
simple CA** from a configured CA certificate and key.

- **Supervisor (client).** When configured for mTLS and it has no valid client certificate, the
  Supervisor generates a key pair and a CSR (via `rcgen`) and sends a `CertificateRequest`. On receiving a
  `TLSCertificate`, it persists the key and certificate in the storage dir and uses them as the
  **client certificate** for mTLS (rustls `with_client_auth_cert`) on every subsequent connection,
  building on [ADR-0012](0012-tls-and-shared-token-auth.md)'s connector. A newly issued certificate
  **rotates** the old one; the Supervisor re-connects with the new one and forgets the previous.
- **Server (a simple CA).** Given a configured CA certificate + key, the Server validates an incoming CSR
  and **issues a short-lived client certificate** (via `rcgen`) bound to the agent's instance UID, returned
  in the `opamp.certificate` connection-settings offer. The OpAMP listener then **requires and validates
  client certificates** against that CA (rustls client-auth) once mTLS is enabled — so the bearer token
  and the client cert can be required together or independently.
- **Opt-in, layered on ADR-0012.** mTLS is configured on top of TLS; without CA configuration the Server
  does not issue certs and does not require them, exactly as [ADR-0012](0012-tls-and-shared-token-auth.md)
  leaves it. The CA key is a **high-value secret**: it is loaded from a file the operator protects, never
  generated implicitly, and never logged.
- **Dependency:** `rcgen` for CSR generation (Supervisor) and CSR signing / certificate issuance (Server),
  justified here.

## Alternatives considered

- **Pre-provisioned client certificates (mTLS without the issuance flow).** The lighter first step: the
  operator hands each Supervisor a client cert + key via configuration, and the Server validates against a
  CA — mTLS, no `CertificateRequest`, no Server-side CA. Rejected as the *sole* solution because it drops
  the requested issuance/rotation flow, but **recommended as a phase-1** the reviewer may prefer before
  the Server becomes a CA. (This ADR can be split: phase 1 = pre-provisioned mTLS, phase 2 = issuance.)
- **An external CA / cert-manager issues; OpAMP only delivers.** Cleaner separation (the Server is not a
  CA), but it needs an integration this project does not have and puts the issuance policy outside the
  fleet control loop. Worth revisiting if a real PKI is present; for a self-contained stack the simple
  built-in CA is the smaller thing.
- **Bearer token only (no mTLS).** Already shipped ([ADR-0012](0012-tls-and-shared-token-auth.md)); mTLS is
  strictly stronger (the client is cryptographically identified, tokens can leak). This ADR is the
  increment beyond it.

## Sources / Prior art

- OpAMP specification — `CertificateRequest`, `TLSCertificate`, connection-credentials management
  (client-cert registration, revocation, rotation):
  <https://github.com/open-telemetry/opamp-spec/blob/main/specification.md>.
- Certificate generation / CSR signing in Rust — `rcgen`: <https://docs.rs/rcgen>.
- rustls client authentication (`with_client_auth_cert`, client-cert verification):
  <https://docs.rs/rustls>. The TLS foundation this builds on: [ADR-0012](0012-tls-and-shared-token-auth.md);
  the connection-settings offer channel: [ADR-0011](0011-server-agent-control-beyond-config.md),
  [ADR-0015](0015-accepts-opamp-connection-settings.md).
- The vendored schema — `CertificateRequest`, `TLSCertificate`, `OpAMPConnectionSettings.certificate` in
  [`crates/opamp-proto/proto/opamp/v1/opamp.proto`](../../crates/opamp-proto/proto/opamp/v1/opamp.proto).

## Consequences

- Positive: the strongest OpAMP authentication — agents are cryptographically identified, and certificates
  can be rotated/revoked from the fleet without redeploying secrets. Completes the ADR-0012 follow-on.
- Negative / trade-offs: **the Server becomes a CA**, holding a high-value key whose compromise forges any
  agent identity — a real operational burden (protection, rotation of the CA itself). More dependencies
  (`rcgen`) and a non-trivial issuance/rotation state machine on both sides; agent private keys now live in
  the storage dir. This is the largest security surface the project has taken on.
- Follow-ups: CA-key rotation; short-lived-cert renewal cadence; OCSP/CRL revocation if the simple
  "issue a new cert" rotation is not enough; the phase-1 pre-provisioned-cert path if the reviewer wants
  mTLS before the Server-as-CA step.
