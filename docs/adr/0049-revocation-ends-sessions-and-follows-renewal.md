# ADR-0049: The Server keeps a revocation list that follows renewal, and a session ends when what admitted it is revoked or its certificate expires

- **Status:** ⚪ superseded by [ADR-0056](0056-revocation-that-follows-renewal-and-reaches-the-gateways.md)
- **Date:** 2026-10-03
- **Deciders:** Markus Brigl
- **Applies to:** admission on `/v1/opamp` and on the download route in `crates/fleet-server/src/transport.rs`, the WebSocket session loop there, the certificate register and revocation list in `crates/fleet-server/src/revocation.rs` and `crates/fleet-server/src/fs/revocation.rs`, the close frame `crates/opamp/src/server.rs` sends, the serial numbers `crates/fleet-server/src/ca.rs` assigns, the `/api/v1/revocations` and `/api/v1/certificates` routes in `crates/fleet-server/src/api.rs`, and the files they persist under `config_dir`

## Context

[ADR-0039](0039-admission-requires-both-proofs-and-enrolment-is-approved.md) admits an Agent with
two proofs, a client certificate in the handshake and a fleet credential, and leaves revocation out
of scope: "certificate revocation, for which short validity plus renewal stands in". Two gaps
follow, both recorded in [`HARDENING.md`](../HARDENING.md) as H1 and H2:

- **A proof cannot be withdrawn.** A stolen certificate stays good until it expires, 90 days by
  default, and a credential stays good until it is removed from `server.toml` and the Server
  restarts — which drops the whole fleet.
- **A session outlives its proof.** The credential and the certificate are checked once per
  WebSocket, at the upgrade. A session admitted yesterday keeps running even if the certificate
  that admitted it has since expired, and nothing could end it early if a proof were withdrawn.

The specification puts security before convenience (Strategy *Security before convenience*) and
asks that the Server accept only authenticated Agent identities (G-17, Q-1). An identity the
operator no longer trusts is not authenticated, however recently it was.

The Baseline names the answer itself: *"Since the Server knows what access headers and a client
certificate the Client uses, the Server can revoke access to individual Agents by marking the
corresponding connection settings as 'revoked' and disconnecting the Client."* The Server is the
only party on this link that verifies anything, so revocation can be Server state; there is no
third party a CRL or OCSP responder would serve.

Four facts shape the decision:

- **The Server's certificates name no unique serial today.** `rcgen` derives a serial from the
  SHA-256 of the public key when none is set. A re-sent CSR for the same key gets the same serial
  twice, and a serial says nothing the key fingerprint does not.
- **Renewal changes the key.** A Client renews at two thirds of its certificate's life with a new
  key ([ADR-0039](0039-admission-requires-both-proofs-and-enrolment-is-approved.md) clause 11). A
  host that renewed shortly before its old certificate was revoked would carry on with the new one.
- **The credential is fleet-wide.** Revoking it shuts out every Agent that still presents it; it
  is the second step of a rotation through a connection-settings offer
  ([ADR-0041](0041-connection-settings-offered-securely-and-server-capabilities.md)), never the
  first.
- **A Gateway hides its downstream certificates.** The Server sees the Gateway's certificate on a
  pooled upstream connection and the downstream credential on its upgrade
  ([ADR-0040](0040-client-modes-and-a-gateway-that-admits-over-mutual-tls.md)); the downstream
  certificate stays on the downstream hop.

## Decision

We will keep a persisted revocation list of certificates, by issuing CA and serial and extended
along every renewal the Server signed, and of credentials, by hash, check it at every admission, and end
each WebSocket session as soon as a proof that admitted it is revoked or its certificate expires.

1. **The Server gives every certificate it signs a random serial.** 16 bytes from the system's
   secure random source, the top bit cleared so the encoding stays positive. Two certificates from
   one key have two serials.

2. **The Server keeps a register of what it signed.** For each certificate: issuer, serial,
   subject, the SHA-256 fingerprint of its public key, `not_after`, the `instance_uid` of the
   message that carried the CSR, its host
   ([ADR-0039](0039-admission-requires-both-proofs-and-enrolment-is-approved.md) clause 7), and — on a
   renewal — the certificate it renewed, its **predecessor**: the certificate the CSR's renewal
   proof names (ADR-0039 clause 27), and without a proof the certificate the connection presented —
   never what `instance_uid` the message names, because that value is self-asserted and a chain
   must not be left on its say-so. A proof is checked against the client CA and its key, so it
   holds through a Gateway too; a downstream Agent without one renews from the Gateway's
   certificate, and what it renews descends from that. An enrolment has no predecessor and is the root of its
   chain. An issuer is identified by the SHA-256 of its DER-encoded name, so no text form of
   a name can fail to match. The register lives under `config_dir`, one file per certificate, and
   each entry is written before the certificate is offered. It holds at most 100 000 certificates,
   at most 10 000 in one chain — below one root, the first certificate the chain renews from — and
   it refuses a renewal once fewer than 1 000 places are left, so renewals alone cannot shut out
   an enrolment. A CSR beyond any of these is refused. An
   entry stays while the certificate, or one renewed from it, is still valid. `GET
   /api/v1/certificates` lists it.

3. **A revoked certificate is named by its CA and serial.** `POST /api/v1/revocations` with
   `{"certificate": {"authority": "client" | "bootstrap", "serial": "<hex>"}}` revokes one, under
   every certificate of that CA file. The client CA covers certificates the Server signed and those
   an operator provisioned from it; the bootstrap CA lets a lost bootstrap certificate be revoked.
   An authority this Server does not have is answered `400`, so a typo cannot pass for a
   revocation.

4. **A revocation follows renewal.** A certificate is revoked when it, or any predecessor in its
   register chain, is on the list. A connection that presents a revoked one is refused before its
   CSR is read, and an open session re-checks its proofs before a CSR it carries is signed.

5. **A revoked credential is named by its value and kept by its hash.** `POST /api/v1/revocations`
   with `{"credential": "<the Authorization value>"}` revokes one; the Server stores the SHA-256 of
   the exact `Authorization` header value and never the value itself. A credential is revoked only
   while `[auth]` accepts it; one it does not is answered `400`, so a typo cannot pass for a
   revocation. `server.toml` keeps only hashes
   ([ADR-0039](0039-admission-requires-both-proofs-and-enrolment-is-approved.md)), so removing a
   revoked credential's hash from it is the operator's to remember; until then it stays revoked.

6. **The list is persisted and bounded.** It lives under `config_dir`, written atomically before
   the request is answered, and survives a restart. It holds at most 100 000 entries, and a
   revocation beyond that is answered `507`. A certificate entry is dropped with its register entry,
   once neither the certificate nor any renewal of it is valid; an entry for a certificate the
   register never held stays until an operator removes it.

7. **A revocation can be lifted.** `GET /api/v1/revocations` lists each entry with its id, kind,
   time and the authority and serial or hash prefix; `DELETE /api/v1/revocations/{id}` removes one,
   answered `404` for an unknown id. Lifting one takes effect at the next connection; it reopens
   nothing.

8. **Admission checks the list after the handshake and before the credential is compared.** On
   `/v1/opamp` and on the download route, a peer whose certificate is revoked, or whose credential
   is, is answered `401` with the challenge of
   [ADR-0039](0039-admission-requires-both-proofs-and-enrolment-is-approved.md) clause 1, and the
   failure counts toward the throttle of clause 24 there. The answer does not say which proof was
   revoked.

9. **A revocation ends the sessions it concerns, and only those.** The Server records, per
   WebSocket session, the issuer and serial of the certificate and the hash of the credential that
   admitted it. A revocation is checked against every open session at once, and a session checks
   itself once more as it starts, so a revocation landing during its admission is not missed; a
   session it concerns
   is closed with WebSocket close code `1008` (policy violation) and the reason `revoked`. Other
   sessions are not touched. On plain HTTP every request is admitted anew, so the next poll is
   refused.

10. **A session ends when its certificate expires.** A WebSocket session is closed with `1008` and
    the reason `certificate expired` at the `not_after` of the certificate that admitted it. A
    Client renews at two thirds of the life and reconnects to prove the new certificate, so a
    healthy fleet never meets this close; it ends what renewal did not replace.

11. **Behind a Gateway the Server revokes what it sees.** A downstream Agent is revoked through
    its credential, by revoking the Gateway's certificate, which ends every Agent the Gateway
    carries and every certificate renewed through it without a proof (clause 2), or by revoking its
    own certificate: a revoked certificate's proof renews nothing. Revoking a downstream
    certificate ends its sessions only once that Agent connects directly.

**Out of scope:** CRL and OCSP for third parties; distributing the list to Gateways; per-Agent
credentials; binding a certificate to an `instance_uid`
([ADR-0039](0039-admission-requires-both-proofs-and-enrolment-is-approved.md) clause 7 stands);
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
- **Distributing the list to Gateways through a custom message.** Rejected for now: it adds a
  protocol extension and makes the Gateway take an admission decision, which ADR-0040 keeps on the
  Server. Recorded as a follow-up.
- **Ending a session by a re-authentication the Server pushes.** Rejected: the Baseline has no
  message for it, and the specification forbids a private side channel.
- **Refusing to start while a revoked credential is still in `server.toml`.** Rejected: the
  revocation is enforced either way, and a Server that will not restart after an operator forgot to
  delete one line would shut out the whole fleet for no gain in security.

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
  the position ADR-0039 took, and the gap this closes.

## Consequences

- Positive: an operator can eject a host within seconds, not within a certificate's lifetime; a
  stolen bootstrap certificate can be withdrawn; a credential rotation can finish without a restart
  that drops the fleet; a session can no longer outlive its certificate. Validity can be shortened
  afterwards on evidence rather than as the only defence.
- Negative / trade-offs: the Server gains two persisted stores and a check per admission; the
  register grows with the fleet times its renewals within one validity period. Revoking the
  credential shuts out every Agent still presenting it, so it must follow a completed rotation.
  Behind a Gateway only the credential and the Gateway's own certificate are revocable. The
  register's bounds turn an admitted member that loops CSRs into a refusal of renewals within its
  chain, and a host holds at most three valid certificates (ADR-0039 clause 7). A downstream Agent
  that renews with a proof renews in its own chain and host; one without a proof renews in the
  Gateway's, and can stop renewal for the Gateway and every Agent it carries until the certificates
  it obtained expire. The bounds keep the damage to one chain and away from enrolment.
- Follow-ups: distributing the list to Gateways; revocation by Agent as a convenience in the
  bundled UI; an audit record of each revocation and each session it closed; shortening
  certificate validity once renewal is proven; bounding issuance per Agent rather than per chain,
  which needs the per-Agent identity this decision leaves out.

## Enforcement

- [`crates/fleet-server/src/revocation.rs`](../../crates/fleet-server/src/revocation.rs) tests:
  `a_revocation_follows_every_renewal` (clause 4), `a_credential_is_kept_by_its_hash_alone`
  (clause 5), `the_list_survives_a_restart_and_can_be_lifted` (clauses 6, 7),
  `a_renewal_stays_revoked_after_its_revoked_ancestor_expires` (clauses 4, 6),
  `the_list_and_the_register_are_bounded`,
  `an_expired_register_entry_is_dropped_with_its_revocation` (clauses 2, 6).
- [`crates/fleet-server/src/fs/revocation.rs`](../../crates/fleet-server/src/fs/revocation.rs)
  test: `the_ledger_survives_a_reopen` (clauses 2, 6).
- [`crates/fleet-server/src/ca.rs`](../../crates/fleet-server/src/ca.rs) test:
  `two_certificates_from_one_key_have_two_serials` (clause 1).
- [`crates/fleet-server/tests/mutual_tls.rs`](../../crates/fleet-server/tests/mutual_tls.rs)
  tests: `a_revoked_certificate_is_refused_on_both_transports_and_the_download` (clauses 3, 7, 8),
  `a_revoked_credential_ends_its_session_and_is_refused` (clauses 5, 8, 9, 11),
  `a_revocation_closes_the_session_it_concerns_and_no_other` (clause 9),
  `a_session_is_closed_when_its_certificate_expires` (clause 10),
  `a_revocation_reaches_a_certificate_renewed_before_it` (clauses 1, 2, 4),
  `a_revocation_names_its_issuer_by_role_whatever_the_issuer_is_called` (clause 3),
  `a_csr_for_another_agent_still_descends_from_the_presented_certificate` (clauses 2, 11).
