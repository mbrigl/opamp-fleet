# ADR-0070: A host fetches from the download route only the artifact offered to an Agent it speaks for, a Gateway fetches each uploaded artifact it relays once and passes it on only to the hosts it relayed it to, and everything else is answered as if it did not exist

- **Status:** 🟢 accepted
- **Date:** 2026-10-06
- **Deciders:** Markus Brigl
- **Applies to:** the download route `GET /api/v1/packages/{agent_type}/{version}/file`, its handler `download_package` in `crates/fleet-server/src/api.rs`, the test of what is offered that `offer_for_assigned` and the download share in `crates/fleet-server/src/packages.rs`, what a host speaks for in `crates/fleet-server/src/revocation.rs` and `crates/fleet-server/src/fleet.rs`, the `download.refused` audit entry, the startup notice of `crates/fleet-server/src/main.rs` when `[client_ca]` is absent, the Non-Goal "Authorization and multi-tenancy" in `docs/SPECIFICATION.md`, the Gateway's package cache and its download route in `crates/fleet-agent/src/gateway/` (`cache.rs`, the route in `mod.rs`, the offers `registry.rs` records), the `[gateway] package_cache_bytes` key of supervisor.toml and its parsing in `crates/fleet-agent/src/config.rs`, the download helpers of `crates/fleet-agent/src/packages.rs` the Gateway fetches with and the Client's waiting on `Retry-After` there, and the cache directory under the Client's `state_dir`
- **Supersedes:** [ADR-0068](0068-a-host-fetches-only-the-packages-offered-to-its-own-agents.md)

## Context

Supersedes [ADR-0068](0068-a-host-fetches-only-the-packages-offered-to-its-own-agents.md)
because a Client behind a Gateway must receive uploaded artifacts, and the maintainer decided that
the Gateway delivers them from a cache of its own rather than by relaying each request. ADR-0068
decides who receives which uploaded artifact, and it left the Gateway out on purpose: its Context
records that downloads behind a Gateway do not reach the Server, its Out of scope names relaying
the download route through a Gateway, and its Enforcement asks a change that makes the Gateway
deliver downloads to revisit clause 3. A Gateway that hands an artifact to a downstream host
answers the same question one hop further down, so its rule belongs in the decision that answers
it at the Server. Every clause of ADR-0068 is restated below under its own number. Clause 3 is
revisited and stands unchanged: a marked Gateway's certificate fetches what is offered to any
Agent, and that breadth is what the cache fetches with. Clauses 8 to 14 bound what the Gateway
passes on to the hosts behind it, so that a downstream host receives from the Gateway no more than
the Server would serve it directly.

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
- **Without a cache in the Gateway, downloads behind it do not reach the Server.** A Client resolves a path
  `download_url` against its own OpAMP endpoint (`resolve_url` in
  `crates/fleet-agent/src/packages.rs`). Behind a Gateway that endpoint is the Gateway, which
  serves only `/v1/opamp` (`opamp::server::router` without `any_path`). The `GET` is answered `404`
  there. With `advertised_url` set, the offered URL is absolute and names the Server. That URL is
  not the Client's own origin, so `Sources::permit` refuses it unless it is listed in `[packages]
  allowed_sources`. If it is listed, the Client fetches it with its anonymous client, which
  presents no certificate, and the Server's handshake refuses it
  ([ADR-0059](0059-admission-by-a-client-certificate-alone.md) clause 23). A Client behind a
  Gateway that serves no download route therefore receives only referenced artifacts, and the only
  downloads a Gateway's certificate makes are those of the Gateway host's own Agents.
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
- **Behind a Gateway, one artifact crosses one link once per Agent.** A Gateway stands at a
  network boundary in front of a site or a segment (specification, Gateway Mode; G-15). A rollout
  that releases one Package to the n Agents behind it sends the same bytes n times over the link
  the Gateway was placed to spare, and asks the Server's store and its fleet lock n times.
- **Relaying each request costs what the cache saves.** A Gateway that relays the route fetches
  with its own certificate, because it terminates the downstream handshake and presents its own
  upstream ([ADR-0071](0071-client-modes-and-a-gateway-that-passes-packages-only-to-the-hosts-they-were-offered-to.md)
  clause 11); no downstream certificate ever reaches the Server. Every relayed request costs one
  token of the Gateway's aggregate bucket (clause 6,
  [ADR-0066](0066-admitted-agents-are-rate-limited-per-host.md) clause 4), so a rollout to more
  Agents than that bucket's burst throttles itself by its own size, and every byte still crosses
  the link once per Agent.
- **The Gateway already sees every offer it carries.** Each `ServerToAgent` for an Agent behind it
  passes through its registry on the way down (ADR-0071 clause 13), with the offer's
  `download_url` and `content_hash`. The registry knows which downstream connection carries the
  Agent, and so the certificate that connection presented. The Gateway forwards the message
  unchanged; reading it changes nothing on the wire.
- **The Gateway's certificate is as broad as its mark makes it.** Under clause 3 it may fetch what
  is offered to any Agent. A cache gives it no artifact it could not fetch already. What a cache
  must not do is widen what a downstream host receives: a Gateway that served whatever it holds to
  any admitted peer would reopen, behind the Gateway, the enumeration and the cross-partition fetch
  that clause 3 closes at the Server.
- **A downstream host is named the way the Server names it.** A host behind a Gateway enrols with
  the Server directly or is provisioned by an operator
  ([ADR-0059](0059-admission-by-a-client-certificate-alone.md) clause 25). A certificate the Server
  issued carries `urn:opamp-fleet:host:<id>`, and an operator who provisions by hand names the host
  with that SAN URI (clause 3). The Gateway reads it from its own handshake. The host register is
  the Server's, and the Gateway does not hold it.
- **Verification stays with the installing Client.** The Client checks the content hash and the
  signature over type, version and hash before it installs, whatever host served the bytes
  ([ADR-0042](0042-signed-package-delivery-from-allowed-sources.md) clauses 4, 5). A cache on the
  path changes where the bytes come from, not what is installed. Checking the hash at the Gateway
  as well keeps it from storing or passing on bytes the offer does not name, and a corrupted fetch
  is noticed once there instead of by every Agent behind it.

## Decision

We will serve an uploaded artifact on the download route only to a member certificate whose host
speaks for an Agent to which that artifact, identified by Agent type, version and Platform, is
currently offered, have a Gateway fetch each uploaded artifact it relays an offer of once with its
own certificate and pass it on only to a downstream host whose Agent it relayed that offer to, and
answer every other request for an artifact, on the Server and on a Gateway, exactly as we answer
one for an artifact that does not exist.

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

8. **A Gateway fetches an uploaded artifact once, when it relays its offer.** When the Gateway
   relays a `ServerToAgent` whose `packages_available` names a file on the Server's download route
   (clause 9) to an Agent, and records that offer (clause 11), it starts fetching each such
   artifact that it neither holds nor is fetching, before any downstream peer asks for it. An offer
   it does not record starts no fetch. The fetch is single-flight per artifact. The Gateway
   resolves the path against its own endpoint and fetches from its Server's origin with its own
   client certificate and no offered headers, by the rules every Client's download follows
   ([ADR-0042](0042-signed-package-delivery-from-allowed-sources.md) clause 4). Clause 3 lets a
   marked Gateway fetch what is offered to any Agent, and each request costs one token of its
   aggregate bucket (clause 6). The message itself is forwarded unchanged and without waiting for
   the fetch (ADR-0071). At most four fetches run at a time; further ones wait for a free slot. A
   `429` or `503` from the Server with `Retry-After` defers the fetch within the bound of clause 15,
   and a deferred fetch gives its slot back while it waits and takes one again before it asks
   anew. Once its first 60 seconds have passed, a fetch is cut at the first chunk that finds it
   below an average of 64 KiB/s since then; the read timeout of clause 4 of ADR-0042 still cuts a
   silent one. A Gateway that shuts down ends its fetches, waiting or not. Any other failure — the Server's refusal (an unmarked Gateway is answered `404`,
   clause 4), a network error, a cut, bytes that do not match the hash — is logged and remembered
   for that artifact. It is fetched again only when it newly appears in an Agent's offer, that is,
   in a relayed offer to an `instance_uid` whose previous recorded offer did not name it.

9. **Only an artifact the Server hosts is cached.** An offered file is the Server's when its
   `download_url` is a path, beginning `/api/v1/packages/` and naming the route's `/file`, which
   the Server sends while `advertised_url` is unset and which a downstream Client resolves against
   its own endpoint, the Gateway. An absolute `download_url` is neither fetched nor served by the
   Gateway. For a referenced artifact's source the downstream Client fetches it as it would
   without a Gateway (ADR-0042). With `advertised_url` set, the Server's route is offered as an
   absolute URL naming the Server, and an uploaded artifact is not delivered behind a Gateway at
   all: the downstream Client's own origin is the Gateway, so `Sources::permit` refuses the Server's
   URL unless `[packages] allowed_sources` lists it, and if it is listed the Client presents no
   certificate there and the Server's handshake refuses it
   ([ADR-0059](0059-admission-by-a-client-certificate-alone.md) clause 23).

10. **The Gateway stores and serves only bytes that match the offer's hash.** An artifact is the
    offered path, query included, together with the offer's `content_hash`. The fetch streams the
    body to a staging file in the cache directory and hashes it on the way; peak memory is one
    chunk. The file is renamed into place only when its SHA-256 equals the offered
    `content_hash`; otherwise it is deleted, the mismatch is logged, and nothing is served. The
    Gateway does not check the signature. The downstream Client checks hash and signature itself
    before it installs (ADR-0042 clause 5), so the cache cannot make an Agent install anything the
    Agent would not install from the Server.

11. **Downstream, a host receives only what the Gateway relayed to its own Agents.** The Gateway
    serves `GET /api/v1/packages/{agent_type}/{version}/file` on its downstream listener, behind
    the handshake and the revocation verdict of its `/v1/opamp`: a certificate from
    `client_ca_file` is required, a revoked one is answered `401` and every request is answered
    `503` while the Gateway holds no current list
    ([ADR-0071](0071-client-modes-and-a-gateway-that-passes-packages-only-to-the-hosts-they-were-offered-to.md) clauses 11, 14).
    - **Binding.** An `instance_uid` is bound to the host (`urn:opamp-fleet:host:<id>`) of the
      certificate of the first downstream connection that reports for it since the Gateway
      started, as the Server binds an `instance_uid` to the host that first reports it
      ([ADR-0059](0059-admission-by-a-client-certificate-alone.md) clause 7). A report over a
      certificate that names no host binds nothing. A host holds at most `max_carried_agents`
      bindings (ADR-0071 clause 4); past that its report for a new `instance_uid` binds nothing,
      and that is logged. The Gateway holds at most 1 000 000 bindings in all; past that a new
      binding takes the place of the least recently reported binding whose `instance_uid` has
      neither a downstream route nor a current offer, and when there is none, it binds nothing,
      and that is logged. Each report refreshes when its binding was last reported.
    - **Offers.** An offer relayed to an `instance_uid` is recorded only when the downstream
      connection it is relayed over presents a certificate of the host the `instance_uid` is
      bound to; any other is not recorded, and that is logged. An Agent's current offer is the
      files of the last recorded `packages_available` relayed to it: a later one replaces it, one
      that names no Server-hosted file removes it, a message without `packages_available` leaves
      it standing (clause 2), and a downstream disconnect does not end it. The offers held are
      bounded by the offers the Server makes through the Gateway. The bindings and the offers are
      kept in memory only, and the offers are indexed by host.
    - **Requests.** A request is served only when the Gateway holds the artifact and a current
      offer of it (the same path and query, byte for byte) to an `instance_uid` bound to the host
      the request's certificate names. A certificate that names no host receives nothing. While an
      offered artifact is being fetched, or waits for a fetch slot, the request is answered `503`
      with `Retry-After: 30` instead of waiting, so a Client's read timeout does not run out while
      the Gateway fetches. An offered artifact that is neither held nor being fetched is fetched on
      a request at most once per relayed offer of it — the next relayed offer re-arms it — and that
      request is answered `503` with `Retry-After: 30`. After a failed fetch it is not fetched
      again until clause 8 says so.
    - **Everything else** is answered `404`, with the same status, headers and body whether no such
      offer was recorded, it was recorded for another host, the artifact could not be fetched or
      verified, it is too large for the cache, or the request names nothing the route knows.

    The bound on what a host receives is clause 3's, applied to what the Server sent through the
    Gateway. The binding, the `404` for an artifact too large or not fetched, and the `503` while a
    fetch runs are the Gateway's own. Together they are the one refusal beyond admission that
    ADR-0071 clause 11 allows a Gateway.

12. **The Gateway logs what it refuses; it keeps no audit record.** The audit record is the
    Server's ([ADR-0063](0063-an-append-only-audit-record-chained-by-hash.md)). A downstream
    request answered `404` because no offer of that artifact was recorded for the requesting host
    is logged at `info` with the host (or that the certificate names none), the certificate's
    serial and the requested path without its query. An offer not recorded because its
    `instance_uid` is bound to another host is logged at `warn` with both hosts. Both kinds of line
    are aggregated per host: the first five in a minute are logged one by one, and the rest of that
    minute are counted into one line when the next minute's first line for that host arrives. The
    Server records the Gateway's own fetches as it records every download (clause 5).

13. **The cache is bounded, and an artifact that does not fit is refused.**
    `[gateway] package_cache_bytes` bounds the bytes the cache holds, stages and is deleting
    together, default `10737418240` (10 GiB); `0` is refused at load. The cache lives in
    `<state_dir>/gateway-packages`, owner-only on Unix, emptied when the Gateway starts. Every
    stored copy has a file name of its own, so deleting an old copy never touches a newer copy of
    the same artifact. Before a fetch stages a byte it reserves the artifact's `Content-Length`, or
    the per-artifact limit when the Server sends none, against `package_cache_bytes`. To make room
    the Gateway first deletes artifacts no current offer names, least recently used first, and then
    offered ones, least recently used first. A file being deleted stays counted until it is gone,
    and one whose deletion fails stays counted until a later attempt deletes it. A held artifact
    whose file has disappeared is forgotten. A fetch that cannot reserve room because other fetches
    hold it fails (clause 8). On a verified rename the reservation becomes the held artifact's
    size; on failure it is released. A fetch is cut, and nothing stored, once its `Content-Length`
    or its body exceeds the smaller of `package_cache_bytes` and `max_artifact_size_bytes`; the
    Gateway logs this once per artifact and does not fetch that artifact again while it runs. Such
    an artifact is not streamed through: a downstream request for it is answered `404`
    (clause 11). No file operation runs while the cache's shared state is locked.

14. **Downstream download requests are not counted by the Gateway.** A downstream request costs a
    lookup in memory and a read from the cache directory. It reaches the Server only through the
    fetches of clauses 8 and 11: one per artifact that newly appears in an Agent's offer and is not
    held, and at most one request-triggered fetch per artifact per relayed offer. Two artifacts that
    do not fit the cache together therefore cannot evict each other in a loop driven by requests.
    Each request to the Server costs a token of the Gateway's aggregate bucket (clause 6). As on
    `/v1/opamp`, the Gateway applies no rate of its own.

15. **A Client waits as its own Server origin asks before it reports a download failed.** A `429`
    or `503` with a `Retry-After` in seconds, answered by the Client's own Server origin — the
    Gateway behind one, otherwise the Server, whose rate limit answers `429` with `Retry-After: 30`
    ([ADR-0066](0066-admitted-agents-are-rate-limited-per-host.md) clause 6, whose follow-up this
    is) and whose admission throttle answers `429` with the seconds of back-off left
    ([ADR-0059](0059-admission-by-a-client-certificate-alone.md) clause 24) — is waited out and the
    download sent again. Each wait is the `Retry-After`, at least 1 second and at most 60 seconds.
    The asking stops 30 minutes after the first request of the download, counting the requests as
    well as the waits: a wait that would end past that point is not begun. Past it, or for a
    `429`/`503` without a `Retry-After` in seconds — an HTTP date, none, or an unreadable one — or
    from any other host, a redirect target off the Server's origin included, the download fails as
    before and is reported `InstallFailed`
    ([ADR-0042](0042-signed-package-delivery-from-allowed-sources.md) clause 3). While it waits the
    status stays `Downloading`. The bound is on asking, not on a transfer that has begun, which
    keeps no total timeout (ADR-0042 clause 4). A Client that shuts down stops waiting and reports
    nothing for that download.

**Out of scope:**

- Artifacts hosted elsewhere. A referenced entry (`source`) is fetched from the operator's host
  under `[packages] allowed_sources`, with whatever headers the offer carries
  ([ADR-0043](0043-the-package-store-references-artifacts-only-over-tls-beyond-the-loopback.md) clause 4,
  [ADR-0042](0042-signed-package-delivery-from-allowed-sources.md)). The Server is not in that
  download path, and who can fetch from that host is decided there. A Gateway does not cache such
  an artifact (clause 9).
- A bound finer than the host ([ADR-0059](0059-admission-by-a-client-certificate-alone.md)
  clause 14).
- Roles, permissions and tenancy on the Operator plane.
- Keeping the cache and the offers it serves across a restart of the Gateway, and the Gateway's own
  Supervisors fetching through the cache.

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
- **Relay the download route through the Gateway, request by request.** The Gateway would fetch
  with its own certificate for every downstream request, so the bytes would cross the Gateway's
  link once per Agent, and every request would cost a token of its aggregate bucket (Context). It
  would also need the same downstream bound as the cache, because the Gateway's certificate
  fetches what is offered to any Agent.
- **Fetch on the first downstream request instead of when the offer is relayed.** Simpler by one
  trigger, but the artifact would only start to cross the link once an Agent asks, and every Agent
  behind the Gateway would be answered `503` for the whole transfer. Fetching when the offer passes
  starts the transfer as early as the Gateway can know it is needed.
- **Let a downstream request wait for the fetch instead of answering `503`.** The Client's read
  timeout of 60 seconds also bounds the wait for the response's headers. A fetch over a slow link
  outlasts it, the Client reports the download failed and echoes the offer's hash, and the Server
  does not offer it again (ADR-0042 clause 3). A `503` with `Retry-After` keeps the Client asking
  within the bound of clause 15.
- **Bind an `instance_uid` on the first offer the Gateway relays for it.** A host that reports
  another host's `instance_uid` between that host's report and the Server's reply would be bound
  instead. The first report is what the Server binds on as well.
- **Rewrite the offer's `download_url` to name the Gateway.** That would make an absolute URL
  cacheable too, but the Gateway forwards messages unchanged (ADR-0071), and a Gateway that edits
  what the Server offers is one more place an offer can be made to say something the Server did
  not.
- **Serve whatever the Gateway holds to any admitted downstream peer.** Admission behind a Gateway
  is fleet membership, as at the Server. Any member behind the Gateway could fetch another
  partition's build once one Agent behind the same Gateway was offered it, which clause 3 closes
  at the Server.
- **Bind a downstream fetch to the connection that carries the Agent instead of its host.** A
  plain-HTTP Client opens a new exchange for every report, a WebSocket Client reconnects, and an
  Agent retrying a failed install may do so on a later connection (clause 2). The host is the bound
  the Server applies, and the one the certificate states.
- **Stream an artifact too large for the cache through without storing it.** The Gateway would
  pass on bytes before their hash is checked, and every downstream request for that artifact would
  become an upstream fetch, which the cache exists to prevent. An operator who rolls out such an
  artifact behind a Gateway raises `package_cache_bytes`; the refusal is logged.
- **Count downstream download requests in a bucket per downstream host.** It would add a rate the
  Gateway does not apply on `/v1/opamp` either (ADR-0071 clause 11). A downstream request reaches
  the Server at most as the one shared fetch, so the Server's bucket for the Gateway already bounds
  what downstream requests cost upstream.
- **Cache referenced artifacts as well.** The Gateway would need the offered headers, which are an
  operator's credential for one host, and would present them from a second host; the Client
  presents them only to the source they were given for (ADR-0042 clause 4). A referenced source is
  the operator's own download server and can be placed near the Agents.
- **Keep the cache and its offers on disk across a restart.** It saves one fetch per artifact
  after a restart, but a persisted offer outlives what the Server last said, and the bound of
  clause 11 would have to be trusted across a process the Gateway does not control. An emptied
  directory and offers in memory need no reconciliation.

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
- [nginx `ngx_http_proxy_module`](https://nginx.org/en/docs/http/ngx_http_proxy_module.html),
  `proxy_cache_lock`: "only one request at a time will be allowed to populate a new cache element
  … Other requests of the same cache element will either wait for a response to appear in the
  cache or the cache lock for this element to be released". `proxy_cache_path` `max_size`: when
  the size is exceeded the cache manager "removes the least recently used data". The single-flight
  fetch and the LRU bound of clauses 8 and 13 follow that established shape, with offers deciding
  what is evicted first.
- The code read for the Gateway clauses: `Forwarding`, `run_on_timed` and `server_tls` in
  `crates/fleet-agent/src/gateway/mod.rs`; `Registry::deliver` in
  `crates/fleet-agent/src/gateway/registry.rs`; the pool's reader in
  `crates/fleet-agent/src/gateway/pool.rs`; `RevocationList::verdict` in
  `crates/fleet-agent/src/gateway/revocations.rs`; `download_and_verify`, `write_stream` and
  `download_client` in `crates/fleet-agent/src/packages.rs`; `to_available` in
  `crates/fleet-server/src/packages.rs`; `HOST_URI_PREFIX` and `facts` in
  `crates/fleet-server/src/ca.rs`.

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
- Positive (G-10, G-15): a Client behind a Gateway receives uploaded artifacts, through the
  Gateway, from a Server that keeps clause 3 unchanged. Each artifact crosses the Gateway's
  upstream link once per Gateway instead of once per Agent, and costs the Server one download.
- Positive (Strategy *Security before convenience*): a host behind a Gateway receives from it only
  what the Server offered through it to that host's own Agents. The Gateway's breadth under
  clause 3 is not passed on, and a downstream host cannot list what the Gateway holds.
- Positive: a nested Gateway is served by the Gateway in front of it like any downstream host: the
  Agents it carries are routed over its connection, which presents its certificate.
- Negative / trade-offs: the Gateway now holds artifacts on disk, up to `package_cache_bytes`
  (10 GiB by default), staged fetches included. Its `state_dir` needs that space.
- Negative / trade-offs: a Gateway that restarts holds no offers. An Agent whose install is still
  in flight reconnects, and its report re-draws the offer, since the Server re-sends an offer
  whose hash the Agent has not echoed. An Agent that echoed the offer with a failed install and
  then retries is answered `404` until the Server sends it a different offer. Pressing the same
  version again yields the same hash, which the hash gate does not re-send; a rollout of another
  version does.
- Negative / trade-offs: a host behind a Gateway holds at most `max_carried_agents` bindings, so a
  host carrying more Agents through one Gateway than that cannot have the rest served from the
  cache. At the global cap a flood from many hosts can push out bindings that have neither a route
  nor an offer; such an Agent is bound again by its next report.
- Negative / trade-offs: an `instance_uid` is bound to the host of the first report for it after
  the Gateway starts. Until that first report a host that reports another host's `instance_uid`
  first is bound instead and served its offers; the real host's peer is answered `404`, and the
  Gateway logs each offer it does not record. The remedy for such a lockout is a restart of the
  Gateway, after which the real host's next report binds it. Within one Gateway this is the bound
  the Server's host register gives directly connected hosts
  ([ADR-0059](0059-admission-by-a-client-certificate-alone.md) clause 7).
- Negative / trade-offs: a fetch that failed is not tried again until the artifact newly appears in
  an Agent's offer, so a transient error upstream can leave the Agents behind the Gateway without
  it until the next rollout. A `429` or `503` with `Retry-After` is not such a failure.
- Negative / trade-offs: a Client now waits up to 30 minutes, in waits of at most 60 seconds, on a
  `429` or `503` from its own Server origin before it reports a download failed (clause 15). A
  failure the Server or Gateway answers that way is reported that much later.
- Negative / trade-offs: an artifact larger than the cache is not delivered behind the Gateway at
  all; the Gateway's log names it, and the downstream Agent reports the failed download.
- Negative / trade-offs: with `advertised_url` set, uploaded artifacts are not delivered behind a
  Gateway at all (clause 9). A fleet with Gateways leaves `advertised_url` unset.
- Negative / trade-offs: a Gateway whose host is not marked delivers no uploaded artifact, as
  before; its log says that the Server refused the fetch.
- Follow-ups: a Selector term that only a Server label satisfies, so that a partition can hold
  against the host it partitions. `SECURITY.md` (the trust-boundary section) and the manual's
  download-route rows in `docs/manual/server.md` state the bound, and `docs/manual/client.md`
  the Gateway's cache.

## Enforcement

Each test carries `Verifies: ADR-0070` ([ADR-0003](0003-decisions-verified-by-tests.md)).

- `crates/fleet-server/src/packages.rs`:
  - `the_download_and_the_offer_test_one_predicate`: for every combination, the offer and the
    download agree on what is offered (clause 1).
  - `an_agent_reporting_another_type_is_offered_nothing` (clause 1).
  - `an_entry_its_deployment_does_not_sign_is_not_offered` (clause 1).
  - `a_referenced_entry_is_never_offered_for_the_route` (clause 1, 4).
- `crates/fleet-server/src/revocation.rs`:
  - `a_host_speaks_for_the_agents_bound_to_it_and_a_gateway_for_any` (clause 3).
- `crates/fleet-server/src/fleet.rs`:
  - `an_artifact_is_offered_to_a_host_only_through_an_agent_it_speaks_for` (clauses 1, 3).
  - `an_offer_still_stands_after_its_hash_is_echoed` (clause 2).
  - `a_version_waiting_for_its_press_is_offered_to_no_host` (clause 2).
- `crates/fleet-server/tests/mutual_tls.rs`:
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
- `crates/fleet-server/src/main.rs`:
  - `a_server_without_client_ca_says_uploaded_artifacts_need_a_host` (clause 3).
- Tests served over `Admission::open`, where no certificate is presented, are unchanged (clause 3):
  - `crates/fleet-agent/tests/packages_e2e.rs`;
  - `crates/fleet-agent/tests/http_transport_e2e.rs`, the two Servers it starts.
- `crates/fleet-server/tests/packages.rs`:
  - `an_uploaded_set_is_offered_downloaded_and_gated` and
    `an_artifact_larger_than_the_framework_default_uploads_and_downloads_intact` use a member
    certificate that names the reporting Agent's host, so that they exercise the offered test.
  - `a_version_waiting_for_its_rollout_cannot_be_fetched` (clause 2).
- `crates/fleet-agent/tests/gateway_tls.rs`:
  - `the_gateway_serves_no_artifact_it_relayed_no_offer_for`: the Server serves the artifact, and
    the Gateway, which relayed no offer of it, answers `404` (clause 11).
- `crates/fleet-agent/tests/gateway_package_cache.rs`, against an upstream the test
  controls:
  - `a_relayed_artifact_is_fetched_once_before_any_request_and_served_to_its_host` — two Agents of
    one host are offered the same artifact, the Gateway fetches it before any request, requests
    while it runs are answered `503` with `Retry-After: 30`, and the upstream served one fetch
    (clauses 8, 11).
  - `another_host_and_a_certificate_naming_no_host_are_answered_as_for_an_artifact_not_held`, which
    compares status, headers and body byte for byte (clause 11).
  - `a_host_reporting_another_hosts_instance_uid_before_the_reply_is_not_served` — host B reports
    host A's `instance_uid` between A's report and the Server's reply; B is answered byte for byte
    as for an artifact not held, and A is served (clause 11).
  - `a_later_offer_replaces_what_an_agent_was_offered` (clause 11).
  - `a_failed_fetch_is_not_repeated_by_requests_or_re_offers_and_is_retried_when_newly_offered`
    (clauses 8, 14).
  - `a_websocket_downstream_receives_the_offer_and_the_artifact` (clauses 8, 11).
  - `a_referenced_artifact_is_neither_fetched_nor_served` (clause 9).
  - `an_artifact_that_fails_its_hash_is_neither_stored_nor_served` (clause 10).
  - `an_artifact_larger_than_the_cache_is_refused_and_not_fetched_again` and
    `a_body_without_content_length_is_cut_at_the_limit` (clause 13).
  - `the_download_route_answers_503_while_the_gateway_holds_no_revocation_list` and
    `a_revoked_certificate_is_refused_on_the_download_route` (clause 11).
  - `a_client_behind_a_gateway_installs_from_an_upstream_slower_than_its_read_timeout` — the
    upstream is held until the Client's own download has been answered `503` by the Gateway, and
    its download then completes and verifies (clauses 11, 15).
- `crates/fleet-agent/tests/gateway_package_cache.rs`, against the real Server on mutual TLS:
  - `a_client_behind_a_marked_gateway_receives_an_uploaded_artifact_through_it` — the Gateway
    fetches with its own certificate what the Server offers an Agent behind it, and the downstream
    Client's own download code (`download_and_verify`) fetches it from the Gateway and verifies its
    hash and signature (clauses 3, 8, 10, 11).
- `crates/fleet-agent/src/gateway/cache.rs`:
  - `room_is_made_from_artifacts_no_longer_offered_first` (clause 13).
  - `only_a_path_on_the_servers_route_is_cached` (clause 9).
  - `requests_while_fetching_are_answered_busy_and_the_upstream_serves_one` (clauses 8, 11).
  - `the_cache_is_emptied_at_start_and_owner_only` (clause 13).
  - `refusals_are_logged_five_a_minute_per_host_and_the_rest_counted` (clause 12).
  - `an_evicted_offered_artifact_is_fetched_again_on_a_request_once_per_offer` (clauses 11, 14).
  - `a_file_whose_deletion_fails_stays_counted_until_deleted` and
    `a_held_artifact_whose_file_disappeared_is_forgotten` (clause 13).
  - `a_fetch_cannot_reserve_room_other_fetches_hold` (clause 13).
  - `no_more_than_four_fetches_run_at_once` (clause 8).
  - `an_offer_over_a_certificate_naming_no_host_records_nothing_and_fetches_nothing` (clauses 8,
    11).
  - `an_unfinished_fetch_leaves_no_entry_behind` (clause 8).
  - `a_host_flooding_instance_uids_cannot_keep_another_hosts_agent_from_being_bound` (clause 11).
  - `a_fetch_below_the_pace_floor_is_cut` (clause 8).
  - `a_deferred_fetch_gives_its_slot_back_while_it_waits` (clause 8).
  - `a_shutdown_ends_a_fetch_that_waits_out_retry_after` (clause 8).
  - `an_artifact_evicted_between_lookup_and_open_is_fetched_again` (clauses 11, 13).
- `crates/fleet-agent/src/packages.rs`:
  - `a_download_waits_out_retry_after_from_its_server_origin`,
    `a_download_gives_up_once_its_waits_reach_the_bound` and
    `a_retry_after_from_another_host_fails_the_download`,
    `a_retry_after_of_zero_does_not_loop_without_bound`,
    `request_time_counts_against_the_bound`,
    `an_unusable_retry_after_is_not_waited_out` and
    `a_retry_after_after_a_redirect_off_the_origin_is_not_waited_out` (clause 15).
- `crates/fleet-agent/src/transport/mod.rs`:
  - `a_shutdown_stops_a_download_waiting_out_retry_after` (clause 15).
- `crates/fleet-agent/src/config.rs`:
  - `the_package_cache_defaults_to_ten_gib_and_rejects_zero` (clause 13).
