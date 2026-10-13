# ADR-0032: The Server keeps the bootstrap CA's key and issues bootstrap certificates whose subject the operator names, and the bootstrap CA lives one year

- **Status:** 🟡 proposed
- **Date:** 2026-10-13
- **Deciders:** Markus Brigl
- **Applies to:** the bootstrap-certificate routes of the Operator plane in `crates/fleet-server/src/api.rs` and the module that issues and records bootstrap certificates, the `[enrolment]` section of `server.toml` and its startup checks in `crates/fleet-server/src/config.rs`, the `bootstrap-certificates/` directory under `config_dir`, the `pki` commands of `opamp-fleetctl` in `crates/fleet-tools/src/pki.rs`, `scripts/dev-pki.sh` and `scripts/seed_test_configs.sh`, the `/api/v1/enrolments` route in `crates/fleet-server/src/api.rs` and the bootstrap certificate's serial an enrolling connection carries in `crates/fleet-server/src/tls.rs`, the Enrolments tab of the bundled UI in `crates/fleet-server/static/index.html`, and every document that tells an operator how to make or hand out a bootstrap certificate
- **Supersedes:** [ADR-0025](0025-one-operator-program-that-makes-the-fleets-certificates.md) clauses 1, 3, 8, 9, 12, 13 and 15; [ADR-0019](0019-admission-by-a-client-certificate-alone-and-a-revocation-list-that-follows-renewal.md) clauses 10, 19 and 22

## Context

A new host reaches the fleet with a bootstrap certificate: it passes the handshake as an
enrolment connection, may only send a CSR, and the CSR waits for an operator's approval inside an
open enrolment window
([ADR-0019](0019-admission-by-a-client-certificate-alone-and-a-revocation-list-that-follows-renewal.md)
clauses 10, 19 to 22). [ADR-0025](0025-one-operator-program-that-makes-the-fleets-certificates.md)
has `opamp-fleetctl pki init` make one bootstrap certificate for the whole fleet on the operator's
machine and keeps the bootstrap CA's key there, beside the server CA's key, on the argument that
"whoever holds the bootstrap CA's key can mint bootstrap certificates".

The forces:

- **The argument does not hold against the key the Server already holds.** The client CA's key is
  on the Server host, because it signs renewals unattended (ADR-0019 clause 9). Whoever holds it
  mints member certificates directly and has no use for a bootstrap certificate, which opens
  nothing without an open window and an approval. Moving the bootstrap CA's key next to it adds
  nothing an attacker on the Server host does not already have. The server CA's key is different:
  with it, an attacker can stand up a Server or a Gateway anywhere that every host trusts, so it
  stays offline.
- **One bootstrap certificate for the whole fleet says nothing about where a request comes from.**
  The operator approving a request sees the bootstrap certificate's subject and fingerprint
  (ADR-0019 clause 22). When that is `CN=<fleet> bootstrap` for every host, the subject cannot
  tell them which customer or site a request belongs to. When a bootstrap certificate per
  customer names that customer, it can.
- **Bootstrap certificates are short-lived and handed out often.** Each lives 10 days by default
  (ADR-0025 clause 9). Making a new one means going to the offline machine every time. On the
  Server it is one request on the Operator plane, which is guarded already (ADR-0019 clause 15),
  and the Server can list what it issued and revoke it by serial (ADR-0019 clause 34).
- **A CA lives as long as what replacing it costs justifies.** Replacing the server CA means a new
  `ca_file` on every host. Replacing the client CA means every host enrolling again. Both keep
  3650 days. Replacing the bootstrap CA touches only `server.toml` and the bootstrap certificates
  not yet used, which live 90 days at most (clause 3 below), so it can be replaced every year at
  little cost, and a key that is online is replaced on a schedule rather than only after a
  compromise.
- **The bootstrap CA stays a CA of its own.** The Server tells an enrolment connection from a
  member's only by its issuer (ADR-0019 clause 19). A single CA for both would make every
  bootstrap certificate a member's in the handshake, and admission would hang on a second check
  whose failure lets a host in.
- **`rustls` does not enforce a CA's end** (ADR-0025, Context). A one-year bootstrap CA that has
  ended keeps passing the handshake. The Server's warning before it ends (ADR-0025 clause 11)
  is what makes the yearly replacement happen. Enforcing the end is a separate decision.
- **A request does not name the record entry it belongs to.** A pending request carries the
  bootstrap certificate's subject and the SHA-256 of the whole certificate (ADR-0019 clause 22).
  The record keeps the SHA-256 of the public key instead (clause 4), the value an enrolment request
  is listed by. Two certificates may share a subject. The serial is what the record, the audit and
  a revocation already name a bootstrap certificate by.
- **The operator who approves works in the UI.** The Enrolments tab is where the window opens and
  a request is approved. Issuing through `curl` and `jq` alone leaves the step the approver takes
  most often outside the tool they approve with, and a UI that only the REST API's existing routes
  drive keeps G-5.

This serves **G-17** (the connection is secured and the Agent is identified), **G-5** (any UI
can drive the fleet) and **Q-1** (secure by default).

## Decision

We will keep the bootstrap CA's key on the Server, have the Server issue bootstrap certificates
through the Operator plane and its bundled UI with a subject the operator names, drop the bootstrap certificate and
the bootstrap CA's key from the operator's offline directory, and give the bootstrap CA a default
life of 365 days while the server CA and the client CA keep 3650.

1. **The Server issues a bootstrap certificate on request.** `POST /api/v1/bootstrap-certificates`
   with a JSON body `{"subject": {…}, "validity_days": n}` (`Content-Type: application/json`, as
   `POST /api/v1/revocations`) makes a fresh ECDSA P-256 key, signs a certificate with the
   bootstrap CA's key and answers `201` with `serial`, `subject`, `not_before`, `not_after`,
   `certificate_pem` and `private_key_pem`, under `Cache-Control: no-store`. The certificate is
   `CA:FALSE`, `digitalSignature`, `clientAuth`, with a 16-byte random serial (ADR-0019 clause 32)
   and no subject alternative name. The private key exists only in that response: it is not
   stored, not logged and not audited, and a lost key is replaced by issuing again.

2. **The operator names the organization, and the Server the rest.** `subject` takes
   `organization` (O) and `organizational_unit` (OU), each optional and at most once, typically
   the customer and the site. A value is 1 to 64 characters of UTF-8 without control characters.
   A key outside this set is answered `400`. The Server sets the common name; the operator never
   does. The subject is descriptive. It authorizes nothing, and no host certificate inherits it.

3. **A bootstrap certificate lives 10 days by default, at most 90, never past its CA.**
   `validity_days` runs from 1 to 90, and anything else is answered `400`. A certificate that
   would outlive the bootstrap CA ends with it, and the response's `not_after` says so.

4. **The Server records what it issued.** Before it answers, the Server writes the serial, the
   subject, the SHA-256 fingerprint of the public key, the issuing time and `not_after` to one file
   per certificate under `config_dir/bootstrap-certificates/`. `GET /api/v1/bootstrap-certificates`
   lists every entry with whether it is revoked. An entry is removed once its certificate has
   ended. At most 10 000 valid entries are kept, and a request beyond that is answered `409`.
   A bootstrap certificate is revoked as ADR-0019 clause 34 states, with the authority
   `bootstrap`. Each issuance is recorded in the audit as `enrolment.bootstrap_issued` with
   serial, subject and `not_after`
   ([ADR-0020](0020-an-append-only-audit-record-chained-by-hash.md)).

5. **Restates ADR-0019 clause 10. A bootstrap certificate carries a host to its first CSR, and an
   operator approves it.** It is issued by the bootstrap CA, never by the client CA, and the Server
   tells it apart from an issued one by that issuer. The Server issues it (clause 1). Any number may
   be valid at once, one per customer, site or batch as the operator chooses, because none opens
   anything on its own: it needs an open enrolment window (ADR-0019 clause 20) and an operator's
   approval (ADR-0019 clause 21). A short validity limits its reach. Handing it to a host is the
   operator's job.

6. **Restates ADR-0019 clause 19. Enrolment has a CA of its own.** `[enrolment]
   bootstrap_ca_file` in `server.toml` names the CA that issues bootstrap certificates, and
   `[enrolment] bootstrap_ca_key_file` names its key, PKCS#8 PEM. The handshake accepts a
   certificate from the client CA or the bootstrap CA (ADR-0019 clause 5). While `[enrolment]` is
   set, a connection is a member's only when its certificate was issued directly by a CA in
   `client_ca_file`. Any other, from the bootstrap CA or from an intermediate below either CA, is
   an enrolment connection, so no chain lifts a bootstrap certificate into the fleet. The Server
   refuses to start on a bootstrap CA that shares a certificate with `client_ca_file`, a bootstrap
   CA key that is the client CA's key or does not match `bootstrap_ca_file`, or an `[enrolment]`
   without `[client_ca]`. Without `bootstrap_ca_key_file` enrolment works as before with bootstrap
   certificates made elsewhere, and `POST /api/v1/bootstrap-certificates` is answered `409`,
   naming the missing key. Without `[enrolment]` no bootstrap certificate passes the handshake,
   and a host gets its first certificate from an operator.

7. **Restates ADR-0025 clause 1. Three commands that make, and a status where the files are.**
   `opamp-fleetctl pki init` makes a fleet's certificates and its package signing key from
   nothing. `opamp-fleetctl pki
   server-cert` makes a server certificate from the existing server CA, for the Server or for a
   Gateway. `opamp-fleetctl pki bootstrap-ca` makes a new bootstrap CA to replace the Server's.
   `opamp-fleetctl pki status` says when the certificates in the offline directory end, and
   `server pki status` says when those of the Server host end. The Server's command runs and exits
   like `hash-credential` and `audit-verify`, before the Server starts serving. The Server binary
   carries no code that makes a CA or a server certificate. It signs host certificates
   (ADR-0019 clause 9) and bootstrap certificates (clause 1). When a certificate counts as ending
   (ADR-0025 clause 11) is one rule in `fleet-core`, plain date arithmetic, so both commands and
   the running Server judge alike, and each program reads the certificate itself. None of the
   commands makes a host certificate or a bootstrap certificate.

8. **Restates ADR-0025 clause 3. `init` writes two directories, and the split is the point.**
   `--server-dir` receives what the Server host needs: `server.pem` and `server-key.pem`,
   `client-ca.pem` and `client-ca-key.pem`, `bootstrap-ca.pem` and `bootstrap-ca-key.pem`, and
   `server.toml.fragment` naming them in `[tls]`, `[client_ca]` and `[enrolment]`, including
   `bootstrap_ca_key_file`. `--offline-dir` receives what stays with the operator:
   `server-ca.pem` and `server-ca-key.pem`; the package signing key, an Ed25519 key in PKCS#8 as
   `package keygen` writes it, as `package-signing-key.pk8`, and its public half in hex as
   `package-signing-key.pub`
   ([ADR-0024](0024-packages-signed-deployments-offered-downloads-and-verified-delivery.md));
   and `supervisor.toml.fragment` naming `server-ca.pem` in `[tls] ca_file`, `bootstrap.pem` and
   `bootstrap-key.pem` under `--host-path` in `cert_file` and `key_file`, where a host puts the
   pair the Server issues it, and the public half in `[packages] verification_key`. No key of the
   server CA and no package signing key is ever written to `--server-dir`. Each fragment names its files by the directory
   they are meant to live in: `--server-path` (default: the absolute path of `--server-dir`) and
   `--host-path` (default: the absolute path of `--offline-dir`). The command prints which
   directory goes where.

9. **Restates ADR-0025 clause 8. A new bootstrap CA replaces the Server's.** `opamp-fleetctl pki
   bootstrap-ca --out <dir>` writes `bootstrap-ca.pem` and `bootstrap-ca-key.pem` to `--out`,
   with the subject a bootstrap CA carries (ADR-0025 clause 5). The operator
   copies both to the Server host in place of `[enrolment]`'s files and restarts the Server.
   Bootstrap certificates from the old CA stop passing the handshake, and those not yet used are
   issued again.

10. **Restates ADR-0025 clause 9. Lifetimes by default, each overridable, never past the CA.** The
    server CA and the client CA live 3650 days, the bootstrap CA 365 days, a server certificate 397
    days. `init` takes `--ca-days` for the server CA and the client CA, `--bootstrap-ca-days` and
    `--server-days`. `server-cert` and `bootstrap-ca` take `--days` for the one certificate they
    make. A server certificate ends no later than the server CA that signs it, and one asked to
    outlive it is cut to the CA's end with a notice. Bootstrap certificates live as clause 3
    states. Host certificates are the running Server's, signed for `[client_ca] validity_days` as
    ADR-0019 clause 9 states.

11. **Restates ADR-0025 clause 12. `pki status` says when each certificate ends.** `server pki
    status --config <server.toml>` lists the files of ADR-0025 clause 11. `opamp-fleetctl pki
    status --offline-dir <dir>` lists the server CA. Each prints every certificate with its
    subject, its serial, its end and the days left, and exits `0` when none is ending in the sense
    of ADR-0025 clause 11, `1` when one is, and `2` when one has ended, so a monitoring job can
    run it.

12. **Restates ADR-0025 clause 13. The project uses its own commands.** `scripts/dev-pki.sh`
    makes its development set with `cargo run -p fleet-tools -- pki init`, writes a complete
    `server.toml` and `supervisor.toml` from the fragments, and needs no `openssl`. Its development
    Client holds a bootstrap pair the development Server issued: `scripts/seed_test_configs.sh
    --enrol` asks the Server for it, writes it where `supervisor.toml` names it, opens the window
    and approves the request. The operator manual describes the `pki` commands and the issuing
    route, reads a serial with `pki status` or from the route's answer, and names no external
    tool. Certificates made elsewhere keep working: the Server reads any PEM that meets the
    requirements these commands write down.

13. **Restates ADR-0025 clause 15. One program, two command trees.** `opamp-fleetctl package`
    holds `fetch`, `pack`, `sha256`, `sign`, `keygen` and `public-key`, each with the options and
    output it has today. `opamp-fleetctl pki` holds `init`, `server-cert`, `bootstrap-ca` and
    `status` as this ADR and ADR-0025 state them. `opamp-fleetctl --version` prints the version
    every binary of this project prints
    ([ADR-0011](0011-versions-resolved-in-the-internal-crate.md)). No other program name exists
    for any of these commands: no alias, no wrapper, no symlink.

14. **Restates ADR-0019 clause 22. A pending request names its bootstrap certificate by serial.**
    `GET /api/v1/enrolments` lists each pending request with its id, its arrival time, the peer
    address, the CSR's subject, the SHA-256 fingerprint of its public key, and the bootstrap
    certificate's subject, fingerprint and serial as `bootstrap_serial`, in the lowercase hex
    without leading zeros that `GET /api/v1/bootstrap-certificates` lists. The Server derives both
    serials with one function, so a request carrying a certificate it issued names that entry.
    `POST /api/v1/enrolments/{id}/approve` and `POST /api/v1/enrolments/{id}/reject` decide one
    request; an unknown or expired id is answered `404`. Both are body-less `POST` acts under
    ADR-0019 clause 16. The Client logs its request's public-key fingerprint, so an operator can
    match a pending request to a host. Renewal by an Agent that holds a certificate from the client
    CA stays automatic (ADR-0019 clause 9) and never enters the queue.

15. **The Enrolments tab issues, lists and revokes bootstrap certificates.** The tab issues one
    with the organization, the organizational unit and the validity an operator types, through
    the route of clause 1. The row of each certificate issued in the open page offers its pair for
    saving as `bootstrap.pem` and `bootstrap-key.pem`, the names `supervisor.toml.fragment` gives
    them (clause 8), as often as the operator likes. The page holds those keys in memory only and
    writes none to browser storage. It drops a key once its certificate is revoked or has ended,
    and every key when it is reloaded or closed, which it asks before while it holds one. A
    certificate issued elsewhere, or before the page was opened, offers no pair. The tab lists the
    certificates of
    `GET /api/v1/bootstrap-certificates` with a press that revokes one by its serial with the
    authority `bootstrap`. Each pending request shows the entry its `bootstrap_serial` names, with
    its subject, serial and end, and says so when no entry matches. The tab uses no route the REST
    API does not offer.

**Out of scope:**

- Carrying the bootstrap certificate's subject, or any customer data, into the host certificate
  issued on approval. Host certificates keep the subject their CSR asks for (ADR-0019 clause 7).
- Customers as a resource of the Server, and separating them from each other. The subject here is
  a label.
- Enforcing a CA's end in the handshake.
- Holding two bootstrap CAs at once while one replaces the other.
- Encrypting the bootstrap CA's key at rest, which ADR-0025 clause 14 leaves unencrypted like the
  client CA's.

## Alternatives considered

- **Keep the bootstrap CA offline and give `pki bootstrap-cert` subject options.** This is the
  smallest change, and it gives each customer a subject of its own. Each certificate is still a
  trip to the offline machine, though, and the Server neither lists nor knows what was issued
  until a host presents it. It protects a key whose loss is already outweighed by the client CA's
  key on the Server.
- **One CA for client and bootstrap certificates, with the bootstrap ones marked by an extension
  or found in a register.** It saves one key pair, but it makes admission depend on a second check
  that lets a host in when it fails. Today a failure falls to the safe side: an enrolment
  connection.
- **One CA for everything.** It combines the above with the server CA's key online: a copy of the
  Server's key would then impersonate the Server to every host.
- **Store the bootstrap certificate's key on the Server for download again.** It is convenient,
  but it leaves a key on the Server host that only the host it is handed to needs. Issuing again
  costs nothing.
- **Let the host send a CSR for its bootstrap certificate.** No key would travel, but the
  bootstrap certificate exists precisely because the host has no channel to the Server yet, and
  one pair per customer is handed to many hosts.
- **Free-form subject as an RFC 4514 string.** It is shorter to specify, but it lets any attribute
  type and encoding into the certificate, which the Server would then have to show back and
  compare. A fixed set of named fields is what an operator fills in a form.
- **Leave the package signing key to `package keygen` alone.** It keeps `init` to certificates,
  but a fleet that skips the second step has no key, and its Clients take no package until
  someone sets `verification_key` by hand. The key belongs where `init` already puts what stays
  offline, and `keygen` remains for a second key or a replacement.
- **Keep `--bootstrap-days` and a first bootstrap pair from `init`.** It gives a fresh fleet a
  bootstrap pair without a running Server, but it keeps a second place where bootstrap
  certificates are made, unlisted and unknown to the Server.
- **Shorter lifetimes for the server CA and the client CA too.** The online client CA's key
  would be replaced more often, but replacing it means every host enrols again, and replacing the
  server CA means a new `ca_file` on every host. Without a way to hold two CAs at once, both stay
  at ten years.
- **Match a request to its entry by the certificate's fingerprint.** The record would keep the
  SHA-256 of the whole certificate as well, and `GET /api/v1/enrolments` would stay as it is. But
  it changes the record's format on disk, and the operator still revokes and reads the audit by
  serial, so the fingerprint would be a second name for the same entry.
- **Keep the pair on the Server so every row offers it, until the certificate ends.** An operator
  could save it again on any day, from any browser. But it is clause 1's key kept after all, and
  it widens who can read it from the request's answer to everyone who can read the record later.
  Holding it in the open page gives the same convenience for the session it is issued in.
- **The UI lists and revokes, and issuing stays with the API.** The private key would then never
  pass through a browser. The key travels to the operator's machine either way, though, in an
  HTTP response read by `curl` or by the page, and the operator who approves would still leave
  the tab to issue.
- **No decision.** Every bootstrap certificate keeps naming the fleet alone, and an approver
  cannot tell one customer's request from another's.

## Sources / Prior art

- [ADR-0019](0019-admission-by-a-client-certificate-alone-and-a-revocation-list-that-follows-renewal.md)
  clauses 9, 15, 19 to 22, 32 and 34, and
  [ADR-0025](0025-one-operator-program-that-makes-the-fleets-certificates.md) clauses 5, 9, 11
  and 14 and its Context on which keys belong on the Server host.
- `rustls-pki-types` `TrustAnchor`: a subject, a key and name constraints, no validity.
  <https://docs.rs/rustls-pki-types>
- RFC 5280 §4.1.2.6 and Appendix A.1: the subject attributes and the upper bound of 64 characters
  for organization, organizational unit and common name. <https://www.rfc-editor.org/rfc/rfc5280>
- Kubernetes TLS bootstrapping: a short-lived bootstrap token issued by the control plane, which
  approves the CSR it carries. <https://kubernetes.io/docs/reference/access-authn-authz/kubelet-tls-bootstrapping/>
- HashiCorp Vault's PKI engine: an issuing CA online, key pairs generated on request and returned
  once, a root kept offline. <https://developer.hashicorp.com/vault/docs/secrets/pki>

## Consequences

- Positive: an operator issues a bootstrap certificate per customer, site or batch from the
  Operator plane, sees its subject on every enrolment request it carries, lists what is valid and
  revokes one by serial (G-17), from the REST API or from the Enrolments tab (G-5).
- Positive: two keys stay on the operator's machine, the server CA's and the package signing key,
  and each does what nothing on the Server host can.
- Positive: the bootstrap CA is replaced every year at the cost of one restart and the unused
  bootstrap certificates.
- Negative / trade-offs: a bootstrap certificate's private key travels in an HTTP response, over
  TLS and to an authenticated operator only. A response kept by a proxy or a terminal history is a
  bootstrap key in the open, which only lets a host into the enrolment queue.
- Negative / trade-offs: a key issued from the UI passes through the browser and lands in the
  operator's download folder, with the permissions that folder gives it. Moving it to the host
  and deleting the copy is the operator's job, as it is for the answer `curl` writes.
- Negative / trade-offs: ADR-0025 is now partly replaced in seven clauses. A later revision of it
  should supersede it whole (ADR-0030, Consequences).
- Negative / trade-offs: a fresh development set needs a running Server before its Client can
  start, and `dev-pki.sh` alone no longer yields a Client that can connect.
- Follow-ups: carrying the bootstrap certificate's subject into the host certificate on approval;
  holding two bootstrap CAs while one replaces the other; enforcing a CA's end in the handshake.

## Enforcement

Tests that will carry `Verifies: ADR-0032`:

- `POST /api/v1/bootstrap-certificates` answers `201` with a certificate the bootstrap CA signed,
  `CA:FALSE`, `clientAuth`, the requested subject, and a key that matches it, under
  `Cache-Control: no-store`. Nothing under `config_dir` and no audit entry contains the private
  key (clause 1).
- An unknown subject key such as `common_name` or `country`, a value over 64 characters or with
  a control character, and `validity_days` of 0 or 91 are each answered `400`. An empty `subject`
  is accepted (clauses 2 and 3).
- A certificate asked to outlive the bootstrap CA ends with it (clause 3).
- An issued certificate appears in `GET /api/v1/bootstrap-certificates`, is recorded as
  `enrolment.bootstrap_issued`, enrols a host whose request lists its subject, and, once revoked
  by its serial, fails admission (clauses 4 and 5).
- The Server refuses to start on a bootstrap CA key that does not match `bootstrap_ca_file` or
  that is the client CA's key. Without `bootstrap_ca_key_file` it starts, admits a bootstrap
  certificate made elsewhere, and answers the issuing route `409` (clause 6).
- `pki init` writes `bootstrap-ca-key.pem` into `--server-dir` and nothing of the bootstrap CA
  into `--offline-dir`. It writes `package-signing-key.pk8` (owner-only) and
  `package-signing-key.pub` into `--offline-dir` alone, the key signs a package the Client's
  verifier accepts under the `verification_key` the fragment names, and the public file holds
  that same value. The bootstrap CA ends 365 days after it starts, and the server CA and the
  client CA 3650 (clauses 8 and 10).
- `opamp-fleetctl pki --help` lists `init`, `server-cert`, `bootstrap-ca` and `status`, and
  `bootstrap-cert` is an unknown command. `pki bootstrap-ca` writes a CA the Server starts on
  (clauses 7, 9 and 13).
- The end-to-end test of ADR-0025 in `crates/fleet-tools/tests/` issues the Client's bootstrap
  pair from the running Server before the Client starts (clause 12).
- A request carrying an issued bootstrap certificate is listed with the `bootstrap_serial` that
  `GET /api/v1/bootstrap-certificates` lists for it (clause 14).
- The bundled UI names the issuing, listing and revocation routes and the fields it reads from
  them, and `PendingEnrolment`, `BootstrapView` and `IssuedBootstrap` in the OpenAPI document
  carry those fields (clauses 14 and 15).

**Not mechanically decidable:** that the operator manual describes issuing through the Server and
names no other way to make a bootstrap certificate (clause 12), and that the UI keeps the private
keys in memory only and drops them as clause 15 states, are held by review.
