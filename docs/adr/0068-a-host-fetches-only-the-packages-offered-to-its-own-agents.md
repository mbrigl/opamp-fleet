# ADR-0068: A host fetches from the download route only the artifact offered to an Agent it speaks for, and everything else is answered as if it did not exist

- **Status:** ⚪ superseded by [ADR-0070](0070-a-host-fetches-only-what-its-agents-are-offered-and-a-gateway-caches-it-for-the-hosts-behind-it.md)
- **Date:** 2026-10-06
- **Deciders:** Markus Brigl
- **Applies to:** the download route `GET /api/v1/packages/{agent_type}/{version}/file`, its handler `download_package` in `crates/fleet-server/src/api.rs`, the test of what is offered that `offer_for_assigned` and the download share in `crates/fleet-server/src/packages.rs`, what a host speaks for in `crates/fleet-server/src/revocation.rs` and `crates/fleet-server/src/fleet.rs`, the `download.refused` audit entry, the startup notice of `crates/fleet-server/src/main.rs` when `[client_ca]` is absent, the Non-Goal "Authorization and multi-tenancy" in `docs/SPECIFICATION.md`

## Context

The download route serves an uploaded artifact's bytes on the Agent plane
([ADR-0043](0043-the-package-store-references-artifacts-only-over-tls-beyond-the-loopback.md) clause 13,
[ADR-0045](0045-packages-and-deployments-that-sign-every-package.md) clause 6). It sits behind the
same TLS handshake as `/v1/opamp`
([ADR-0054](0054-tls-1-3-plaintext-on-the-loopback-alone-and-bounded-planes-bodies-and-messages.md)
clause 8) and requires a certificate from the client CA
([ADR-0059](0059-admission-by-a-client-certificate-alone.md) clause 23). Admission decides fleet
membership. None of these says which member may fetch which artifact. Today
`download_package` serves any artifact in the store to any member that names it: type, version and
Platform are all in the path and the query, and both are easy to guess.

Membership is fleet-wide, but releases are not. A rollout releases a pinned Package to one Agent
at a time ([ADR-0027](0027-rollout-and-what-reaches-an-agent.md) clauses 2, 3, 5), and a
Deployment aims at a partition of the fleet by a host property such as `channel`, `region` or
`tenant` ([ADR-0045](0045-packages-and-deployments-that-sign-every-package.md) clause 11). With
the route open, a host in one partition can fetch another partition's builds. It can fetch a
version the operator has saved but not yet released, such as the next canary build or a build
licensed to one tenant. A compromised host can also map the whole store by trying versions. An
artifact encrypted with an `archive_key` stays unreadable
([ADR-0043](0043-the-package-store-references-artifacts-only-over-tls-beyond-the-loopback.md) clause 6), but most
artifacts are not encrypted. The specification puts security before convenience and says that no
vulnerability can be ruled out (Strategy *Security before convenience*). The host that leaks least
when it is compromised is the one that was never given more than its own Agents receive.

Forces:

- **The host is the one bound the Server has.** A certificate proves fleet membership and the
  host it was issued to, not an Agent. An `instance_uid` belongs to the host whose certificate
  first reported it. A host marked as a Gateway speaks for any Agent
  ([ADR-0059](0059-admission-by-a-client-certificate-alone.md) clause 7). Admission is a fleet-wide
  trust boundary, and within it the host is the only bound between Agents (clause 14). Any test of
  "may fetch" can be no finer than the host.
- **What reaches an Agent is already decided, per Agent.** Its package offer is composed from its
  assignment alone. The offer is the assigned Package's entry for the Platform the Agent reports,
  signed by the Deployment that released it
  ([ADR-0027](0027-rollout-and-what-reaches-an-agent.md) clause 3,
  [ADR-0045](0045-packages-and-deployments-that-sign-every-package.md) clause 14). Type fit is a
  mandatory precondition of every offer
  ([ADR-0043](0043-the-package-store-references-artifacts-only-over-tls-beyond-the-loopback.md) clause 9). The download
  needs no rule of its own. It needs the same test the offer uses.
- **The code has a gap against that clause.** `offer_for_assigned` (through `assigned_entry`)
  tests the Platform but not the Agent type. Type fit is checked when the operator rolls out, and
  is not checked again when the offer is composed. An Agent that reports a different `service.name`
  after the rollout is still offered the old type's Package.
- **Configurations already reach only their own Agent.** A composed config map travels in the
  `ServerToAgent` addressed to one `instance_uid`. It is composed from what was released to that
  Agent ([ADR-0016](0016-configurations-and-the-rest-api.md) clauses 6, 7,
  [ADR-0027](0027-rollout-and-what-reaches-an-agent.md) clause 3). It answers only reports the host
  may make for that `instance_uid`. No route on the Agent plane serves configurations.
- **Downloads behind a Gateway do not reach the Server today.** A Client resolves a path
  `download_url` against its own OpAMP endpoint (`resolve_url` in
  `crates/fleet-agent/src/packages.rs`). Behind a Gateway that endpoint is the Gateway, which
  serves only `/v1/opamp` (`opamp::server::router` without `any_path`). The `GET` is answered `404`
  there. With `advertised_url` set, the offered URL is absolute and names the Server. That URL is
  not the Client's own origin, so `Sources::permit` refuses it unless it is listed in `[packages]
  allowed_sources`. If it is listed, the Client fetches it with its anonymous client, which
  presents no certificate, and the Server's handshake refuses it
  ([ADR-0059](0059-admission-by-a-client-certificate-alone.md) clause 23). A Client behind a
  Gateway therefore receives only referenced artifacts. The only downloads a Gateway's certificate
  makes are those of the Gateway host's own Agents.
- **A partition is only as strong as the hosts it partitions.** A Selector matches the effective
  description, which is what the Agent reports plus the Server's labels
  ([ADR-0045](0045-packages-and-deployments-that-sign-every-package.md) clauses 8, 11). It cannot
  tell the two sources apart, and where they collide, what the Agent reports wins
  (`labels::effective_description`). A compromised host can therefore report another partition's
  key and value, such as the Client's own `[attributes]` table would, under a fresh
  `instance_uid`. That Agent waits in the fleet view
  ([ADR-0027](0027-rollout-and-what-reaches-an-agent.md) clause 6). The next bulk rollout of that
  partition's Deployment assigns it, and the host can then fetch what was released to it. This
  holds for a partition set by a Server label as well, because a reported attribute with the
  label's key satisfies the same Selector. What this decision bounds is fetching what was released
  to Agents the host does not speak for. It does not bound an Agent the host itself places in a
  partition.
- **Hiding is cheap only if it is complete.** An answer that differs between "exists, not yours"
  and "does not exist" lets a host list the store. HTTP allows a server to hide a forbidden resource
  behind `404` (RFC 9110 §15.5.4).

## Decision

We will serve an uploaded artifact on the download route only to a member certificate whose host
speaks for an Agent to which that artifact, identified by Agent type, version and Platform, is
currently offered, and answer every other request for an artifact exactly as we answer one for an
artifact that does not exist.

1. **"Currently offered" is the offer's own test, shared.** An artifact `(agent_type, version,
   os, arch)` is offered to an Agent when all of the following hold:
   - the Agent declares `AcceptsPackages`;
   - it holds a package assignment naming that Package;
   - the Agent type equals the `service.name` it reports;
   - the Platform it reports, canonicalised, is the requested one, and the Package holds an
     uploaded entry for it;
   - the Deployment named in the assignment holds a signature for that entry.

   The offer and the download decide this from the same parts in `packages.rs`: one test of the
   Agent type and the Platform (`fits`), the assignment, and the signing Deployment. The download
   resolves the entry and its signers once before it takes the fleet lock, so each record costs no
   store lookup. A test that runs every combination through both keeps them from disagreeing.
   Adding the type test closes the gap against
   [ADR-0043](0043-the-package-store-references-artifacts-only-over-tls-beyond-the-loopback.md) clause 9 in the same change.

2. **An offer stands until the assignment changes, not until it was last sent.** The hash gate
   suppresses re-sending an offer whose `all_packages_hash` the Agent has echoed
   ([ADR-0027](0027-rollout-and-what-reaches-an-agent.md) clause 3). It does not withdraw the
   offer, so an Agent retrying after a failed install can fetch again. Neither connection state nor
   the version test ([ADR-0027](0027-rollout-and-what-reaches-an-agent.md) clause 17) enters the
   test. Matching does not enter it either: a version that waits for an operator's press is no
   one's offer, and it cannot be fetched until the press releases it.

3. **What a certificate speaks for.** The host a member certificate names
   (`urn:opamp-fleet:host:<id>`, [ADR-0059](0059-admission-by-a-client-certificate-alone.md)
   clause 7) speaks for the `instance_uid`s bound to it in the host register. A host marked as a
   Gateway speaks for any Agent: the register binds no `instance_uid` to it, and the Server keeps
   no record of which Agents a Gateway carries. A Gateway's certificate therefore may fetch what is
   offered to any Agent. A certificate that names no host speaks for no Agent and fetches nothing.
   A Client whose Server signs CSRs holds a host certificate after its first connection
   ([ADR-0059](0059-admission-by-a-client-certificate-alone.md) clause 9). An operator who
   provisions certificates by hand names the host with that SAN URI. A Server started without
   `[client_ca]` logs one notice at startup: uploaded artifacts reach only hosts whose certificate
   names a host (`urn:opamp-fleet:host:<id>`). A request that presents no certificate at all exists
   only behind `Admission::open`, which serves tests and loopback development and which no Server
   configuration serves ([ADR-0059](0059-admission-by-a-client-certificate-alone.md) clause 6). Such
   a request is not tested for an offer, just as it is not tested at admission.

4. **Every other request for an artifact is answered `404`, as for one that does not exist.** The
   status, the body and the headers are the same whether the store holds no such artifact, holds
   only a referenced entry for it, or holds an artifact that is not offered to any Agent this host
   speaks for. The Server decides this before it opens a file. Two answers are unchanged:
   - a malformed identity or Platform token is still `400`, which concerns syntax and reveals
     nothing about the store;
   - admission refusals (no member certificate, a bootstrap certificate, a revoked certificate)
     are still `401` ([ADR-0059](0059-admission-by-a-client-certificate-alone.md) clause 23).

5. **The handler decides, after it has parsed the request.** `download_package` parses the
   identity (`PackageId::new`) and the Platform (`query.platform()`) as it does now, so one parser
   decides which artifact is meant and a malformed token is answered `400` before any offer is
   tested. It then reads the host from the presented certificate: from the `PeerCertificate`
   extension through `ca::facts`, or from the `Proofs` the guard puts into the request. It asks
   the fleet `Fleet::offers_artifact(host, id, platform)`, which reads the host register
   (`Revocations::speaks_for`) and the Agent records under the fleet lock. Only after that does it
   look for the file. `admit_download` keeps only admission
   ([ADR-0059](0059-admission-by-a-client-certificate-alone.md) clause 23) and records
   `download.admitted` as now. A request that fails the offered test is also recorded as
   `download.refused`, outcome `refused`, with check `not offered`, the host, the certificate's
   serial and the requested type, version and Platform. That entry goes through the refusal
   aggregation of [ADR-0063](0063-an-append-only-audit-record-chained-by-hash.md) clause 5. It does
   not count toward the admission throttle
   ([ADR-0059](0059-admission-by-a-client-certificate-alone.md) clause 24): the handshake
   succeeded, so the refusal is not a guess at a credential, and counting it would throttle every
   member behind the same address.

6. **Each download request is counted against the requesting host's rate.** It costs one token
   from that host's bucket of [ADR-0066](0066-admitted-agents-are-rate-limited-per-host.md). A
   download names no `instance_uid`, so for a host marked as a Gateway the token comes from the
   Gateway's aggregate bucket (ADR-0066 clause 4). That bucket bounds how often a Gateway's request
   can scan the fleet's records under the fleet lock.

7. **Configurations: no change.** An Agent already receives only its own composed map, over
   OpAMP, from what was released to it (Context). This decision states that bound for the record
   and adds nothing to it.

**Out of scope:**

- Artifacts hosted elsewhere. A referenced entry (`source`) is fetched from the operator's host
  under `[packages] allowed_sources`, with whatever headers the offer carries
  ([ADR-0043](0043-the-package-store-references-artifacts-only-over-tls-beyond-the-loopback.md) clause 4,
  [ADR-0042](0042-signed-package-delivery-from-allowed-sources.md)). The Server is not in that
  download path, and who can fetch from that host is decided there.
- Relaying the download route through a Gateway, so that a Client behind a Gateway can fetch an
  uploaded artifact.
- A bound finer than the host ([ADR-0059](0059-admission-by-a-client-certificate-alone.md)
  clause 14).

- Roles, permissions and tenancy on the Operator plane.

## Alternatives considered

- **Leave the route open to every member.** This is the status quo. Membership becomes a key to
  every build the store holds, including builds released to another partition and builds released
  to no one yet. It also lets a compromised host list the store. The cost of closing it is one
  lookup per download.
- **Answer `403` for an artifact that exists but is not offered.** This is the more literal status
  code, but it tells a host which type, version and Platform the store holds, which is the
  enumeration this decision closes. RFC 9110 explicitly allows `404` for this purpose.
- **Bind the fetch to an Agent instead of the host**, by naming an `instance_uid` in the
  `download_url` or in a header. The `instance_uid` is self-asserted, and the only thing the
  Server can check against is the host binding. The result is the same bound with an extra request
  parameter that the Baseline's `DownloadableFile` does not need.
- **Signed, expiring download URLs**, like pre-signed object-store links. Whoever holds the URL
  can fetch, and the URL ends up in logs. The Server would need a signing key and its rotation.
  An expiry would also conflict with a retry after a failed install, which re-reads an offer the
  hash gate no longer re-sends. The handshake already identifies the host.
- **Allow whatever is a candidate for the host's Agents** (what matching would release now). This
  would let a host fetch a build the operator saved but did not release. That breaks the rule that
  a release happens only by an operator's act
  ([ADR-0027](0027-rollout-and-what-reaches-an-agent.md) clause 1).
- **Limit a Gateway to the Agents currently carried on its connections.** The Server would have to
  track which connection or host carries each Agent, which it does not do today: the register
  binds nothing to a Gateway, and `owner` is a WebSocket connection id. That tracking would buy
  nothing, because a Gateway's certificate may report under any `instance_uid`
  ([ADR-0059](0059-admission-by-a-client-certificate-alone.md) clause 14) and so can receive any
  Agent's offer over OpAMP anyway.
- **Treat a certificate that names no host as membership alone**, able to fetch anything offered.
  This would keep downloads unchanged for hand-provisioned certificates without the SAN URI. But it
  removes the bound for exactly the certificates the register cannot track.
- **Bind certificates that name no host by their issuer and serial**, as
  [ADR-0066](0066-admitted-agents-are-rate-limited-per-host.md) keys its buckets, so that a hand-provisioned certificate speaks for the Agents that
  reported with it. This would keep G-10 working without changes for a fleet whose Server signs no
  CSRs. It was not chosen. It means a second binding register next to the host register, and its
  entries would have to be carried across certificate replacements that the Server never sees,
  because without `[client_ca]` the Server does not issue the next certificate. The bound would
  also lapse whenever an operator replaces a certificate by hand. The G-10 cost of declining it
  falls on such fleets: they deliver uploaded artifacts only after their certificates name a host,
  and referenced artifacts are unaffected.
- **Check the offer in the `admit_download` middleware.** The middleware would have to parse the
  path and query a second time, and could answer "not offered" for a request the handler would
  have answered `400`. Two parsers could disagree on which artifact is meant.
- **Count `not offered` refusals toward the admission throttle.** That throttle counts a peer
  address, so one misbehaving host would throttle every member behind a shared address. The record
  already holds the refusals, aggregated.

## Sources / Prior art

- [OpAMP specification v0.20.0](https://github.com/open-telemetry/opamp-spec/blob/v0.20.0/specification.md),
  section *Packages*: "The PackagesAvailable message describes the packages that are available on
  the Server for this Agent". Its URLs "point to package files on a Download Server (which may be on
  the same host as the OpAMP Server or a different host)". Section *Downloading Packages*, step 3:
  the Agent uses "an HTTP GET message" with the offered headers. Section *Packages / Security
  Considerations* and section *Security* say nothing about which Agent may fetch which file. This
  decision fills that gap on the Server and changes nothing on the wire.
- The vendored schema `crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto`: `PackagesAvailable` is
  the "List of packages that the Server offers to the Agent"; `DownloadableFile.download_url`.
- [RFC 9110 §15.5.4](https://www.rfc-editor.org/rfc/rfc9110#section-15.5.4): "An origin server
  that wishes to 'hide' the current existence of a forbidden target resource MAY instead respond
  with a status code of 404 (Not Found)."
- The code read for this record: `offer_for_assigned`, `assigned_entry` and `to_available` in
  `crates/fleet-server/src/packages.rs`; `packages_offer` and `AgentRecord` in
  `crates/fleet-server/src/fleet.rs`; `check_report` and `Host` in
  `crates/fleet-server/src/revocation.rs`; `download_package` in `crates/fleet-server/src/api.rs`;
  `admit_download` and `Proofs` in `crates/fleet-server/src/transport.rs`; `resolve_url`,
  `Sources` and `send_download` in `crates/fleet-agent/src/packages.rs`; the Gateway's router in
  `crates/fleet-agent/src/gateway/mod.rs`.

## Consequences

- Positive (Strategy *Security before convenience*): a compromised host gets from the store only
  what its own Agents were released. What was released to another host's Agents
  stays with them, within the limit the Negative entry on partitions names. An unreleased build cannot be fetched by anyone, and the store's contents
  cannot be listed by probing.
- Positive (G-10): the offer and the download share one test, so a Client that was offered an
  artifact can always fetch it. The type check added to the offer closes a gap against
  [ADR-0043](0043-the-package-store-references-artifacts-only-over-tls-beyond-the-loopback.md) clause 9.
- Positive (Q-1): no setting turns the bound off. It holds whatever the configuration is.
- Negative / trade-offs: a certificate that names no host fetches nothing. Two cases are affected:
  a fleet whose Server signs no CSRs (no `[client_ca]`) and whose operator provisions
  certificates without the host SAN URI, and a third-party Agent with such a certificate. Each now
  gets `404` where it used to get bytes. The startup notice (clause 3) and the audit entry say why.
  The manual (`docs/manual/server.md`, `[client_ca]` and the download route) states that without
  `[client_ca]` a hand-provisioned certificate must carry `urn:opamp-fleet:host:<id>` to receive
  uploaded artifacts.
- Negative / trade-offs: a partition is only as strong as the hosts it partitions (Context). A
  compromised host can report another partition's attributes under a fresh `instance_uid`, be
  assigned by that partition's next bulk rollout, and then fetch what was released. Today a
  partition set by a Server label is not stronger, because a reported attribute satisfies the same
  Selector. Making labels authoritative for a partition would close this: a Selector term that
  only a label can satisfy, or a reported key that is refused when it shadows a label key. That
  is a follow-up. The fleet view shows such an Agent as waiting before the press that assigns it.
- Negative / trade-offs: one lookup under the fleet lock per download. For an ordinary host this
  is a lookup per bound `instance_uid` (at most 256). For a Gateway it is a scan of the fleet's
  records, because it speaks for any Agent. The rate limit of clause 6 bounds how often that scan runs. An index from artifact to offering
  Agents is an option if the scan shows up.
- Negative / trade-offs: a Gateway's certificate stays as broad as its mark already makes it.
  This record makes that explicit and does not narrow it.
- Negative / trade-offs: an Agent that reports a different `service.name` after it was rolled out
  to is no longer offered the old type's Package. That follows from
  [ADR-0043](0043-the-package-store-references-artifacts-only-over-tls-beyond-the-loopback.md) clause 9, but it is new
  behaviour on the wire.
- Follow-ups: a Selector term that only a Server label satisfies, so that a partition can hold
  against the host it partitions; delivering uploaded artifacts to Clients behind a Gateway, by relaying the route or
  by fetching with the Gateway's own certificate. `SECURITY.md` (the trust-boundary section) and
  the manual's download-route rows in `docs/manual/server.md` will each state the new bound.

## Enforcement

The tests are planned. Each one carries `Verifies: ADR-0068`
([ADR-0003](0003-decisions-verified-by-tests.md)).

- `crates/fleet-server/src/packages.rs` (planned):
  - `the_download_and_the_offer_test_one_predicate`: for every combination, the offer and the
    download agree on what is offered (clause 1).
  - `an_agent_reporting_another_type_is_offered_nothing` (clause 1).
  - `an_entry_its_deployment_does_not_sign_is_not_offered` (clause 1).
  - `a_referenced_entry_is_never_offered_for_the_route` (clause 1, 4).
- `crates/fleet-server/src/revocation.rs` (planned):
  - `a_host_speaks_for_the_agents_bound_to_it_and_a_gateway_for_any` (clause 3).
- `crates/fleet-server/src/fleet.rs` (planned):
  - `an_artifact_is_offered_to_a_host_only_through_an_agent_it_speaks_for` (clauses 1, 3).
  - `an_offer_still_stands_after_its_hash_is_echoed` (clause 2).
  - `a_version_waiting_for_its_press_is_offered_to_no_host` (clause 2).
- `crates/fleet-server/tests/mutual_tls.rs` (planned):
  - `a_host_fetches_the_artifact_offered_to_its_own_agent` (clause 3).
  - `a_host_is_answered_404_for_an_artifact_offered_only_to_another_host` (clauses 3, 4).
  - `an_artifact_not_offered_and_one_not_held_are_answered_alike`, which compares status, body and
    headers byte for byte (clause 4).
  - `a_marked_gateway_fetches_what_is_offered_to_any_agent` (clause 3).
  - `a_certificate_naming_no_host_fetches_nothing` (clause 3).
  - `a_refused_fetch_leaves_one_download_refused_entry_naming_its_check`, which also checks that
    the throttle did not count it (clause 5).
  - `a_malformed_token_is_answered_400_before_the_offer_is_tested` (clause 5).
  - `a_download_costs_a_token_of_the_hosts_bucket` (clause 6).
- `crates/fleet-server/src/main.rs` (planned):
  - `a_server_without_client_ca_says_uploaded_artifacts_need_a_host` (clause 3).
- Tests served over `Admission::open`, where no certificate is presented, are unchanged (clause 3):
  - `crates/fleet-agent/tests/packages_e2e.rs`;
  - `crates/fleet-agent/tests/http_transport_e2e.rs`, the two Servers it starts.
- `crates/fleet-server/tests/packages.rs`:
  - `an_uploaded_set_is_offered_downloaded_and_gated` and
    `an_artifact_larger_than_the_framework_default_uploads_and_downloads_intact` move to a member
    certificate that names the reporting Agent's host, so that they exercise the offered test.
  - `a_version_waiting_for_its_rollout_cannot_be_fetched` (planned, clause 2).
- `crates/fleet-agent/src/gateway/mod.rs` (planned):
  - `the_gateway_serves_no_download_route` keeps the Context statement about Gateways true. A
    change that makes the Gateway relay downloads has to revisit clause 3.
