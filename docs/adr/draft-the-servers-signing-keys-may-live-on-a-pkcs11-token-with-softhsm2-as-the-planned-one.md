# ADR-draft-the-servers-signing-keys-may-live-on-a-pkcs11-token-with-softhsm2-as-the-planned-one: The Server's signing keys may live on a PKCS#11 token, with SoftHSM2 as the planned one

- **Status:** 🟡 proposed
- **Date:** 2026-10-22
- **Deciders:** Markus Brigl
- **Applies to:** the `CertificateSigner` port in `crates/fleet-server/src/fleet.rs` and the local CA in `crates/fleet-server/src/ca.rs` and every PKCS#11 adapter beside it, the signing of bootstrap certificates in `crates/fleet-server/src/bootstrap.rs`, the audit record's checkpoints and export in `crates/fleet-server/src/audit_log.rs` and `audit.rs`, the `[client_ca]`, `[enrolment]`, `[audit]` and `[pkcs11]` sections of `server.toml` and their parsing in `crates/fleet-server/src/config.rs`, `config/server.toml`, the `pki init`, `pki token-init` and `pki bootstrap-ca` commands in `crates/fleet-tools/src/pki.rs`, `server audit-verify` and `server pki status` in `crates/fleet-server/src/main.rs`, the `cryptoki` dependency of the Server and of `crates/fleet-tools` in `Cargo.toml` and `deny.toml`, SoftHSM2 with its `softhsm2.conf` and token directory in the Dev Container and on the CI runner, `docs/ARCHITECTURE.md`, and every document that tells an operator where the CA keys live

## Context

The Server signs two kinds of certificate. As the local client CA it signs every host certificate
(ADR-0021 clause 8), and as the bootstrap CA it issues bootstrap certificates on request
([ADR-0022](0022-bootstrap-certificates-and-enrolment.md)).
Both keys are PKCS#8 PEM files that `opamp-fleetctl pki init` writes into the Server's directory
and the Server reads unattended, unencrypted
([ADR-0032](0032-the-operator-program-opamp-fleetctl.md) clauses 7 and 16).
Whoever reads `client-ca-key.pem` once, from the host, from a backup or from a snapshot of its
disk, can sign a member certificate for any host until the CA is replaced, and nothing in the
fleet would tell that certificate from a real one.

The audit record is a hash chain in files
([ADR-0023](0023-an-append-only-audit-record.md)). A chain shows that entries were
removed or changed in the middle, but whoever can rewrite the store can rewrite the whole chain
from the point of the change, and `server audit-verify` would find nothing. No key signs any part
of it.

The PostgreSQL store, [ADR-0029](0029-the-servers-state-and-its-stores.md), moves the
Server's records into a database. That widens who can rewrite the audit record: a database
administrator or a stolen backup does not need the Server's host. Several Server instances
([ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it](draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it.md))
each need the CA keys, and copying a key file to every instance multiplies the places it can be
read from.

The `CertificateSigner` port in `crates/fleet-server/src/fleet.rs` already separates the signing
of a host certificate from the code that decides to sign it; the bootstrap certificates are signed
in `crates/fleet-server/src/bootstrap.rs` with the bootstrap CA's `KeyPair` directly. The CA keys
are ECDSA P-256 (`rcgen::KeyPair::generate` in `crates/fleet-tools/src/pki.rs`), which every
PKCS#11 token supports. `rcgen` 0.14 signs with any key that implements its `SigningKey` trait,
so the certificate is built by the same code and only the signature comes from elsewhere.

A hardware or cloud HSM costs money and operation that a first step does not need. SoftHSM2 speaks
the same PKCS#11 interface, is free, and runs wherever the Server runs, so the Server can be built
and tested against it now and moved to a hardware token later by naming another module. Whether the
Server keeps its records in files or in PostgreSQL does not change what a token protects. The
working note "server split", recorded outside the ADRs, sets the target in D3 and D9.

The specification's quality goal Q-1 asks that no Agent be admitted without a client certificate;
the CA key is what makes that certificate worth anything.

## Decision

We will let the Server keep the client CA's, the bootstrap CA's and the audit record's keys on a
token it reaches over PKCS#11, generated there and never leaving it, as an option beside the
key files whichever store holds the records, and plan, build and test it against SoftHSM2.

1. **One PKCS#11 token, named in `server.toml`.** `[pkcs11]` names `module`, the path of the
   PKCS#11 library; `token_label`; and `pin_file`, a file that holds the user PIN. The PIN file may
   be read by its owner and its group and written by its owner alone, as `password_file` may
   ([ADR-0029](0029-the-servers-state-and-its-stores.md) clause 24); one that others may read, or
   its group may write, fails startup naming it. The PIN is never written into `server.toml`
   (ADR-0021 clause 21). The Server loads the module at startup through `cryptoki` and opens one
   session pool on the token. PKCS#11 initialises a module once per process, so instances that
   share a process, as tests run them, share one `cryptoki` context. A module that does not load, a
   token that is not found or a PIN that is refused fails startup, naming the key.

2. **Clause 8 of
   [ADR-draft-admission-by-client-certificate-and-revocation](draft-admission-by-client-certificate-and-revocation.md)
   decides it: The Server signs CSRs as a local CA while
   `[client_ca]` is configured.**

3. **Clause 17 of
   [ADR-draft-admission-by-client-certificate-and-revocation](draft-admission-by-client-certificate-and-revocation.md)
   decides it: Enrolment has a CA of its own.**

4. **The token is optional, whichever store holds the records.** With `[pkcs11]` set, each key may
   be named by its label; without it, by its file. `[database] store` does not decide
   it. A `key_label` while `[pkcs11]` is not set fails startup, naming both keys.

5. **The token signs; the Server builds.** A PKCS#11 key implements `rcgen`'s `SigningKey`: its
   public half is read from the token once, and each signature is one `C_Sign` with `CKM_ECDSA`
   over the SHA-256 the Server computes, its `r || s` encoded as the DER `ECDSA-Sig-Value` X.509
   asks for. The certificate's contents, its serial and its validity are made as with a key file.
   The adapter sits behind the `CertificateSigner` port and behind a signer port of the same shape
   for bootstrap certificates and audit checkpoints, so a separate signing service can later take
   a port's place without a change to the use cases.

6. **The Server signs only what a use case justifies, and audits every signature.** A host
   certificate is signed for a valid renewal proof, an approved enrolment request or a member's
   CSR on its own connection; a bootstrap certificate for an operator's request; a checkpoint on
   the schedule clause 8 points to. Every signature made with a token key writes an audit entry
   naming the key's label, the certificate's issuer and serial or the checkpoint's sequence, and
   the use case that asked for it.

7. **SoftHSM2 is the planned token, and says so.** The Server is built and tested against
   SoftHSM2; a hardware or cloud HSM is a change of `[pkcs11] module`, nothing else. SoftHSM2 keeps
   its token in files on one host, so it serves the Server instances of that host alone
   ([ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it](draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it.md)
   clause 2). A network HSM's module opens its own connection to the device, with its own TLS
   stack; that connection is the module's and the operator's, outside the Server's TLS provider
   ([ADR-draft-tls-1-3-two-planes-and-bounded-input](draft-tls-1-3-two-planes-and-bounded-input.md)
   clause 21). When the
   module's `C_GetInfo` names `SoftHSM` as its manufacturer, the Server starts, logs a warning that
   the keys have no hardware protection, and writes the audit entry `pkcs11.software_token` at
   every startup. SoftHSM2 is what the Dev Container and the CI runner install for the tests,
   beside the packages they already install, within
   [ADR-0004](0004-dev-container-runtime.md).
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
   - The Server's image carries no PKCS#11 module
     ([ADR-0034](0034-releases-attestations-and-the-server-image.md)). An operator who runs the
     image with a token mounts the module into the container, with what it loads, and names its
     path in `[pkcs11] module`; with SoftHSM2 also its configuration, named by `SOFTHSM2_CONF`, and
     its token directory. A module that does not load in the container fails startup (clause 1).

8. **The audit key signs the heads of the audit chains.** `[audit] key_label` names a key on the
   token that signs checkpoints of the heads of every audit chain. What a checkpoint holds, when
   and by which process it is signed, where it is kept and how `server audit-verify` checks it are
   decided by
   [ADR-draft-the-heads-of-the-audit-chains-are-signed-with-a-key-on-the-token](draft-the-heads-of-the-audit-chains-are-signed-with-a-key-on-the-token.md).

9. **The audit record may be exported, off by default.** `[audit.export]` names an OTLP/HTTP
   endpoint, and the Server sends every entry as an OTLP log record to it, over TLS 1.3 beyond the
   loopback, one of the outbound connections
   [ADR-draft-tls-1-3-two-planes-and-bounded-input](draft-tls-1-3-two-planes-and-bounded-input.md)
   clause 21 allows. The export queues and retries on its own and never blocks a decision; an entry
   it drops after its queue is full is counted and the count is itself an audit entry. A system that
   the Server cannot rewrite is the only anchor that holds against a compromised Server.

10. **Clauses 7 and 8 of
    [ADR-draft-the-operator-program-opamp-fleetctl](draft-the-operator-program-opamp-fleetctl.md)
    decide it: `init` writes two directories, and the
    keys on the token are made by `pki token-init` on the Server host.**

11. **Clause 12 of
    [ADR-draft-the-operator-program-opamp-fleetctl](draft-the-operator-program-opamp-fleetctl.md)
    decides it: A new bootstrap CA replaces the
    Server's.**

12. **Clause 16 of
    [ADR-draft-the-operator-program-opamp-fleetctl](draft-the-operator-program-opamp-fleetctl.md)
    decides it: Key files are stored unencrypted; token
    keys never leave the token.**

13. **An existing fleet moves to new CA keys at its ordinary renewal.** A key file cannot be
    imported into a token without leaving a copy outside it, so the move makes a new client CA on
    the token, with `opamp-fleetctl pki token-init` on the Server host. For a transition,
    `client_ca_file` names the old and the new CA certificate; the handshake trusts both, and the
    Server signs every renewal with the new key. Once the last certificate the old CA signed has
    ended, the operator removes it from `client_ca_file`. A new bootstrap CA takes the old one's
    place as clause 11 makes it.

**Out of scope:** the server CA's key and the package signing key, which stay offline with the
operator; the TLS listener's own key; a separate signing service behind the signer port; keys on
the Client; what the OTLP export's receiver does with the entries; how several Server instances
share the token's keys, which clause 2 of the instance draft decides.

## Alternatives considered

- **PKCS#11 required for every installation.** One signing path to test, but every single-node
  Server would install and run at least SoftHSM2 for a key that sits on the same host either way.
  Rejected; it stays an option.
- **PKCS#11 required with the PostgreSQL store.** One Server on PostgreSQL keeps its key on one
  host as a Server on files does; the database does not make a key file more exposed. What needs
  shared keys is several instances, and clause 2 of the instance draft makes an instance with a CA
  key file the only one. Rejected.
- **Encrypted key files with a passphrase from a file.** It protects a backup, but the passphrase
  sits on the same host as the file, and every instance holds both. Rejected.
- **A separate signing service from the start.** It could check each justification against the
  database itself and survive a compromised Server, but it is a second process to deploy and
  secure, with its own transport and admission. The signer port keeps it possible. Rejected, as in
  the working note.
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
- [PKCS #11 Specification Version
  3.1](https://docs.oasis-open.org/pkcs11/pkcs11-spec/v3.1/pkcs11-spec-v3.1.html)
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
- Positive: the audit record's checkpoints (clause 8) show a rewrite in the database to anyone
  holding the audit key's public half, and the export can put the record where the Server cannot
  reach it.
- Negative / trade-offs: an installation that uses the token needs SoftHSM2 or an HSM, a PIN file,
  and a CA move at its first switch, which costs one renewal cycle with two trusted CAs.
- Negative / trade-offs: a C library loaded at runtime, the PKCS#11 module, runs inside the
  Server's process, and `cryptoki` is a new dependency for `cargo-deny`.
- Negative / trade-offs: every signature waits for the token; a slow or unreachable HSM delays or
  refuses renewals and enrolments, and the Server reports it as it reports any signing error.
- Negative / trade-offs: a compromised Server can still have the token sign while it runs; the
  audit entries of clause 6 and the export of clause 9 are what show it afterwards.
- Follow-ups: a separate signing service behind the signer port; a glossary entry for the HSM and
  the token.

## Enforcement

Tests that will carry `Verifies:
ADR-draft-the-servers-signing-keys-may-live-on-a-pkcs11-token-with-softhsm2-as-the-planned-one`,
against SoftHSM2 where a token is needed:

- A host certificate and a bootstrap certificate signed through a SoftHSM2 token verify against
  the CA certificate on the token, and each signature wrote an audit entry naming the key label.
- A `pin_file` that others may read, or its group may write, fails startup naming it.
- `[client_ca]` with both `key_file` and `key_label`, or a `key_label` without `[pkcs11]`, fails
  startup naming the keys; with either store, a `key_file` alone starts without a token.
- A module whose manufacturer is `SoftHSM` starts with the warning and the
  `pkcs11.software_token` audit entry.
- The Server starts against a SoftHSM2 token whose `softhsm2.conf` is named only by `SOFTHSM2_CONF`,
  and `server.toml` holds no path of SoftHSM2's own beside `[pkcs11] module`.
- `opamp-fleetctl pki token-init` creates the client CA's, the bootstrap CA's and the audit key on
  the token, none of them extractable, signs both CA certificates through the token, writes no key
  file, and refuses a label the token already holds; `pki init --ca-keys-on-token` writes neither
  the client CA nor the bootstrap CA.
- With `[audit.export]` pointing at an unreachable endpoint, admission and enrolment still answer,
  and the dropped count reaches the audit record.
- `client_ca_file` with two CA certificates admits a member of either, and a renewal is signed by
  the token's key.
