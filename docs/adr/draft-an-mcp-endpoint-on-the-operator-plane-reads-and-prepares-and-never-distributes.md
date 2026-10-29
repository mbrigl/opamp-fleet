# ADR-draft-an-mcp-endpoint-on-the-operator-plane-reads-and-prepares-and-never-distributes: An MCP endpoint on the Operator plane reads and prepares, and never distributes

- **Status:** 🟡 proposed
- **Date:** 2026-10-22
- **Deciders:** Markus Brigl
- **Applies to:** the `/mcp` route and its protected resource metadata on the Operator plane in `crates/fleet-server/src/`, its tool list and the `operator_cases` methods it calls, the `[mcp]` section of `server.toml` and its parsing in `crates/fleet-server/src/config.rs`, `config/server.toml`, the `rmcp` dependency in `Cargo.toml` and `deny.toml`, the audit entries of an MCP call, and every document that tells an operator how to connect an MCP client

## Context

The Server's Operator plane serves the REST API and the bundled UI
([ADR-0025](0025-configurations-and-the-rest-api.md),
[ADR-0012](0012-tls-1-3-two-planes-and-bounded-input.md)).
The specification's goal G-5 makes that API the contract through which any client drives the
fleet. An assistant that speaks the Model Context Protocol (MCP) can use the REST API only through
code written for it; an MCP endpoint lets such a client discover what it may do as typed tools and
call them on behalf of the person using it.

An assistant also acts on what it reads, and what it reads can be written by someone else: a
Configuration body, an Agent's reported attributes, a label. A tool that reaches the fleet would let
such text steer a rollout, a restart or a revocation. The project already separates preparing from
distributing: saving only saves, and a `PUT` changes what *could* be rolled out, never what *is*
([ADR-0026](0026-deployments-and-rollouts.md) clause 1). Nothing an assignment pins can be deleted
([ADR-0026](0026-deployments-and-rollouts.md) clause 12). Every act that reaches an Agent or changes
who is admitted is its own route: a rollout, a restart, a revocation, an enrolment approval, a
bootstrap certificate, the enrolment window, the Gateway mark.

The Server has no rights model. The specification lists authorization as a Non-Goal, and every
operator who passes the Operator plane's guard may do everything
([ADR-draft-the-operator-plane-also-signs-in-with-oidc-beside-basic](draft-the-operator-plane-also-signs-in-with-oidc-beside-basic.md)
clause 6). A ceiling at a level such as `editor` would need levels in the identity provider (see
*Alternatives considered*). Without levels the ceiling can only be the set of tools the endpoint
offers.

Since
[ADR-draft-two-use-case-layers-stand-between-the-servers-planes-and-its-core](draft-two-use-case-layers-stand-between-the-servers-planes-and-its-core.md)
every Operator-plane endpoint reaches state through `operator_cases` alone, so an MCP tool can be
one call of the same method the REST route calls.

MCP revision 2026-07-28 defines Streamable HTTP without protocol-level sessions: every message is a
`POST` to one endpoint, answered with JSON or a request-scoped event stream. A server must validate
the `Origin` header and answer an invalid one `403`. Its authorization is optional; where it is
used, the MCP server is an OAuth 2.1 resource server that serves protected resource metadata
(RFC 9728), names it in a `401`'s `WWW-Authenticate`, and accepts only tokens issued for itself
(RFC 8707), never a token meant for another resource.

## Decision

We will serve an MCP endpoint on the Operator plane whose tools read and prepare through
`operator_cases`, and offer no tool that distributes, revokes, restarts, admits or hands out a
secret.

1. **One endpoint, off by default.** With `[mcp]` in `server.toml` the Operator plane serves `/mcp`
   over MCP's Streamable HTTP transport, revision 2026-07-28, through `rmcp`'s server mounted as an
   axum route ([ADR-0008](0008-five-crates-on-tokio-and-axum.md)
   clause 13). Without `[mcp]` the route does not exist. The Server keeps no state between
   requests: no session, no standalone stream; a `GET` or `DELETE` on `/mcp` is answered `405`. It
   offers tools only — no resources, prompts, sampling or elicitation.

2. **The plane's guard and limits apply.** `/mcp` is a route of the Operator plane: TLS 1.3, the
   body and connection bounds of ADR-0012, the throttle, and the guard of
   [ADR-draft-the-operator-plane-also-signs-in-with-oidc-beside-basic](draft-the-operator-plane-also-signs-in-with-oidc-beside-basic.md)
   clause 1 — Basic credentials in the `Authorization` header, a bearer token validated as that
   ADR's clause 2 says, or both, as configured. A request whose `Origin` header is present and is
   not the Operator plane's own origin is answered `403`. A request whose `Host` names neither a DNS
   name nor an address of the Operator plane's certificate is answered `403`. The MCP client's
   self-reported `clientInfo` is a label, never an identity.

3. **With OIDC, `/mcp` is its own protected resource.** When OIDC is configured:
   - The Server serves `/.well-known/oauth-protected-resource/mcp` without credentials. It names
     `resource`, the endpoint's canonical URI `https://<host>[:<port>]/mcp` with `<host>` from
     `[mcp] public_name`, and `authorization_servers`, the configured OIDC issuer, and nothing
     else.
   - A `401` on `/mcp` carries `WWW-Authenticate: Bearer resource_metadata="…"` and no Basic
     challenge.
   - A bearer token is accepted on `/mcp` only with the audience that
     [ADR-draft-the-operator-plane-also-signs-in-with-oidc-beside-basic](draft-the-operator-plane-also-signs-in-with-oidc-beside-basic.md)
     clause 2 sets for it: `aud` names that canonical URI, and `azp` does not stand in. The same
     clause refuses such a token on `/api/v1`, and an ID token on every route. A token whose
     audience is the UI's client, or any other, is answered `401` on `/mcp`, and the Server passes
     no token on.
   - Registering the MCP client — pre-registered, a client ID metadata document, or dynamic
     registration — is the identity provider's business. The Server serves no registration and no
     token endpoint.
   Without OIDC no metadata is served, and an MCP client sends Basic credentials as a configured
   header.

4. **The tools read.** One tool per read route of the REST API: the Agents, the Hosts, the
   Configurations, the Packages, the Deployments, the certificates the client CA signed, the
   revocation list, the enrolment window and the waiting enrolment requests, and the bootstrap
   certificates' list. A list tool takes the limit, cursor and filters of
   [ADR-draft-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server](draft-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server.md)
   (paging) and returns one page. No tool returns a bootstrap PKCS#12 file, a password, a key, or a
   credential of `[rest.auth]`.

5. **The tools prepare.** One tool per act that changes only what *could* be rolled out
   (ADR-0026 clause 1): saving a Configuration; setting an Agent's labels; creating or saving a
   Deployment, setting its Selector, and adding or removing a Configuration or a Package in it. A
   Package, its entries and its signatures are read and never written: artifact bytes do not pass
   through an assistant's context, and a signature is the operator's release gate.

6. **Nothing else is a tool.** No tool rolls out, restarts, forgets or deletes anything, revokes or
   withdraws a revocation, approves or rejects an enrolment, opens or closes the enrolment window,
   issues or deletes a bootstrap certificate, or sets a Gateway mark. The tool list is fixed in
   code, each tool names the one `operator_cases` method it calls, and a tool carries no logic of
   its own: arguments in, the method's result or error out.

7. **Every change is audited as its REST counterpart.** A preparing tool writes the audit entry
   its REST route writes, with the person the guard admitted, `channel` `mcp`, and the MCP
   client's reported name and version. A read is not audited, as a REST read is not.

**Out of scope:** a rights model and levels of any kind; tools that distribute, whatever a later
rights model allows; MCP resources, prompts and subscriptions; the protocol revisions before
2026-07-28 and their sessions; an authorization server of the Server's own; MCP on the Agent plane.

## Alternatives considered

- **MCP behind the identity provider's levels.** `viewer`, `editor` and `releaser` are cumulative
  roles in the provider, and a token carrying none of them is refused. MCP is at most `editor`: it
  reads and prepares drafts, labels and Deployments, and offers no tool at `releaser` level, so
  publishing, rolling out, revoking, restarting and approving stay with a person in the UI or the
  CLI. The ceiling lives in the provider, where the MCP client is registered without full scope
  and mapped to `viewer` and `editor` only, so its tokens never carry `releaser`; the missing
  tools are the second line. It needs a rights model the specification excludes and the Server
  does not have. The tool list holds the same ceiling without one, and a later rights model can
  narrow it. Rejected.
- **Every REST route as a tool, with a confirmation the client shows.** The confirmation lives in
  the MCP client, which the Server neither controls nor can verify. An assistant steered by text it
  read could still roll out. Rejected.
- **Package uploads and signatures as tools.** Uploading needs the artifact's bytes in the call,
  which an assistant has no business holding, and attaching a signature is the step that makes a
  Package releasable with the operator's offline key. Rejected.
- **Deleting a Configuration or a Deployment as a tool.** Nothing an assignment pins can be deleted
  (ADR-0026 clause 12), so a delete does not reach an Agent; but it destroys an operator's work
  without a way back, on text an assistant read. Rejected; deleting stays with the UI and the API.
- **A separate MCP server process in front of the REST API.** It keeps MCP out of the Server, but
  is a second component to run and harden, holds a credential to the whole API, and puts the
  person behind a proxy the audit cannot see through. Rejected.
- **Sessions of the earlier Streamable HTTP revisions.** They need state per client, which several
  Server instances would have to share, and revision 2026-07-28 removed them. Rejected.
- **No MCP; assistants use the REST API through generated code.** It works, but leaves each
  assistant to build its own client and its own ceiling. Rejected.

## Sources / Prior art

- The working note "server split", section D6, recorded outside the ADRs.
- [MCP authorization, revision
  2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization):
  authorization optional; protected resource metadata (RFC 9728) required where used; `401` with
  `WWW-Authenticate: Bearer resource_metadata`; audience validation per RFC 8707; "MCP servers MUST
  NOT accept or transit any other tokens".
- [MCP Streamable HTTP, revision
  2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http):
  one `POST` endpoint, no protocol-level sessions and no `GET` stream, `Origin` validation with
  `403`, `405` for `GET` and `DELETE` from older clients.
- `rmcp` 3.6.0 ([repository](https://github.com/modelcontextprotocol/rust-sdk), Apache-2.0):
  `ProtocolVersion::V_2026_07_28` as `LATEST`; `StreamableHttpService`, a tower service an axum
  router mounts; `allowed_hosts` and `allowed_origins` in its server configuration. A probe with
  the features `server`, `macros` and `transport-streamable-http-server` resolved 84 crates with no
  TLS stack, no `reqwest`, no `aws-lc-rs` and no OpenSSL; its `auth` feature, which would pull
  `oauth2` and `reqwest`, stays off.
- The REST routes in `crates/fleet-server/src/api.rs`, and the rule that saving only saves in
  ADR-0026 clauses 1 and 24.

## Consequences

- Positive: an assistant can read the fleet and prepare a change — a Configuration, labels, a
  Deployment — that an operator then reviews and rolls out in the UI. G-5 gains a second client
  kind without a second contract.
- Positive: text an assistant reads cannot make it reach an Agent, revoke a certificate or admit a
  host, whatever the client or the model does.
- Positive: the stateless transport needs nothing shared between Server instances.
- Negative / trade-offs: a new dependency, `rmcp`, with `schemars` and its macros, for
  `cargo-deny` to check, and an MCP revision to follow.
- Negative / trade-offs: the tool list is a second surface beside the OpenAPI document; a new
  preparing route needs its tool added by hand, and a new distributing route must be kept out.
- Negative / trade-offs: with OIDC, the identity provider needs a client for MCP and an audience
  mapper for the canonical URI, which the operator documentation has to describe.
- Follow-ups: a rights model, should the specification take one up, and whether it narrows the
  tools per person; MCP resources for large read-only content such as an effective configuration.

## Enforcement

Tests that will carry `Verifies:
ADR-draft-an-mcp-endpoint-on-the-operator-plane-reads-and-prepares-and-never-distributes`:

- Without `[mcp]`, `/mcp` is answered `404`; with it, `tools/list` names exactly the read and
  preparing tools of clauses 4 and 5, and each tool's `operator_cases` method is on an allow list
  that contains no method behind a rollout, restart, delete, revocation, enrolment decision,
  enrolment window, bootstrap certificate or Gateway mark.
- A request with a foreign `Origin`, or a `Host` the certificate does not name, is answered `403`;
  a `GET` on `/mcp` is answered `405`.
- Without credentials `/mcp` is answered `401`; with OIDC the challenge names the resource
  metadata, the metadata names the canonical URI and the issuer, a token for another audience is
  refused on `/mcp`, and a token for `/mcp` is refused on `/api/v1`.
- A preparing tool writes the audit entry of its REST route with `channel` `mcp` and the client's
  name; a read tool writes none.
- No tool's result contains a PKCS#12 file, a key or a password.
