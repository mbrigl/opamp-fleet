# ADR-0058: The Operator plane also signs in with OIDC, beside Basic

- **Status:** 🟡 proposed
- **Date:** 2026-10-22
- **Deciders:** Markus Brigl
- **Applies to:** the Operator plane's guard and its `401` in `crates/fleet-server/src/api.rs`, the token validation and the issuer's key cache beside it, the actor of every Operator-plane audit entry, the sign-in of the bundled UI under `crates/fleet-server/static/`, the Operator plane's Content Security Policy, the `[rest.auth]` and `[rest.auth.oidc]` sections of `server.toml` and their parsing in `crates/fleet-server/src/config.rs`, `config/server.toml`, the `jsonwebtoken` dependency in `Cargo.toml` and `deny.toml`, and every document that tells an operator how to sign in
- **Supersedes:** [ADR-0021](0021-admission-by-a-client-certificate-alone-and-a-revocation-list-that-follows-renewal.md) clauses 15 and 18

## Context

The Operator plane is guarded as a whole by `[rest.auth]`, Basic only
([ADR-0021](0021-admission-by-a-client-certificate-alone-and-a-revocation-list-that-follows-renewal.md)
clause 15). Every route, the UI at `/` included, answers `401` with
`WWW-Authenticate: Basic realm="opamp"`, and the browser's native prompt is the login. The
operators' credential is the only one and carries no roles (clause 18). ADR-0021 rejected
pluggable authenticators for its own change and named the plane's guard as the seam where one
could be added later.

Companies that run a fleet sign their people in through an identity provider. Basic credentials
in `server.toml` are then a second list of people, kept apart from the one that decides who works
there, and the audit names a user name that nothing outside the Server knows. Keycloak is the
provider the project tests against; OpenID Connect is what it and every comparable provider speak.

Basic stays useful: `curl` and scripts send it without a token exchange, and it works while the
identity provider is down. The specification's Non-Goal "Authorization and multi-tenancy" stays as
it is: this change decides who gets in, not what a person may do. A later rights model will be
defined in the Server and must find each person again across sessions, so the person's key is the
token's issuer and subject, never a name or an e-mail address.

The build constrains the token library: TLS and signatures run on *ring*, and `deny.toml` bans
`aws-lc-rs` and OpenSSL
([ADR-0013](0013-tls-1-3-plaintext-on-the-loopback-alone-and-bounded-planes-bodies-and-messages.md)
clause 5). `reqwest` is already a dependency and speaks TLS 1.3 alone.

A reader without this conversation would expect the UI's token in a cookie or browser storage, a
client secret in `server.toml`, and rights read from the token's roles. This ADR decides against
all three.

## Decision

We will let the Operator plane accept a bearer token from an OpenID Connect provider beside Basic,
validated by the Server against the provider's keys, with the bundled UI as a public client that
holds its tokens in memory, and with the token deciding who acts and nothing about what they may do.

1. **Restates ADR-0021 clause 15. The Operator plane is guarded by `[rest.auth]`, by Basic, OIDC
   or both, required beyond the loopback.**
   - `[rest.auth] basic_users` is a map of `user = "$argon2id$…"`
     ([ADR-0021](0021-admission-by-a-client-certificate-alone-and-a-revocation-list-that-follows-renewal.md)
     clause 26); several users allow rotation and the withdrawal of one operator's credential. An
     entry with an empty name or password fails startup.
   - `[rest.auth.oidc]` names `issuer`, `client_id`, an optional `required_claim` (a claim name
     and the value it must hold) and an optional `ca_file`. It names no client secret.
   - `[rest.auth]` holds `basic_users`, `[rest.auth.oidc]` or both; a section with neither fails
     startup. A `[rest] listen` that is not LOOPBACK without `[rest.auth]` is refused at startup
     with a message naming both keys. On a LOOPBACK `listen` the section stays optional, and the
     plane's loopback default
     ([ADR-0013](0013-tls-1-3-plaintext-on-the-loopback-alone-and-bounded-planes-bodies-and-messages.md)
     clause 7) is what protects it.
   - Without `[rest.auth.oidc]`, every route on the listener — `/api/v1/…`,
     `/api/v1/openapi.json`, `/api/v1/docs` and the UI at `/` — answers `401` with
     `WWW-Authenticate: Basic realm="opamp"` without a matching credential, and the browser's
     prompt is the login, as before.
   - With `[rest.auth.oidc]`, the UI's shell at `/` and the files it loads, `/api/v1/docs` and its
     script, and `/api/v1/openapi.json` are served without a credential; none of them carries
     fleet data. Every other `/api/v1/…` route takes either a Basic credential or a bearer token.
     Its `401` carries `WWW-Authenticate: Bearer`, and the Basic challenge only when
     `basic_users` is set and the request does not come from the UI's own script, so the browser's
     prompt never opens over the UI's sign-in.
   - The Agent plane, the package download included, is untouched by it.

2. **A bearer token is a JWT the Server validates on every request.**
   - The signature is checked against the issuer's JSON Web Key Set, with an allow-list of RS256,
     PS256 and ES256. The algorithm in the header must match the family of the key the `kid` names;
     `none`, every HMAC algorithm and a key of another family are refused.
   - `iss` equals `issuer`; `aud` names `client_id`, or `azp` does when `aud` names another
     audience; `exp` and `nbf` are required and checked with a leeway of at most 30 seconds; a
     `typ` header, when present, is `JWT` or `at+jwt`. With `required_claim`, the claim must hold
     the configured value.
   - A token that fails any check is answered `401` and audited as a refusal, the reason named and
     the token never echoed.

3. **The issuer's keys are fetched lazily, bounded, and the Server starts without them.**
   - The JWKS URI comes from the issuer's discovery document. Both are fetched over `https://` on
     the outbound TLS 1.3 client
     ([ADR-0013](0013-tls-1-3-plaintext-on-the-loopback-alone-and-bounded-planes-bodies-and-messages.md)
     clause 18), with `ca_file` as an added root for a provider a company PKI signs, a timeout, and
     a size bound on each body. An `http://` issuer is accepted on LOOPBACK alone.
   - The key set is cached. A token naming an unknown `kid` triggers a refetch at most once per
     minute.
   - While the keys cannot be loaded, the Server runs, Basic works, and a request with a bearer
     token is answered `503`. Nothing about the provider is checked at startup beyond the
     configuration's shape.

4. **The library is `jsonwebtoken` without a crypto backend, on *ring*.** The Server enables none
   of the crate's `aws_lc_rs` and `rust_crypto` features and installs a `CryptoProvider` of its own
   whose verifiers are *ring*'s, for the three algorithms of clause 2. One `Validation` is built per
   key family, with `iss`, `aud`, `exp` and `nbf` required; `azp` is checked by the Server.

5. **The bundled UI is a public client.** It signs in with the authorization code flow and PKCE
   (`S256`, the verifier made with WebCrypto), from its own script, with no script from elsewhere.
   It holds the access and the refresh token in memory only — no cookie, no `localStorage`, no
   `sessionStorage` — refreshes them in the background, and sends `Authorization: Bearer` on every
   API call. A reload goes through the provider again, whose own session answers without a prompt.
   Signing out goes to the provider's end-session endpoint. The Operator plane's Content Security
   Policy adds the issuer's origin to `connect-src`, computed at startup; every other directive
   stays as it is.

6. **Restates ADR-0021 clause 18. The operators' credentials carry no rights.** A Basic credential
   and a token are the only credentials of the Operator plane, and neither reaches an Agent's host.
   Every authenticated operator can do everything the plane offers. The Server reads no role,
   group or scope from a token to decide what a request may do.

7. **The audit names who acted, and how.** An act through a token records the person as the
   token's `iss` and `sub`, with `preferred_username` as the readable name; an act through Basic
   records the user name as before. Each entry says which of the two it was. The person's
   identity is taken from the validated token, never from the request header again.

**Out of scope:** rights and levels of any kind, and the specification change they need; service
accounts and the client credentials flow; a session cookie or a server-side confidential client;
OIDC on the Agent plane; token introspection or revocation lists of the provider; signing in
`opamp-fleetctl` with a device flow.

## Alternatives considered

- **OIDC replacing Basic.** One way in, but no access while the provider is down, and every
  script would need a token exchange. Rejected by the maintainer.
- **A server-side confidential client with a session cookie** (a backend for the frontend). The
  token would never reach JavaScript, but it brings a session store, CSRF defences and a cookie
  policy — what ADR-0021 rejected — and a client secret in `server.toml`. Rejected.
- **A reverse proxy such as oauth2-proxy in front of the plane.** No code in the Server, but a
  second component to run and harden, and the audit would lose the person unless the Server
  trusted a header the proxy sets. Rejected.
- **Rights from the token's roles** (`viewer`, `editor`, `releaser` as roles in the provider). It
  needs no store in the Server, but it fixes the rights model before it is decided and ties it to
  one provider's claim layout. Rejected for now; rights will be defined in the Server.
- **`jsonwebtoken` with its `aws_lc_rs` or `rust_crypto` backend.** The first is banned; the second
  brings the `rsa` crate with an advisory that has no fix. Rejected in favour of a *ring* provider
  of our own.
- **`biscuit`.** *ring*-native and enforcing the algorithm match itself, but a small community to
  depend on. Rejected.
- **`openidconnect`, `josekit`, `jwt-simple`, `aliri`.** ID tokens only, OpenSSL, BoringSSL, or
  OpenSSL for EC keys. Rejected.
- **No JWT crate, JWS verification with *ring* directly.** No new dependency, but the header and
  claim parsing and every claim check would be ours to write and keep correct. Rejected; the crate
  carries that part and the crypto stays ours.

## Sources / Prior art

- The working note of 2026-10-15 on the Server, section C, its analysis points and the decisions
  of 2026-10-16, recorded outside the ADRs.
- [OpenID Connect Core 1.0](https://openid.net/specs/openid-connect-core-1_0.html) and
  [Discovery 1.0](https://openid.net/specs/openid-connect-discovery-1_0.html).
- [RFC 7636](https://www.rfc-editor.org/rfc/rfc7636) (PKCE),
  [RFC 8725](https://www.rfc-editor.org/rfc/rfc8725) (JWT best current practices: algorithm
  allow-lists, key-family checks), [RFC 9068](https://www.rfc-editor.org/rfc/rfc9068) (`at+jwt`
  access tokens), and the IETF draft "OAuth 2.0 for Browser-Based Applications" on public clients
  and token storage.
- `jsonwebtoken` 11.1.0 ([docs.rs](https://docs.rs/jsonwebtoken/11.1.0/jsonwebtoken/)), MIT: no
  backend in its default features, `CryptoProvider` with `verifier_factory` and `install_default`
  in `src/crypto/mod.rs`, the `JwtVerifier` trait.
- [Keycloak: audience support](https://www.keycloak.org/docs/latest/server_admin/#audience-support):
  an access token names `account` as audience unless an audience mapper adds the client; the setup
  guide must say so.
- The code as it stands: `authenticate` and `record_act` in `crates/fleet-server/src/api.rs`, which
  read the Basic user name from the header; the plane's security headers in the same file.

## Consequences

- Positive: operators sign in with the account their company already manages, and leaving the
  company ends their access without touching `server.toml`.
- Positive: the audit names a person by a key the provider vouches for, which a later rights model
  in the Server can attach to.
- Positive: Basic keeps scripts and the incident path working when the provider is down.
- Negative / trade-offs: a new dependency, and a crypto provider of our own to keep correct; the
  negative tests of clause 2 carry that.
- Negative / trade-offs: a token held in the page is reachable by script injected into it. The
  Content Security Policy, which allows no inline script but hashed ones and no foreign origin but
  the issuer's for `connect-src`, is what stands against that.
- Negative / trade-offs: the UI's shell, the API document and its viewer become readable without a
  credential when OIDC is set. They hold no fleet data, and the API's shape is public in the source.
- Negative / trade-offs: the provider's client must be set up correctly — public client, PKCE,
  redirect URI, audience mapper — and a wrong setup shows as `401` until it is fixed.
- Follow-ups: rights defined in the Server and keyed by `iss` and `sub`, with the specification
  change that narrows its Non-Goal; signing in `opamp-fleetctl` through a device flow; the glossary
  terms identity provider and bearer token.

## Enforcement

Tests that will carry `Verifies: ADR-0058`, against an in-process test issuer with its own keys,
discovery document and JWKS endpoint:

- A valid token on `/api/v1/agents` is admitted; one with a wrong `iss`, an `aud` and `azp` naming
  another client, an expired `exp`, a future `nbf`, or a `required_claim` that does not hold is
  answered `401` and audited.
- A token signed with `none`, with HS256 over the RSA key's bytes, with RS256 against an EC key,
  or with ES256 on the wrong curve is refused.
- A token naming an unknown `kid` refetches the key set once, and a second one within the minute
  does not.
- With the issuer unreachable, the Server starts, Basic is admitted, and a bearer request is
  answered `503`.
- With `[rest.auth.oidc]`, `/` and `/api/v1/openapi.json` answer without a credential and
  `/api/v1/agents` answers `401` with a Bearer challenge; without it, `/` answers `401` with the
  Basic challenge.
- `[rest.auth]` with neither `basic_users` nor `[rest.auth.oidc]`, and a non-loopback `listen`
  without `[rest.auth]`, fail startup naming the keys.
- An act through a token is audited with `iss`, `sub` and `preferred_username`; an act through
  Basic with the user name; each says which.
- The Content Security Policy names the issuer's origin in `connect-src` with OIDC and does not
  without it.
- `cargo tree` of the Server contains no `aws-lc-rs` and no `rsa`.

**Not mechanically decidable:** that the UI never writes a token to browser storage. A review of
`crates/fleet-server/static/` checks it, and the test above that refuses foreign origins in the
policy limits what a mistake could reach.
