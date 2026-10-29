# ADR-draft-the-operator-plane-also-signs-in-with-oidc-beside-basic: The Operator plane also signs in with OIDC, beside Basic

- **Status:** 🟡 proposed
- **Date:** 2026-10-22
- **Deciders:** Markus Brigl
- **Applies to:** the Operator plane's guard and its `401` in `crates/fleet-server/src/api.rs`, the token validation and the issuer's key cache beside it, the actor of every Operator-plane audit entry, the sign-in of the bundled UI under `crates/fleet-server/static/`, the Operator plane's Content Security Policy, the `[rest.auth]` and `[rest.auth.oidc]` sections of `server.toml` and their parsing in `crates/fleet-server/src/config.rs`, `config/server.toml`, the `jsonwebtoken` dependency in `Cargo.toml` and `deny.toml`, the outbound connection to the issuer, clauses 13 and 16 of ADR-0021 and clause 21 of ADR-0012, and every document that tells an operator how to sign in

## Context

The Operator plane is guarded as a whole by `[rest.auth]`, Basic only
([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)
clause 13). Every route, the UI at `/` included, answers `401` with
`WWW-Authenticate: Basic realm="opamp"`, and the browser's native prompt is the login. The
operators' credential is the only one and carries no roles (clause 16). ADR-0021 rejected
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
([ADR-0012](0012-tls-1-3-two-planes-and-bounded-input.md)
clause 5). `reqwest` is already a dependency and speaks TLS 1.3 alone.

A reader without this conversation would expect the UI's token in a cookie or browser storage, a
client secret in `server.toml`, and rights read from the token's roles. This ADR decides against
all three.

This ADR changes three clauses decided elsewhere: the guard of the Operator plane and the rule that
its credential carries no rights
([ADR-0021](0021-admission-by-client-certificate-and-revocation.md) clauses 13 and 16, restated by
[ADR-draft-admission-by-client-certificate-and-revocation](draft-admission-by-client-certificate-and-revocation.md)),
and the list of the Server's outbound connections
([ADR-0012](0012-tls-1-3-two-planes-and-bounded-input.md) clause 21, restated by
[ADR-draft-tls-1-3-two-planes-and-bounded-input](draft-tls-1-3-two-planes-and-bounded-input.md)).
Its clauses 1, 3 and 6 state them as they stand with OIDC.

## Decision

We will let the Operator plane accept a bearer token from an OpenID Connect provider beside Basic,
validated by the Server against the provider's keys, with the bundled UI as a public client that
holds its tokens in memory, and with the token deciding who acts and nothing about what they may do.

1. **The Operator plane is guarded by `[rest.auth]`, by Basic, OIDC or both, required beyond the
   loopback.** This is ADR-0021's clause 13 as it stands with OIDC.
   - `[rest.auth] basic_users` is a map of `user = "$argon2id$…"`; several users allow rotation
     and the withdrawal of one operator's credential. `basic_users_file` names a file holding that
     map instead ([ADR-0030](0030-the-servers-configuration.md) clause 5). An entry with an empty
     name or password fails startup.
   - `[rest.auth.oidc]` names `issuer`, `client_id`, `required_claim` (a claim name and the value
     it must hold) and an optional `ca_file`. It names no client secret. Without `required_claim`
     it fails startup, naming the key: every account of the issuer would otherwise be an operator
     who can do everything (clause 6).
   - `[rest.auth]` holds Basic users, by `basic_users` or `basic_users_file`, `[rest.auth.oidc]`,
     or both; a section with neither fails startup. A `[rest] listen` that is not LOOPBACK without
     `[rest.auth]` is refused at startup with a message naming both keys. On a LOOPBACK `listen`
     the section stays optional, and the plane's loopback default
     ([ADR-0012](0012-tls-1-3-two-planes-and-bounded-input.md) clause 8) is what protects it.
   - Without `[rest.auth.oidc]`, every route on the listener — `/api/v1/…`, `/api/v1/openapi.json`,
     `/api/v1/docs` and the UI at `/` — answers `401` with `WWW-Authenticate: Basic realm="opamp"`
     without a matching credential, and the browser's prompt is the login.
   - With `[rest.auth.oidc]`, the UI's shell at `/` and the files it loads, `/api/v1/docs` and its
     script, and `/api/v1/openapi.json` are served without a credential; none of them carries fleet
     data. Every other `/api/v1/…` route takes either a Basic credential or a bearer token. Its
     `401` carries `WWW-Authenticate: Bearer`, and the Basic challenge too when Basic users are
     configured and the request is not made by a page's script. The browser says which it is in
     `Sec-Fetch-Mode`, a header a page's script cannot set: a request with `cors`, `same-origin`
     or `no-cors` there gets no Basic challenge, and one with `navigate`, or without the header, as
     `curl` and scripts send it, does. The browser's prompt therefore never opens over the UI's
     sign-in.
   - The Agent plane, the package download included, is untouched by it.

2. **A bearer token is a JWT the Server validates on every request.**
   - The signature is checked against the issuer's JSON Web Key Set, with an allow-list of RS256,
     PS256 and ES256. The algorithm in the header must match the family of the key the `kid` names;
     `none`, every HMAC algorithm and a key of another family are refused.
   - `iss` equals `issuer`; `exp` and `nbf` are required and checked with a leeway of at most 30
     seconds; a `typ` header, when present, is `JWT` or `at+jwt`. The claim `required_claim` names
     must hold the configured value.
   - The audience a token must name depends on the route. On `/mcp`, where
     [ADR-draft-an-mcp-endpoint-on-the-operator-plane-reads-and-prepares-and-never-distributes](draft-an-mcp-endpoint-on-the-operator-plane-reads-and-prepares-and-never-distributes.md)
     serves it, `aud` names that endpoint's canonical URI (its clause 3). On every other route
     `aud` names `client_id`, and a token whose `aud` names the canonical URI of `/mcp` is refused
     there, even when it names `client_id` too. `azp` stands in for `aud` on no route.
   - An ID token is never a bearer token. A token that carries `nonce`, `at_hash` or `c_hash`, the
     claims OpenID Connect gives an ID token and RFC 9068 gives no access token, is refused,
     whatever its `typ`. The bundled UI sends a `nonce` with every authorization request
     (clause 5), so every ID token issued to it carries one.
   - A token that fails any check is answered `401` and audited as a refusal, the reason named and
     the token never echoed.

3. **The issuer's keys are fetched lazily, bounded, and the Server starts without them.**
   - The JWKS URI comes from the issuer's discovery document. Both are fetched over `https://` on
     the outbound TLS 1.3 client, with `ca_file` as an added root for a provider a company PKI
     signs, a timeout, and a size bound on each body. An `http://` issuer is accepted on LOOPBACK
     alone.
   - The issuer is an outbound connection of the Server beside those
     [ADR-draft-tls-1-3-two-planes-and-bounded-input](draft-tls-1-3-two-planes-and-bounded-input.md)
     clause 21 names: the issuer `[rest.auth.oidc] issuer` names, for its discovery document and
     the JSON Web Key Set that document names, over the provider of that ADR's clause 5. It is the
     only one this ADR adds.
   - The key set is cached. A token naming an unknown `kid` triggers a refetch at most once per
     minute.
   - While the keys cannot be loaded, the Server runs, Basic works, and a request with a bearer
     token is answered `503`. Nothing about the provider is checked at startup beyond the
     configuration's shape.

4. **The library is `jsonwebtoken` without a crypto backend, on *ring*.** The Server enables none
   of the crate's `aws_lc_rs` and `rust_crypto` features and installs a `CryptoProvider` of its own
   whose verifiers are *ring*'s, for the three algorithms of clause 2. One `Validation` is built per
   key family and audience of clause 2, with `iss`, `aud`, `exp` and `nbf` required; the claims
   that mark an ID token are checked by the Server.

5. **The bundled UI is a public client.** It signs in with the authorization code flow and PKCE
   (`S256`, the verifier made with WebCrypto) and a fresh `nonce` in every authorization request,
   from its own script, with no script from elsewhere.
   It holds the access and the refresh token in memory only — no cookie, no `localStorage`, no
   `sessionStorage` — refreshes them in the background, and sends `Authorization: Bearer` on every
   API call. A reload goes through the provider again, whose own session answers without a prompt.
   Signing out goes to the provider's end-session endpoint. The Operator plane's Content Security
   Policy adds the issuer's origin to `connect-src`, computed at startup; every other directive
   stays as it is.

6. **The operators' credentials carry no rights.** This is ADR-0021's clause 16 as it stands with
   OIDC. A Basic credential and a token are the only credentials of the Operator plane, and neither
   reaches an Agent's host. Every authenticated operator can do everything the plane offers. The
   Server reads no role, group or scope from a token to decide what a request may do.

7. **The audit names who acted, and how.** An act through a token records the person as the
   token's `iss` and `sub`, with `preferred_username` as the readable name; an act through Basic
   records the user name. Each entry says which of the two it was. The person's
   identity is taken from the validated token, never from the request header again.

**Out of scope:** a bearer token from any issuer but the one `[rest.auth.oidc]` names; rights and
levels of any kind, and the specification change they need; service
accounts and the client credentials flow; a session cookie or a server-side confidential client;
OIDC on the Agent plane; token introspection or revocation lists of the provider; signing in
`opamp-fleetctl` with a device flow, so that it sends a bearer token as the UI does and automation
runs through it on behalf of a person.

Local break-glass commands for an incident while the identity provider is down are not decided
here: `server revoke <serial>`, which revokes a certificate and ends its sessions, and
`server enrolment close`, which closes the enrolment window, beside `server audit-verify` and
`server pki status`. They act on the database with the Server's own configuration and credentials,
so their authentication is shell access on a Server host; the set stays that small because an
incident needs a peer cut off, not a rollout. How each run is audited, since it holds no lease
([ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance](draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance.md)),
and that a run which writes to the database sends the notification of
[ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it](draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it.md)
clause 9, so that every instance ends the sessions a revocation concerns, are decided with them.

End-to-end tests against a real Keycloak are not decided here either. Component tests keep the
in-process test issuer with its own JWKS; Keycloak would run beside PostgreSQL and the object
store as a service of `observability/compose.yaml`, the Compose project on the host that is
forwarded into the Dev Container, and as a workflow service in CI.

## Alternatives considered

- **OIDC replacing Basic.** One way in, but no access while the provider is down, and every
  script would need a token exchange. Rejected by the maintainer.
- **A server-side confidential client with a session cookie** (a backend for the frontend). The
  token would never reach JavaScript, but it brings a session store, CSRF defences and a cookie
  policy — what ADR-0021 rejected — and a client secret in `server.toml`. Rejected.
- **A reverse proxy such as oauth2-proxy in front of the plane.** No code in the Server, but a
  second component to run and harden, and the audit would lose the person unless the Server
  trusted a header the proxy sets. Rejected.
- **Rights from the token's roles.** Levels are roles in the provider, read from a configurable
  claim (default `realm_access.roles`), and the Server stores no assignments and administers no
  users. `viewer` reads; `editor` also stores drafts, labels and Deployments, nothing that reaches
  an Agent; `releaser` also publishes, rolls out, revokes, restarts, approves enrolments, issues
  bootstrap certificates and reads the audit record. The levels are cumulative, so one role per
  person suffices, and a token carrying none of them is refused. The Non-Goal "Authorization and
  multi-tenancy" would narrow to "fine-grained authorization and multi-tenancy". It needs no store
  in the Server, but it fixes the rights model before it is decided and ties it to one provider's
  claim layout. Rejected; rights, once the specification has them, are the Server's.
- **`jsonwebtoken` with its `aws_lc_rs` or `rust_crypto` backend.** The first is banned; the second
  brings the `rsa` crate with an advisory that has no fix. Rejected in favour of a *ring* provider
  of our own.
- **`azp` standing in for `aud`.** It would admit a token issued to the UI's client for another
  resource, whose `aud` names that resource. RFC 9068 and RFC 8725 have a resource server accept
  only a token whose audience is its own; `azp` names the client a token was issued to, not the
  resource it is for. Rejected; the provider's audience mapper names `client_id` in `aud`.
- **Refusing every token whose `typ` is not `at+jwt`.** It is RFC 9068's own test, but not every
  provider issues that header by default: Auth0's default access token profile uses `JWT`.
  Rejected for the claims an ID token carries and an access token does not.
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
  access tokens, and its `typ` check that keeps an ID token from passing as one), OpenID Connect
  Core 1.0 section 2 (`nonce`, `at_hash` and `c_hash` as ID token claims), Auth0's
  [access token profiles](https://auth0.com/docs/secure/tokens/access-tokens/access-token-profiles)
  (`at+jwt` only in its RFC 9068 profile), and the IETF draft "OAuth 2.0 for Browser-Based
  Applications" on public clients and token storage.
- [Fetch Metadata Request Headers](https://www.w3.org/TR/fetch-metadata/) (W3C):
  `Sec-Fetch-Mode`, set by the browser and not by a page's script.
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
- Negative / trade-offs: accepting this ADR restates ADR-0021's clauses 13 and 16 and ADR-0012's
  clause 21, or their restatements where those are accepted first, with clauses 1, 6 and 3; the
  rest of those ADRs stands as it is.
- Follow-ups: rights defined in the Server and keyed by `iss` and `sub`, with the specification
  change that narrows its Non-Goal; signing in `opamp-fleetctl` through a device flow; the glossary
  terms identity provider and bearer token.

## Enforcement

Tests that will carry `Verifies: ADR-draft-the-operator-plane-also-signs-in-with-oidc-beside-basic`,
against an in-process test issuer with its own keys, discovery document and JWKS endpoint:

- A valid token on `/api/v1/agents` is admitted; one with a wrong `iss`, an `aud` naming another
  audience while `azp` names `client_id`, an `aud` naming the canonical URI of `/mcp` beside
  `client_id`, an ID token of the UI's client with its `nonce`, or one with `at_hash` and no
  `nonce`, an expired `exp`, a future `nbf`, or a `required_claim` that does not hold is answered
  `401` and audited.
- A token signed with `none`, with HS256 over the RSA key's bytes, with RS256 against an EC key,
  or with ES256 on the wrong curve is refused.
- A token naming an unknown `kid` refetches the key set once, and a second one within the minute
  does not.
- With the issuer unreachable, the Server starts, Basic is admitted, and a bearer request is
  answered `503`.
- With `[rest.auth.oidc]`, `/` and `/api/v1/openapi.json` answer without a credential and
  `/api/v1/agents` answers `401` with a Bearer challenge, beside a Basic challenge when Basic users
  are configured and the request carries no `Sec-Fetch-Mode` or `navigate` there, and without one
  for `Sec-Fetch-Mode: cors`; without it, `/` answers `401` with the Basic challenge.
- `[rest.auth]` with neither `basic_users` nor `[rest.auth.oidc]`, `[rest.auth.oidc]` without
  `required_claim`, and a non-loopback `listen` without `[rest.auth]`, fail startup naming the
  keys.
- An act through a token is audited with `iss`, `sub` and `preferred_username`; an act through
  Basic with the user name; each says which.
- The Content Security Policy names the issuer's origin in `connect-src` with OIDC and does not
  without it.
- `cargo tree` of the Server contains no `aws-lc-rs` and no `rsa`.

**Not mechanically decidable:** that the UI never writes a token to browser storage. A review of
`crates/fleet-server/static/` checks it, and the test above that refuses foreign origins in the
policy limits what a mistake could reach.
