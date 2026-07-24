# ADR-0031: The Server keeps a revocation list that follows renewal and hands it to the Gateways, and a session ends when what admitted it is revoked or its certificate expires

- **Status:** 🟢 accepted
- **Date:** 2026-10-06
- **Deciders:** Markus Brigl
- **Applies to:** admission on `/v1/opamp` and on the download route in `crates/fleet-server/src/transport.rs`, the WebSocket session loop there, the certificate register and revocation list in `crates/fleet-server/src/revocation.rs` and `crates/fleet-server/src/fs/revocation.rs`, the close frame `crates/opamp/src/server.rs` sends, the serial numbers `crates/fleet-server/src/ca.rs` assigns, the `/api/v1/revocations` and `/api/v1/certificates` routes in `crates/fleet-server/src/api.rs`, the Gateways' `/v1/gateway/revocations` route on the Agent plane, and the files they persist under `config_dir`

## Context

[ADR-0026](0026-admission-by-a-client-certificate-alone.md) admits an Agent by a client
certificate in the handshake and by nothing else, so a certificate is the one proof to revoke and
there is no credential beside it. Without a revocation list, two gaps follow, both recorded in
[`HARDENING.md`](../HARDENING.md) as H1 and H2:

- **A proof cannot be withdrawn.** A stolen certificate stays good until it expires, 90 days by
  default.
- **A session outlives its proof.** The certificate is checked once per WebSocket, at the upgrade.
  A session admitted yesterday keeps running even if the certificate that admitted it has since
  expired, and nothing could end it early if the certificate were withdrawn.

The specification puts security before convenience (Strategy *Security before convenience*) and
asks that the Server accept only authenticated Agent identities (G-17, Q-1). An identity the
operator no longer trusts is not authenticated, however recently it was.

The Baseline names the answer itself: *"Since the Server knows what access headers and a client
certificate the Client uses, the Server can revoke access to individual Agents by marking the
corresponding connection settings as 'revoked' and disconnecting the Client."* The Server is the
only party on this link that verifies anything, so revocation can be Server state; there is no
third party a CRL or OCSP responder would serve.

Three facts shape the decision:

- **The Server's certificates name no unique serial today.** `rcgen` derives a serial from the
  SHA-256 of the public key when none is set. A re-sent CSR for the same key gets the same serial
  twice, and a serial says nothing the key fingerprint does not.
- **Renewal changes the key.** A Client renews at two thirds of its certificate's life with a new
  key ([ADR-0026](0026-admission-by-a-client-certificate-alone.md) clause 11). A host that renewed shortly
  before its old certificate was revoked would carry on with the new one.
- **A Gateway hides its downstream certificates.** The Server sees the Gateway's certificate on a
  pooled upstream connection ([ADR-0034](0034-client-modes-and-a-gateway-that-passes-packages-only-to-the-hosts-they-were-offered-to.md)); the downstream certificate stays on the downstream hop,
  and nothing of the downstream Agent's proof reaches the Server.

## Decision

We will keep a persisted revocation list of certificates, by issuing CA and serial and extended
along every renewal the Server signed, check it at every admission, end each WebSocket session as
soon as the certificate that admitted it is revoked or expires, and hand the certificates on it,
resolved along their chains, to every host an operator marked as a Gateway.

1. **The Server gives every certificate it signs a random serial.** 16 bytes from the system's
   secure random source, the top bit cleared so the encoding stays positive. Two certificates from
   one key have two serials.

2. **The Server keeps a register of what it signed.** For each certificate: issuer, serial,
   subject, the SHA-256 fingerprint of its public key, `not_after`, the `instance_uid` of the
   message that carried the CSR, its host
   ([ADR-0026](0026-admission-by-a-client-certificate-alone.md) clause 7), and — on a renewal — the
   certificate it renewed, its **predecessor**: the certificate the CSR's renewal proof names
   ([ADR-0026](0026-admission-by-a-client-certificate-alone.md) clause 27), and without a proof the certificate the connection presented — never what
   `instance_uid` the message names, because that value is self-asserted and a chain must not be
   left on its say-so. A proof is checked against the client CA and its key, so it holds through a
   Gateway too; a downstream Agent without one renews from the Gateway's certificate, and what it
   renews descends from that. An enrolment has no predecessor and is the root of its chain. An
   issuer is identified by the SHA-256 of its DER-encoded name, so no text form of a name can fail
   to match. The register lives under `config_dir`, one file per certificate, and each entry is
   written before the certificate is offered. It holds at most 100 000 certificates, at most
   10 000 in one chain — below one root, the first certificate the chain renews from — and it
   refuses a renewal once fewer than 1 000 places are left, so renewals alone cannot shut out an
   enrolment. A CSR beyond any of these is refused. An entry stays while the certificate, or one
   renewed from it, is still valid. `GET /api/v1/certificates` lists it.

3. **A revoked certificate is named by its CA and serial.** `POST /api/v1/revocations` with
   `{"certificate": {"authority": "client" | "bootstrap", "serial": "<hex>"}}` revokes one, under
   every certificate of that CA file. The client CA covers certificates the Server signed and those
   an operator provisioned from it; the bootstrap CA lets a lost bootstrap certificate be revoked.
   An authority this Server does not have is answered `400`, so a typo cannot pass for a
   revocation.

4. **A revocation follows renewal.** A certificate is revoked when it, or any predecessor in its
   register chain, is on the list. A connection that presents a revoked one is refused before its
   CSR is read, and an open session re-checks its certificate before a CSR it carries is signed.

5. **There is no credential to revoke.** `POST /api/v1/revocations` with `{"credential": …}` is
   answered `400`, naming the field and saying that the Agent plane admits by client certificate
   alone. A credential entry already on the persisted list is dropped when the list is loaded,
   with one log line that names the number of entries dropped, and the list is written back
   without it; the Server starts either way.

6. **The list is persisted and bounded.** It lives under `config_dir`, written atomically before
   the request is answered, and survives a restart. It holds at most 100 000 entries, and a
   revocation beyond that is answered `507`. A certificate entry is dropped with its register entry,
   once neither the certificate nor any renewal of it is valid; an entry for a certificate the
   register never held stays until an operator removes it.

7. **A revocation can be lifted.** `GET /api/v1/revocations` lists each entry with its id, kind,
   time, authority and serial; `DELETE /api/v1/revocations/{id}` removes one, answered `404` for an
   unknown id. Lifting one takes effect at the next connection; it reopens nothing.

8. **Admission checks the list after the handshake.** On `/v1/opamp` and on the download route, a
   peer whose certificate is revoked is answered `401` without a challenge, as
   [ADR-0026](0026-admission-by-a-client-certificate-alone.md) answers every refusal behind the
   handshake, and the failure counts toward its admission throttle. The answer does not say that
   the certificate was revoked.

9. **A revocation ends the sessions it concerns, and only those.** The Server records, per
   WebSocket session, the issuer and serial of the certificate that admitted it. A revocation is
   checked against every open session at once, and a session checks itself once more as it starts,
   so a revocation landing during its admission is not missed; a session it concerns is closed with
   WebSocket close code `1008` (policy violation) and the reason `revoked`. Other sessions are not
   touched. On plain HTTP every request is admitted anew, so the next poll is refused.

10. **A session ends when its certificate expires.** A WebSocket session is closed with `1008` and
    the reason `certificate expired` at the `not_after` of the certificate that admitted it. A
    Client renews at two thirds of the life and reconnects to prove the new certificate, so a
    healthy fleet never meets this close; it ends what renewal did not replace.

11. **Behind a Gateway the Gateway refuses what the Server revoked.** A downstream Agent is
    revoked by revoking the Gateway's certificate, which ends every Agent the Gateway carries and
    every certificate renewed through it without a proof (clause 2); or by revoking its own
    certificate, which the Gateway refuses from the list of clause 12 ([ADR-0034](0034-client-modes-and-a-gateway-that-passes-packages-only-to-the-hosts-they-were-offered-to.md) clause 14), and
    whose proof renews nothing.

12. **The list reaches the Gateways from a route of their own.** `GET /v1/gateway/revocations` on
    the Agent plane is admitted as `/v1/opamp` is — the handshake's client certificate from the
    client CA, checked against the list — and is answered only to a certificate whose host an
    operator marked as a Gateway (`PUT /api/v1/hosts/{host}/gateway`); any other is answered `403`.
    The body is JSON: every certificate of the client CA that is revoked by clause 4, its own entry
    or a predecessor's, each by the SHA-256 of its issuer's name and its serial, so the Gateway
    resolves no chain; and a version that changes with every change to the list, carried as an
    `ETag`, so a fetch with `If-None-Match` is answered `304`. Bootstrap certificates are not on it:
    a Gateway admits no bootstrap certificate. The route is outside the OpenAPI document, as the
    package download is.

**Out of scope:** CRL and OCSP for third parties; what a Gateway does with the list ([ADR-0034](0034-client-modes-and-a-gateway-that-passes-packages-only-to-the-hosts-they-were-offered-to.md));
binding a certificate to an `instance_uid` (the certificate proves membership and its host, not an
Agent's identity, as [ADR-0026](0026-admission-by-a-client-certificate-alone.md) decides);
shortening certificate validity, which the H6 measure of `HARDENING.md` takes up once this is in
force; a revocation view in the bundled UI; an audit record of revocations.

## Alternatives considered

- **A maximum session age instead of, or beside, targeted closing.** Rejected. A fixed age closes
  every session of the fleet on a clock, a reconnect storm's worth of handshakes for sessions whose
  proofs are fine, and still leaves a revoked one running until the age is reached. The certificate's
  own `not_after` is the bound that already exists and costs a healthy fleet nothing (clause 10).
- **Revocation by serial alone, without the renewal chain.** Rejected: a host that renewed shortly
  before the revocation keeps a valid certificate, and the operator has to revoke the successor
  they cannot see. The register costs one small entry per issued certificate, dropped on expiry.
- **Revocation by public key fingerprint.** Rejected: renewal changes the key, so it has the same
  hole as serial alone, and an operator reading a certificate with `openssl x509` sees the serial,
  not a key hash.
- **Naming the issuer by its distinguished name as text.** Rejected: RFC 4514 reverses the order
  of the parts and escapes commas, OpenSSL's default form does neither, and a parser's form is a
  third; a revocation typed in one would silently miss a certificate stored in another. The Server
  has two CAs at most, so their role names them without ambiguity.
- **Linking a renewal to the presented certificate only when the message names its owner.**
  Rejected: the `instance_uid` a message names is self-asserted, so one message naming another
  value would lift a renewal out of its chain. Linking always revokes too much behind a Gateway,
  never too little.
- **Dropping a register entry at its own expiry.** Rejected: the revocation of an expired
  certificate would go with it, and a renewal of that certificate would be admitted again.
- **Revocation by Agent, from the fleet view.** Rejected as the primitive: `instance_uid` is
  self-asserted and re-keyable, and behind a Gateway the Server sees only the Gateway's
  certificate. It may come later as a convenience on top of clause 3, using the register's
  `instance_uid`.
- **CRL or OCSP.** Rejected: both serve relying parties other than the issuer. Here the issuer is
  the only relying party, and an online responder would be infrastructure guarding nothing extra.
- **Distributing the list to Gateways through a custom message.** Rejected: a `CustomMessage` is
  `[Development]` in the Baseline and not implemented here, and it would tie the list to an Agent's
  session rather than to the Gateway as a host. A route of its own on the plane the Gateway
  already reaches carries it with the admission that plane already has.
- **The list for every member, not only marked Gateways.** Rejected: only a Gateway acts on it, and
  every member that could fetch it would learn which certificates of the fleet were withdrawn.
- **Ending a session by a re-authentication the Server pushes.** Rejected: the Baseline has no
  message for it, and the specification forbids a private side channel.
- **Refusing to start while a credential entry is on the persisted list.** Rejected: the entry
  names nothing the Server still accepts, so it guards nothing, and a Server that will not start
  after an upgrade would shut out the whole fleet for no gain in security.

## Sources / Prior art

- OpAMP specification v0.20.0, *Revoking Access*: *"the Server can revoke access to individual
  Agents by marking the corresponding connection settings as 'revoked' and disconnecting the
  Client."* ([opamp-spec](https://github.com/open-telemetry/opamp-spec/blob/v0.20.0/specification.md))
- [RFC 5280 §4.1.2.2](https://www.rfc-editor.org/rfc/rfc5280#section-4.1.2.2): a serial is a positive integer of
  at most 20 octets, unique per issuer.
- [RFC 6455 §7.4.1](https://www.rfc-editor.org/rfc/rfc6455#section-7.4.1): close code `1008`, *"an endpoint is
  terminating the connection because it has received a message that violates its policy"*.
- Kubernetes has no client-certificate revocation and relies on short lifetimes and rotation
  ([kubernetes#46287](https://github.com/kubernetes/kubernetes/issues/46287),
  [kubelet certificate rotation](https://kubernetes.io/docs/tasks/tls/certificate-rotation/)) —
  the position of admission without a revocation list, and the gap this closes.

## Consequences

- Positive: an operator can eject a host within seconds, not within a certificate's lifetime; a
  stolen bootstrap certificate can be withdrawn; a session can no longer outlive its certificate.
  Validity can be shortened afterwards on evidence rather than as the only defence. The list holds
  one kind of entry, and no revocation shuts out the whole fleet at once.
- Negative / trade-offs: the Server gains two persisted stores and a check per admission; the
  register grows with the fleet times its renewals within one validity period. Behind a Gateway a
  revocation takes effect when the Gateway next fetches the list, and a Gateway needs its host
  marked before it admits anyone; the list is now the only way the Server's revocation of a
  downstream certificate reaches that Agent. The register's bounds turn an admitted member that
  loops CSRs into a refusal of renewals within its chain, and a host holds at most three valid
  certificates ([ADR-0026](0026-admission-by-a-client-certificate-alone.md) clause 7). A downstream Agent
  that renews with a proof renews in its own chain and host; one without a proof renews in the
  Gateway's, and can stop renewal for the Gateway and every Agent it carries until the certificates
  it obtained expire. The bounds keep the damage to one chain and away from enrolment. An operator
  tool that still posts `{"credential": …}` is answered `400` and has to be changed.
- Follow-ups: revocation by Agent as a convenience in the bundled UI; an audit record of each
  revocation and each session it closed; shortening certificate validity once renewal is proven;
  bounding issuance per Agent rather than per chain, which needs a per-Agent identity this decision
  leaves out.

## Enforcement

- [`crates/fleet-server/src/revocation.rs`](../../crates/fleet-server/src/revocation.rs) tests:
  `a_revocation_follows_every_renewal` (clause 4),
  `the_list_survives_a_restart_and_can_be_lifted` (clauses 6, 7),
  `a_renewal_stays_revoked_after_its_revoked_ancestor_expires` (clauses 4, 6),
  `the_list_and_the_register_are_bounded`,
  `an_expired_register_entry_is_dropped_with_its_revocation` (clauses 2, 6).
- [`crates/fleet-server/src/fs/revocation.rs`](../../crates/fleet-server/src/fs/revocation.rs)
  tests: `the_ledger_survives_a_reopen` (clauses 2, 6);
  `a_persisted_credential_entry_is_dropped_on_load` (clause 5): a list holding a credential entry
  beside a certificate entry loads with the certificate entry alone, is written back without the
  credential entry, and the drop is logged.
- [`crates/fleet-server/src/ca.rs`](../../crates/fleet-server/src/ca.rs) test:
  `two_certificates_from_one_key_have_two_serials` (clause 1).
- [`crates/fleet-server/tests/mutual_tls.rs`](../../crates/fleet-server/tests/mutual_tls.rs)
  tests: `a_revoked_certificate_is_refused_on_both_transports_and_the_download` (clauses 3, 7, 8),
  which also asserts that the `401` carries no `WWW-Authenticate` header;
  `a_revocation_closes_the_session_it_concerns_and_no_other` (clause 9),
  `a_session_is_closed_when_its_certificate_expires` (clause 10),
  `a_revocation_reaches_a_certificate_renewed_before_it` (clauses 1, 2, 4),
  `a_revocation_names_its_issuer_by_role_whatever_the_issuer_is_called` (clause 3),
  `a_csr_for_another_agent_still_descends_from_the_presented_certificate` (clauses 2, 11),
  `a_marked_gateway_is_handed_the_revoked_certificates_with_their_renewals` (clause 12, an
  unmarked member answered `403` and an unchanged list `304`),
  `a_bootstrap_certificate_is_not_handed_the_list` (clause 12);
  `a_credential_revocation_is_answered_400` (clause 5): `{"credential": …}` is refused with `400`
  and the list is unchanged.
- [`crates/fleet-agent/tests/gateway_revocation_e2e.rs`](../../crates/fleet-agent/tests/gateway_revocation_e2e.rs)
  test: `a_certificate_the_server_revokes_is_refused_behind_the_gateway` (clauses 11, 12).
