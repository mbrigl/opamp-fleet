# ADR-0022: Bootstrap certificates and enrolment

- **Status:** 🟢 accepted
- **Date:** 2026-10-19
- **Deciders:** Markus Brigl
- **Applies to:** the bootstrap-certificate and enrolment routes of the Operator plane in `crates/fleet-server/src/api.rs`, the issuing and recording of bootstrap certificates in `crates/fleet-server/src/bootstrap.rs`, the signing of host certificates in `crates/fleet-server/src/ca.rs`, the approval and renewal paths in `crates/fleet-server/src/fleet.rs`, the pending queue in `crates/fleet-server/src/enrolment.rs` and its answer in `crates/fleet-server/src/transport.rs`, the `[enrolment]` section of `server.toml` in `crates/fleet-server/src/config.rs`, the password rule in `crates/fleet-core/src/pkcs12.rs`, the Enrolments tab in `crates/fleet-server/static/index.html`, `service install`, its flags and the questionnaire in `crates/fleet-agent/src/cli.rs`, `crates/fleet-agent/src/main.rs` and `crates/fleet-agent/src/config_init.rs`, the enrolment in `crates/fleet-agent/src/service/enrol.rs`, the steps `packaging/linux/postinst` prints, the `p12-keystore` crate in `Cargo.toml`, and every document that tells an operator how to issue, hand out, revoke, list or delete a bootstrap certificate, approve an enrolment, or install a Client

## Context

A host becomes a Host by enrolment: it presents a bootstrap certificate, files a CSR, and waits
until an operator approves it. Admission itself is by the client certificate the Server issues
then ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)). This ADR governs the
bootstrap certificate from its issue to the host certificate it leads to, and the install that
carries it onto a host.

The forces:

- **The bootstrap key must not travel in the clear.** Between the UI and the host a plaintext key
  is a file in a download folder, a ticket or a mail. Whoever holds it can join the enrolment queue
  under that bootstrap certificate's subject, which is what the approver judges by.
- **A key the page drops is gone.** An operator who closes the tab, or who hands the same
  bootstrap certificate to a second host a day later, would have to issue a new one, and the record
  would grow by one certificate per mishap, each valid for days.
- **PKCS#12 is the format operators' tools know** for "a certificate with its key, under a
  password": OpenSSL, Windows, macOS Keychain and Java import it. OpenSSL 3 writes it by default
  with AES-256-CBC under PBKDF2 and an HMAC-SHA-256 MAC. `rustls` and `rcgen` read PEM and DER
  only, and OpenSSL is banned from the build (`deny.toml`,
  [ADR-0012](0012-tls-1-3-two-planes-and-bounded-input.md)).
- **The password protects one thing:** the bootstrap key inside `bootstrap.p12`. That key does not
  admit a host; it lets a host file an enrolment request while a window is open, and every request
  waits for an operator. A leaked file is attacked offline, where no rate limit applies, and the
  requests it yields take places in the queue and may arrive through Gateways. The operator types
  the password twice, once at issue and once on the host, often for one host at a time.
- **One bootstrap certificate is handed to many hosts,** per customer and site. Whoever holds one
  could fill the whole queue with requests for fresh keys, so that every other host is refused
  until the window closes; a bound per certificate must still let a whole site enrol at once.
- **A Host is described by its subject values** — country, state, locality, organization,
  organizational unit, organization identifier — carried over from the bootstrap certificate it
  enrolled with, and they enter the effective description as `fleet.host.*`. A Selector can then
  target a customer or a site by values the Server issued and no Agent can forge. The certificate
  carries what is stable and assigns responsibility; anything that changes belongs elsewhere.
- **A bootstrap certificate may come from a foreign PKI,** when the Server holds no
  `bootstrap_ca_key_file`, so the Server cannot assume that what it reads from one is well-formed.
  The CSR's subject is chosen by the host itself and is never trusted.
- **A revoked row stays until its certificate ends,** up to 90 days, and counts toward the record's
  10 000 entries. The revocation is what keeps a certificate out; the record is what the operator
  reads.
- **A packaged install never lies beside the fleet's files.** The `.deb` and the `.rpm` deliver the
  program under `/usr/libexec/<PRODUCT_NAME>/`, and `std::env::current_exe` resolves symbolic links
  on Linux. Its post-install must not enrol: the enrolment waits for an operator, and `apt` or
  `dnf` would hold the package database while it waits.
- **A scripted install has no terminal.** A password must reach it without being typed, without
  the process list and without the environment, which a child process inherits.

This serves **G-17** (the connection is secured and the Agent is identified), **G-11** (the Client
runs as an OS service, installed by script), **G-15** (a host only a Gateway reaches enrols
through it), **G-5** (any UI drives the fleet through the REST API alone), **Q-1** (no credential
travels unencrypted beyond the loopback), **Q-2** (untrusted input is bounded and fuzzed) and
**Q-3** (a new dependency passes the supply-chain gate). The subject values describe and select;
they authorize nothing, and the specification's Non-Goal *Authorization and multi-tenancy* stands.

## Decision

We will issue every bootstrap certificate as a password-protected PKCS#12 file the Server keeps
for download, carry its subject values into every host certificate an enrolment yields and every
renewal after it, bound the enrolment queue per bootstrap certificate, and have the Client's install
find the bootstrap identity beside its program and enrol with it.

1. **The operator names six subject attributes, organization and organizational unit required and
   the others optional, and the Server the rest.** `subject` takes:
   - `country` (C): exactly two upper-case letters, ISO 3166-1 alpha-2, encoded as
     `PrintableString`;
   - `state` (ST), `locality` (L), `organization` (O), `organizational_unit` (OU) and
     `organization_identifier` (2.5.4.97): each 1 to 64 characters of UTF-8 without control
     characters, encoded as `UTF8String`, in no format of the Server's choosing. A UUID is one such
     value.

   `organization` and `organizational_unit` are required: a request without either is answered
   `400`, naming it, so every Host has a customer and a site its Selectors can rely on. The other
   four are optional. Each may appear at most once. A key outside this set is answered `400`. The
   Server sets the common name by the rule of the fleet's certificate names
   ([ADR-0032](0032-the-operator-program-opamp-fleetctl.md)), and the operator never does. The
   order in the subject is C, ST, L, O, OU, organizationIdentifier, CN. The values describe and
   select through the effective description; they authorize nothing.

2. **A bootstrap certificate's subject is the operator's choice per issue, and the host certificate
   carries it.** The common name of a bootstrap certificate follows the rule of the fleet's
   certificate names ([ADR-0032](0032-the-operator-program-opamp-fleetctl.md)). Its other
   attributes are what the issuing request names (clause 1), not the bootstrap CA's; the
   organization behind the CA is read from its issuer. A host certificate carries the bootstrap
   certificate's values (clause 12, and the approval's copy of them in
   [ADR-0015](0015-client-modes-and-the-gateway.md)) and its own common name under that rule.

3. **The Server issues a bootstrap certificate on request, always as a PKCS#12 file.**
   `POST /api/v1/bootstrap-certificates` with a JSON body
   `{"subject": {…}, "validity_days": n, "password": "…"}` (`Content-Type: application/json`, as
   `POST /api/v1/revocations`; `validity_days` as
   [ADR-0032](0032-the-operator-program-opamp-fleetctl.md) clause 14 bounds it), `password`
   required — a request without it is answered `400`, naming it — makes a fresh ECDSA P-256 key
   and signs a certificate with the bootstrap CA's key.
   The certificate is `CA:FALSE`, `digitalSignature`, `clientAuth`, with a 16-byte random serial
   ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)) and no subject alternative
   name. The Server makes the PKCS#12 file of clause 4, records and keeps it as the record of
   issued bootstrap certificates states
   ([ADR-0029](0029-the-servers-state-and-its-stores.md)), and answers `201` with `serial`,
   `subject`, `not_before`, `not_after` and `certificate_pem`, and no `private_key_pem`. The key
   exists in plaintext only in the Server's memory while it issues; it leaves the Server only
   inside that file. A host that needs the pair as PEM takes it out of the file with
   `opamp-fleetctl pki p12-extract` ([ADR-0032](0032-the-operator-program-opamp-fleetctl.md)).

4. **The file.** A `bootstrap.p12` holds one private key and the one bootstrap certificate it
   belongs to, and nothing else: neither the bootstrap CA, nor the server CA, nor any other
   certificate. Key and certificate are encrypted with PBES2 — PBKDF2-HMAC-SHA-256 at 600 000
   iterations and AES-256-CBC — and the file carries an HMAC-SHA-256 MAC at 600 000 iterations. The
   Server writes and the Client reads it with `p12-keystore` (clause 13).

5. **A password is 8 to 128 characters of UTF-8 without control characters.** Anything else is
   answered `400`. The rule applies where a password is chosen, when a bootstrap certificate is
   issued; a file issued before opens with the password it was issued under. The password is not
   stored, logged or audited anywhere, on the Server or on the host. It reaches the Server on the
   Operator plane, which beyond the loopback serves TLS only
   ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)).

6. **The server CA stays a file of its own.** `server-ca.pem` is the trust anchor written as
   `[tls] ca_file` and kept for every later connection and renewal, while the bootstrap identity
   serves once and is removed ([ADR-0019](0019-the-installed-service-and-its-state.md)). It comes
   from the operator's offline directory, not from the Server it is meant to verify, and the Server
   never puts it into a PKCS#12 file.

7. **Only a revoked entry is deleted, and its revocation stays.**
   `DELETE /api/v1/bootstrap-certificates/{serial}` removes the entry and any PKCS#12 file of a
   certificate whose revocation is on the list, and answers `204`. It answers `409` for a
   certificate that is not revoked — revoke it first — and `404` for a serial the record does not
   hold. It touches the revocation list not at all: the certificate stays refused at the handshake
   for as long as it is valid ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)).
   A request that still carries it is shown as *not issued here*, as one with a certificate made
   elsewhere is. Lifting the revocation of a deleted certificate is still possible through
   `DELETE /api/v1/revocations/{id}` and lets it enrol again; the record does not list it again.
   The route is cross-site protected as every changing route of the plane is
   ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)).

8. **The Enrolments tab issues, lists, revokes and deletes bootstrap certificates.**
   - The tab issues one through the route of clause 3, with clause 1's six fields, the validity and
     the password the operator types; organization, organizational unit and the password are
     required. The country is chosen from a list of ISO 3166-1 codes.
   - The password is typed twice, and the tab sends nothing while the two differ.
   - The page never holds a private key: every certificate it issues is kept by the Server as a
     PKCS#12 file.
   - The row of each certificate whose entry says `p12` offers **Save bootstrap.p12**, in any page
     and at any time, through the download route of the record of issued bootstrap certificates
     ([ADR-0029](0029-the-servers-state-and-its-stores.md)). A certificate issued elsewhere offers
     nothing to save.
   - The tab lists the certificates of `GET /api/v1/bootstrap-certificates`, each with its subject
     values. A valid one has a press that revokes it by its serial with the authority `bootstrap`.
     A revoked one has a **Delete** press that asks first and then deletes its entry through the
     route of clause 7.
   - Each pending request shows the subject values, serial and end of the entry its
     `bootstrap_serial` names (clause 10), and says so when no entry matches.
   - The tab uses no route the REST API does not offer.

9. **An enrolment connection may only send a CSR, the CSR waits for an operator, and one bootstrap
   certificate holds at most a share of the queue.** It is admitted only while the window is open;
   outside a window it is answered `503`, which the throttle of failed handshakes per address
   ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)) does not count, so hosts
   waiting for an operator cannot throttle the members behind the same address. The Server reads
   nothing from its messages but `connection_settings_request.opamp.certificate_request`: it
   creates no Agent record, assigns nothing and makes no offer but the issued certificate. A
   message without a CSR is answered with the Server's capabilities and nothing else, so the Client
   learns it may send one. The CSR enters a pending queue keyed by its public key, so a re-sent CSR
   joins its own entry; the Client keeps its pending key and re-sends the same CSR until it is
   answered. The queue holds at most 1024 requests. The requests of one bootstrap certificate, by
   its issuer and serial, hold at most `[enrolment] max_pending_per_bootstrap` of them, 256 by
   default; the value runs from 1 to 1024, and anything else fails startup naming the key. A CSR
   beyond either bound is answered `ServerErrorResponse` `Unavailable` with `retry_info`, and one
   beyond the share of its bootstrap certificate is audited once per certificate and window as
   `enrolment.refused` with the reason. A re-sent CSR that already has its entry is never refused
   by either bound. On approval the Server signs it
   ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)) and offers the certificate
   on that connection the way it offers any certificate; the Agent then reconnects with it. On
   rejection the CSR is answered `BadRequest`, and the connection is closed.

10. **Pending requests are listed and decided through the REST API, by the values that will be
    carried.**
    - `GET /api/v1/enrolments` lists each pending request with:
      - its id, its arrival time and the peer address;
      - the SHA-256 fingerprint of its public key;
      - the bootstrap certificate's fingerprint and serial (`bootstrap_serial`), in the lowercase
        hex without leading zeros that `GET /api/v1/bootstrap-certificates` lists;
      - the six subject values an approval would carry, read under the approval's rules
        ([ADR-0015](0015-client-modes-and-the-gateway.md)), or, where those rules refuse them,
        `subject_refusal`: the reason, naming the attribute. The operator sees before pressing
        Approve what it will carry or why it will be refused.
    - The CSR's subject is not listed, because it is neither trusted nor carried.
    - The Server derives both serials with one function, so a request carrying a certificate it
      issued names that entry.
    - `POST /api/v1/enrolments/{id}/approve` and `POST /api/v1/enrolments/{id}/reject` decide one
      request; an unknown or expired id is answered `404`. Both are body-less `POST` acts under the
      Operator plane's cross-site rule
      ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)).
    - The Client logs its request's public-key fingerprint, so an operator can match a pending
      request to a host.
    - Renewal by an Agent that holds a certificate from the client CA stays automatic
      ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)) and never enters the
      queue.

11. **A certificate proves membership and its host, and carries its bootstrap certificate's
    subject values.**
    - The Server does not require the subject to match `instance_uid`, `service.instance.name`, or
      anything else an Agent reports.
    - The request dictates neither the certificate's subject nor its powers. The Server sets the
      subject to:
      - the six attributes of clause 1, copied from the bootstrap certificate an approval reads
        ([ADR-0015](0015-client-modes-and-the-gateway.md)) or from the certificate a renewal
        renews (clause 12);
      - the common name the rule of the fleet's certificate names derives
        ([ADR-0032](0032-the-operator-program-opamp-fleetctl.md)).
    - The Server signs a client-authentication leaf only — `CA:FALSE`, `digitalSignature`,
      `clientAuth`. It drops every requested SAN and puts in one of its own, the URI
      `urn:opamp-fleet:host:<id>`.
    - The CSR's subject is still searched for claims
      ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)) and is never copied.
    - The host is minted when an operator approves an enrolment and carried on by every renewal
      ([ADR-0015](0015-client-modes-and-the-gateway.md)). A certificate an operator provisioned
      without one is given a host derived from its issuer and serial when it is first renewed.
    - An `instance_uid` is spoken for by the host whose certificate first reported it:
      - A connection that presents another host's certificate and reports for it is re-keyed
        through `AgentIdentification` to an identity of its own.
      - A CSR in that message may not claim the old one.
      - A re-key an Agent asks for keeps its host.
    - A host holds at most three valid certificates at a time and speaks for at most 256
      `instance_uid`s.
    - A Gateway carries other hosts' Agents, so an operator marks it as one —
      `PUT /api/v1/hosts/{host}/gateway` — and its certificate then speaks for any Agent. Behind
      it, membership alone applies. `GET /api/v1/hosts` lists each host, whether it is a Gateway,
      what it speaks for and how many valid certificates it holds.

12. **A renewal carries the predecessor's values unchanged.** A renewal
    ([ADR-0015](0015-client-modes-and-the-gateway.md)) copies the six attributes from the
    certificate it renews, and only from it, without applying clause 1's rules again: a host whose
    certificate was provisioned before them still renews. A value its encoding cannot hold fails
    the renewal. The Server checks that the new certificate carries exactly those values. A Host's
    subject values change only through a new enrolment.

13. **Issuance adds `rcgen`, `time`, `x509-parser` and `p12-keystore`, and no system
    dependency.** `rcgen` (features stated: `crypto`, `ring`, `pem`, `x509-parser`) generates the
    CSR and signs it; `time` expresses validity; `x509-parser` reads a certificate's validity for
    the renewal window. The `ring` backend is the provider
    [ADR-0012](0012-tls-1-3-two-planes-and-bounded-input.md) already installs, so no OpenSSL and no
    cmake enter the build. `ring` compiles C and assembly in its build script, so a C compiler is
    needed either way; what this keeps is the absence of system libraries. `p12-keystore` (pinned
    exactly, `default-features = false`, so its `pbes1` feature with `rc2` and `des` is not linked)
    writes the PKCS#12 file in the Server and reads it in the Client. It is pure Rust on the
    RustCrypto crates; it handles the password-based encryption only, and the TLS handshake stays
    on `ring`. `dialoguer` gains its `password` feature for the hidden question; its one
    dependency, `zeroize`, is already in the tree.

14. **Beside the program is the directory of the running executable, unless `--fleet-files` names
    another.** The install reads it from the operating system (`std::env::current_exe`), before
    the program is staged, so it is where the operator ran the program from.
    `service install --fleet-files <dir>` names the directory the install looks in instead, for all
    of the fleet's files, with or without `--interactive`; wherever this ADR or
    [ADR-0019](0019-the-installed-service-and-its-state.md) says "beside the program", that
    directory is meant when the flag is given. A `<dir>` that is not a readable directory fails the
    install before anything is written, naming the flag. The install never looks in both. A file
    counts only when it is a readable regular file under its exact name. **A bootstrap identity**
    is either the bootstrap pair, `bootstrap.pem` with `bootstrap-key.pem`, or `bootstrap.p12`;
    wherever a "bootstrap pair" is named for the install, a bootstrap identity is meant. The
    bootstrap pair counts only as a pair; one of the two alone is named in a warning and not used.
    When `bootstrap.p12` and any file of the pair lie there together, a warning names them and none
    of them is used, so the install never guesses which identity is meant.
    `package-signing-key.pub` counts when its content, trimmed, is 64 hexadecimal digits; any other
    content is named in a warning and not used.

15. **The Client reads a `bootstrap.p12` bounded and in memory.** A file over 64 KiB is refused
    before it is read. It must decrypt and pass its MAC under the password, use only the schemes of
    clause 4's family (PBES2 with PBKDF2-HMAC-SHA-2 and AES; no PBES1, RC2 or 3DES), and hold
    exactly one private key with the one certificate whose public key matches it; anything else is
    refused, naming the file. The key and the certificate are held in memory for the enrolment, and
    the decrypted key is never written to disk. The identity the enrolment then makes and stores is
    the Client's own (clause 9), as with the pair.

16. **The questionnaire asks only what has no useful default on a fresh host, and offers no answer
    the Client would refuse.** It asks for the Server `endpoint`, suggesting
    `wss://127.0.0.1:4320/v1/opamp`; a `ws://` or `http://` endpoint is refused unless its host is a
    loopback IP literal — `127.0.0.1` or `::1`; a host name is never loopback, `localhost`
    included — and the question is asked again with a message saying that beyond the loopback the
    endpoint must be `wss://` or `https://`. It asks for the Agent `name`, reported as
    `service.instance.name`. It asks for the CA certificate of the Server, written as
    `[tls] ca_file`, for every `wss://` or `https://` endpoint, without a question before it. It
    asks for the bootstrap identity only while the Client is not enrolled — while its state
    directory holds no certificate the Server issued: first for the bootstrap certificate, a PEM
    certificate or a PKCS#12 file. A path ending in `.p12` or `.pfx` is read as PKCS#12: the
    password is asked next, with input hidden, unless `--bootstrap-password-file` gave it
    (clause 18), and a password the file does not open under is asked again. Any other path is read
    as PEM, and its private key is asked next. It asks for the package verification key, written as
    `[packages] verification_key`; an empty answer is accepted, and the install then says that the
    Client installs no package, its own update included, until a key is configured.

    **Each question's default is what the file holds, else what lies beside the program**
    (clause 14), else none; the password has no default. The bootstrap identity is neither copied
    nor written into the file: the questionnaire enrols with it directly against the Server, after
    the file is written. It connects with the identity where it lies, sends the CSR (clause 9),
    prints the request's key fingerprint and waits until an operator approves it, then stores the
    issued certificate in the state directory, where the Client looks for it first. An interrupted
    wait leaves the written file and registers nothing; running the install again resumes the
    request in flight, printing its fingerprint. A rejected request stays rejected for its key, and
    the wait goes on until it is interrupted: removing the pending key and request from the state
    directory starts a fresh one. On an enrolled Client no file of the identity is asked: the
    Client renews what it was issued. A certificate already issued by the Server's client CA goes
    in `[tls] cert_file` and `key_file`, by editing the file. The identity is required while it is
    asked, and a path that names no readable file, here or for the CA, is asked for again.

    **The answered CA is copied beside the written file**, as `server-ca.pem`, owner-only on Unix,
    and the file names the copy. A copy left by an earlier answer is replaced. The copy is handed
    over to the service account with the file, as the install hands over the configuration
    ([ADR-0037](0037-the-accounts-the-service-runs-under.md)). The questionnaire asks nothing about
    the self-update: the file carries `[packages] self_update = true`, and the verification key
    decides whether that takes effect
    ([ADR-0018](0018-the-client-updates-itself-from-a-signed-package.md)). Everything else
    is written as commented defaults. No question asks for a credential, and no written file
    has an `[auth]` section; a leftover `[auth]` section is kept and ignored. The
    bootstrap password is not a credential of either plane: it opens a file on the host and is
    never sent.

17. **`service install --endpoint <URL>` writes the first configuration without a terminal, with
    what lies beside the program.** It writes the file `--interactive` writes, through the same
    renderer, with the endpoint given rather than asked: the same shape, mode `0600`, never
    overwriting an existing file, validated by the rule the Client applies to its endpoint at
    startup before anything is written — a `ws://` or `http://` endpoint whose host is not
    `127.0.0.1` or `::1` is refused, naming the setting, and `localhost` is no exception. It
    conflicts with `--interactive` and carries the self-update answer
    ([ADR-0018](0018-the-client-updates-itself-from-a-signed-package.md)). For a `wss://` or
    `https://` endpoint, a `server-ca.pem` beside the program — in the directory `--fleet-files`
    names, when given (clause 14) — is copied and written as `[tls] ca_file` as the questionnaire's
    answer would be, and a `package-signing-key.pub` beside it is written as
    `[packages] verification_key`. Each file it takes is printed. It has no flag naming the CA file
    or the client identity one by one: `--fleet-files` names the one directory the fixed names are
    looked up in, and `--bootstrap-password-file` (clause 18) the password of a `bootstrap.p12`,
    which is not a credential. It takes no credential, since there is none. The installers do not
    write TOML themselves. **The same rule binds every default an installer supplies:** no
    installer writes, prefills or prints a value the Client would refuse at startup, and none writes
    a setting that stands in for a client certificate.

18. **A non-interactive install enrols with a bootstrap identity it finds.** When the configuration
    — written by `--endpoint` or already on disk — has a `wss://` or `https://` endpoint, the
    Client is not enrolled, and a bootstrap identity lies beside the program, the install enrols
    with it as the questionnaire does (clause 16): it prints the request's key fingerprint, waits
    until an operator approves it or the wait is interrupted, and stores the issued certificate. It
    needs no terminal. An installer that runs the program from a directory holding a bootstrap
    identity therefore waits for an approval; one that runs it from where none lies does not.
    **For `bootstrap.p12` the password comes from
    `service install --bootstrap-password-file <path>`:** the file's first line, a trailing `\n`
    or `\r\n` removed. The file is read once and neither copied, changed nor removed. A
    `bootstrap.p12` taken without the flag, or a file it does not open under, fails the install
    before anything is written, naming the flag; the flag with no `bootstrap.p12` to open fails the
    same way. The flag names a password, not an identity or a credential. There is no environment
    variable and no command-line value for the password.

19. **A packaged host enrols in a step after the package.** The post-install of the `.deb` and the
    `.rpm` runs `service install` from the payload path as the Linux packages' install states
    ([ADR-0037](0037-the-accounts-the-service-runs-under.md)): it finds no fleet files and enrols
    nothing, so `apt` and `dnf` never wait for an approval. The host is enrolled by the step the
    post-install prints ([ADR-0037](0037-the-accounts-the-service-runs-under.md)), interactively,
    or by a script that puts the fleet's files in a directory, names it with `--fleet-files`, and
    runs the non-interactive install of clause 18:

    ```console
    sudo opamp-fleet service install --endpoint wss://<server>/v1/opamp \
      --fleet-files /root/fleet --bootstrap-password-file /root/fleet/bootstrap.pass
    sudo systemctl start opamp-fleet
    ```

    That step waits for the operator's approval as clause 18 states; a configuration-management
    tool runs it as a task that may wait. Removing the directory afterwards is the script's.

**Out of scope:**

- A PKCS#12 file for anything but a bootstrap certificate: the Client's issued identity, the
  Server's or a Gateway's certificate, and the `pki` commands of `opamp-fleetctl`.
- Changing the password of a kept PKCS#12 file, or making one for a certificate issued without a
  password; the operator issues a new certificate.
- A strength rule for the password beyond its length, and a password the Server generates.
- Removing `bootstrap.p12` or the password file from the host after enrolment, as
  [ADR-0019](0019-the-installed-service-and-its-state.md) leaves it for the pair.
- Reading a PKCS#12 file made by another tool beyond what clause 15 accepts.
- A `bootstrap.p12` named in `supervisor.toml`, so that the service enrols with it when it starts.
- Asking anything in a package's post-install, or preseeding answers through debconf or RPM
  macros.
- Deleting host certificates or their register entries; deleting a valid, unrevoked bootstrap
  certificate; removing revocation-list entries automatically once the certificate has ended,
  which the revocation list's own rules govern
  ([ADR-0029](0029-the-servers-state-and-its-stores.md)).
- Subject attributes beyond the six; per-host values (an asset number, a hostname), which a
  bootstrap certificate shared by many hosts cannot carry; changing a Host's values without a new
  enrolment; authorization by subject values; the ETSI EN 319 412-1 semantics of
  `organizationIdentifier` (`VATAT-…`), which a private fleet PKI does not need.
- A rate per bootstrap certificate, as opposed to a share of the queue; a bound per bootstrap
  certificate on how many hosts it may enrol in all.

## Alternatives considered

- **Issuing without a password, the pair handed out once as PEM in the response and held by the
  page.** It needs no password to be passed on and the Server keeps nothing. But the private key
  then travels unencrypted in an HTTP answer and sits in the page's memory, a closed tab costs a
  new certificate, and nothing is left to download in a later session. With the password required
  the key never leaves the Server unencrypted, and `opamp-fleetctl pki p12-extract` gives a host
  the PEM it may need.
- **Hand out both the PKCS#12 file and the PEM key.** The key would again leave in the clear, and
  the password would protect a copy nobody needs.
- **One PEM file holding the certificate and the key.** One file, no new dependency, but the key
  still travels in the clear and nothing on the Server is kept to download again.
- **An encrypted PKCS#8 key (`ENCRYPTED PRIVATE KEY`) beside the PEM certificate.** It encrypts the
  key with the same PBES2 scheme, but stays two files, and few operator tools import it as one
  identity the way they import PKCS#12.
- **The `p12` crate.** It reads and writes only the legacy schemes (RC2, 3DES, SHA-1 MAC), which
  OpenSSL 3 no longer writes by default.
- **OpenSSL's PKCS#12 support.** OpenSSL is banned from the build
  ([ADR-0012](0012-tls-1-3-two-planes-and-bounded-input.md), `deny.toml`).
- **The Server keeps the PEM key and hands it out again.** It would hold plaintext keys of every
  valid bootstrap certificate; the kept PKCS#12 file is useless without a password the Server
  never stores.
- **The Server keeps the password, or derives it.** Whoever reads the Server's state would then
  open every kept file, and the password would protect nothing the directory's mode does not.
- **The Server keeps nothing, and the operator keeps the `.p12`.** The human decided that the file
  stays downloadable in later sessions.
- **The server CA inside the PKCS#12 file.** Tools read further certificates in a PKCS#12 file as
  the chain of its key's certificate and import them as intermediate or root authorities; the
  server CA belongs to no such chain. It outlives the bootstrap identity, so the install would have
  to extract and keep it anyway, and the Server would deliver the anchor that verifies itself.
- **A password of 12 to 128 characters.** The stronger file, but operators found it too long for a
  password read out and typed by hand per host.
- **A password of 4 to 128 characters.** Short enough to read out, and an operator asked for it.
  Four characters of printable ASCII have about 8.1 × 10⁷ values, and under PBKDF2-HMAC-SHA-256 at
  600 000 iterations a leaked file opens in hours of GPU time. Whoever opens it files enrolment
  requests that take places in the queue and may arrive through Gateways, so the operator's
  approval would be the only barrier left. Eight characters have about 6.6 × 10¹⁵ values, out of
  practical reach of an offline attack on one file, and are still short enough to read out.
- **4 characters, with a warning below 12 in the UI and the API.** It keeps the choice and states
  the risk each time; the length rule itself decides instead.
- **A password the Server generates and shows once.** Strong without a choice to type, but the
  operator still has to carry it to the host, and the issuing route would return a secret. Left
  open.
- **Password from an environment variable or a command-line value.** A command line is visible in
  the process list; an environment variable is inherited by every child, the Supervisor's Managed
  Processes included, and readable under `/proc/<pid>/environ`.
- **The service enrols with a `bootstrap.p12` named in `supervisor.toml`** (`[tls] bootstrap_p12`,
  `bootstrap_password_file`). `apt install` alone would then do, but the password file lies beside
  the configuration, readable by the service account, for as long as nobody removes it, and the
  password protects nothing the file's mode does not. Enrolling on first start is rejected for the
  pair as well.
- **Preseeding the post-install (debconf, RPM macros).** debconf exists for the `.deb` only, keeps
  its answers in its database, and the post-install would wait for the approval while it holds the
  package database.
- **Let `service install --endpoint` write what the loader merely parses, and leave the security
  rule to the start.** The operator would learn at the first start, on a host nobody watches, what
  the installer could have refused while they were at the keyboard.
- **A fixed directory such as `/etc/<PRODUCT_NAME>/bootstrap/` that the install also searches.**
  It saves one flag, but makes the install look in two places, and a forgotten identity there
  would enrol the next install unasked.
- **Delete the revocation with a deleted entry.** A certificate still valid could enrol again the
  moment its row disappeared.
- **Delete a valid certificate as well, revoking it on the way.** One press would do two
  irreversible things; two presses keep revocation the deliberate act it is.
- **Hide revoked rows in the UI only, behind a filter.** The record would keep the entries the
  operator wanted gone, and a script would still see them; the REST API is the integration
  contract (G-5).
- **Organization and organizational unit optional.** A bootstrap certificate could then name no
  customer, `fleet.host.organization` and `fleet.host.site` would be missing on some Hosts, and a
  Selector on them would silently skip those. The human made both required.
- **O and OU only.** Enough for customer and site, but the human wanted the location and a
  customer number optional beside them.
- **Free key-value pairs as SAN URIs (`urn:opamp-fleet:attr:<key>=<value>`).** They extend without
  bound and map one-to-one onto labels. But they are not standard, other tools do not show them as
  a subject, and they invite per-host values into a certificate shared by many hosts.
- **A private extension under an enterprise OID.** It can carry anything, but needs a Private
  Enterprise Number, and nothing outside this project reads it.
- **`serialNumber` (2.5.4.5), `street`, `postalCode`, `title`, `description`, personal names or an
  e-mail address.** Each is either per host, unstable, free text, or personal data that would land
  in every certificate and audit line.
- **Copy the values from the CSR.** The host chooses them, so a host could name any customer.
- **Store the values on the Host only, not in the certificate.** Simpler to issue, but the
  certificate would no longer say whom it belongs to, and a Host record rebuilt from the register
  would lose them.
- **Skip the check at approval and trust the bootstrap CA.** A foreign bootstrap CA may write
  anything. A refusal that names the attribute costs one re-issue.
- **A small fixed bound on the queue per certificate, such as 16.** It stops a flood sooner, but a
  bootstrap certificate handed to one site could not enrol more than 16 hosts in one window.
- **No bound per certificate, relying on revocation.** Revoking the certificate withdraws its
  waiting requests and ends the flood, but only once an operator sees it, and every honest host is
  refused until then.
- **A bound per peer address.** Hosts behind one NAT or one Gateway share an address, and one
  attacker can use many. The certificate is what the Server can count reliably.
- **Grow the queue.** A larger queue moves the bound but does not stop one certificate from taking
  all of it.
- **No decision.** The key travels in the clear, a closed tab costs a new certificate, a scripted
  install of a PKCS#12 identity is impossible, and a host certificate says nothing about whom it
  belongs to.
- **The host sends a CSR for its bootstrap certificate.** No key travels, but the bootstrap
  certificate exists because the host has no channel to the Server yet, and one bootstrap
  certificate per customer is handed to many hosts.
- **The UI lists and revokes, and issuing stays with the API.** The private key never passes
  through a browser, but it reaches the operator's machine either way, in an HTTP response read by
  `curl` or by the page, and the operator who approves leaves the tab to issue.
- **A free-form subject as an RFC 4514 string.** It is shorter to specify, but it lets any
  attribute type and encoding into the certificate, which the Server then shows back and compares.
  A fixed set of named fields is what an operator fills in a form.
- **The CSR's subject in the host certificate, or the client CA's O and OU in every host
  certificate.** The first shows the Agent's name in the register, but that name is self-asserted,
  and the register already lists the host and the `instance_uid` of each certificate
  ([ADR-0029](0029-the-servers-state-and-its-stores.md) clause 9); the second repeats the issuer in
  the subject and takes the place the bootstrap certificate's subject is carried into.
- **An enrolment request matched to its bootstrap certificate by the certificate's
  fingerprint.** The record would keep the SHA-256 of the whole certificate as well, but the
  operator revokes and reads the audit by serial, so the fingerprint is a second name for the same
  entry (clause 10).

## Sources / Prior art

- [ADR-0021](0021-admission-by-client-certificate-and-revocation.md) (admission, the Operator
  plane, revocation), [ADR-0015](0015-client-modes-and-the-gateway.md) (approval through a proved
  bootstrap certificate, renewal, enrolment through a Gateway),
  [ADR-0019](0019-the-installed-service-and-its-state.md) and
  [ADR-0037](0037-the-accounts-the-service-runs-under.md) (the installed service and the Linux
  packages), [ADR-0029](0029-the-servers-state-and-its-stores.md) (the record of issued bootstrap
  certificates), [ADR-0032](0032-the-operator-program-opamp-fleetctl.md) (certificate names,
  `pki p12-extract`), [ADR-0023](0023-an-append-only-audit-record.md) (the audit record).
- RFC 7292, PKCS #12 v1.1: <https://www.rfc-editor.org/rfc/rfc7292>. RFC 8018, PKCS #5 v2.1
  (PBES2, PBKDF2): <https://www.rfc-editor.org/rfc/rfc8018>.
- RFC 9579, PBMAC1 in PKCS #12, obsoleted by RFC 9879 — the MAC this ADR does not use, since
  `p12-keystore` 0.4.1 has no PBMAC1 in its source: <https://www.rfc-editor.org/rfc/rfc9579>.
- OpenSSL 3.0 `openssl-pkcs12(1)`, read from the manual page of OpenSSL 3.0.22: AES-256-CBC with
  PBKDF2 and a SHA-256 MAC are the defaults, at 2048 iterations; RC2 and 3DES are the `-legacy`
  option. <https://docs.openssl.org/3.0/man1/openssl-pkcs12/>
- OWASP Password Storage Cheat Sheet: PBKDF2-HMAC-SHA-256 at 600 000 iterations. It slows each
  guess, but does not make a small value space large. Recalled, not re-read for the password rule.
  <https://cheatsheetseries.owasp.org/cheatsheets/Password_Storage_Cheat_Sheet.html>
- NIST SP 800-63B, section 5.1.1.2: a memorized secret of at least 8 characters, unless the
  verifier rate-limits online guessing. Recalled, not re-read.
- `p12-keystore` 0.4.1, read from its published crate
  (<https://crates.io/crates/p12-keystore/0.4.1>, <https://github.com/ancwrd1/p12-keystore>):
  MIT or Apache-2.0; Rust 1.85; writes `PbeWithHmacSha256AndAes256` with an HMAC-SHA-256 MAC by
  default, 10 000 iterations unless set; refuses MAC, PBES1 and PBKDF2 iteration counts above
  1 000 000 on import (0.3.2); `pbes1` is a default feature that pulls `rc2`, `des` and `cbc`.
- A probe crate depending on `p12-keystore = "=0.4.1"` with `default-features = false`, compared
  by `cargo tree` against this workspace: 21 crates are new (`p12-keystore`, `pkcs12`
  0.2.0-pre.0, `cms` 0.3.0-pre.1, `pkcs5`, `pkcs8`, `spki`, `der`, `der_derive`, `x509-cert`,
  `pem-rfc7468`, `pbkdf2`, `scrypt`, `salsa20`, `hmac` 0.13, `aes-gcm`, `aead`, `ctr`, `ghash`,
  `polyval`, `universal-hash`, `flagset`); `aes`, `cbc`, `sha1`, `sha2`, `rand` and `x509-parser`
  are already present at the same versions; `rc2` and `des` are absent.
- `dialoguer` 0.12: the feature `password` enables `Password` and depends on `zeroize` only.
- ITU-T X.520 / RFC 5280: the attribute types C, ST, L, O, OU and organizationIdentifier
  (2.5.4.97); `countryName` as a two-letter `PrintableString`.
- ETSI EN 319 412-1, section 5.1.4: the structured semantics of `organizationIdentifier` for eIDAS
  certificates, which this decision does not adopt.
- ISO 3166-1 alpha-2 for the country codes.
- The code: `crates/fleet-server/src/ca.rs` (the host certificate's subject is set, the CSR's
  discarded), `crates/fleet-server/src/bootstrap.rs` (`Subject`), `MAX_PENDING` and `submit` in
  `crates/fleet-server/src/enrolment.rs`, where the bootstrap certificate's `CertId` is part of
  every request's `Requester`, and `enrol` in `crates/fleet-server/src/transport.rs`.

## Consequences

- Positive: the key leaves the Server only encrypted, and an operator can hand the same bootstrap
  certificate to a second host, or download it again after closing the tab, without issuing a new
  one (G-17). One file per identity on the host, which operators' tools can also inspect.
- Positive: a scripted install of a PKCS#12 identity needs one flag and one file (G-11), and a host
  installed from a `.deb` or `.rpm` is enrolled by configuration management with one command after
  the package, without the package manager waiting.
- Positive: a host certificate says whom it belongs to, and the Host's subject values come from a
  certificate the Server signed; a Selector on them cannot be forged by an Agent. The enrolment
  queue shows the values that will be carried, not a name the host chose.
- Positive: one bootstrap certificate can block at most a quarter of the queue by default; four
  certificates would be needed to fill it, and each can be revoked on its own.
- Positive: a revoked certificate leaves the list in two presses instead of staying up to 90 days,
  no longer counts toward the 10 000 entries, and every deletion is on the audit record.
- Negative / trade-offs: the Server keeps key material, encrypted, for up to 90 days per bootstrap
  certificate. Whoever reads the kept files and guesses a password joins the enrolment queue; the
  8-character minimum and 600 000 iterations bound the guessing, and the operator's approval still
  stands between the queue and the fleet. An operator types the password per host.
- Negative / trade-offs: 21 crates enter the build, two of them pre-releases (`pkcs12`, `cms`).
  Every bump is reviewed as a dependency change (Q-3).
- Negative / trade-offs: a PKCS#12 file made by another tool with legacy schemes or a PBMAC1 MAC is
  refused by the Client; such a file is re-exported with OpenSSL 3's defaults.
- Negative / trade-offs: the password file of a scripted install lies on the host until someone
  removes it, as the pair does.
- Negative / trade-offs: a value fixed at issue stays until the host enrols again. A customer
  renamed means a re-enrolment of its hosts, or Deployments keyed on `organization_identifier`
  instead of the name. A bootstrap certificate from a foreign PKI with a malformed subject cannot
  be approved; the operator issues one that fits. Certificates issued without subject values give
  Hosts with none until they enrol again.
- Negative / trade-offs: a site that enrols more than 256 hosts with one bootstrap certificate in
  one window waits for the operator to approve some, or raises the key.
- Negative / trade-offs: a deleted entry's revocation lives on in the revocation list, where only
  `GET /api/v1/revocations` shows it; a pending request that still carries the certificate reads
  *not issued here*, which hides that this Server once issued it.
- Follow-ups: a view of the revocation list in the UI; the operator manual's installation and
  enrolment pages and the OpenAPI document of the routes; a fuzz target for the Client's PKCS#12
  reader; a rate per bootstrap certificate, which enrolment through a Gateway needs, since all of
  its requests arrive from one address; a password the Server generates.

## Enforcement

Tests that will carry `Verifies: ADR-0022`:

- Issuing without `organization`, without `organizational_unit` or without `password` is `400`,
  naming the field; the answer to an issue never carries `private_key_pem` (clauses 1 and 3).
- Issuing writes the PKCS#12 file owner-only, and the download route returns a file that opens
  under the password and holds exactly the certificate and its key (clauses 3 and 4).
- `check_password` takes 8 and 128 characters, and answers 7 and 129 characters and a control
  character with its `400` text. The Server's issuing route answers `400` for a password of 7
  characters and issues one of 8 (clause 5).
- Issuance and download write their audit entries
  ([ADR-0029](0029-the-servers-state-and-its-stores.md) clause 11), and neither entry nor any log
  line holds the password (clause 5).
- Issuing with all six attributes yields a bootstrap certificate whose subject holds them in the
  order of clause 1, with `country` as a `PrintableString`. A lower-case or three-letter country,
  an empty or 65-character value, a control character and an unknown key are each `400`
  (clause 1).
- A foreign bootstrap certificate without OU is refused at approval with `409` naming
  `organizational_unit`; one from another CA with OU twice is refused at approval with `409`
  naming the attribute, and nothing is signed (clause 10, and the approval's rules of
  [ADR-0015](0015-client-modes-and-the-gateway.md)).
- Approving a request copies the bootstrap certificate's values into the host certificate and
  ignores the CSR's subject (clause 11).
- A renewal carries the predecessor's values unchanged (clause 12).
- `GET /api/v1/enrolments` lists the bootstrap certificate's subject values and no CSR subject
  (clause 10).
- With `max_pending_per_bootstrap = 2`, a third request of one bootstrap certificate is answered
  `Unavailable` and audited, a request of another bootstrap certificate is still queued, and a
  re-sent request of the first certificate still joins its entry. `max_pending_per_bootstrap = 0`
  and `= 1025` fail startup naming the key (clause 9).
- Deleting a revoked entry answers `204`, removes the entry and its PKCS#12 file, writes
  `enrolment.bootstrap_deleted`, and leaves the revocation on the list: the certificate is still
  refused at the handshake. Deleting a valid entry answers `409` and an unknown serial `404`
  (clause 7); a refused audit answers `503` with the entry kept
  ([ADR-0029](0029-the-servers-state-and-its-stores.md) clause 11).
- `bootstrap.p12` is found beside the program; lying beside a file of the pair, neither is taken.
  With `--fleet-files <dir>` the files are found in `<dir>` and not beside the program, and
  `--endpoint` copies `server-ca.pem` from it; a `<dir>` that is no readable directory fails the
  install before anything is written (clauses 14 and 17).
- The Client refuses a file over 64 KiB before reading it, a wrong password, a PBES1 file, and a
  file with no key or a key without its certificate (clause 15).
- A non-interactive install with `bootstrap.p12` and `--bootstrap-password-file` enrols; without
  the flag, or with the flag and no file, it fails before writing anything (clause 18).
- A fuzz target for the Client's PKCS#12 reader runs in CI (Q-2).
- `cargo deny` keeps `rc2` and `des` out of the graph: a `[bans]` entry names them (clause 13).

**Not mechanically decidable:** that the UI never stores a typed password, that the server CA is
never put into a PKCS#12 file, and that the tab offers **Delete** only on revoked rows and asks
before it deletes are held by review (clauses 6, 7 and 8); that the `.deb` and `.rpm` post-install
enrols nothing is held by review of `packaging/linux/postinst` (clause 19).
