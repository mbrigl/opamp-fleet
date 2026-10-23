# ADR-0053: The Server's signing keys may live on a PKCS#11 token, with SoftHSM2 as the planned one

- **Status:** 🟡 proposed
- **Date:** 2026-10-22
- **Deciders:** Markus Brigl
- **Applies to:** the `CertificateSigner` port in `crates/fleet-server/src/fleet.rs` and the local CA in `crates/fleet-server/src/ca.rs` and every PKCS#11 adapter beside it, the signing of bootstrap certificates in `crates/fleet-server/src/bootstrap.rs`, the audit record's checkpoints and export in `crates/fleet-server/src/audit_log.rs` and `audit.rs`, the `[client_ca]`, `[enrolment]`, `[audit]` and `[pkcs11]` sections of `server.toml` and their parsing in `crates/fleet-server/src/config.rs`, `config/server.toml`, the `pki init` and `pki bootstrap-ca` commands in `crates/fleet-tools/src/pki.rs`, `server audit-verify` and `server pki status` in `crates/fleet-server/src/main.rs`, the `cryptoki` dependency in `Cargo.toml` and `deny.toml`, SoftHSM2 with its `softhsm2.conf` and token directory in the Dev Container and on the CI runner, `docs/ARCHITECTURE.md`, and every document that tells an operator where the CA keys live
- **Supersedes:** [ADR-0021](0021-admission-by-a-client-certificate-alone-and-a-revocation-list-that-follows-renewal.md) clauses 9 and 19; [ADR-0028](0028-one-operator-program-that-makes-the-fleets-certificates.md) clauses 3, 8 and 14

## Context

The Server signs two kinds of certificate. As the local client CA it signs every host certificate
(ADR-0021 clause 9), and as the bootstrap CA it issues bootstrap certificates on request
([ADR-0029](0029-a-bootstrap-certificate-as-a-password-protected-pkcs12-file-the-server-keeps.md)).
Both keys are PKCS#8 PEM files that `opamp-fleetctl pki init` writes into the Server's directory
and the Server reads unattended, unencrypted
([ADR-0028](0028-one-operator-program-that-makes-the-fleets-certificates.md) clauses 3 and 14).
Whoever reads `client-ca-key.pem` once, from the host, from a backup or from a snapshot of its
disk, can sign a member certificate for any host until the CA is replaced, and nothing in the
fleet would tell that certificate from a real one.

The audit record is a hash chain in files
([ADR-0022](0022-an-append-only-audit-record-chained-by-hash.md)). A chain shows that entries were
removed or changed in the middle, but whoever can rewrite the store can rewrite the whole chain
from the point of the change, and `server audit-verify` would find nothing. No key signs any part
of it.

The PostgreSQL store, [ADR-0050](0050-the-servers-records-may-live-in-postgresql.md), moves the
Server's records into a database. That widens who can rewrite the audit record: a database
administrator or a stolen backup no longer needs the Server's host. Several Server instances, a
later step, would each need the CA keys, and copying a key file to every instance multiplies the
places it can be read from.

The `CertificateSigner` port in `crates/fleet-server/src/fleet.rs` already separates the signing
of a host certificate from the code that decides to sign it; the bootstrap certificates are signed
in `crates/fleet-server/src/bootstrap.rs` with the bootstrap CA's `KeyPair` directly. The CA keys
are ECDSA P-256 (`rcgen::KeyPair::generate` in `crates/fleet-tools/src/pki.rs`), which every
PKCS#11 token supports. `rcgen` 0.14 signs with any key that implements its `SigningKey` trait,
so the certificate is built as today and only the signature comes from elsewhere.

A hardware or cloud HSM costs money and operation that a first step does not need. SoftHSM2 speaks
the same PKCS#11 interface, is free, and runs wherever the Server runs, so the Server can be built
and tested against it now and moved to a hardware token later by naming another module. Whether the
Server keeps its records in files or in PostgreSQL does not change what a token protects. The
working note "server split", recorded outside the ADRs, sets the target in D3 and D9.

The specification's quality goal Q-1 asks that no Agent be admitted without a client certificate;
the CA key is what makes that certificate worth anything.

## Decision

We will let the Server keep the client CA's, the bootstrap CA's and the audit record's keys on a
token it reaches over PKCS#11, generated there and never leaving it, as an option beside today's
key files whichever store holds the records, and plan, build and test it against SoftHSM2.

1. **One PKCS#11 token, named in `server.toml`.** `[pkcs11]` names `module`, the path of the
   PKCS#11 library; `token_label`; and `pin_file`, a file the Server's account alone may read that
   holds the user PIN. The PIN is never written into `server.toml`
   (ADR-0021 clause 26). The Server loads the module at startup through `cryptoki` and opens one
   session pool on the token. A module that does not load, a token that is not found or a PIN
   that is refused fails startup, naming the key.

2. **Restates ADR-0021 clause 9. The Server signs CSRs as a local CA while `[client_ca]` is
   configured.** `[client_ca]` names `cert_file`, `validity_days` (default 30, `0` refused; a
   month bounds what a certificate stolen unnoticed is good for, and a host offline for a
   fortnight still renews on its own) and the CA's key, as exactly one of `key_file`, a PKCS#8 PEM
   file, or `key_label`, the label of a private key object on the `[pkcs11]` token. Naming both or
   neither fails startup naming the two keys. The key is its own, never the listener's, because
   the CA key is the fleet's trust anchor. `AcceptsConnectionSettingsRequest` is declared only
   while the section is present. A Client that holds no Server-issued certificate, or holds one in
   its renewal window, generates a keypair and a PEM CSR locally and sends it as
   `ConnectionSettingsRequest.opamp.certificate_request`, only once the Server has declared the
   capability ([ADR-0014](0014-connection-settings-offered-without-a-credential-and-server-capabilities.md)
   clause 15). A CSR on a connection whose certificate chains to the client CA is signed at once;
   a CSR on an enrolment connection waits for an operator (ADR-0021 clause 21). A CSR that does not
   parse, or that arrives while no `[client_ca]` is configured, is answered with a
   `ServerErrorResponse` of type `BadRequest`. The issued certificate travels back as an ordinary
   connection-settings offer with no private key in it.

3. **Restates ADR-0021 clause 19. Enrolment has a CA of its own.** `[enrolment] bootstrap_ca_file`
   in `server.toml` names the CA that issues bootstrap certificates, and its key is named by
   exactly one of `bootstrap_ca_key_file`, PKCS#8 PEM, or `bootstrap_ca_key_label`, a private key
   object on the `[pkcs11]` token. The handshake accepts a certificate from the client CA or the
   bootstrap CA (ADR-0021 clause 5). While `[enrolment]` is set, a connection is a member's only
   when its certificate was issued directly by a CA in `client_ca_file`. Any other, from the
   bootstrap CA or from an intermediate below either CA, is an enrolment connection, so no chain
   lifts a bootstrap certificate into the fleet. The Server refuses to start on a bootstrap CA that
   shares a certificate with `client_ca_file`, a bootstrap CA key that is the client CA's key or
   does not match `bootstrap_ca_file`, both or neither of the two key settings when the Server is
   to issue, or an `[enrolment]` without `[client_ca]`. Without either key setting enrolment works
   with bootstrap certificates made elsewhere, and `POST /api/v1/bootstrap-certificates` is
   answered `409`, naming the missing key. Without `[enrolment]` no bootstrap certificate passes
   the handshake, and a host gets its first certificate from an operator.

4. **The token is optional, whichever store holds the records.** With `[pkcs11]` set, each key may
   be named by its label; without it, by its file, as today. `[database] store` does not decide
   it. A `key_label` while `[pkcs11]` is not set fails startup, naming both keys.

5. **The token signs; the Server builds.** A PKCS#11 key implements `rcgen`'s `SigningKey`: its
   public half is read from the token once, and each signature is one `C_Sign` with `CKM_ECDSA`
   over the SHA-256 the Server computes, its `r || s` encoded as the DER `ECDSA-Sig-Value` X.509
   asks for. The certificate's contents, its serial and its validity are made as today. The
   adapter sits behind the `CertificateSigner` port and behind a signer port of the same shape for
   bootstrap certificates and audit checkpoints, so a separate signing service can later take a
   port's place without a change to the use cases.

6. **The Server signs only what a use case justifies, and audits every signature.** A host
   certificate is signed for a valid renewal proof, an approved enrolment request or a member's
   CSR on its own connection; a bootstrap certificate for an operator's request; a checkpoint on
   the schedule of clause 8. Every signature made with a token key writes an audit entry naming
   the key's label, the certificate's issuer and serial or the checkpoint's sequence, and the use
   case that asked for it.

7. **SoftHSM2 is the planned token, and says so.** The Server is built and tested against
   SoftHSM2; a hardware or cloud HSM is a change of `[pkcs11] module`, nothing else. When the
   module's `C_GetInfo` names `SoftHSM` as its manufacturer, the Server starts, logs a warning that
   the keys have no hardware protection, and writes the audit entry `pkcs11.software_token` at
   every startup. SoftHSM2 is what the Dev Container and the CI runner install for the tests,
   beside the packages they already install, within
   [ADR-0002](0002-dev-container-runtime.md).
   - SoftHSM2 reads a configuration of its own, `softhsm2.conf`, from the path in `SOFTHSM2_CONF`
     or from `/etc/softhsm2.conf`. Its `directories.tokendir` names the directory that holds the
     tokens, encrypted under their PINs. The Server neither writes nor reads either; it loads the
     module, and the module finds its configuration as it always does.
   - In production `softhsm2.conf`, the token directory and the token's initialisation are the
     operator's, as an HSM's setup is. The token directory is the keys: it belongs in the backup,
     readable by the Server's account alone, and a copy of it with the PIN file is a copy of every
     key on the token.
   - In the Dev Container and on the CI runner the project creates both: a `softhsm2.conf` whose
     token directory lies outside the repository, and a token initialised for the tests, its PIN in
     a file beside it. Nothing of it is committed.
   - With a hardware or cloud HSM neither file exists; the vendor's client configures how the
     module reaches the device.

8. **The audit key signs checkpoints of the audit record.** While `[audit] key_label` is set, the
   Server signs, about once a minute and at shutdown, a checkpoint holding the head of the audit
   chain it writes — its sequence and hash — with the audit key, and stores it beside the record.
   `server audit-verify` checks every checkpoint's signature against the audit key's public half,
   which `server pki status` prints and an operator keeps, and checks the chain against the last
   checkpoint that names it. A chain that was cut or rewritten after a checkpoint fails the check,
   naming the checkpoint. This protects against tampering in the store by whoever
   cannot use the token; it does not protect against a compromised Server, which can have the
   token sign while it runs.

9. **The audit record may be exported, off by default.** `[audit.export]` names an OTLP/HTTP
   endpoint, and the Server sends every entry as an OTLP log record to it, over TLS 1.3 beyond the
   loopback. The export queues and retries on its own and never blocks a decision; an entry it
   drops after its queue is full is counted and the count is itself an audit entry. A system that
   the Server cannot rewrite is the only anchor that holds against a compromised Server.

10. **Restates ADR-0028 clause 3. `init` writes two directories, and the split is the point.**
    `--server-dir` receives what the Server host needs: `server.pem` and `server-key.pem`,
    `client-ca.pem`, `bootstrap-ca.pem`, and `server.toml.fragment` naming them in `[tls]`,
    `[client_ca]` and `[enrolment]`. Without `--pkcs11-module`, `client-ca-key.pem` and
    `bootstrap-ca-key.pem` are written beside them and named in `key_file` and
    `bootstrap_ca_key_file`. With `--pkcs11-module`, `--token-label` and `--pin-file`, the client
    CA's, the bootstrap CA's and the audit key are generated on the token with `CKA_EXTRACTABLE`
    false and `CKA_SENSITIVE` true, the CA certificates are signed through the token, nothing key is
    written to `--server-dir`, and the fragment names `[pkcs11]`, `key_label`,
    `bootstrap_ca_key_label` and `[audit] key_label`. `--offline-dir` receives what stays with the
    operator: `server-ca.pem` and `server-ca-key.pem`; the package signing key, an Ed25519 key in
    PKCS#8 as `package keygen` writes it, as `package-signing-key.pk8`, and its public half in hex as
    `package-signing-key.pub`
    ([ADR-0026](0026-packages-signed-deployments-offered-downloads-and-verified-delivery.md)); and
    `supervisor.toml.fragment` naming `server-ca.pem` in `[tls] ca_file`, `bootstrap.pem` and
    `bootstrap-key.pem` under `--host-path` in `cert_file` and `key_file`, where a host puts the
    pair the Server issues it, and the public half in `[packages] verification_key`. No key of the
    server CA and no package signing key is ever written to `--server-dir` or to a token. Each
    fragment names its files by the directory they are meant to live in: `--server-path` (default:
    the absolute path of `--server-dir`) and `--host-path` (default: the absolute path of
    `--offline-dir`). The command prints which directory goes where.

11. **Restates ADR-0028 clause 8. A new bootstrap CA replaces the Server's.** `opamp-fleetctl pki
    bootstrap-ca --out <dir>` makes a new bootstrap CA. Without `--pkcs11-module` it writes
    `bootstrap-ca.pem` and `bootstrap-ca-key.pem` to `<dir>`; with it, the key is generated on the
    token under a new label, `bootstrap-ca.pem` is written to `<dir>`, and the command prints the
    label for `bootstrap_ca_key_label`. It never overwrites a file or a token object.

12. **Restates ADR-0028 clause 14. Key files are stored unencrypted; token keys never leave the
    token.** Where the Server reads a CA key from a file, it reads it unattended, and the file is
    protected by where it is kept and by its mode, as every other key of the project is. A key on
    a token is created non-extractable and is used only through `C_Sign`; the Server never reads
    it, and only the PIN file is a secret on the Server's host.

13. **An existing fleet moves to new CA keys at its ordinary renewal.** A key file cannot be
    imported into a token without leaving a copy outside it, so the move makes a new client CA on
    the token. For a transition, `client_ca_file` names the old and the new CA certificate; the
    handshake trusts both, and the Server signs every renewal with the new key. Once the last
    certificate the old CA signed has ended, the operator removes it from `client_ca_file`. A new
    bootstrap CA takes the old one's place as clause 11 makes it.

**Out of scope:** the server CA's key and the package signing key, which stay offline with the
operator; the TLS listener's own key; a separate signing service behind the signer port; keys on
the Client; the format of the stored checkpoints, which the store holding the audit record
decides; what the OTLP export's receiver does with the entries; whether several Server instances
require the token and how they share it, which is that later decision's.

## Alternatives considered

- **PKCS#11 required for every installation.** One signing path to test, but every single-node
  Server would install and run at least SoftHSM2 for a key that sits on the same host either way.
  Rejected; it stays an option.
- **PKCS#11 required with the PostgreSQL store.** One Server on PostgreSQL keeps its key on one
  host as a Server on files does; the database does not make a key file more exposed. What needs
  shared keys is several instances, and that decision can require them. Rejected.
- **Encrypted key files with a passphrase from a file.** It protects a backup, but the passphrase
  sits on the same host as the file, and every instance holds both. Rejected.
- **A separate signing service from the start.** It could check each justification against the
  database itself and survive a compromised Server, but it is a second process to deploy and
  secure, with its own transport and admission. The signer port keeps it possible. Rejected for
  now, as the working note decided.
- **A cloud KMS API instead of PKCS#11.** It binds the Server to one provider's API and SDK.
  PKCS#11 is what HSMs, cloud HSM services and SoftHSM2 all speak. Rejected.
- **Import the existing key files into the token.** It keeps every issued certificate valid, but
  the key has existed outside the token and may exist there still. Rejected; a new CA with a
  transition costs one renewal cycle.
- **A hardware HSM from the start.** It protects the key in hardware, but every developer and CI
  run would need one, and the interface is the same as SoftHSM2's. Rejected for the first step.
- **SoftHSM2 refused in production.** It would force hardware on every installation that wants a
  token, including those whose operator accepts a software token for its other properties — keys
  that the Server cannot read and that no file copy carries. Rejected; the warning and the audit
  entry make the choice visible.
- **Sign every audit entry.** One `C_Sign` per decision puts the token in the path of every
  admission. A checkpoint a minute bounds what can be rewritten unseen to that minute. Rejected.
- **`cryptoki`'s alternatives** (`pkcs11`, `rust-pkcs11`). Older wrappers, unmaintained or with
  raw bindings only. Rejected for `cryptoki`.

## Sources / Prior art

- The working note "server split", sections D3 and D9, recorded outside the ADRs.
- [`cryptoki`](https://docs.rs/cryptoki/0.12.1/cryptoki/) 0.12.1
  ([repository](https://github.com/parallaxsecond/rust-cryptoki)), Apache-2.0: `Pkcs11::new` loads
  the module at runtime through `libloading` from pre-generated bindings, so no `bindgen` and no
  system header enter the build; `Mechanism::EccKeyPairGen`, `Mechanism::Ecdsa` and
  `Mechanism::EcdsaSha256`; `Info::manufacturer_id` from `C_GetInfo`.
- `rcgen` 0.14.10 (the version in `Cargo.lock`): the `SigningKey` and `PublicKeyData` traits in
  `src/key_pair.rs`, and `Issuer::new(params, signing_key)` for any `SigningKey`.
- [PKCS #11 Specification Version 3.1](https://docs.oasis-open.org/pkcs11/pkcs11-spec/v3.1/pkcs11-spec-v3.1.html)
  (OASIS): `CKA_EXTRACTABLE`, `CKA_SENSITIVE`, `CKM_ECDSA` and its `r || s` signature format.
- [SoftHSM2](https://github.com/softhsm/SoftHSMv2), which reports `SoftHSM` as its manufacturer.
- RFC 5480 and RFC 5758: the DER `ECDSA-Sig-Value` an X.509 signature carries.
- The code as it stands: `CertificateSigner` in `crates/fleet-server/src/fleet.rs`, the local CA
  in `crates/fleet-server/src/ca.rs`, bootstrap issuance in `crates/fleet-server/src/bootstrap.rs`,
  key generation in `crates/fleet-tools/src/pki.rs`, the audit chain in
  `crates/fleet-server/src/audit_log.rs`.

## Consequences

- Positive: where the token is used, Q-1 rests on a key no one can copy. A stolen backup, disk
  image or database dump holds no CA key.
- Positive: the Server is written against PKCS#11 once; SoftHSM2 now and a hardware HSM later
  differ in configuration only.
- Positive: the audit record's checkpoints show a rewrite in the database to anyone holding the
  audit key's public half, and the export can put the record where the Server cannot reach it.
- Negative / trade-offs: an installation that uses the token needs SoftHSM2 or an HSM, a PIN file,
  and a CA move at its first switch, which costs one renewal cycle with two trusted CAs.
- Negative / trade-offs: a C library loaded at runtime, the PKCS#11 module, runs inside the
  Server's process, and `cryptoki` is a new dependency for `cargo-deny`.
- Negative / trade-offs: every signature waits for the token; a slow or unreachable HSM delays or
  refuses renewals and enrolments, and the Server reports it as it reports any signing error.
- Negative / trade-offs: a compromised Server can still have the token sign while it runs; the
  audit entries of clause 6 and the export of clause 9 are what show it afterwards.
- Follow-ups: the store and format of checkpoints, together with audit chains per instance;
  whether several Server instances require the token, how they share it, and which instance signs
  the checkpoints; a separate signing
  service behind the signer port; a glossary entry for the HSM and the token.

## Enforcement

Tests that will carry `Verifies: ADR-0053`, against SoftHSM2 where a token is needed:

- A host certificate and a bootstrap certificate signed through a SoftHSM2 token verify against
  the CA certificate on the token, and each signature wrote an audit entry naming the key label.
- `[client_ca]` with both `key_file` and `key_label`, or a `key_label` without `[pkcs11]`, fails
  startup naming the keys; with either store, a `key_file` alone starts as today.
- A module whose manufacturer is `SoftHSM` starts with the warning and the
  `pkcs11.software_token` audit entry.
- The Server starts against a SoftHSM2 token whose `softhsm2.conf` is named only by `SOFTHSM2_CONF`,
  and `server.toml` holds no path of SoftHSM2's own beside `[pkcs11] module`.
- `opamp-fleetctl pki init --pkcs11-module …` creates key objects that are not extractable and
  writes no CA key file to `--server-dir`.
- `server audit-verify` accepts a record whose checkpoints match, and names the chain and the
  checkpoint when an entry after a checkpoint was changed or removed, or when a checkpoint's
  signature does not verify.
- With `[audit.export]` pointing at an unreachable endpoint, admission and enrolment still answer,
  and the dropped count reaches the audit record.
- `client_ca_file` with two CA certificates admits a member of either, and a renewal is signed by
  the token's key.
